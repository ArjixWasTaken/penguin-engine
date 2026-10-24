//! Desktop wallpaper mode: plays a Wallpaper Engine wallpaper on the desktop
//! background of a Wayland session through wlr-layer-shell, one layer
//! surface (and one `Wallpaper`) per output. A control socket
//! ([`wpe_ipc`]) changes wallpapers and settings at runtime; the choices are
//! saved and restored on the next start.

mod commands;
mod control;
mod loading;
mod state;
mod systemd;

use std::io::Read;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, RawDisplayHandle, RawWindowHandle,
    WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, Region};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::channel;
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::{
    EventLoop, Interest, LoopHandle, Mode, PostAction,
};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::{
    wl_output::{self, WlOutput},
    wl_pointer::WlPointer,
    wl_seat::WlSeat,
    wl_surface::WlSurface,
};
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, Proxy, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState, SimpleGlobal};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_simple, registry_handlers,
};
use wpe_ipc::{Fill, Settings};
use wpe_render::{Gpu, wgpu};
use wpe_runtime::{FillMode, Wallpaper};

use loading::{LoadSpec, Loads, NewSurface, SurfaceParts};
use state::{Overrides, SavedOutput, State};
use systemd::NotifyState;

/// `BTN_LEFT` from linux/input-event-codes.h.
const BTN_LEFT: u32 = 0x110;

#[derive(Parser)]
#[command(about = "Play a Wallpaper Engine wallpaper as the desktop background (Wayland)")]
struct Args {
    /// Wallpaper directory (with project.json), scene.pkg, scene JSON file,
    /// or the workshop id of an installed wallpaper. It is shown on every
    /// output and saved; without one, the wallpapers saved last time return.
    wallpaper: Option<String>,
    /// Wallpaper Engine's stock assets directory.
    #[arg(long)]
    assets: Option<PathBuf>,
    /// How the wallpaper fills the output: crop, fit or stretch.
    #[arg(long)]
    fill: Option<Fill>,
    /// Layer-shell layer: background (default) or bottom. On KDE Plasma
    /// `background` shares the desktop's layer; `bottom` is always above the
    /// desktop and below windows.
    #[arg(long)]
    layer: Option<wpe_ipc::Layer>,
    /// Only show the wallpaper on this output (connector name, e.g. DP-1).
    #[arg(long)]
    output: Option<String>,
    /// Let pointer input through to whatever is underneath (the wallpaper
    /// then gets no cursor events).
    #[arg(long)]
    passthrough: bool,
    /// Play sound layers, a video wallpaper's sound, and react to system audio
    /// (on the first output only).
    #[arg(long)]
    audio: bool,
    /// Sound volume in percent.
    #[arg(long, value_parser = clap::value_parser!(u32).range(0..=100))]
    volume: Option<u32>,
    /// Frame rate cap (0 = follow the display).
    #[arg(long)]
    fps: Option<u32>,
    /// Log each output's frame rate every few seconds.
    #[arg(long)]
    stats: bool,
    /// Set a user property of the wallpaper given: `name=value`, the value
    /// as JSON (`true`, `0.5`, `"1 0 0"`) or else a string. Repeatable.
    #[arg(long = "property", value_name = "NAME=VALUE", value_parser = parse_property)]
    properties: Vec<(String, serde_json::Value)>,
    /// Control socket path, instead of $XDG_RUNTIME_DIR/wallpaper-engine-rs.sock
    /// (ignored when systemd passes the socket in).
    #[arg(long)]
    socket: Option<PathBuf>,
}

impl Args {
    /// The settings given on the command line.
    fn overrides(&self) -> Overrides {
        Overrides {
            fps: self.fps,
            audio: self.audio.then_some(true),
            volume: self.volume,
            fill: self.fill,
            layer: self.layer,
            passthrough: self.passthrough.then_some(true),
        }
    }
}

