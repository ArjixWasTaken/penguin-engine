//! Embeddable wallpaper runtime: load a wallpaper and drive its render loop.

mod bones;
mod clock;
mod files;
mod frame;
pub mod material;
mod media;
mod model_data;
mod script;
mod shadow;
mod sound;
pub mod steam;
mod storage;
mod video;
mod video_wallpaper;
pub mod web_wallpaper;

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use chrono::Timelike;
use glam::{Vec2, Vec3};
use wpe_assets::tex::{Mip, TexFormat, TexImage, Texture};
use wpe_render::{FrameGlobals, Gpu, GpuTexture, SceneRenderer, wgpu};
use wpe_scene::{OrthoCamera, Scene};

use frame::{FrameBuilder, FrameCache};

pub use files::{LoadError, WallpaperFiles};

/// Wait for background shutdown work (dropped video streams) to finish.
/// Apps call this before exiting, after dropping their wallpapers.
pub fn shutdown() {
    video::finish_teardowns();
    #[cfg(feature = "web")]
    web_wallpaper::shutdown(std::time::Duration::from_secs(3));
}
pub use material::{SlotTexture, TextureStore};
pub use wpe_assets::project::{Project, PropertyOption, UserProperty};
pub use wpe_scene::FillMode;

/// Mouse buttons wallpapers react to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

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
    groups: std::collections::HashMap<i32, bool>,
    steps: std::collections::HashMap<i32, frame::LayerSteps>,
    videos: std::collections::HashMap<i32, Arc<video::VideoStream>>,
}

impl Built {
    fn view<'a>(
        &'a self,
        objects: &'a [serde_json::Value],
        general: &'a serde_json::Value,
        sounds: &'a std::collections::HashMap<i32, sound::SoundState>,
    ) -> script::SceneView<'a> {
        script::SceneView {
            doc: &self.doc,
            objects,
            general,
            layers: &self.layers,
            groups: &self.groups,
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
    /// The scene's perspective camera (models, perspective layers) and
    /// whether the scene is 3D.
    perspective: Option<wpe_scene::PerspectiveCamera>,
    is_3d: bool,
    /// 3D scenes' camera paths, moving `perspective`, and the scene time
    /// they were last advanced to.
    camera_paths: Option<wpe_scene::CameraPathPlayer>,
    camera_path_time: Option<f32>,
    /// The scene's lights with their ambient and skylight colours, when
    /// any light moves (their uniforms are then refreshed every frame).
    /// The 3D camera scripts set (eye, centre, up), this frame.
    script_view: Option<[Vec3; 3]>,
    moving_lights: Option<(Vec<wpe_scene::SceneLight>, Vec3, Vec3)>,
    /// Shadow maps and volumetric passes of the lights.
    light_fx: shadow::LightEffects,
    /// A video wallpaper plays its video's sound when audio is on.
    video_wallpaper: bool,
    audio_enabled: bool,
    /// Script geometry, shared with the scripts.
    model_data: model_data::SharedModelDatas,
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
    /// A web wallpaper's page.
    #[cfg(feature = "web")]
    web: Option<web_wallpaper::WebState>,
    /// Spectrum of the latest audio frame, for scripts.
    spectrum: Option<wpe_audio::AudioSpectrum>,
    start: Instant,
    /// When the wallpaper was paused, freezing its clock.
    paused_at: Option<Instant>,
    /// Videos playing when the wallpaper was paused, to resume them.
    paused_videos: Vec<Arc<video::VideoStream>>,
    /// Master volume of sound layers and a video wallpaper's sound, 0..1.
    volume: f32,
    /// `project.json`.
    project: wpe_assets::Project,
    /// Smoothed cursor over the output, 0..1 with Y down.
    pointer: Vec2,
    /// Where the cursor actually is.
    pointer_target: Vec2,
    /// The pointer camera parallax follows this frame.
    parallax_pointer: Vec2,
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
    /// Animation clocks and puppet bones, shared with the scripts.
    clocks: clock::SharedClocks,
    bones: bones::SharedBones,
    /// Draws showing a layer's sprite sheet: layer id and step.
    sprite_steps: Vec<(i32, usize)>,
    /// Scripts' `localStorage` between runs.
    storage: storage::Storage,
    /// Where scripts last saw the pointer.
    script_pointer: Option<Vec2>,
}

/// What a frame build made, before it is installed in a [`Wallpaper`]: the
/// renderer already has the frame; these are the parts the wallpaper keeps.
/// Unlike the wallpaper, it may cross threads.
struct FrameBuild {
    built: Built,
    light_fx: shadow::LightEffects,
    system_slots: Vec<frame::SystemSlot>,
    sprite_steps: Vec<(i32, usize)>,
    bindings: Vec<frame::Binding>,
    particles: Vec<frame::ParticleInstance>,
    camera: OrthoCamera,
    perspective: wpe_scene::PerspectiveCamera,
    moving_lights: Option<(Vec<wpe_scene::SceneLight>, Vec3, Vec3)>,
    is_3d: bool,
    parallax_delay: f32,
    shake: Option<wpe_scene::Shake>,
}

