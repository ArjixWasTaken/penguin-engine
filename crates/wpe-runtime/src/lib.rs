//! Embeddable wallpaper runtime: load a wallpaper and drive its render loop.

mod clock;
mod files;
mod frame;
pub mod material;
mod media;
mod script;
mod sound;
mod video;

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use chrono::Timelike;
use glam::Vec2;
use wpe_assets::tex::{Mip, TexFormat, TexImage, Texture};
use wpe_render::{FrameGlobals, Gpu, GpuTexture, SceneRenderer, wgpu};
use wpe_scene::{OrthoCamera, Scene};

use frame::{FrameBuilder, FrameCache};

pub use files::{LoadError, WallpaperFiles};
pub use material::{SlotTexture, TextureStore};
pub use wpe_scene::FillMode;

pub(crate) fn rgba_texture(width: u32, height: u32, pixels: Vec<u8>) -> Texture {
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
    text: &mut Option<wpe_text::TextRenderer>,
) {
    if !scene.layers.iter().any(|l| l.text.is_some()) {
        return;
    }
    let renderer = text.get_or_insert_with(wpe_text::TextRenderer::new);
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

/// Bind `texture` to every slot of engine texture `name`, or each slot's
/// fallback without one.
fn bind_system(
    renderer: &mut SceneRenderer,
    slots: &[frame::SystemSlot],
    name: &str,
    texture: Option<&Arc<GpuTexture>>,
) {
    for s in slots.iter().filter(|s| s.name == name) {
        let texture = texture.unwrap_or(&s.fallback);
        renderer.set_texture(s.step, s.slot, texture.clone());
    }
}

/// The objects of scene JSON `doc`.
fn objects(doc: &serde_json::Value) -> Vec<serde_json::Value> {
    doc.get("objects")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// What a frame build produced that scripts attach to.
struct Built {
    doc: wpe_assets::Scene,
    layers: Vec<wpe_scene::ImageLayer>,
    steps: std::collections::HashMap<i32, frame::LayerSteps>,
    videos: std::collections::HashMap<i32, Arc<video::VideoStream>>,
}

impl Built {
    fn view<'a>(
        &'a self,
        objects: &'a [serde_json::Value],
        sounds: &'a std::collections::HashMap<i32, sound::SoundState>,
    ) -> script::SceneView<'a> {
        script::SceneView {
            doc: &self.doc,
            objects,
            layers: &self.layers,
            steps: &self.steps,
            videos: &self.videos,
            sounds,
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
    /// The scene document, as scripts have edited it.
    scene_json: serde_json::Value,
    props: wpe_scene::UserProperties,
    /// Taken while a frame builds.
    cache: Option<FrameCache>,
    text: Option<wpe_text::TextRenderer>,
    bindings: Vec<frame::Binding>,
    particles: Vec<frame::ParticleInstance>,
    scripts: Option<script::Scripts>,
    /// The wallpaper's files, for assets loaded while running (text fonts).
    vfs: wpe_vfs::Vfs,
    /// Spectrum of the latest audio frame, for scripts.
    spectrum: Option<wpe_audio::AudioSpectrum>,
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
    sounds: sound::Sounds,
    media: Option<media::Media>,
    /// Engine texture slots of the current frame.
    system_slots: Vec<frame::SystemSlot>,
    /// Animation clocks, shared with the scripts.
    clocks: clock::SharedClocks,
    /// Draws showing a layer's sprite sheet: layer id and step.
    sprite_steps: Vec<(i32, usize)>,
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
        let props = wpe_scene::UserProperties::from_project(&files.project);
        let mut wallpaper = Self {
            title: files
                .project
                .title
                .clone()
                .unwrap_or(files.scene_file.clone()),
            fill: FillMode::default(),
            camera: OrthoCamera {
                scene_size: Vec2::ONE,
                zoom: 1.0,
            },
            renderer: SceneRenderer::new(gpu, output_format),
            scene_json: files.scene_json.clone(),
            cache: Some(FrameCache::new(
                gpu,
                TextureStore::default(),
                white_texture(),
            )),
            text: None,
            bindings: Vec::new(),
            particles: Vec::new(),
            scripts: None,
            spectrum: None,
            start: Instant::now(),
            pointer: Vec2::splat(0.5),
            pointer_target: Vec2::splat(0.5),
            last_frame: None,
            parallax_delay: 0.0,
            shake: None,
            parallax_influence: files.scene.general.cameraparallaxmouseinfluence.value,
            time_override: None,
            audio: None,
            sounds: sound::Sounds::load(&files, &props),
            media: None,
            system_slots: Vec::new(),
            clocks: Default::default(),
            sprite_steps: Vec::new(),
            props,
            vfs: files.vfs,
        };
        let built = wallpaper.build_frame(gpu, files.scene);
        tracing::info!(
            "scene {}: {}x{}, {} image layers",
            files.scene_file,
            wallpaper.camera.scene_size.x,
            wallpaper.camera.scene_size.y,
            built.layers.len()
        );
        let objects = objects(&wallpaper.scene_json);
        let sounds = wallpaper.sounds.states();
        wallpaper.scripts = script::Scripts::load(
            &wallpaper.vfs,
            &files.project,
            &built.view(&objects, &sounds),
            wallpaper.camera.scene_size,
            wallpaper.clocks.clone(),
        );
        if wallpaper.scripts.is_some() || !wallpaper.system_slots.is_empty() {
            wallpaper.media = media::Media::start();
        }
        wallpaper.sync_scripts(gpu);
        Ok(wallpaper)
    }

    /// Build the frame for `doc` (the parsed scene JSON) and make it current.
    fn build_frame(&mut self, gpu: &Gpu, doc: wpe_assets::Scene) -> Built {
        let vfs = &self.vfs;
        let mut cache = self
            .cache
            .take()
            .expect("the cache returns after each build");
        let mut scene = Scene::build(vfs, &doc, &self.props, |name| cache.store.size(vfs, name));
        render_text_layers(
            vfs,
            &self.props,
            &mut scene,
            &mut cache.store,
            &mut self.text,
        );

        let mut builder = FrameBuilder::new(gpu, vfs, cache);
        builder.set_parallax(scene.parallax, scene.camera.scene_size);
        builder.layers(&scene.layers);
        if let Some(bloom) = scene.bloom {
            builder.bloom(bloom);
        }
        let (real, total) = builder.stats;
        tracing::debug!("{real} of {total} draws use their real shader");
        let (frame, cache) = builder.finish();
        self.cache = Some(cache);

        let c = scene.clear_color;
        self.renderer.clear_color = wgpu::Color {
            r: c.x as f64,
            g: c.y as f64,
            b: c.z as f64,
            a: 1.0,
        };
        self.renderer.set_frame(gpu, frame.targets, frame.steps);
        for id in &frame.hidden {
            for &step in frame.layer_steps.get(id).map_or(&[][..], |s| &s.all) {
                self.renderer.set_visible(step, false);
            }
        }
        self.system_slots = frame.system_slots;
        if let Some(media) = &mut self.media {
            let (texture, _) = media.thumbnail(gpu);
            bind_system(
                &mut self.renderer,
                &self.system_slots,
                media::THUMBNAIL,
                texture.as_ref(),
            );
        }
        {
            let mut clocks = self.clocks.borrow_mut();
            for binding in &frame.bindings {
                binding.register_clocks(&mut clocks);
            }
            for &(layer, step) in &frame.sprite_steps {
                if let Some((frames, duration)) = self.renderer.sprite_timing(step)
                    && duration > 0.0
                {
                    let fps = frames as f32 / duration;
                    let clock = clock::Clock::new(fps, frames as f32, false, true);
                    clocks.register(layer, clock::TEXTURE_ANIMATION.to_owned(), clock);
                }
            }
        }
        self.sprite_steps = frame.sprite_steps;
        self.camera = scene.camera;
        self.bindings = frame.bindings;
        self.particles = frame.particles;
        self.parallax_delay = scene.parallax.map_or(0.0, |p| p.delay);
        self.shake = scene.shake;
        Built {
            doc,
            layers: scene.layers,
            steps: frame.layer_steps,
            videos: frame.layer_videos,
        }
    }

    /// Apply the scene changes scripts queued (rebuilding the frame when the
    /// structure changed) and push their writes to the renderer.
    fn sync_scripts(&mut self, gpu: &Gpu) {
        let Some(mut scripts) = self.scripts.take() else {
            return;
        };
        let mut edits = scripts.take_edits();
        if !edits.is_empty() {
            script::resolve_paths(&mut edits, |path| self.vfs.read(path).is_ok());
            script::apply_edits(&mut self.scene_json, &edits);
            match serde_json::from_value::<wpe_assets::Scene>(self.scene_json.clone()) {
                Ok(doc) => {
                    let started = Instant::now();
                    let built = self.build_frame(gpu, doc);
                    tracing::debug!(
                        "rebuilt the frame for {} script edits ({} layers) in {:?}",
                        edits.len(),
                        built.layers.len(),
                        started.elapsed()
                    );
                    let objects = objects(&self.scene_json);
                    scripts.rebind(&built.view(&objects, &self.sounds.states()));
                }
                Err(e) => tracing::warn!("scene edited by scripts doesn't parse: {e}"),
            }
        }
        for (id, command) in scripts.take_sound_commands() {
            self.sounds.apply(id, command);
        }
        scripts.apply(gpu, &self.vfs, &mut self.renderer, self.text.as_mut());
        self.scripts = Some(scripts);
    }

    /// Start or stop audio: playing the wallpaper's sound layers and
    /// analysing system audio for audio-reactive shaders.
    pub fn set_audio(&mut self, enabled: bool) {
        self.sounds.enable(enabled);
        if !enabled {
            self.audio = None;
            return;
        }
        if self.audio.is_none() {
            self.audio = wpe_audio::AudioCapture::start()
                .inspect_err(|e| tracing::warn!("audio capture unavailable: {e}"))
                .ok();
        }
    }

    /// Refresh the audio spectrum. Without capture, `WPE_FAKE_AUDIO` feeds a
    /// test pattern (for audio-reactive wallpapers in headless runs).
    fn update_audio(&mut self, time: f32) {
        let spectrum = match &mut self.audio {
            Some(capture) => capture.spectrum(),
            None if std::env::var_os("WPE_FAKE_AUDIO").is_some() => {
                wpe_audio::AudioSpectrum::synthetic(time)
            }
            None => return,
        };
        self.spectrum = Some(spectrum);
        for (name, values) in spectrum.uniforms() {
            self.renderer.set_global_uniform(name, values);
        }
    }

    fn run_scripts(&mut self, gpu: &Gpu, time: f32) {
        if let Some(media) = &mut self.media {
            let events = media.poll();
            if let Some(scripts) = &mut self.scripts {
                for event in &events {
                    scripts.emit(event.handler_name(), &event.to_json());
                }
            }
            let (texture, changed) = media.thumbnail(gpu);
            if changed {
                bind_system(
                    &mut self.renderer,
                    &self.system_slots,
                    media::THUMBNAIL,
                    texture.as_ref(),
                );
            }
        }
        if let Some(scripts) = &mut self.scripts {
            let ended = self.clocks.borrow_mut().take_ended();
            scripts.update(time, self.pointer, self.spectrum, &ended);
        }
        self.sync_scripts(gpu);
    }

    /// Video textures and the streams feeding them.
    fn videos(&self) -> &[(Arc<GpuTexture>, Arc<video::VideoStream>)] {
        self.cache.as_ref().map_or(&[], |c| &c.videos)
    }

    /// The left mouse button was pressed or released at the current pointer.
    pub fn mouse_button(&mut self, down: bool) {
        if let Some(scripts) = &mut self.scripts {
            scripts.mouse_button(self.pointer_target, down);
        }
    }

    fn animate(&mut self, time: f32) {
        let frame = frame::FrameState {
            time,
            pointer: self.pointer,
        };
        let mut clocks = self.clocks.borrow_mut();
        clocks.advance(time);
        for binding in &self.bindings {
            binding.apply(&mut self.renderer, frame, &clocks);
        }
        for &(layer, step) in &self.sprite_steps {
            let time = clocks
                .get(layer, clock::TEXTURE_ANIMATION)
                .map(clock::Clock::seconds);
            self.renderer.set_sprite_time(step, time);
        }
        drop(clocks);
        let scene = self.camera.scene_size;
        let cursor = Vec2::new(self.pointer.x, 1.0 - self.pointer.y) * scene;
        for particles in &mut self.particles {
            particles.advance(&mut self.renderer, time, Some(cursor.extend(0.0)));
        }
    }

    fn upload_video_frames(&mut self, gpu: &Gpu) {
        for (texture, stream) in self.videos() {
            stream.update(gpu, texture);
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
        self.update_audio(globals.time);
        self.run_scripts(gpu, globals.time);
        self.renderer.render(gpu, encoder, globals, output, size);
    }

    /// Render one frame offscreen and return it as RGBA8 pixels.
    pub fn capture(&mut self, gpu: &Gpu, size: (u32, u32)) -> Vec<u8> {
        for (_, stream) in self.videos() {
            stream.wait_started(std::time::Duration::from_secs(5));
        }
        self.upload_video_frames(gpu);
        let globals = self.globals(size);
        self.animate(globals.time);
        self.update_audio(globals.time);
        self.run_scripts(gpu, globals.time);
        self.renderer.capture(gpu, globals, size)
    }
}