fn parse_property(s: &str) -> Result<(String, serde_json::Value), String> {
    let (name, value) = s.split_once('=').ok_or("expected NAME=VALUE")?;
    let value = serde_json::from_str(value).unwrap_or_else(|_| value.into());
    Ok((name.to_owned(), value))
}

fn fill_mode(fill: Fill) -> FillMode {
    match fill {
        Fill::Crop => FillMode::Crop,
        Fill::Fit => FillMode::Fit,
        Fill::Stretch => FillMode::Stretch,
    }
}

fn shell_layer(layer: wpe_ipc::Layer) -> Layer {
    match layer {
        wpe_ipc::Layer::Background => Layer::Background,
        wpe_ipc::Layer::Bottom => Layer::Bottom,
    }
}

/// The Wayland display, handed to wgpu's instance.
#[derive(Debug)]
struct Display(Connection);

impl HasDisplayHandle for Display {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        let ptr =
            NonNull::new(self.0.backend().display_ptr().cast()).ok_or(HandleError::Unavailable)?;
        let raw = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(ptr));
        // SAFETY: the display lives as long as the connection we hold.
        Ok(unsafe { DisplayHandle::borrow_raw(raw) })
    }
}

/// Frame rate counter.
struct Stats {
    since: Instant,
    frames: u32,
    /// Over the last full window.
    fps: f32,
    /// When the last frame was drawn, and the longest wait between two
    /// frames in this window and the last full one.
    last: Option<Instant>,
    longest: Duration,
    longest_in_window: Duration,
}

impl Stats {
    const WINDOW: Duration = Duration::from_secs(3);

    fn new() -> Self {
        Self {
            since: Instant::now(),
            frames: 0,
            fps: 0.0,
            last: None,
            longest: Duration::ZERO,
            longest_in_window: Duration::ZERO,
        }
    }

    /// Count a frame; true when a window completed.
    fn frame(&mut self) -> bool {
        let now = Instant::now();
        if let Some(last) = self.last.replace(now) {
            self.longest_in_window = self.longest_in_window.max(now - last);
        }
        self.frames += 1;
        let elapsed = self.since.elapsed();
        if elapsed < Self::WINDOW {
            return false;
        }
        self.fps = self.frames as f32 / elapsed.as_secs_f32();
        self.since = Instant::now();
        self.frames = 0;
        self.longest = std::mem::take(&mut self.longest_in_window);
        true
    }

    /// The frame rate now (lower when frames stopped coming).
    fn current(&self) -> f32 {
        let elapsed = self.since.elapsed();
        // Before the first full window, or when frames stopped coming.
        if self.fps == 0.0 || elapsed > Self::WINDOW * 2 {
            self.frames as f32 / elapsed.as_secs_f32()
        } else {
            self.fps
        }
    }
}

/// The wallpaper on one output.
struct Screen {
    // Drop order: the wgpu surface goes before the Wayland surface under it.
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    wallpaper: Wallpaper,
    /// Where `wallpaper` was loaded from.
    path: PathBuf,
    fractional: Option<WpFractionalScaleV1>,
    viewport: Option<WpViewport>,
    layer: LayerSurface,
    output: WlOutput,
    name: String,
    /// Size in surface-local (logical) pixels, from the last configure.
    logical: (u32, u32),
    /// Preferred fractional scale in 120ths (with a viewport).
    scale120: u32,
    /// Integer buffer scale (without a viewport).
    buffer_scale: i32,
    configured: bool,
    /// Waiting on a frame callback.
    frame_pending: bool,
    /// A `--fps` timer is armed.
    timer_pending: bool,
    next_frame: Instant,
    paused: bool,
    /// Needs a frame even while paused (resized, changed).
    dirty: bool,
    fill: Fill,
    stats: Stats,
}

impl Drop for Screen {
    fn drop(&mut self) {
        if let Some(f) = self.fractional.take() {
            f.destroy();
        }
        if let Some(v) = self.viewport.take() {
            v.destroy();
        }
    }
}

