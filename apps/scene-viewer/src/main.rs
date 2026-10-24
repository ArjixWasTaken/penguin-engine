use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};
use wpe_render::{Gpu, wgpu};
use wpe_runtime::{FillMode, Wallpaper};

#[derive(Parser)]
#[command(about = "View a Wallpaper Engine scene wallpaper")]
struct Args {
    /// Wallpaper directory (with project.json), scene.pkg, scene JSON file,
    /// or the workshop id of an installed wallpaper.
    #[arg(value_parser = parse_wallpaper)]
    wallpaper: PathBuf,
    /// Wallpaper Engine's stock assets directory.
    #[arg(long)]
    assets: Option<PathBuf>,
    /// Render one frame offscreen to this PNG and exit.
    #[arg(long)]
    screenshot: Option<PathBuf>,
    /// Output size for --screenshot, or the initial window size.
    #[arg(long, default_value = "1920x1080", value_parser = parse_size)]
    size: (u32, u32),
    #[arg(long, value_enum, default_value_t = Fill::Auto)]
    fill: Fill,
    /// Scene time in seconds for --screenshot.
    #[arg(long, default_value_t = 0.0)]
    time: f32,
    /// With --screenshot, run this many frames at 30 fps from --time first,
    /// clicking across the scene now and then (exercises scripts).
    #[arg(long, default_value_t = 1)]
    frames: u32,
    /// With --screenshot, the pointer for the final frame: `x,y`, 0..1 over
    /// the output with Y down.
    #[arg(long, value_parser = parse_pointer)]
    pointer: Option<glam::Vec2>,
    /// Play sound layers, a video wallpaper's sound, and react to system audio.
    #[arg(long)]
    audio: bool,
    /// Log the frame rate every few seconds.
    #[arg(long)]
    stats: bool,
    /// Frame rate cap (0 = follow the display).
    #[arg(long, default_value_t = 0)]
    fps: u32,
    /// Set a user property, as the settings would: `name=value`, the value
    /// as JSON (`true`, `0.5`, `"1 0 0"`) or else a string. Repeatable.
    #[arg(long = "property", value_name = "NAME=VALUE", value_parser = parse_property)]
    properties: Vec<(String, serde_json::Value)>,
}

fn parse_pointer(s: &str) -> Result<glam::Vec2, String> {
    let (x, y) = s.split_once(',').ok_or("expected X,Y")?;
    let n = |v: &str| v.trim().parse::<f32>().map_err(|e| e.to_string());
    Ok(glam::Vec2::new(n(x)?, n(y)?))
}

fn parse_property(s: &str) -> Result<(String, serde_json::Value), String> {
    let (name, value) = s.split_once('=').ok_or("expected NAME=VALUE")?;
    let value = serde_json::from_str(value).unwrap_or_else(|_| value.into());
    Ok((name.to_owned(), value))
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Fill {
    Auto,
    Crop,
    Fit,
    Stretch,
}

impl From<Fill> for FillMode {
    fn from(f: Fill) -> Self {
        match f {
            Fill::Auto => FillMode::Auto,
            Fill::Crop => FillMode::Crop,
            Fill::Fit => FillMode::Fit,
            Fill::Stretch => FillMode::Stretch,
        }
    }
}

fn parse_wallpaper(s: &str) -> Result<PathBuf, String> {
    wpe_runtime::steam::resolve_wallpaper(s)
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once('x').ok_or("expected WIDTHxHEIGHT")?;
    let parse = |v: &str| v.parse::<u32>().map_err(|e| e.to_string());
    Ok((parse(w)?, parse(h)?))
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,wgpu_core=warn,wgpu_hal=warn,naga=error,script=error,servo_base=error,paint=warn,webrender=warn,layout=warn,style=warn,net=warn,fonts=warn,constellation=warn,storage=error,profile_traits=error".into()),
        )
        .init();
    let args = Args::parse();
    let assets = args
        .assets
        .clone()
        .or_else(wpe_runtime::steam::default_assets_dir)
        .context("Wallpaper Engine assets directory not found; pass --assets")?;

    if let Some(out) = &args.screenshot {
        let result = screenshot(&args, &assets, out);
        wpe_runtime::shutdown();
        return result;
    }

    let event_loop = EventLoop::new()?;
    let mut app = App {
        args,
        assets,
        state: None,
        error: None,
    };
    event_loop.run_app(&mut app)?;
    let error = app.error.take();
    drop(app);
    wpe_runtime::shutdown();
    error.map_or(Ok(()), Err)
}