/// What a frame build reads and updates: the parts of a wallpaper that
/// don't belong to its scripts.
struct BuildContext<'a> {
    gpu: &'a Gpu,
    vfs: &'a wpe_vfs::Vfs,
    props: &'a wpe_scene::UserProperties,
    scene_json: &'a serde_json::Value,
    model_data: &'a model_data::ModelDatas,
    cache: &'a mut Option<FrameCache>,
    text: &'a mut Option<wpe_text::TextRenderer>,
    camera_paths: &'a mut Option<wpe_scene::CameraPathPlayer>,
    renderer: &'a mut SceneRenderer,
}

/// Where a load's time went, for the logs.
#[derive(Debug, Default, Clone, Copy)]
struct LoadTimings {
    files: std::time::Duration,
    scene: std::time::Duration,
    text: std::time::Duration,
    frame: std::time::Duration,
    renderer: std::time::Duration,
    pipelines: std::time::Duration,
    build: frame::BuildTimings,
    textures: std::time::Duration,
}

/// Build the frame for `doc` (the parsed scene JSON) and give it to the
/// renderer, creating its pipelines.
fn build_frame(
    cx: BuildContext<'_>,
    doc: wpe_assets::Scene,
    timings: &mut LoadTimings,
) -> FrameBuild {
    let vfs = cx.vfs;
    let mut cache = cx.cache.take().expect("the cache returns after each build");
    let decoded = cache.store.decode_time;
    let started = Instant::now();
    let mut scene = Scene::build(vfs, &doc, cx.props, |name| cache.store.size(vfs, name));
    timings.scene += started.elapsed();
    let started = Instant::now();
    render_text_layers(vfs, cx.props, &mut scene, &mut cache.store, cx.text);
    timings.text += started.elapsed();

    let started = Instant::now();
    // The lit shaders' cookie sampler shows the cookie light's texture.
    cache.store.alias(
        vfs,
        wpe_scene::COOKIE_ALIAS,
        wpe_scene::cookie_light(&scene.lights).and_then(|l| l.cookie.as_deref()),
    );

    let mut builder = FrameBuilder::new(cx.gpu, vfs, cache);
    builder.set_hdr(scene.hdr);
    builder.set_user_properties(cx.props);
    builder.set_scene_combos(
        scene
            .fog
            .combos()
            .into_iter()
            .map(|(k, v)| (k, v.to_string()))
            .collect(),
    );
    builder.set_parallax(scene.parallax, scene.camera.scene_size);
    let lights = scene.light_uniforms();
    builder.set_models(
        lights
            .combos
            .iter()
            .map(|(k, v)| (k.clone(), v.to_string()))
            .collect(),
        cx.model_data,
        scene.is_3d,
    );
    if scene.is_3d && cx.camera_paths.is_none() {
        let fade = cx.props.flag(&doc.general.camerafade);
        let paths = wpe_scene::CameraPath::load_scene(vfs, &doc);
        *cx.camera_paths = wpe_scene::CameraPathPlayer::new(paths, fade);
    }
    // 3D scenes render planar reflections when objects ask for them
    // (`reflected`), mirroring only those, or when they have camera paths
    // (Wallpaper Engine's `reflected_perspective` camera), mirroring all.
    let reflected = scene.models.iter().any(|m| m.reflected);
    if scene.is_3d && (reflected || cx.camera_paths.is_some()) {
        builder.enable_reflection(reflected);
    }
    builder.objects(&scene.layers, &scene.models, &scene.draw_order);
    let postprocessing = doc
        .general
        .orthogonalprojection
        .as_ref()
        .and_then(|p| p.postprocessing.as_deref());
    let volumetrics = builder.volumetrics(&scene.lights, postprocessing);
    if let Some(bloom) = scene.bloom {
        builder.bloom(bloom);
    }
    builder.reflection_passes();
    let (real, total) = builder.stats;
    tracing::debug!("{real} of {total} draws use their real shader");
    let build_timings = builder.timings;
    let (frame, cache) = builder.finish();
    timings.textures += cache.store.decode_time - decoded;
    *cx.cache = Some(cache);
    timings.frame += started.elapsed();
    timings.build.shaders += build_timings.shaders;
    timings.build.programs += build_timings.programs;
    timings.build.uploads += build_timings.uploads;

    let started = Instant::now();
    let renderer = cx.renderer;
    let c = scene.clear_color;
    renderer.clear_color = wgpu::Color {
        r: c.x as f64,
        g: c.y as f64,
        b: c.z as f64,
        a: 1.0,
    };
    renderer.set_scene_format(if scene.hdr {
        wpe_render::HDR_FORMAT
    } else {
        wpe_render::DEFAULT_FORMAT
    });
    renderer.set_frame(cx.gpu, frame.targets, frame.steps);
    for (step, name, scale) in &frame.texel_uniforms {
        renderer.set_texel_uniform(*step, name, *scale);
    }
    for (name, values) in lights.uniforms.iter().chain(&scene.fog.uniforms()) {
        renderer.set_global_uniform(name, values);
    }
    let casters = shadow::casters(&objects(cx.scene_json), cx.props, &frame.layer_steps);
    tracing::debug!("{} draws cast shadows", casters.len());
    renderer.set_shadow_casters(casters);
    let mut light_fx = shadow::LightEffects::new(volumetrics, scene.is_3d);
    light_fx.update(renderer, &scene.lights);
    for id in &frame.hidden {
        for &step in frame.layer_steps.get(id).map_or(&[][..], |s| &s.all) {
            renderer.set_visible(step, false);
        }
    }
    timings.renderer += started.elapsed();
    let started = Instant::now();
    renderer.warm_pipelines(cx.gpu);
    timings.pipelines += started.elapsed();
    let moving_lights = scene
        .lights
        .iter()
        .any(|l| l.motion.is_transform_animated())
        .then(|| {
            (
                scene.lights.clone(),
                scene.ambient_color,
                scene.skylight_color,
            )
        });
    FrameBuild {
        light_fx,
        system_slots: frame.system_slots,
        sprite_steps: frame.sprite_steps,
        bindings: frame.bindings,
        particles: frame.particles,
        camera: scene.camera,
        perspective: scene.perspective,
        moving_lights,
        is_3d: scene.is_3d,
        parallax_delay: scene.parallax.map_or(0.0, |p| p.delay),
        shake: scene.shake,
        built: Built {
            doc,
            groups: scene.groups,
            layers: scene.layers,
            steps: frame.layer_steps,
            videos: frame.layer_videos,
        },
    }
}