impl Screen {
    /// A screen showing `wallpaper` (from `path`) on a surface made for it.
    fn new(
        parts: SurfaceParts,
        wallpaper: Wallpaper,
        path: PathBuf,
        output: WlOutput,
        name: String,
        fill: Fill,
    ) -> Self {
        Self {
            surface: parts.surface,
            config: parts.config,
            wallpaper,
            path,
            fractional: parts.fractional,
            viewport: parts.viewport,
            layer: parts.layer,
            output,
            name,
            logical: (0, 0),
            scale120: parts.scale120,
            buffer_scale: parts.buffer_scale,
            configured: false,
            frame_pending: false,
            timer_pending: false,
            next_frame: Instant::now(),
            paused: false,
            dirty: true,
            fill,
            stats: Stats::new(),
        }
    }

    fn buffer_size(&self) -> (u32, u32) {
        let (w, h) = self.logical;
        if self.viewport.is_some() {
            let scale = |v: u32| ((v * self.scale120 + 60) / 120).max(1);
            (scale(w), scale(h))
        } else {
            let s = self.buffer_scale.max(1) as u32;
            (w * s, h * s)
        }
    }

    /// Apply the current logical size and scale to the swapchain.
    fn reconfigure(&mut self, gpu: &Gpu) {
        if self.logical.0 == 0 || self.logical.1 == 0 {
            return;
        }
        let (bw, bh) = self.buffer_size();
        match &self.viewport {
            Some(viewport) => {
                viewport.set_destination(self.logical.0 as i32, self.logical.1 as i32)
            }
            None => self
                .layer
                .wl_surface()
                .set_buffer_scale(self.buffer_scale.max(1)),
        }
        if (self.config.width, self.config.height) != (bw, bh) || !self.configured {
            self.config.width = bw;
            self.config.height = bh;
            self.surface.configure(&gpu.device, &self.config);
            tracing::info!(
                "{}: {}x{} logical, {bw}x{bh} buffer",
                self.name,
                self.logical.0,
                self.logical.1
            );
        }
        self.configured = true;
        self.dirty = true;
    }

    /// Show `wallpaper` (from `path`) instead of the current one.
    fn replace_wallpaper(&mut self, mut wallpaper: Wallpaper, path: PathBuf) {
        wallpaper.fill = fill_mode(self.fill);
        wallpaper.set_paused(self.paused);
        let started = Instant::now();
        drop(std::mem::replace(&mut self.wallpaper, wallpaper));
        tracing::debug!(
            "{}: old wallpaper dropped in {:.1?}",
            self.name,
            started.elapsed()
        );
        self.path = path;
        self.dirty = true;
    }

    fn set_fill(&mut self, fill: Fill) {
        self.fill = fill;
        self.wallpaper.fill = fill_mode(fill);
        self.dirty = true;
    }

    fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        self.wallpaper.set_paused(paused);
        self.dirty = true;
    }

    /// The input region for `passthrough`, committed with the next frame.
    fn set_passthrough(&self, compositor: &CompositorState, passthrough: bool) {
        let surface = self.layer.wl_surface();
        if passthrough {
            match Region::new(compositor) {
                Ok(region) => surface.set_input_region(Some(region.wl_region())),
                Err(e) => tracing::warn!("no input region: {e}"),
            }
        } else {
            surface.set_input_region(None);
        }
    }

    /// Render and present a frame, asking for a frame callback. Returns
    /// false when no frame could be presented.
    fn draw(&mut self, gpu: &Gpu, qh: &QueueHandle<App>, log_stats: bool) -> bool {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&gpu.device, &self.config);
                return false;
            }
            other => {
                tracing::debug!("{}: skipping frame: {other:?}", self.name);
                return false;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        self.wallpaper.render(
            gpu,
            &mut encoder,
            &view,
            (self.config.width, self.config.height),
        );
        // The callback request is committed by the present below; it fires
        // when the compositor wants the next frame, and not at all while the
        // surface is hidden, which pauses rendering.
        let surface = self.layer.wl_surface();
        surface.frame(qh, surface.clone());
        self.frame_pending = true;
        gpu.queue.submit([encoder.finish()]);
        gpu.queue.present(frame);

        self.dirty = false;
        if self.stats.frame() && log_stats {
            tracing::info!(
                "{}: {:.1} fps, longest frame gap {:.1?}",
                self.name,
                self.stats.fps,
                self.stats.longest
            );
        }
        true
    }
}

