//! Embeddable wallpaper runtime: load a wallpaper and drive its render loop.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use glam::{Mat4, Vec2, Vec4};
use wpe_assets::material::Blending;
use wpe_assets::tex::{Mip, TexFormat, TexImage, Texture};
use wpe_assets::{AssetError, Project, load_json, resolve_scene_file};
use wpe_render::{BlendMode, Gpu, GpuTexture, ImageDraw, SceneRenderer, upload_texture, wgpu};
use wpe_scene::{ImageLayer, OrthoCamera, Scene};
use wpe_vfs::{PhysicalFs, PkgFs, Vfs, VfsError};

pub use wpe_scene::FillMode;

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error(transparent)]
    Vfs(#[from] VfsError),
    #[error(transparent)]
    Asset(#[from] AssetError),
    #[error("{0} is not a wallpaper directory, .pkg or scene .json")]
    BadPath(PathBuf),
}

/// Where a wallpaper's files come from.
struct Source {
    dir: PathBuf,
    pkg: Option<PathBuf>,
    scene_file: Option<String>,
}

impl Source {
    fn resolve(path: &Path) -> Result<Self, LoadError> {
        let bad = || LoadError::BadPath(path.to_owned());
        if path.is_dir() {
            let pkg = path.join("scene.pkg");
            return Ok(Self {
                dir: path.to_owned(),
                pkg: pkg.is_file().then_some(pkg),
                scene_file: None,
            });
        }
        let dir = path.parent().ok_or_else(bad)?.to_owned();
        let name = path
            .file_name()
            .ok_or_else(bad)?
            .to_string_lossy()
            .into_owned();
        match path.extension().and_then(|e| e.to_str()) {
            Some("pkg") => Ok(Self {
                dir,
                pkg: Some(path.to_owned()),
                scene_file: None,
            }),
            Some("json") => Ok(Self {
                dir,
                pkg: None,
                scene_file: Some(name),
            }),
            _ => Err(bad()),
        }
    }
}

/// Mount the stock assets, then the wallpaper directory, then its package.
fn build_vfs(assets: &Path, source: &Source) -> Result<Vfs, LoadError> {
    let mut vfs = Vfs::new();
    vfs.mount("assets", PhysicalFs::new(assets)?);
    vfs.mount("wallpaper", PhysicalFs::new(&source.dir)?);
    if let Some(pkg) = &source.pkg {
        vfs.mount("pkg", PkgFs::open(pkg)?);
    }
    Ok(vfs)
}

/// Path of the `.tex` a material texture name refers to.
fn texture_path(vfs: &Vfs, name: &str) -> String {
    if name.ends_with(".tex") && vfs.contains(name) {
        name.to_owned()
    } else {
        format!("materials/{name}.tex")
    }
}

fn white_texture() -> Texture {
    Texture {
        format: TexFormat::Rgba8,
        flags: Default::default(),
        texb_version: 0,
        width: 1,
        height: 1,
        map_width: 1,
        map_height: 1,
        depth: 1,
        images: vec![TexImage {
            mips: vec![Mip {
                width: 1,
                height: 1,
                depth: 1,
                data: vec![255; 4],
            }],
        }],
        sprite_frames: Vec::new(),
        variants: Vec::new(),
        video: None,
    }
}

fn blend_mode(b: &Blending) -> BlendMode {
    match b {
        Blending::Normal | Blending::Disabled => BlendMode::Normal,
        Blending::Additive => BlendMode::Additive,
        Blending::TranslucentPa => BlendMode::TranslucentPremultiplied,
        _ => BlendMode::Translucent,
    }
}

/// A loaded wallpaper ready to render.
pub struct Wallpaper {
    pub title: String,
    pub fill: FillMode,
    camera: OrthoCamera,
    renderer: SceneRenderer,
}

impl Wallpaper {
    /// Load the wallpaper at `path` (a directory with `project.json`, a
    /// `.pkg`, or a scene `.json`) against the stock `assets` directory.
    pub fn load(
        gpu: &Gpu,
        output_format: wgpu::TextureFormat,
        assets: &Path,
        path: &Path,
    ) -> Result<Self, LoadError> {
        let source = Source::resolve(path)?;
        let vfs = build_vfs(assets, &source)?;

        let project_path = source.dir.join("project.json");
        let project: Project = if project_path.is_file() {
            wpe_assets::parse_json(&std::fs::read_to_string(&project_path).map_err(|e| {
                VfsError::Io {
                    path: project_path.display().to_string(),
                    source: e,
                }
            })?)
            .map_err(|source| AssetError::Json {
                path: project_path.display().to_string(),
                source,
            })?
        } else {
            Project::default()
        };
        let scene_file = source
            .scene_file
            .clone()
            .unwrap_or_else(|| resolve_scene_file(&project, &vfs));
        let scene_json: wpe_assets::Scene = load_json(&vfs, &scene_file)?;

        let textures: RefCell<HashMap<String, Option<Texture>>> = RefCell::default();
        let load_texture = |name: &str| {
            textures
                .borrow_mut()
                .entry(name.to_owned())
                .or_insert_with(|| {
                    let path = texture_path(&vfs, name);
                    let parsed = vfs
                        .read(&path)
                        .map_err(|e| e.to_string())
                        .and_then(|b| Texture::parse(&b).map_err(|e| e.to_string()));
                    parsed
                        .inspect_err(|e| tracing::warn!("texture {name}: {e}"))
                        .ok()
                })
                .as_ref()
                .map(|t| match t.sprite_frames.first() {
                    Some(f) if t.is_sprite() => Vec2::new(f.width, f.height),
                    _ => Vec2::new(t.map_width as f32, t.map_height as f32),
                })
        };
        let scene = Scene::build(&vfs, &scene_json, load_texture);
        tracing::info!(
            "scene {scene_file}: {}x{}, {} image layers",
            scene.camera.scene_size.x,
            scene.camera.scene_size.y,
            scene.layers.len()
        );

        let mut textures = textures.into_inner();
        let mut uploaded: HashMap<String, Arc<GpuTexture>> = HashMap::new();
        let mut upload = |name: &str| -> Option<Arc<GpuTexture>> {
            if let Some(t) = uploaded.get(name) {
                return Some(t.clone());
            }
            let tex = textures
                .entry(name.to_owned())
                .or_insert_with(|| {
                    vfs.read(&texture_path(&vfs, name))
                        .ok()
                        .and_then(|b| Texture::parse(&b).ok())
                })
                .as_ref()?;
            let gpu_tex = Arc::new(upload_texture(gpu, tex, name));
            uploaded.insert(name.to_owned(), gpu_tex.clone());
            Some(gpu_tex)
        };
        let white = Arc::new(upload_texture(gpu, &white_texture(), "white"));

        let draws = scene
            .layers
            .iter()
            .filter_map(|layer| {
                let texture = match layer.texture(0) {
                    Some(name) if name.starts_with("_rt_") => {
                        tracing::debug!(
                            "layer {:?}: render target inputs not supported yet",
                            layer.name
                        );
                        return None;
                    }
                    Some(name) => upload(name)?,
                    None if layer.solid => white.clone(),
                    None => return None,
                };
                Some(image_draw(layer, texture))
            })
            .collect();

        let mut renderer = SceneRenderer::new(gpu, output_format);
        let c = scene.clear_color;
        renderer.clear_color = wgpu::Color {
            r: c.x as f64,
            g: c.y as f64,
            b: c.z as f64,
            a: 1.0,
        };
        renderer.set_draws(gpu, draws);

        Ok(Self {
            title: project.title.unwrap_or_else(|| scene_file.clone()),
            fill: FillMode::default(),
            camera: scene.camera,
            renderer,
        })
    }

    fn view_proj(&self, size: (u32, u32)) -> Mat4 {
        self.camera.view_proj(size, self.fill)
    }

    /// Record one frame into `output`, which is `size` pixels.
    pub fn render(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
        size: (u32, u32),
    ) {
        let vp = self.view_proj(size);
        self.renderer.render(gpu, encoder, vp, output, size);
    }

    /// Render one frame offscreen and return it as RGBA8 pixels.
    pub fn capture(&mut self, gpu: &Gpu, size: (u32, u32)) -> Vec<u8> {
        let vp = self.view_proj(size);
        self.renderer.capture(gpu, vp, size)
    }
}

fn image_draw(layer: &ImageLayer, texture: Arc<GpuTexture>) -> ImageDraw {
    ImageDraw {
        model: layer.transform,
        size: layer.size,
        color: Vec4::from((layer.color * layer.brightness, layer.alpha)),
        blend: blend_mode(&layer.material.blending),
        texture,
    }
}
