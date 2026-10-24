//! Embeddable wallpaper runtime: load a wallpaper and drive its render loop.

mod files;
pub mod material;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use glam::{Mat4, Vec4};
use wpe_assets::material::Blending;
use wpe_assets::tex::{Mip, TexFormat, TexImage, Texture};
use wpe_render::{BlendMode, Gpu, GpuTexture, ImageDraw, SceneRenderer, upload_texture, wgpu};
use wpe_scene::{ImageLayer, OrthoCamera, Scene};

pub use files::{LoadError, WallpaperFiles};
pub use material::TextureStore;
pub use wpe_scene::FillMode;

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
        let files = WallpaperFiles::open_with_assets_dir(assets, path)?;
        let vfs = &files.vfs;
        let mut store = TextureStore::default();
        let scene = Scene::build(vfs, &files.scene, |name| store.size(vfs, name));
        tracing::info!(
            "scene {}: {}x{}, {} image layers",
            files.scene_file,
            scene.camera.scene_size.x,
            scene.camera.scene_size.y,
            scene.layers.len()
        );

        let mut uploaded: HashMap<String, Arc<GpuTexture>> = HashMap::new();
        let mut upload = |name: &str| -> Option<Arc<GpuTexture>> {
            if let Some(t) = uploaded.get(name) {
                return Some(t.clone());
            }
            let tex = store.get(vfs, name)?;
            let gpu_tex = Arc::new(upload_texture(gpu, &tex, name));
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
            title: files
                .project
                .title
                .clone()
                .unwrap_or(files.scene_file.clone()),
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