/// An output we show wallpapers on (it may have no screen yet, when it has
/// no wallpaper or its wallpaper failed to load).
struct KnownOutput {
    output: WlOutput,
    name: String,
}

struct App {
    args: Args,
    assets: PathBuf,
    conn: Connection,
    qh: QueueHandle<App>,
    loop_handle: LoopHandle<'static, App>,
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    viewporter: Option<SimpleGlobal<WpViewporter, 1>>,
    fractional_manager: Option<SimpleGlobal<WpFractionalScaleManagerV1, 1>>,
    pointers: Vec<WlPointer>,
    outputs: Vec<KnownOutput>,
    screens: Vec<Screen>,
    /// Created with the first surface, shared by every output and the
    /// threads loading wallpapers.
    gpu: Option<Arc<Gpu>>,
    /// Wallpapers loading.
    loads: Loads,
    /// The settings in effect.
    settings: Settings,
    /// Settings from the command line.
    cli: Overrides,
    config_path: Option<PathBuf>,
    state: State,
    state_path: Option<PathBuf>,
    /// systemd has been told we're up.
    ready: bool,
    exit: bool,
    error: Option<anyhow::Error>,
}

impl App {
    fn screen_index(&self, surface: &WlSurface) -> Option<usize> {
        self.screens
            .iter()
            .position(|s| s.layer.wl_surface() == surface)
    }

    fn screen_for(&self, output: &WlOutput) -> Option<usize> {
        self.screens.iter().position(|s| s.output == *output)
    }

    fn save_state(&self) {
        if let Some(path) = &self.state_path
            && let Err(e) = self.state.save(path)
        {
            tracing::warn!("cannot save the state: {e:#}");
        }
    }

    /// Sound plays from the first screen only.
    fn apply_audio(&mut self) {
        let audio = self.settings.audio;
        for (i, screen) in self.screens.iter_mut().enumerate() {
            screen.wallpaper.set_audio(audio && i == 0);
        }
    }

    /// Put a layer surface showing `saved` on `output`, once it has loaded.
    fn create_screen(&mut self, output: &WlOutput, name: &str, saved: &SavedOutput) -> Result<()> {
        let surface = self.new_surface(output, name)?;
        let spec = LoadSpec {
            output: output.clone(),
            name: name.to_owned(),
            path: saved.wallpaper.clone(),
            properties: saved.property_list(),
            fill: saved.fill.unwrap_or(self.settings.fill),
            request: None,
        };
        self.start_load(spec, Some(surface));
        Ok(())
    }