/// A wallpaper loaded up to its scripts: files read, scene built, textures
/// decoded and uploaded, shaders compiled and their pipelines created.
/// Made by [`Wallpaper::prepare`], which may run on any thread; it becomes
/// a [`Wallpaper`] with [`Self::finish`] on the thread that renders it.
pub struct PreparedWallpaper {
    title: String,
    scene_file: String,
    project: wpe_assets::Project,
    props: wpe_scene::UserProperties,
    scene_json: serde_json::Value,
    vfs: wpe_vfs::Vfs,
    renderer: SceneRenderer,
    cache: Option<FrameCache>,
    text: Option<wpe_text::TextRenderer>,
    camera_paths: Option<wpe_scene::CameraPathPlayer>,
    video_wallpaper: bool,
    parallax_influence: f32,
    sounds: sound::Sounds,
    storage: storage::Storage,
    /// What scripts stored last time.
    stored: Option<serde_json::Value>,
    frame: FrameBuild,
    #[cfg(feature = "web")]
    web: Option<wpe_web::WebPage>,
    /// How long preparing took.
    pub elapsed: std::time::Duration,
}

// The point of preparing: the result moves to the render thread.
const _: () = {
    const fn send<T: Send>() {}
    send::<PreparedWallpaper>()
};

impl PreparedWallpaper {
    /// The wallpaper's title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Make the wallpaper: start its scripts (running their `init`) and
    /// hook its animations up to them. Runs on the thread that renders it.
    pub fn finish(self, gpu: &Gpu) -> Wallpaper {
        let started = Instant::now();
        let parallax_influence = self.parallax_influence;
        let mut wallpaper = Wallpaper {
            title: self.title,
            fill: FillMode::default(),
            camera: OrthoCamera {
                scene_size: Vec2::ONE,
                zoom: 1.0,
            },
            perspective: None,
            is_3d: false,
            camera_paths: self.camera_paths,
            camera_path_time: None,
            moving_lights: None,
            light_fx: Default::default(),
            script_view: None,
            model_data: Default::default(),
            renderer: self.renderer,
            scene_json: self.scene_json,
            cache: self.cache,
            video_wallpaper: self.video_wallpaper,
            audio_enabled: false,
            text: self.text,
            bindings: Vec::new(),
            particles: Vec::new(),
            scripts: None,
            spectrum: None,
            start: Instant::now(),
            paused_at: None,
            paused_videos: Vec::new(),
            volume: 1.0,
            project: self.project,
            pointer: Vec2::splat(0.5),
            pointer_target: Vec2::splat(0.5),
            parallax_pointer: Vec2::splat(0.5),
            last_frame: None,
            parallax_delay: 0.0,
            shake: None,
            parallax_influence,
            time_override: None,
            audio: None,
            sounds: self.sounds,
            media: None,
            system_slots: Vec::new(),
            clocks: Default::default(),
            bones: Default::default(),
            sprite_steps: Vec::new(),
            storage: self.storage,
            script_pointer: None,
            props: self.props,
            vfs: self.vfs,
            #[cfg(feature = "web")]
            web: self.web.map(|page| web_wallpaper::WebState {
                page,
                target: None,
                shown: None,
                shared: None,
                audio_sent: None,
            }),
        };
        let built = wallpaper.install(gpu, self.frame);
        tracing::info!(
            "scene {}: {}x{}, {} image layers",
            self.scene_file,
            wallpaper.camera.scene_size.x,
            wallpaper.camera.scene_size.y,
            built.layers.len()
        );
        let objects = objects(&wallpaper.scene_json);
        let sounds = wallpaper.sounds.states();
        let scripts_started = Instant::now();
        wallpaper.scripts = script::Scripts::load(
            &wallpaper.vfs,
            &wallpaper.project,
            &built.view(&objects, &wallpaper.scene_json["general"], &sounds),
            wallpaper.camera.scene_size,
            wallpaper.clocks.clone(),
            wallpaper.bones.clone(),
            wallpaper.model_data.clone(),
            self.stored,
        );
        let scripts = scripts_started.elapsed();
        if wallpaper.scripts.is_some() || !wallpaper.system_slots.is_empty() {
            wallpaper.media = media::Media::start();
        }
        let sync_started = Instant::now();
        wallpaper.sync_scripts(gpu, 0.0);
        tracing::debug!(
            "finished \"{}\" in {:.1?} (scripts {:.1?}, their first edits {:.1?})",
            wallpaper.title,
            started.elapsed(),
            scripts,
            sync_started.elapsed()
        );
        wallpaper
    }
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
        Self::load_with_properties(gpu, output_format, assets, path, &[])
    }

    /// [`Self::load`] with user properties set to `properties` (name and
    /// value) from the start, as if `project.json` declared those values.
    pub fn load_with_properties(
        gpu: &Gpu,
        output_format: wgpu::TextureFormat,
        assets: &Path,
        path: &Path,
        properties: &[(String, serde_json::Value)],
    ) -> Result<Self, LoadError> {
        Ok(Self::prepare(gpu, output_format, assets, path, properties)?.finish(gpu))
    }

    /// The part of [`Self::load_with_properties`] that may run on another
    /// thread than the one rendering: everything but the scripts. Finish it
    /// with [`PreparedWallpaper::finish`].
    pub fn prepare(
        gpu: &Gpu,
        output_format: wgpu::TextureFormat,
        assets: &Path,
        path: &Path,
        properties: &[(String, serde_json::Value)],
    ) -> Result<PreparedWallpaper, LoadError> {
        let started = Instant::now();
        let mut timings = LoadTimings::default();
        let mut files = WallpaperFiles::open_with_assets_dir(assets, path)?;
        for (name, value) in properties {
            if let Some(p) = files.project.general.properties.get_mut(name) {
                p.value = value.clone();
            }
        }
        let props = wpe_scene::UserProperties::from_project(&files.project);
        let mut store = TextureStore::default();
        let video_wallpaper = files.video.is_some();
        if let Some((mp4, width, height)) = files.video.take() {
            store.insert(
                video_wallpaper::TEXTURE,
                video_wallpaper::texture(mp4, width, height),
            );
        }
        #[cfg(feature = "web")]
        let web = match files.web.take() {
            Some((dir, file)) => {
                let (width, height) = web_wallpaper::DEFAULT_SIZE;
                store.insert(
                    web_wallpaper::TEXTURE,
                    video_wallpaper::blank(width, height),
                );
                Some(web_wallpaper::open(
                    &dir,
                    &file,
                    &files.project,
                    (width, height),
                    web_wallpaper::can_share(gpu),
                )?)
            }
            None => None,
        };
        #[cfg(not(feature = "web"))]
        if files.web.is_some() {
            return Err(LoadError::WebUnsupported);
        }
        let sounds = sound::Sounds::load(&files, &props);
        let storage = storage::Storage::for_wallpaper(path);
        let stored = storage.load();
        timings.files = started.elapsed();

        let mut renderer = SceneRenderer::new(gpu, output_format);
        let mut cache = Some(FrameCache::new(gpu, store, white_texture()));
        let mut text = None;
        let mut camera_paths = None;
        let model_data = model_data::ModelDatas::default();
        let frame = build_frame(
            BuildContext {
                gpu,
                vfs: &files.vfs,
                props: &props,
                scene_json: &files.scene_json,
                model_data: &model_data,
                cache: &mut cache,
                text: &mut text,
                camera_paths: &mut camera_paths,
                renderer: &mut renderer,
            },
            files.scene,
            &mut timings,
        );
        let objects = objects(&files.scene_json);
        if objects.iter().any(|o| o.to_string().contains("\"script\"")) {
            // Scripts ask videos' length and seek them from `init`; open the
            // files now so those calls don't wait.
            for stream in frame.built.videos.values() {
                stream.duration();
            }
        }
        let title = files
            .project
            .title
            .clone()
            .unwrap_or(files.scene_file.clone());
        let elapsed = started.elapsed();
        let t = timings;
        tracing::debug!(
            "prepared \"{title}\" in {elapsed:.1?}: files {:.1?}, scene {:.1?}, text {:.1?}, \
             frame {:.1?} (texture decoding {:.1?}, shaders {:.1?}, programs {:.1?}, \
             uploads {:.1?}), renderer {:.1?}, pipelines {:.1?}",
            t.files,
            t.scene,
            t.text,
            t.frame,
            t.textures,
            t.build.shaders,
            t.build.programs,
            t.build.uploads,
            t.renderer,
            t.pipelines,
        );
        Ok(PreparedWallpaper {
            title,
            scene_file: files.scene_file,
            parallax_influence: frame.built.doc.general.cameraparallaxmouseinfluence.value,
            project: files.project,
            props,
            scene_json: files.scene_json,
            vfs: files.vfs,
            renderer,
            cache,
            text,
            camera_paths,
            video_wallpaper,
            sounds,
            storage,
            stored,
            frame,
            #[cfg(feature = "web")]
            web,
            elapsed,
        })
    }

    /// Build the frame for `doc` (the parsed scene JSON) and make it current.
    fn build_frame(&mut self, gpu: &Gpu, doc: wpe_assets::Scene) -> Built {
        let model_data = self.model_data.clone();
        let model_data = model_data.borrow();
        let build = build_frame(
            BuildContext {
                gpu,
                vfs: &self.vfs,
                props: &self.props,
                scene_json: &self.scene_json,
                model_data: &model_data,
                cache: &mut self.cache,
                text: &mut self.text,
                camera_paths: &mut self.camera_paths,
                renderer: &mut self.renderer,
            },
            doc,
            &mut LoadTimings::default(),
        );
        drop(model_data);
        self.install(gpu, build)
    }

    /// Make a built frame current: hook its animations up to the clocks and
    /// bones scripts share, and bind engine textures.
    fn install(&mut self, gpu: &Gpu, build: FrameBuild) -> Built {
        self.light_fx = build.light_fx;
        self.system_slots = build.system_slots;
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
            let mut bones = self.bones.borrow_mut();
            for binding in &build.bindings {
                binding.register_clocks(&mut clocks);
                binding.register_bones(&mut bones);
            }
            for &(layer, step) in &build.sprite_steps {
                if let Some((frames, duration)) = self.renderer.sprite_timing(step)
                    && duration > 0.0
                {
                    let fps = frames as f32 / duration;
                    let clock = clock::Clock::new(fps, frames as f32, false, true);
                    clocks.register(layer, clock::TEXTURE_ANIMATION.to_owned(), clock);
                }
            }
        }
        self.sprite_steps = build.sprite_steps;
        self.camera = build.camera;
        self.perspective = Some(build.perspective);
        self.moving_lights = build.moving_lights;
        self.is_3d = build.is_3d;
        self.bindings = build.bindings;
        self.particles = build.particles;
        self.parallax_delay = build.parallax_delay;
        self.shake = build.shake;
        build.built
    }

    /// Apply the scene changes scripts queued (rebuilding the frame when the
    /// structure changed) and push their writes to the renderer. `time` is
    /// the frame's scene time, to re-animate a rebuilt frame.
    fn sync_scripts(&mut self, gpu: &Gpu, time: f32) {
        let Some(mut scripts) = self.scripts.take() else {
            return;
        };
        let mut edits = scripts.take_edits();
        if !edits.is_empty() {
            tracing::trace!("script edits: {edits:?}");
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
                    let sounds = self.sounds.states();
                    scripts.rebind(&built.view(&objects, &self.scene_json["general"], &sounds));
                    // The new frame's draws haven't been animated yet.
                    self.animate(time);
                }
                Err(e) => tracing::warn!("scene edited by scripts doesn't parse: {e}"),
            }
        }
        if let Some(data) = scripts.take_storage() {
            self.storage.changed(data);
        }
        for (id, command) in scripts.take_sound_commands() {
            self.sounds.apply(id, command);
        }
        for (id, command) in scripts.take_particle_commands() {
            for particles in self.particles.iter_mut().filter(|p| p.layer == id) {
                match &command {
                    script::ParticleCommand::Override(json) => particles.set_override(json),
                    script::ParticleCommand::Emit(count) => particles.emit(*count),
                    script::ParticleCommand::Playing(playing) => particles.set_playing(*playing),
                    script::ParticleCommand::Stop => particles.stop(),
                }
            }
        }
        scripts.apply(gpu, &self.vfs, &mut self.renderer, self.text.as_mut());
        self.scripts = Some(scripts);
    }

    /// Change a user property, as a settings UI does: the scene is rebuilt
    /// with the new value and scripts get `applyUserProperties`. `value` is
    /// the property's value (`true`, `0.5`, `"1 0 0"`, ...).
    pub fn set_user_property(&mut self, gpu: &Gpu, name: &str, value: serde_json::Value) {
        self.props.set(name, value.clone());
        #[cfg(feature = "web")]
        if let Some(web) = &self.web {
            // Pages apply properties themselves.
            let kind = self
                .project
                .general
                .properties
                .get(name)
                .and_then(|p| p.kind.as_deref());
            let changed = serde_json::json!({ name: web_wallpaper::property_json(kind, &value) });
            web.page.apply_properties(&changed);
            return;
        }
        let doc = match serde_json::from_value::<wpe_assets::Scene>(self.scene_json.clone()) {
            Ok(doc) => doc,
            Err(e) => {
                tracing::warn!("scene doesn't parse: {e}");
                return;
            }
        };
        let built = self.build_frame(gpu, doc);
        if let Some(scripts) = &mut self.scripts {
            let objects = objects(&self.scene_json);
            let sounds = self.sounds.states();
            scripts.rebind(&built.view(&objects, &self.scene_json["general"], &sounds));
            scripts.user_property_changed(name, value);
        }
    }

    /// The user changed the wallpaper's general settings (`json` as Wallpaper
    /// Engine passes them): scripts' `applyGeneralSettings` see them.
    pub fn apply_general_settings(&mut self, json: &serde_json::Value) {
        #[cfg(feature = "web")]
        if let Some(web) = &self.web {
            web.page.apply_general(json);
        }
        if let Some(scripts) = &mut self.scripts {
            scripts.emit("applyGeneralSettings", json);
        }
    }

    /// `project.json` of the wallpaper.
    pub fn project(&self) -> &wpe_assets::Project {
        &self.project
    }

    /// Whether this is a video wallpaper (rather than a scene).
    pub fn is_video(&self) -> bool {
        self.video_wallpaper
    }

    /// Whether this is a web wallpaper.
    pub fn is_web(&self) -> bool {
        #[cfg(feature = "web")]
        return self.web.is_some();
        #[cfg(not(feature = "web"))]
        false
    }

    /// The current value of user property `name`.
    pub fn user_property(&self, name: &str) -> Option<&serde_json::Value> {
        self.props.get(name)
    }

    /// Seconds since the wallpaper started, not counting time paused.
    fn clock_time(&self) -> f32 {
        let now = self.paused_at.unwrap_or_else(Instant::now);
        (now - self.start).as_secs_f32()
    }

    pub fn paused(&self) -> bool {
        self.paused_at.is_some()
    }

    /// Freeze or unfreeze the wallpaper: its clock (animations, scripts'
    /// time), videos and sound layers. A paused wallpaper still renders, the
    /// same frame each time.
    pub fn set_paused(&mut self, paused: bool) {
        if paused == self.paused() {
            return;
        }
        if paused {
            self.paused_at = Some(Instant::now());
            self.paused_videos = self
                .videos()
                .iter()
                .filter(|(_, stream)| stream.is_playing())
                .map(|(_, stream)| stream.clone())
                .collect();
            for stream in &self.paused_videos {
                stream.set_paused(true);
            }
        } else if let Some(at) = self.paused_at.take() {
            self.start += at.elapsed();
            for stream in self.paused_videos.drain(..) {
                stream.set_paused(false);
            }
        }
        self.sounds.set_paused(paused);
        #[cfg(feature = "web")]
        if let Some(web) = &self.web {
            web.page.set_paused(paused);
        }
    }

    /// Master volume (0..1) of the sound layers and a video wallpaper's sound.
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        self.sounds.set_master(self.volume);
        self.apply_video_audio();
    }

    /// Start or stop audio: playing the wallpaper's sound layers and
    /// analysing system audio for audio-reactive shaders.
    pub fn set_audio(&mut self, enabled: bool) {
        self.sounds.enable(enabled);
        self.audio_enabled = enabled;
        self.apply_video_audio();
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

    /// Move every video to `seconds` (looping ones wrap around), so a
    /// screenshot shows that moment of them.
    pub fn seek_videos(&self, seconds: f32) {
        for (_, stream) in self.videos() {
            let duration = stream.duration();
            let at = f64::from(seconds);
            stream.seek(if duration > 0.0 { at % duration } else { at });
        }
        for (_, stream) in self.videos() {
            stream.wait_seeked(std::time::Duration::from_secs(2));
        }
    }

    /// The codecs of the video sound being played (video wallpapers with
    /// audio on).
    pub fn video_audio(&self) -> Vec<String> {
        self.videos()
            .iter()
            .filter_map(|(_, stream)| stream.audio_codec())
            .collect()
    }

    /// A video wallpaper's sound follows the audio setting (videos inside
    /// scenes stay silent, as in Wallpaper Engine).
    fn apply_video_audio(&self) {
        if self.video_wallpaper {
            for (_, stream) in self.videos() {
                stream.set_audio(self.audio_enabled.then_some(self.volume));
            }
        }
        // A web wallpaper's own sound likewise.
        #[cfg(feature = "web")]
        if let Some(web) = &self.web {
            web.page
                .set_volume(if self.audio_enabled { self.volume } else { 0.0 });
        }
    }

    /// Hand the audio spectrum to a web wallpaper's audio listeners, about
    /// 30 times a second as Wallpaper Engine does.
    #[cfg(feature = "web")]
    fn feed_web_audio(&mut self) {
        let (Some(web), Some(spectrum)) = (&mut self.web, &self.spectrum) else {
            return;
        };
        if !web.page.wants_audio()
            || web
                .audio_sent
                .is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(33))
        {
            return;
        }
        web.audio_sent = Some(Instant::now());
        let mut bands = [0.0; 128];
        bands[..64].copy_from_slice(&spectrum.left64);
        bands[64..].copy_from_slice(&spectrum.right64);
        web.page.send_audio(&bands);
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

    fn run_scripts(&mut self, gpu: &Gpu, time: f32, size: (u32, u32)) {
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
            if self.script_pointer != Some(self.pointer_target) {
                self.script_pointer = Some(self.pointer_target);
                scripts.cursor_moved(self.pointer_target);
            }
            let screen = [size.0 as f32, size.1 as f32];
            scripts.update(time, self.pointer, screen, self.spectrum);
        }
        self.sync_scripts(gpu, time);
    }

    /// Lay a web wallpaper's page out at the output's `size` (rebuilding
    /// the frame around a texture that size) and show its newest frame, or
    /// wait up to `wait` for one at that size.
    #[cfg(feature = "web")]
    fn update_web(&mut self, gpu: &Gpu, size: (u32, u32), wait: Option<std::time::Duration>) {
        let Some(mut web) = self.web.take() else {
            return;
        };
        if web.page.size() != size && size.0 > 0 && size.1 > 0 {
            web.page.resize(size);
            let texture = web_wallpaper::TEXTURE;
            self.vfs
                .mount("web", video_wallpaper::documents(texture, size.0, size.1));
            if let Some(cache) = &mut self.cache {
                cache.replace(&self.vfs, texture, video_wallpaper::blank(size.0, size.1));
            }
            let scene =
                wpe_assets::load_json::<serde_json::Value>(&self.vfs, video_wallpaper::SCENE_FILE)
                    .map_err(|e| e.to_string())
                    .and_then(|json| {
                        let doc = serde_json::from_value::<wpe_assets::Scene>(json.clone())
                            .map_err(|e| e.to_string())?;
                        Ok((json, doc))
                    });
            match scene {
                Ok((json, doc)) => {
                    self.scene_json = json;
                    self.build_frame(gpu, doc);
                }
                Err(e) => tracing::warn!("web wallpaper scene: {e}"),
            }
            web.target = None;
            web.shown = None;
        }
        if web.target.is_none() {
            web.target = self
                .cache
                .as_mut()
                .and_then(|c| c.uploaded(&self.vfs, web_wallpaper::TEXTURE));
        }
        match wait {
            Some(timeout) => web.wait_frame(gpu, timeout),
            None => web.frame(gpu),
        }
        self.web = Some(web);
    }

    /// Video textures and the streams feeding them.
    fn videos(&self) -> &[(Arc<GpuTexture>, Arc<video::VideoStream>)] {
        self.cache.as_ref().map_or(&[], |c| &c.videos)
    }

    /// A mouse button was pressed or released at the current pointer.
    /// Scenes only see the left button; web pages see all three.
    pub fn pointer_button(&mut self, button: MouseButton, down: bool) {
        match button {
            MouseButton::Left => self.mouse_button(down),
            #[cfg(feature = "web")]
            _ => {
                if let Some(web) = &self.web {
                    let (x, y) = web.page_point(self.pointer_target);
                    let button = match button {
                        MouseButton::Middle => web_wallpaper::Button::Middle,
                        _ => web_wallpaper::Button::Right,
                    };
                    web.page.input(wpe_web::Input::Button(button, down, x, y));
                }
            }
            #[cfg(not(feature = "web"))]
            _ => {}
        }
    }

    /// Scrolled by `dx`, `dy` output pixels at the current pointer (web
    /// pages only).
    pub fn scroll(&mut self, dx: f32, dy: f32) {
        #[cfg(feature = "web")]
        if let Some(web) = &self.web {
            let (x, y) = web.page_point(self.pointer_target);
            web.page.input(wpe_web::Input::Wheel { dx, dy, x, y });
        }
        #[cfg(not(feature = "web"))]
        let _ = (dx, dy);
    }

    /// Output pixels per logical pixel; web pages lay out in logical pixels.
    pub fn set_scale(&mut self, scale: f32) {
        #[cfg(feature = "web")]
        if let Some(web) = &mut self.web {
            web.page.set_scale(scale);
        }
        #[cfg(not(feature = "web"))]
        let _ = scale;
    }

    /// The left mouse button was pressed or released at the current pointer.
    pub fn mouse_button(&mut self, down: bool) {
        #[cfg(feature = "web")]
        if let Some(web) = &self.web {
            let (x, y) = web.page_point(self.pointer_target);
            let button = web_wallpaper::Button::Left;
            web.page.input(wpe_web::Input::Button(button, down, x, y));
        }
        if let Some(scripts) = &mut self.scripts {
            scripts.mouse_button(self.pointer_target, down);
        }
    }

    fn animate(&mut self, time: f32) {
        let frame = frame::FrameState {
            time,
            parallax: self.parallax_pointer,
        };
        let mut clocks = self.clocks.borrow_mut();
        let mut bones = self.bones.borrow_mut();
        let dt = clocks.advance(time);
        bones.step(dt);
        if let Some((lights, ambient, skylight)) = &mut self.moving_lights {
            for light in lights.iter_mut() {
                light.set_transform(light.motion.transform_at(time));
            }
            self.light_fx.update(&mut self.renderer, lights);
            for (name, values) in wpe_scene::light_uniforms(lights, *ambient, *skylight).uniforms {
                self.renderer.set_global_uniform(&name, &values);
            }
        }
        let model_data = self.model_data.borrow();
        for binding in &self.bindings {
            binding.apply(&mut self.renderer, frame, &clocks, &mut bones, &model_data);
        }
        drop(model_data);
        drop(bones);
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
        #[cfg(feature = "web")]
        if let Some(web) = &self.web
            && pointer != self.pointer_target
        {
            let (x, y) = web.page_point(pointer);
            web.page.input(wpe_web::Input::Move(x, y));
        }
        self.pointer_target = pointer;
    }

    /// The pointer camera parallax follows: the centre while the whole
    /// scene is shown (fitted), where parallax made to pan a cropped scene
    /// would only push it off the output.
    fn parallax_pointer(&self, size: (u32, u32)) -> Vec2 {
        match self.camera.fill_for(size, self.fill) {
            FillMode::Fit => Vec2::splat(0.5),
            _ => self.pointer,
        }
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

    /// Move the perspective camera along its camera paths to this frame's time.
    fn advance_camera_path(&mut self) {
        let time = self.time_override.unwrap_or_else(|| self.clock_time());
        if let Some(player) = &mut self.camera_paths {
            let dt = self.camera_path_time.map_or(time, |last| time - last);
            self.camera_path_time = Some(time);
            player.advance(dt.max(0.0) as f64);
            if let Some(camera) = &mut self.perspective {
                (camera.eye, camera.center, camera.up) = player.look_at();
            }
        }
        // Scripts see the 3D camera and may move it (`setCameraTransforms`).
        self.script_view = None;
        if self.is_3d
            && let (Some(camera), Some(scripts)) = (&self.perspective, &mut self.scripts)
        {
            self.script_view = scripts.view_camera([camera.eye, camera.center, camera.up]);
        }
    }

    fn globals(&self, size: (u32, u32)) -> FrameGlobals {
        let now = chrono::Local::now().time();
        let centred = Vec2::new(self.parallax_pointer.x - 0.5, 0.5 - self.parallax_pointer.y);
        let time = self.time_override.unwrap_or_else(|| self.clock_time());
        let mut camera = self.camera;
        let script_camera = self.scripts.as_ref().and_then(|s| s.camera());
        if let Some((_, zoom)) = script_camera {
            camera.zoom = if camera.zoom > 0.0 { camera.zoom } else { 1.0 } * zoom;
        }
        let mut view_proj = camera.view_proj(size, self.fill);
        // Scripts' `setCameraTransforms` moves where the camera looks.
        if let Some((center, _)) = script_camera {
            let offset = camera.scene_size / 2.0 - center;
            view_proj *= glam::Mat4::from_translation(offset.extend(0.0));
        }
        if let Some(shake) = &self.shake {
            let offset = shake.offset(time) * self.camera.scene_size * 0.01;
            view_proj *= glam::Mat4::from_translation(offset.extend(0.0));
        }
        let mut perspective_camera = self.perspective;
        if let (Some(p), Some([eye, center, up])) = (&mut perspective_camera, self.script_view) {
            (p.eye, p.center, p.up) = (eye, center, up);
        }
        let perspective = match &perspective_camera {
            Some(p) => FrameGlobals {
                perspective: p.view_proj(size, self.fill),
                perspective_view: p.view(size, self.fill),
                perspective_proj: p.proj(size, self.fill),
                perspective_eye: p.eye(),
                reflected_view: p.reflected_view(size, self.fill),
                reflected_proj: p.reflected_proj(size, self.fill),
                reflected: p.reflected_view_proj(size, self.fill),
                reflected_eye: p.reflected_eye(),
                ..Default::default()
            },
            None => FrameGlobals::default(),
        };
        FrameGlobals {
            time,
            daytime: now.num_seconds_from_midnight() as f32 / 86_400.0,
            pointer: self.pointer,
            parallax: Vec2::splat(0.5) + centred * self.parallax_influence,
            view_proj,
            screen: Vec2::new(size.0 as f32, size.1 as f32),
            eye: (self.camera.scene_size / 2.0).extend(1000.0),
            ..perspective
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
        #[cfg(feature = "web")]
        self.update_web(gpu, size, None);
        self.upload_video_frames(gpu);
        self.smooth_pointer();
        self.advance_camera_path();
        self.parallax_pointer = self.parallax_pointer(size);
        let globals = self.globals(size);
        self.animate(globals.time);
        self.light_fx.select(&mut self.renderer, &globals);
        self.update_audio(globals.time);
        #[cfg(feature = "web")]
        self.feed_web_audio();
        self.run_scripts(gpu, globals.time, size);
        self.renderer.render(gpu, encoder, globals, output, size);
    }

    /// Render one frame offscreen and return it as RGBA8 pixels.
    pub fn capture(&mut self, gpu: &Gpu, size: (u32, u32)) -> Vec<u8> {
        for (_, stream) in self.videos() {
            stream.wait_started(std::time::Duration::from_secs(5));
        }
        #[cfg(feature = "web")]
        self.update_web(gpu, size, Some(std::time::Duration::from_secs(10)));
        self.upload_video_frames(gpu);
        self.advance_camera_path();
        // No wall clock to smooth over: the pointer is where it was put.
        self.pointer = self.pointer_target;
        self.parallax_pointer = self.parallax_pointer(size);
        let globals = self.globals(size);
        self.animate(globals.time);
        self.light_fx.select(&mut self.renderer, &globals);
        self.update_audio(globals.time);
        #[cfg(feature = "web")]
        self.feed_web_audio();
        self.run_scripts(gpu, globals.time, size);
        self.renderer.capture(gpu, globals, size)
    }
}
