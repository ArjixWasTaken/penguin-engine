//! Desktop wallpaper mode: plays a Wallpaper Engine wallpaper on the desktop
//! background of a Wayland session through wlr-layer-shell, one layer
//! surface (and one `Wallpaper`) per output.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, RawDisplayHandle, RawWindowHandle,
    WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, Region};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
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
use wpe_render::{Gpu, wgpu};
use wpe_runtime::{FillMode, Wallpaper};

/// `BTN_LEFT` from linux/input-event-codes.h.
const BTN_LEFT: u32 = 0x110;

#[derive(Parser)]
#[command(about = "Play a Wallpaper Engine wallpaper as the desktop background (Wayland)")]
struct Args {
    /// Wallpaper directory (with project.json), scene.pkg, or scene JSON file.
    wallpaper: PathBuf,
    /// Wallpaper Engine's stock assets directory.
    #[arg(long)]
    assets: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = Fill::Crop)]
    fill: Fill,
    /// Layer-shell layer. `background` sits under everything; on KDE Plasma
    /// it is covered by plasmashell's own desktop, so use `bottom` there
    /// (above the desktop icons, below windows).
    #[arg(long, value_enum, default_value_t = LayerArg::Background)]
    layer: LayerArg,
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
    /// Frame rate cap (0 = follow the display).
    #[arg(long, default_value_t = 0)]
    fps: u32,
    /// Log each output's frame rate every few seconds.
    #[arg(long)]
    stats: bool,
    /// Set a user property: `name=value`, the value as JSON (`true`, `0.5`,
    /// `"1 0 0"`) or else a string. Repeatable.
    #[arg(long = "property", value_name = "NAME=VALUE", value_parser = parse_property)]
    properties: Vec<(String, serde_json::Value)>,
}