    /// A layer surface for `output`, not committed yet.
    fn new_surface(&mut self, output: &WlOutput, name: &str) -> Result<NewSurface> {
        let qh = self.qh.clone();
        let info = self
            .output_state
            .info(output)
            .context("output without info")?;
        tracing::info!(
            "output {name}: {} {} {:?} scale {}",
            info.make,
            info.model,
            info.logical_size,
            info.scale_factor
        );

        let wl_surface = self.compositor.create_surface(&qh);
        let layer = self.layer_shell.create_layer_surface(
            &qh,
            wl_surface,
            shell_layer(self.settings.layer),
            Some("wallpaper"),
            Some(output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        let scaling = self
            .viewporter
            .as_ref()
            .and_then(|v| v.get().ok())
            .zip(self.fractional_manager.as_ref().and_then(|f| f.get().ok()));
        let (viewport, fractional) = match scaling {
            Some((viewporter, manager)) => (
                Some(viewporter.get_viewport(layer.wl_surface(), &qh, ())),
                Some(manager.get_fractional_scale(layer.wl_surface(), &qh, ())),
            ),
            None => (None, None),
        };
        // Until the screen exists, failures must not leak the objects.
        let cleanup = |viewport: Option<WpViewport>, fractional: Option<WpFractionalScaleV1>| {
            if let Some(v) = viewport {
                v.destroy();
            }
            if let Some(f) = fractional {
                f.destroy();
            }
        };

        let surface = (|| -> Result<_> {
            let raw_window_handle = RawWindowHandle::Wayland(WaylandWindowHandle::new(
                NonNull::new(layer.wl_surface().id().as_ptr().cast()).context("null wl_surface")?,
            ));
            let raw_display_handle = Display(self.conn.clone()).display_handle()?.as_raw();
            let instance = match &self.gpu {
                Some(gpu) => gpu.instance.clone(),
                None => {
                    wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(
                        Box::new(Display(self.conn.clone())),
                    ))
                }
            };
            // SAFETY: the wl_surface (owned by `layer`) outlives the wgpu
            // surface: `Screen` drops `surface` first.
            let surface = unsafe {
                instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                    raw_display_handle: Some(raw_display_handle),
                    raw_window_handle,
                })?
            };
            if self.gpu.is_none() {
                let gpu = pollster::block_on(Gpu::new(instance, Some(&surface)))?;
                self.gpu = Some(Arc::new(gpu));
            }
            Ok(surface)
        })();
        let surface = match surface {
            Ok(s) => s,
            Err(e) => {
                cleanup(viewport, fractional);
                return Err(e);
            }
        };
        let gpu = self.gpu.as_ref().unwrap();