fn screenshot(args: &Args, assets: &std::path::Path, out: &std::path::Path) -> Result<()> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = pollster::block_on(Gpu::new(instance, None))?;
    let mut wallpaper = Wallpaper::load(
        &gpu,
        wgpu::TextureFormat::Rgba8Unorm,
        assets,
        &args.wallpaper,
    )?;
    for (name, value) in &args.properties {
        wallpaper.set_user_property(&gpu, name, value.clone());
    }
    wallpaper.fill = args.fill.into();
    if args.audio {
        wallpaper.set_audio(true);
    }
    wallpaper.seek_videos(args.time);
    // A page runs on the wall clock and lays out at the output size, so it
    // is shown at full size throughout and given time to load its assets.
    let web = wallpaper.is_web();
    let small = if web { args.size } else { (64, 64) };
    if web {
        // Pages advance only as frames are drawn.
        let settle =
            std::time::Instant::now() + std::time::Duration::from_secs_f32(args.time.max(3.0));
        while std::time::Instant::now() < settle {
            wallpaper.capture(&gpu, args.size);
            std::thread::sleep(std::time::Duration::from_millis(33));
        }
    }
    for frame in 1..args.frames {
        wallpaper.time_override = Some(args.time + frame as f32 / 30.0);
        // Every half second, press and release somewhere on a 4x4 grid.
        let click = frame / 15;
        match frame % 15 {
            7 => {
                let cell = glam::Vec2::new((click % 4) as f32, (click / 4 % 4) as f32);
                wallpaper.set_pointer((cell + 0.5) / 4.0);
                wallpaper.mouse_button(true);
            }
            8 => wallpaper.mouse_button(false),
            _ => {}
        }
        wallpaper.capture(&gpu, small);
        if web {
            std::thread::sleep(std::time::Duration::from_millis(33));
        }
    }
    wallpaper.time_override = Some(args.time + args.frames.saturating_sub(1) as f32 / 30.0);
    if let Some(pointer) = args.pointer {
        wallpaper.set_pointer(pointer);
    }
    let pixels = wallpaper.capture(&gpu, args.size);
    image::RgbaImage::from_raw(args.size.0, args.size.1, pixels)
        .context("capture size mismatch")?
        .save(out)?;
    tracing::info!("wrote {}", out.display());
    if args.audio {
        tracing::info!("video sound playing: {:?}", wallpaper.video_audio());
    }
    Ok(())
}

/// Frame counter for `--stats`.
struct Stats {
    since: std::time::Instant,
    frames: u32,
    busy: std::time::Duration,
}

struct State {
    stats: Option<Stats>,
    /// When the next capped frame is due.
    next_frame: std::time::Instant,
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    gpu: Gpu,
    wallpaper: Wallpaper,
}

struct App {
    args: Args,
    assets: PathBuf,
    state: Option<State>,
    error: Option<anyhow::Error>,
}

impl App {
    fn init(&mut self, event_loop: &ActiveEventLoop) -> Result<State> {
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("scene-viewer")
                    .with_inner_size(PhysicalSize::new(self.args.size.0, self.args.size.1)),
            )?,
        );
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(
                Box::new(event_loop.owned_display_handle()),
            ));
        let surface = instance.create_surface(window.clone())?;
        let gpu = pollster::block_on(Gpu::new(instance, Some(&surface)))?;

        let caps = surface.get_capabilities(&gpu.adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&gpu.adapter, size.width.max(1), size.height.max(1))
            .context("surface is incompatible with the adapter")?;
        config.format = format;
        config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(&gpu.device, &config);

        let mut wallpaper = Wallpaper::load(&gpu, format, &self.assets, &self.args.wallpaper)?;
        for (name, value) in &self.args.properties {
            wallpaper.set_user_property(&gpu, name, value.clone());
        }
        wallpaper.fill = self.args.fill.into();
        wallpaper.set_audio(self.args.audio);
        window.set_title(&format!("{} — scene-viewer", wallpaper.title));
        Ok(State {
            next_frame: std::time::Instant::now(),
            stats: self.args.stats.then(|| Stats {
                since: std::time::Instant::now(),
                frames: 0,
                busy: Default::default(),
            }),
            window,
            surface,
            config,
            gpu,
            wallpaper,
        })
    }
}