fn parse_property(s: &str) -> Result<(String, serde_json::Value), String> {
    let (name, value) = s.split_once('=').ok_or("expected NAME=VALUE")?;
    let value = serde_json::from_str(value).unwrap_or_else(|_| value.into());
    Ok((name.to_owned(), value))
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Fill {
    Crop,
    Fit,
    Stretch,
}

impl From<Fill> for FillMode {
    fn from(f: Fill) -> Self {
        match f {
            Fill::Crop => FillMode::Crop,
            Fill::Fit => FillMode::Fit,
            Fill::Stretch => FillMode::Stretch,
        }
    }
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum LayerArg {
    Background,
    Bottom,
}

fn default_assets_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    [
        ".local/share/Steam/steamapps/common/wallpaper_engine/assets",
        ".steam/steam/steamapps/common/wallpaper_engine/assets",
        ".var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps/common/wallpaper_engine/assets",
    ]
    .into_iter()
    .map(|p| home.join(p))
    .find(|p| p.is_dir())
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

/// Frame counter for `--stats`.
struct Stats {
    since: Instant,
    frames: u32,
}

/// The wallpaper on one output.
struct Screen {
    // Drop order: the wgpu surface goes before the Wayland surface under it.
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    wallpaper: Wallpaper,
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
    stats: Option<Stats>,
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
    }

    /// Render and present a frame, asking for a frame callback. Returns
    /// false when no frame could be presented.
    fn draw(&mut self, gpu: &Gpu, qh: &QueueHandle<App>) -> bool {
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

        if let Some(stats) = &mut self.stats {
            stats.frames += 1;
            let elapsed = stats.since.elapsed().as_secs_f32();
            if elapsed >= 3.0 {
                tracing::info!("{}: {:.1} fps", self.name, stats.frames as f32 / elapsed);
                *stats = Stats {
                    since: Instant::now(),
                    frames: 0,
                };
            }
        }
        true
    }
}

struct App {
    args: Args,
    assets: PathBuf,
    conn: Connection,
    loop_handle: LoopHandle<'static, App>,
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    viewporter: Option<SimpleGlobal<WpViewporter, 1>>,
    fractional_manager: Option<SimpleGlobal<WpFractionalScaleManagerV1, 1>>,
    pointers: Vec<WlPointer>,
    screens: Vec<Screen>,
    /// Created with the first surface, shared by every output.
    gpu: Option<Gpu>,
    exit: bool,
    error: Option<anyhow::Error>,
}

impl App {
    fn screen_index(&self, surface: &WlSurface) -> Option<usize> {
        self.screens
            .iter()
            .position(|s| s.layer.wl_surface() == surface)
    }

    fn add_output(&mut self, qh: &QueueHandle<Self>, output: WlOutput) -> Result<()> {
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
        tracing::info!(
            "output {name}: {} {} {:?} scale {}",
            info.make,
            info.model,
            info.logical_size,
            info.scale_factor
        );

        let wl_surface = self.compositor.create_surface(qh);
        let layer = match self.args.layer {
            LayerArg::Background => Layer::Background,
            LayerArg::Bottom => Layer::Bottom,
        };
        let layer = self.layer_shell.create_layer_surface(
            qh,
            wl_surface,
            layer,
            Some("wallpaper"),
            Some(&output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        if self.args.passthrough {
            let region = Region::new(&self.compositor)?;
            layer
                .wl_surface()
                .set_input_region(Some(region.wl_region()));
        }
        let scaling = self
            .viewporter
            .as_ref()
            .and_then(|v| v.get().ok())
            .zip(self.fractional_manager.as_ref().and_then(|f| f.get().ok()));
        let (viewport, fractional) = match scaling {
            Some((viewporter, manager)) => (
                Some(viewporter.get_viewport(layer.wl_surface(), qh, ())),
                Some(manager.get_fractional_scale(layer.wl_surface(), qh, ())),
            ),
            None => (None, None),
        };

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
        // SAFETY: the wl_surface (owned by `layer`) outlives the wgpu surface:
        // `Screen` drops `surface` first.
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(raw_display_handle),
                raw_window_handle,
            })?
        };
        if self.gpu.is_none() {
            self.gpu = Some(pollster::block_on(Gpu::new(instance, Some(&surface)))?);
        }
        let gpu = self.gpu.as_ref().unwrap();

        let caps = surface.get_capabilities(&gpu.adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .context("surface is incompatible with the adapter")?;
        let (lw, lh) = info.logical_size.unwrap_or((1920, 1080));
        let mut config = surface
            .get_default_config(&gpu.adapter, lw.max(1) as u32, lh.max(1) as u32)
            .context("surface is incompatible with the adapter")?;
        config.format = format;
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

        let mut wallpaper = Wallpaper::load(gpu, format, &self.assets, &self.args.wallpaper)?;
        for (name, value) in &self.args.properties {
            wallpaper.set_user_property(gpu, name, value.clone());
        }
        wallpaper.fill = self.args.fill.into();
        // One sound source is enough when the wallpaper spans several outputs.
        let audio = self.args.audio && self.screens.is_empty();
        wallpaper.set_audio(audio);
        tracing::info!("{name}: playing \"{}\"", wallpaper.title);

        // The initial commit, without a buffer; the configure follows.
        layer.commit();
        self.screens.push(Screen {
            surface,
            config,
            wallpaper,
            fractional,
            viewport,
            layer,
            output,
            name,
            logical: (0, 0),
            scale120: info.scale_factor.max(1) as u32 * 120,
            buffer_scale: info.scale_factor.max(1),
            configured: false,
            frame_pending: false,
            timer_pending: false,
            next_frame: Instant::now(),
            stats: self.args.stats.then(|| Stats {
                since: Instant::now(),
                frames: 0,
            }),
        });
        Ok(())
    }

    /// Draw the screen now, or arm a timer for when `--fps` allows it.
    fn schedule(&mut self, qh: &QueueHandle<Self>, index: usize) {
        let Some(gpu) = &self.gpu else { return };
        let screen = &mut self.screens[index];
        if !screen.configured || screen.frame_pending || screen.timer_pending {
            return;
        }
        let now = Instant::now();
        if self.args.fps > 0 && screen.next_frame > now {
            screen.timer_pending = true;
            let surface = screen.layer.wl_surface().clone();
            let qh = qh.clone();
            let timer = Timer::from_deadline(screen.next_frame);
            let inserted = self.loop_handle.insert_source(timer, move |_, _, app| {
                if let Some(i) = app.screen_index(&surface) {
                    app.screens[i].timer_pending = false;
                    app.schedule(&qh, i);
                }
                TimeoutAction::Drop
            });
            if let Err(e) = inserted {
                tracing::error!("cannot arm frame timer: {e}");
                screen.timer_pending = false;
            }
            return;
        }
        if self.args.fps > 0 {
            let interval = Duration::from_secs_f64(1.0 / self.args.fps as f64);
            screen.next_frame = screen.next_frame.max(now - interval) + interval;
        }
        if !screen.draw(gpu, qh) {
            // Retry shortly; nothing will call back otherwise.
            screen.timer_pending = true;
            let surface = screen.layer.wl_surface().clone();
            let qh = qh.clone();
            let timer = Timer::from_duration(Duration::from_millis(100));
            let _ = self.loop_handle.insert_source(timer, move |_, _, app| {
                if let Some(i) = app.screen_index(&surface) {
                    app.screens[i].timer_pending = false;
                    app.schedule(&qh, i);
                }
                TimeoutAction::Drop
            });
        }
    }

    fn rescale(&mut self, qh: &QueueHandle<Self>, index: usize) {
        if let Some(gpu) = &self.gpu {
            let screen = &mut self.screens[index];
            if screen.configured {
                screen.reconfigure(gpu);
                self.schedule(qh, index);
            }
        }
    }
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
        .or_else(default_assets_dir)
        .context("Wallpaper Engine assets directory not found; pass --assets")?;

    let conn = Connection::connect_to_env().context("cannot connect to a Wayland compositor")?;
    let (globals, queue) = registry_queue_init::<App>(&conn)?;
    let qh = queue.handle();
    let mut event_loop: EventLoop<'static, App> = EventLoop::try_new()?;
    WaylandSource::new(conn.clone(), queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("cannot watch the Wayland socket: {}", e.error))?;

    // SIGINT/SIGTERM write to a socket the loop watches, so the surfaces
    // are torn down in order.
    let (signal_rx, signal_tx) = UnixStream::pair()?;
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::low_level::pipe::register(signal, signal_tx.try_clone()?)?;
    }
    event_loop
        .handle()
        .insert_source(
            Generic::new(signal_rx, Interest::READ, Mode::Level),
            |_, _, app| {
                tracing::info!("signal received, exiting");
                app.exit = true;
                Ok(PostAction::Remove)
            },
        )
        .map_err(|e| anyhow::anyhow!("cannot watch signals: {}", e.error))?;

    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor missing")?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .context("the compositor does not support wlr-layer-shell")?;
    let viewporter = SimpleGlobal::bind(&globals, &qh).ok();
    let fractional_manager = SimpleGlobal::bind(&globals, &qh).ok();
    let mut app = App {
        args,
        assets,
        conn: conn.clone(),
        loop_handle: event_loop.handle(),
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        compositor,
        layer_shell,
        viewporter,
        fractional_manager,
        pointers: Vec::new(),
        screens: Vec::new(),
        gpu: None,
        exit: false,
        error: None,
    };

    while !app.exit {
        event_loop.dispatch(None, &mut app)?;
    }
    app.screens.clear();
    let _ = conn.flush();
    app.error.map_or(Ok(()), Err)
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        surface: &WlSurface,
        factor: i32,
    ) {
        if let Some(i) = self.screen_index(surface)
            && self.screens[i].viewport.is_none()
        {
            self.screens[i].buffer_scale = factor;
            self.rescale(qh, i);
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

    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, surface: &WlSurface, _: u32) {
        if let Some(i) = self.screen_index(surface) {
            self.screens[i].frame_pending = false;
            self.schedule(qh, i);
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

    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: WlOutput) {
        if let Err(e) = self.add_output(qh, output) {
            self.error = Some(e);
            self.exit = true;
        }
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: WlOutput) {
        self.screens.retain(|s| {
            let keep = s.output != output;
            if !keep {
                tracing::info!("output {} removed", s.name);
            }
            keep
        });
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        self.screens
            .retain(|s| s.layer.wl_surface() != layer.wl_surface());
    }

    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
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
        self.schedule(qh, i);
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
        qh: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event
            && let Some(i) = app
                .screens
                .iter()
                .position(|s| s.fractional.as_ref() == Some(proxy))
        {
            app.screens[i].scale120 = scale;
            app.rescale(qh, i);
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