        let caps = surface.get_capabilities(&gpu.adapter);
        let (lw, lh) = info.logical_size.unwrap_or((1920, 1080));
        let config = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .and_then(|format| {
                let mut config =
                    surface.get_default_config(&gpu.adapter, lw.max(1) as u32, lh.max(1) as u32)?;
                config.format = format;
                Some(config)
            });
        let Some(mut config) = config else {
            cleanup(viewport, fractional);
            anyhow::bail!("surface is incompatible with the adapter");
        };
        // Frame callbacks pace us; a mailbox never blocks in present (FIFO
        // can stall the whole loop while a surface is hidden).
        config.present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };
        if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
            config.alpha_mode = wgpu::CompositeAlphaMode::Opaque;
        }

        Ok(NewSurface::new(SurfaceParts {
            surface,
            config,
            fractional,
            viewport,
            layer,
            scale120: info.scale_factor.max(1) as u32 * 120,
            buffer_scale: info.scale_factor.max(1),
        }))
    }

    /// A new output appeared: show its saved wallpaper (or the last one set).
    fn add_output(&mut self, output: WlOutput) -> Result<()> {
        let info = self
            .output_state
            .info(&output)
            .context("output without info")?;
        let name = info
            .name
            .clone()
            .unwrap_or_else(|| format!("output-{}", info.id));
        if let Some(wanted) = &self.args.output
            && *wanted != name
        {
            tracing::info!("skipping output {name} (not --output {wanted})");
            return Ok(());
        }
        self.outputs.push(KnownOutput {
            output: output.clone(),
            name: name.clone(),
        });
        match self.state.for_output(&name).cloned() {
            Some(saved) => self.create_screen(&output, &name, &saved),
            None => {
                tracing::info!("output {name}: no wallpaper set");
                Ok(())
            }
        }
    }

    /// Draw the screen now, or arm a timer for when the frame rate cap
    /// allows it.
    fn schedule(&mut self, index: usize) {
        let Some(gpu) = &self.gpu else { return };
        let qh = &self.qh;
        let screen = &mut self.screens[index];
        if !screen.configured || screen.frame_pending || screen.timer_pending {
            return;
        }
        if screen.paused && !screen.dirty {
            return;
        }
        let now = Instant::now();
        let fps = self.settings.fps;
        if fps > 0 && screen.next_frame > now {
            screen.timer_pending = true;
            let surface = screen.layer.wl_surface().clone();
            let timer = Timer::from_deadline(screen.next_frame);
            let inserted = self.loop_handle.insert_source(timer, move |_, _, app| {
                if let Some(i) = app.screen_index(&surface) {
                    app.screens[i].timer_pending = false;
                    app.schedule(i);
                }
                TimeoutAction::Drop
            });
            if let Err(e) = inserted {
                tracing::error!("cannot arm frame timer: {e}");
                screen.timer_pending = false;
            }
            return;
        }
        if fps > 0 {
            let interval = Duration::from_secs_f64(1.0 / fps as f64);
            screen.next_frame = screen.next_frame.max(now - interval) + interval;
        }
        if !screen.draw(gpu, qh, self.args.stats) {
            // Retry shortly; nothing will call back otherwise.
            screen.timer_pending = true;
            let surface = screen.layer.wl_surface().clone();
            let timer = Timer::from_duration(Duration::from_millis(100));
            let _ = self.loop_handle.insert_source(timer, move |_, _, app| {
                if let Some(i) = app.screen_index(&surface) {
                    app.screens[i].timer_pending = false;
                    app.schedule(i);
                }
                TimeoutAction::Drop
            });
            return;
        }
        if !self.ready {
            self.ready = true;
            self.update_status();
            systemd::notify(&[NotifyState::Ready]);
            tracing::info!("ready");
        }
    }

    fn schedule_all(&mut self) {
        for i in 0..self.screens.len() {
            self.schedule(i);
        }
    }

    fn rescale(&mut self, index: usize) {
        if let Some(gpu) = &self.gpu {
            let screen = &mut self.screens[index];
            if screen.configured {
                screen.reconfigure(gpu);
                self.schedule(index);
            }
        }
    }

    /// Tell systemd what is playing where.
    fn update_status(&self) {
        let text = if self.screens.is_empty() {
            "No wallpaper".to_owned()
        } else {
            self.screens
                .iter()
                .map(|s| {
                    let paused = if s.paused { " (paused)" } else { "" };
                    format!("{}: {}{paused}", s.name, s.wallpaper.title)
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        systemd::status(&text);
    }
}

/// A pipe that signal handlers write to, watched by the loop.
fn signal_pipe(signals: &[i32]) -> Result<UnixStream> {
    let (rx, tx) = UnixStream::pair()?;
    for &signal in signals {
        signal_hook::low_level::pipe::register(signal, tx.try_clone()?)?;
    }
    Ok(rx)
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,wgpu_core=warn,wgpu_hal=warn,naga=error".into()),
        )
        .init();
    let args = Args::parse();
    let assets = args
        .assets
        .clone()
        .or_else(wpe_runtime::steam::default_assets_dir)
        .context("Wallpaper Engine assets directory not found; pass --assets")?;

    let config_path = state::config_path();
    let config = match &config_path {
        Some(path) => state::load_config(path).unwrap_or_else(|e| {
            tracing::warn!("{e:#}; using the default settings");
            Settings::default()
        }),
        None => Settings::default(),
    };
    let state_path = state::state_path();
    let mut state = state_path.as_deref().map(State::load).unwrap_or_default();
    let cli = args.overrides();
    let settings = state::startup_settings(&config, &state, &cli);
    if let Some(spec) = &args.wallpaper {
        let path = wpe_runtime::steam::resolve_wallpaper(spec).map_err(anyhow::Error::msg)?;
        let path = path.canonicalize().unwrap_or(path);
        let mut saved = SavedOutput::new(path);
        saved.properties = args.properties.iter().cloned().collect();
        state.set_everywhere(saved);
        if let Some(path) = &state_path
            && let Err(e) = state.save(path)
        {
            tracing::warn!("cannot save the state: {e:#}");
        }
    } else if !args.properties.is_empty() {
        tracing::warn!("--property only applies to a wallpaper given on the command line");
    }

    // Before touching the display, so a second daemon stops right here.
    let socket_path = args.socket.clone().or_else(wpe_ipc::socket_path);
    let socket = control::listen(socket_path.as_deref())?;

    let conn = Connection::connect_to_env().context("cannot connect to a Wayland compositor")?;
    let (globals, queue) = registry_queue_init::<App>(&conn)?;
    let qh = queue.handle();
    let mut event_loop: EventLoop<'static, App> = EventLoop::try_new()?;
    let mut wayland = WaylandSource::new(conn.clone(), queue);

    // Signals write to sockets the loop watches, so the surfaces are torn
    // down in order (SIGINT/SIGTERM) and settings reload between frames
    // (SIGHUP).
    let exit_rx = signal_pipe(&[signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM])?;
    event_loop
        .handle()
        .insert_source(
            Generic::new(exit_rx, Interest::READ, Mode::Level),
            |_, _, app| {
                tracing::info!("signal received, exiting");
                app.exit = true;
                Ok(PostAction::Remove)
            },
        )
        .map_err(|e| anyhow::anyhow!("cannot watch signals: {}", e.error))?;
    let hup_rx = signal_pipe(&[signal_hook::consts::SIGHUP])?;
    event_loop
        .handle()
        .insert_source(
            Generic::new(hup_rx, Interest::READ, Mode::Level),
            |_, rx, app| {
                let mut buf = [0u8; 64];
                let _ = (&**rx).read(&mut buf);
                tracing::info!("SIGHUP: reloading the settings");
                systemd::notify(&[NotifyState::Reloading]);
                if let Err(e) = app.reload_settings() {
                    tracing::error!("{e}");
                }
                systemd::notify(&[NotifyState::Ready]);
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| anyhow::anyhow!("cannot watch signals: {}", e.error))?;
    systemd::start_watchdog(&event_loop.handle());
    // Nothing to render (no wallpaper yet, no outputs): up all the same.
    event_loop
        .handle()
        .insert_source(Timer::from_duration(Duration::from_secs(2)), |_, _, app| {
            if !app.ready && app.screens.is_empty() && app.loads.is_empty() {
                app.ready = true;
                app.update_status();
                systemd::notify(&[NotifyState::Ready]);
                tracing::info!("ready, with no wallpaper showing");
            }
            TimeoutAction::Drop
        })
        .map_err(|e| anyhow::anyhow!("cannot arm a timer: {e}"))?;

    // Wallpapers prepared on worker threads come back here.
    let (load_tx, load_rx) = channel::channel();
    event_loop
        .handle()
        .insert_source(load_rx, |event, _, app| {
            if let channel::Event::Msg(done) = event {
                app.load_done(done);
            }
        })
        .map_err(|e| anyhow::anyhow!("cannot watch the loading threads: {}", e.error))?;

    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor missing")?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .context("the compositor does not support wlr-layer-shell")?;
    let viewporter = SimpleGlobal::bind(&globals, &qh).ok();
    let fractional_manager = SimpleGlobal::bind(&globals, &qh).ok();
    let mut app = App {
        args,
        assets,
        conn: conn.clone(),
        qh: qh.clone(),
        loop_handle: event_loop.handle(),
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        compositor,
        layer_shell,
        viewporter,
        fractional_manager,
        pointers: Vec::new(),
        outputs: Vec::new(),
        screens: Vec::new(),
        gpu: None,
        loads: Loads::new(load_tx),
        settings,
        cli,
        config_path,
        state,
        state_path,
        ready: false,
        exit: false,
        error: None,
    };

    // Learn the outputs (and start loading their wallpapers) before
    // answering anyone, so the first request (the one that socket-activated
    // us) sees them.
    for _ in 0..2 {
        wayland.queue().roundtrip(&mut app)?;
    }
    wayland
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("cannot watch the Wayland socket: {}", e.error))?;
    let _socket = control::serve(socket, &event_loop.handle())?;

    while !app.exit {
        event_loop.dispatch(None, &mut app)?;
    }
    systemd::notify(&[NotifyState::Stopping]);
    app.screens.clear();
    // Dropped video streams finish shutting down in the background.
    wpe_runtime::shutdown();
    let _ = conn.flush();
    app.error.map_or(Ok(()), Err)
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &WlSurface,
        factor: i32,
    ) {
        if let Some(i) = self.screen_index(surface)
            && self.screens[i].viewport.is_none()
        {
            self.screens[i].buffer_scale = factor;
            self.rescale(i);
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &WlSurface, _: u32) {
        if let Some(i) = self.screen_index(surface) {
            self.screens[i].frame_pending = false;
            self.schedule(i);
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: WlOutput) {
        if let Err(e) = self.add_output(output) {
            // A wallpaper named on the command line that doesn't load is
            // fatal at startup, as before; later, the output stays empty.
            if self.args.wallpaper.is_some() && !self.ready {
                self.error = Some(e);
                self.exit = true;
            } else {
                tracing::error!("{e:#}");
            }
        }
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: WlOutput) {
        self.outputs.retain(|o| {
            let keep = o.output != output;
            if !keep {
                tracing::info!("output {} removed", o.name);
            }
            keep
        });
        self.screens.retain(|s| s.output != output);
        self.cancel_loads(&output);
        // Sound moves to the new first screen.
        self.apply_audio();
        self.update_status();
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        self.screens
            .retain(|s| s.layer.wl_surface() != layer.wl_surface());
        self.apply_audio();
        self.update_status();
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let Some(i) = self.screen_index(layer.wl_surface()) else {
            return;
        };
        let Some(gpu) = &self.gpu else { return };
        let screen = &mut self.screens[i];
        let (mut w, mut h) = configure.new_size;
        if w == 0 || h == 0 {
            let size = self
                .output_state
                .info(&screen.output)
                .and_then(|info| info.logical_size)
                .unwrap_or((1920, 1080));
            (w, h) = (size.0 as u32, size.1 as u32);
        }
        screen.logical = (w, h);
        screen.reconfigure(gpu);
        self.schedule(i);
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(pointer) => self.pointers.push(pointer),
                Err(e) => tracing::warn!("no pointer: {e}"),
            }
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            self.pointers.retain(|p| {
                let on_seat = p
                    .data::<smithay_client_toolkit::seat::pointer::PointerData>()
                    .is_some_and(|d| d.seat() == &seat);
                if on_seat {
                    p.release();
                }
                !on_seat
            });
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let Some(i) = self.screen_index(&event.surface) else {
                continue;
            };
            let screen = &mut self.screens[i];
            let (w, h) = screen.logical;
            tracing::trace!(
                "{}: pointer {:?} at {:?}",
                screen.name,
                event.kind,
                event.position
            );
            match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    screen.wallpaper.set_pointer(glam::Vec2::new(
                        event.position.0 as f32 / w.max(1) as f32,
                        event.position.1 as f32 / h.max(1) as f32,
                    ));
                }
                PointerEventKind::Press {
                    button: BTN_LEFT, ..
                } => screen.wallpaper.mouse_button(true),
                PointerEventKind::Release {
                    button: BTN_LEFT, ..
                } => screen.wallpaper.mouse_button(false),
                _ => {}
            }
        }
    }
}

impl Dispatch<WpFractionalScaleV1, ()> for App {
    fn event(
        app: &mut Self,
        proxy: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event
            && let Some(i) = app
                .screens
                .iter()
                .position(|s| s.fractional.as_ref() == Some(proxy))
        {
            app.screens[i].scale120 = scale;
            app.rescale(i);
        }
    }
}

impl Dispatch<WpViewport, ()> for App {
    fn event(
        _: &mut Self,
        _: &WpViewport,
        _: <WpViewport as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_compositor!(App);
delegate_output!(App);
delegate_seat!(App);
delegate_pointer!(App);
delegate_layer!(App);
delegate_registry!(App);
delegate_simple!(App, WpViewporter, 1);
delegate_simple!(App, WpFractionalScaleManagerV1, 1);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}