impl State {
    fn redraw(&mut self) {
        let started = std::time::Instant::now();
        self.draw();
        if let Some(stats) = &mut self.stats {
            stats.frames += 1;
            stats.busy += started.elapsed();
            let elapsed = stats.since.elapsed();
            if elapsed.as_secs_f32() >= 3.0 {
                tracing::info!(
                    "{:.1} fps, {:.2} ms CPU per frame",
                    stats.frames as f32 / elapsed.as_secs_f32(),
                    stats.busy.as_secs_f32() * 1000.0 / stats.frames as f32
                );
                *stats = Stats {
                    since: std::time::Instant::now(),
                    frames: 0,
                    busy: Default::default(),
                };
            }
        }
    }

    fn draw(&mut self) {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.gpu.device, &self.config);
                return;
            }
            other => {
                tracing::debug!("skipping frame: {other:?}");
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        self.wallpaper.render(
            &self.gpu,
            &mut encoder,
            &view,
            (self.config.width, self.config.height),
        );
        self.gpu.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        self.gpu.queue.present(frame);
    }
}

impl ApplicationHandler for App {
    fn new_events(&mut self, _: &ActiveEventLoop, cause: winit::event::StartCause) {
        if let (winit::event::StartCause::ResumeTimeReached { .. }, Some(state)) =
            (cause, &self.state)
        {
            state.window.request_redraw();
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        match self.init(event_loop) {
            Ok(state) => self.state = Some(state),
            Err(e) => {
                self.error = Some(e);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(state) = &mut self.state else { return };
        match event {
            WindowEvent::CloseRequested
            | WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => event_loop.exit(),
            WindowEvent::Resized(size) => {
                state.config.width = size.width.max(1);
                state.config.height = size.height.max(1);
                state.surface.configure(&state.gpu.device, &state.config);
                state.window.request_redraw();
            }
            WindowEvent::CursorMoved { position, .. } => {
                let size = state.window.inner_size();
                state.wallpaper.set_pointer(glam::Vec2::new(
                    position.x as f32 / size.width.max(1) as f32,
                    position.y as f32 / size.height.max(1) as f32,
                ));
            }
            WindowEvent::MouseInput {
                state: button_state,
                button,
                ..
            } => {
                let button = match button {
                    winit::event::MouseButton::Left => wpe_runtime::MouseButton::Left,
                    winit::event::MouseButton::Right => wpe_runtime::MouseButton::Right,
                    winit::event::MouseButton::Middle => wpe_runtime::MouseButton::Middle,
                    _ => return,
                };
                state
                    .wallpaper
                    .pointer_button(button, button_state == ElementState::Pressed);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // winit scrolls up for positive values; wallpapers take down.
                let (dx, dy) = match delta {
                    winit::event::MouseScrollDelta::LineDelta(x, y) => (x * 40.0, y * 40.0),
                    winit::event::MouseScrollDelta::PixelDelta(p) => (p.x as f32, p.y as f32),
                };
                state.wallpaper.scroll(-dx, -dy);
            }
            WindowEvent::RedrawRequested => {
                state
                    .wallpaper
                    .set_scale(state.window.scale_factor() as f32);
                state.redraw();
                if self.args.fps == 0 {
                    state.window.request_redraw();
                } else {
                    let interval = std::time::Duration::from_secs_f64(1.0 / self.args.fps as f64);
                    let next =
                        state.next_frame.max(std::time::Instant::now() - interval) + interval;
                    state.next_frame = next;
                    event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(next));
                }
            }
            _ => {}
        }
    }
}
