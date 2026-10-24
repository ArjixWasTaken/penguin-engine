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

fn rgba_texture(width: u32, height: u32, pixels: Vec<u8>) -> Texture {
    Texture {
        format: TexFormat::Rgba8,
        flags: wpe_assets::tex::TexFlags::CLAMP_UVS,
        texb_version: 0,
        width,
        height,
        map_width: width,
        map_height: height,
        depth: 1,
        images: vec![TexImage {
            mips: vec![Mip {
                width,
                height,
                depth: 1,
                data: pixels,
            }],
        }],
        sprite_frames: Vec::new(),
        variants: Vec::new(),
        video: None,
    }
}

/// Rasterise every text layer, sizing and anchoring its card and making its
/// image available to its material.
fn render_text_layers(
    vfs: &wpe_vfs::Vfs,
    props: &wpe_scene::UserProperties,
    scene: &mut Scene,
    store: &mut TextureStore,
) {
    if !scene.layers.iter().any(|l| l.text.is_some()) {
        return;
    }
    let mut renderer = wpe_text::TextRenderer::new();
    let user = |name: &str| props.get(name).cloned();
    for layer in &mut scene.layers {
        let Some(obj) = layer.text.as_deref() else {
            continue;
        };
        let style = wpe_text::TextStyle::from_object(obj, user);
        let text = wpe_text::resolve_text(obj, user);
        match renderer.render(vfs, &style, &text) {
            Ok(image) => {
                layer.size = glam::Vec2::from_array(image.card_size);
                layer.motion.local_offset = glam::Vec2::from_array(image.anchor_offset).extend(0.0);
                layer.transform = layer.motion.transform_at(0.0);
                store.insert(
                    wpe_scene::text_texture_name(layer.id),
                    rgba_texture(image.width, image.height, image.pixels),
                );
            }
            Err(e) => tracing::warn!("text layer {:?}: {e}", layer.name),
        }
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

/// A loaded wallpaper ready to render.
pub struct Wallpaper {
    pub title: String,
    pub fill: FillMode,
    camera: OrthoCamera,
    renderer: SceneRenderer,
    videos: Vec<(Arc<GpuTexture>, video::VideoStream)>,
    bindings: Vec<frame::Binding>,
    start: Instant,
    /// Smoothed cursor over the output, 0..1 with Y down.
    pointer: Vec2,
    /// Where the cursor actually is.
    pointer_target: Vec2,
    /// Wall-clock time of the last frame, for pointer smoothing.
    last_frame: Option<Instant>,
    parallax_delay: f32,
    shake: Option<wpe_scene::Shake>,
    /// `general.cameraparallaxmouseinfluence`.
    parallax_influence: f32,
    /// Fixed scene time in seconds instead of the wall clock.
    pub time_override: Option<f32>,
    audio: Option<wpe_audio::AudioCapture>,
    sounds: Option<wpe_audio::SoundOutput>,
    /// Encoded files and settings of each sound layer, played on demand.
    sound_layers: Vec<SoundLayer>,
}

struct SoundLayer {
    files: Vec<Vec<u8>>,
    volume: f32,
    mode: wpe_audio::Playback,
}

fn sound_layers(files: &WallpaperFiles, props: &wpe_scene::UserProperties) -> Vec<SoundLayer> {
    files
        .scene
        .objects
        .iter()
        .filter_map(|obj| match obj {
            wpe_assets::scene::SceneObject::Sound(s) => Some(s),
            _ => None,
        })
        .filter(|s| !s.startsilent && props.flag(&s.common.visible))
        .filter_map(|s| {
            let data: Vec<Vec<u8>> = s
                .sound
                .iter()
                .filter_map(|path| {
                    files
                        .vfs
                        .read(path)
                        .inspect_err(|e| tracing::warn!("sound {path}: {e}"))
                        .ok()
                        .map(|b| b.into_owned())
                })
                .collect();
            let mode = match s.playbackmode.as_deref() {
                Some("single") => wpe_audio::Playback::Single,
                _ => wpe_audio::Playback::Loop,
            };
            (!data.is_empty()).then_some(SoundLayer {
                files: data,
                volume: props.value(&s.volume),
                mode,
            })
        })
        .collect()
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
        let mut scene = Scene::build(vfs, &files.scene, &props, |name| store.size(vfs, name));
        render_text_layers(vfs, &props, &mut scene, &mut store);
        tracing::info!(
            "scene {}: {}x{}, {} image layers",
            files.scene_file,
            scene.camera.scene_size.x,
            scene.camera.scene_size.y,
            scene.layers.len()
        );

        let mut builder = FrameBuilder::new(gpu, vfs, store, white_texture());
        builder.set_parallax(scene.parallax, scene.camera.scene_size);
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
            bindings: frame.bindings,
            start: Instant::now(),
            pointer: Vec2::splat(0.5),
            pointer_target: Vec2::splat(0.5),
            last_frame: None,
            parallax_delay: scene.parallax.map_or(0.0, |p| p.delay),
            shake: scene.shake,
            parallax_influence: files.scene.general.cameraparallaxmouseinfluence.value,
            time_override: None,
            audio: None,
            sounds: None,
            sound_layers: sound_layers(&files, &props),
        })
    }

    /// Start or stop audio: playing the wallpaper's sound layers and
    /// analysing system audio for audio-reactive shaders.
    pub fn set_audio(&mut self, enabled: bool) {
        if !enabled {
            self.audio = None;
            self.sounds = None;
            return;
        }
        if self.audio.is_none() {
            self.audio = wpe_audio::AudioCapture::start()
                .inspect_err(|e| tracing::warn!("audio capture unavailable: {e}"))
                .ok();
        }
        if self.sounds.is_none() && !self.sound_layers.is_empty() {
            match wpe_audio::SoundOutput::open() {
                Ok(mut out) => {
                    for layer in &self.sound_layers {
                        if let Err(e) = out.play(layer.files.clone(), layer.volume, layer.mode) {
                            tracing::warn!("sound layer: {e}");
                        }
                    }
                    self.sounds = Some(out);
                }
                Err(e) => tracing::warn!("audio output unavailable: {e}"),
            }
        }
    }

    fn update_audio(&mut self) {
        let Some(capture) = &mut self.audio else {
            return;
        };
        let spectrum = capture.spectrum();
        for (name, values) in spectrum.uniforms() {
            self.renderer.set_global_uniform(name, values);
        }
    }

    fn animate(&mut self, time: f32) {
        let frame = frame::FrameState {
            time,
            pointer: self.pointer,
        };
        for binding in &self.bindings {
            binding.apply(&mut self.renderer, frame);
        }
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
        self.pointer_target = pointer;
    }

    /// Move the smoothed pointer towards the cursor (`cameraparallaxdelay`).
    fn smooth_pointer(&mut self) {
        let now = Instant::now();
        let dt = self.last_frame.map_or(0.0, |t| (now - t).as_secs_f32());
        self.last_frame = Some(now);
        let t = if self.parallax_delay > 0.0 {
            (dt / self.parallax_delay).min(1.0)
        } else {
            1.0
        };
        self.pointer += (self.pointer_target - self.pointer) * t;
    }

    fn globals(&self, size: (u32, u32)) -> FrameGlobals {
        let now = chrono::Local::now().time();
        let centred = Vec2::new(self.pointer.x - 0.5, 0.5 - self.pointer.y);
        let time = self
            .time_override
            .unwrap_or_else(|| self.start.elapsed().as_secs_f32());
        let mut view_proj = self.camera.view_proj(size, self.fill);
        if let Some(shake) = &self.shake {
            let offset = shake.offset(time) * self.camera.scene_size * 0.01;
            view_proj *= glam::Mat4::from_translation(offset.extend(0.0));
        }
        FrameGlobals {
            time,
            daytime: now.num_seconds_from_midnight() as f32 / 86_400.0,
            pointer: self.pointer,
            parallax: Vec2::splat(0.5) + centred * self.parallax_influence,
            view_proj,
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
        self.smooth_pointer();
        let globals = self.globals(size);
        self.animate(globals.time);
        self.update_audio();
        self.renderer.render(gpu, encoder, globals, output, size);
    }

    /// Render one frame offscreen and return it as RGBA8 pixels.
    pub fn capture(&mut self, gpu: &Gpu, size: (u32, u32)) -> Vec<u8> {
        for (_, stream) in &self.videos {
            stream.wait_started(std::time::Duration::from_secs(5));
        }
        self.upload_video_frames(gpu);
        let globals = self.globals(size);
        self.animate(globals.time);
        self.renderer.capture(gpu, globals, size)
    }
}
