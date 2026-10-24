//! Embeddable wallpaper runtime: load a wallpaper and drive its render loop.

mod files;
mod frame;
pub mod material;
mod video;

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use chrono::Timelike;
use glam::Vec2;
use wpe_assets::tex::{Mip, TexFormat, TexImage, Texture};
use wpe_render::{FrameGlobals, Gpu, GpuTexture, SceneRenderer, wgpu};
use wpe_scene::{OrthoCamera, Scene};

use frame::FrameBuilder;

pub use files::{LoadError, WallpaperFiles};
pub use material::{SlotTexture, TextureStore};
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

/// A loaded wallpaper ready to render.
pub struct Wallpaper {
    pub title: String,
    pub fill: FillMode,
    camera: OrthoCamera,
    renderer: SceneRenderer,
    videos: Vec<(Arc<GpuTexture>, video::VideoStream)>,
    start: Instant,
    /// Cursor over the output, 0..1 with Y down.
    pointer: Vec2,
    /// `general.cameraparallaxmouseinfluence`.
    parallax_influence: f32,
    /// Fixed scene time in seconds instead of the wall clock.
    pub time_override: Option<f32>,
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
        let props = wpe_scene::UserProperties::from_project(&files.project);
        let scene = Scene::build(vfs, &files.scene, &props, |name| store.size(vfs, name));
        tracing::info!(
            "scene {}: {}x{}, {} image layers",
            files.scene_file,
            scene.camera.scene_size.x,
            scene.camera.scene_size.y,
            scene.layers.len()
        );

        let mut builder = FrameBuilder::new(gpu, vfs, store, white_texture());
        builder.layers(&scene.layers);
        if let Some(bloom) = scene.bloom {
            builder.bloom(bloom);
        }
        let (real, total) = builder.stats;
        tracing::info!("{real} of {total} draws use their real shader");
        let frame = builder.finish();

        let mut renderer = SceneRenderer::new(gpu, output_format);
        let c = scene.clear_color;
        renderer.clear_color = wgpu::Color {
            r: c.x as f64,
            g: c.y as f64,
            b: c.z as f64,
            a: 1.0,
        };
        renderer.set_frame(gpu, frame.targets, frame.steps);

        Ok(Self {
            title: files
                .project
                .title
                .clone()
                .unwrap_or(files.scene_file.clone()),
            fill: FillMode::default(),
            camera: scene.camera,
            renderer,
            videos: frame.videos,
            start: Instant::now(),
            pointer: Vec2::splat(0.5),
            parallax_influence: files.scene.general.cameraparallaxmouseinfluence.value,
            time_override: None,
        })
    }

    fn upload_video_frames(&mut self, gpu: &Gpu) {
        for (texture, stream) in &self.videos {
            if let Some(frame) = stream.take_frame() {
                texture.write_rgba(gpu, &frame);
            }
        }
    }

    /// Cursor position over the output, 0..1 with Y down.
    pub fn set_pointer(&mut self, pointer: Vec2) {
        self.pointer = pointer;
    }

    fn globals(&self, size: (u32, u32)) -> FrameGlobals {
        let now = chrono::Local::now().time();
        let centred = Vec2::new(self.pointer.x - 0.5, 0.5 - self.pointer.y);
        FrameGlobals {
            time: self
                .time_override
                .unwrap_or_else(|| self.start.elapsed().as_secs_f32()),
            daytime: now.num_seconds_from_midnight() as f32 / 86_400.0,
            pointer: self.pointer,
            parallax: Vec2::splat(0.5) + centred * self.parallax_influence,
            view_proj: self.camera.view_proj(size, self.fill),
            screen: Vec2::new(size.0 as f32, size.1 as f32),
            eye: (self.camera.scene_size / 2.0).extend(1000.0),
        }
    }

    /// Record one frame into `output`, which is `size` pixels.
    pub fn render(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
        size: (u32, u32),
    ) {
        self.upload_video_frames(gpu);
        let globals = self.globals(size);
        self.renderer.render(gpu, encoder, globals, output, size);
    }

    /// Render one frame offscreen and return it as RGBA8 pixels.
    pub fn capture(&mut self, gpu: &Gpu, size: (u32, u32)) -> Vec<u8> {
        for (_, stream) in &self.videos {
            stream.wait_started(std::time::Duration::from_secs(5));
        }
        self.upload_video_frames(gpu);
        let globals = self.globals(size);
        self.renderer.capture(gpu, globals, size)
    }
}
