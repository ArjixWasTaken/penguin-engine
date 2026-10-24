//! Loading wallpapers without stopping the render loop: the heavy part
//! ([`Wallpaper::prepare`]: files, scene, textures, shaders, pipelines) runs
//! on a worker thread while the old wallpaper keeps playing; the result
//! comes back through a channel the event loop watches, and the short rest
//! ([`PreparedWallpaper::finish`]: scripts) runs here before the swap.
//!
//! Each output has at most one load in flight. A newer one for the same
//! output supersedes it: its result is dropped when it arrives, and the
//! `set` request waiting on it is answered with an error.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use smithay_client_toolkit::reexports::calloop::channel::Sender;
use smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput;
use smithay_client_toolkit::reexports::protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::WpFractionalScaleV1;
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::WpViewport;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::LayerSurface;
use wpe_ipc::{Fill, Response};
use wpe_render::{Gpu, wgpu};
use wpe_runtime::{PreparedWallpaper, Wallpaper};

use crate::control::Responder;
use crate::{App, Screen, fill_mode};

/// A prepared wallpaper (or why it failed) coming back from a worker.
pub struct Done {
    id: u64,
    result: Result<PreparedWallpaper, String>,
}

/// A layer surface made for an output without a screen, waiting for its
/// wallpaper. It is committed (and mapped) only once that arrives.
pub struct NewSurface(Option<SurfaceParts>);

pub struct SurfaceParts {
    // Drop order: the wgpu surface goes before the Wayland surface under it.
    pub surface: wgpu::Surface<'static>,
    pub config: wgpu::SurfaceConfiguration,
    pub fractional: Option<WpFractionalScaleV1>,
    pub viewport: Option<WpViewport>,
    pub layer: LayerSurface,
    pub scale120: u32,
    pub buffer_scale: i32,
}

impl NewSurface {
    pub fn new(parts: SurfaceParts) -> Self {
        Self(Some(parts))
    }

    fn format(&self) -> wgpu::TextureFormat {
        self.0.as_ref().expect("taken only when used").config.format
    }

    fn into_parts(mut self) -> SurfaceParts {
        self.0.take().expect("taken only when used")
    }
}

impl Drop for NewSurface {
    fn drop(&mut self) {
        if let Some(parts) = self.0.take() {
            if let Some(v) = parts.viewport {
                v.destroy();
            }
            if let Some(f) = parts.fractional {
                f.destroy();
            }
        }
    }
}

/// A `set` request waiting for its outputs' loads. It changes all of them
/// or, when one fails, none.
pub struct SetRequest {
    responder: Option<Responder>,
}

impl SetRequest {
    pub fn new(responder: Responder) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            responder: Some(responder),
        }))
    }

    fn answer(&mut self, response: Response) {
        if let Some(responder) = self.responder.take() {
            responder.send(response);
        }
    }
}

/// What to load for an output, and what to do with it.
pub struct LoadSpec {
    pub output: WlOutput,
    pub name: String,
    pub path: PathBuf,
    pub properties: Vec<(String, serde_json::Value)>,
    /// The fill of a new screen.
    pub fill: Fill,
    /// The `set` request it is for (then it's saved in the state), if any.
    pub request: Option<Rc<RefCell<SetRequest>>>,
}

/// A load in flight for one output.
struct PendingLoad {
    id: u64,
    spec: LoadSpec,
    /// The surface to show it on, when the output has no screen yet.
    surface: Option<NewSurface>,
    started: Instant,
    /// The worker's result, kept until the other loads of the request are in.
    result: Option<Result<PreparedWallpaper, String>>,
}

/// The loads in flight.
pub struct Loads {
    next_id: u64,
    pending: Vec<PendingLoad>,
    sender: Sender<Done>,
}

impl Loads {
    pub fn new(sender: Sender<Done>) -> Self {
        Self {
            next_id: 0,
            pending: Vec::new(),
            sender,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Whether `output` waits for a wallpaper on a surface of its own.
    pub fn has_surface_for(&self, output: &WlOutput) -> bool {
        self.pending
            .iter()
            .any(|l| l.spec.output == *output && l.surface.is_some())
    }
}

fn same_request(a: &Option<Rc<RefCell<SetRequest>>>, b: &Rc<RefCell<SetRequest>>) -> bool {
    a.as_ref().is_some_and(|a| Rc::ptr_eq(a, b))
}

/// Prepare on a worker thread and send the result back.
fn spawn_prepare(
    gpu: Arc<Gpu>,
    assets: PathBuf,
    sender: Sender<Done>,
    id: u64,
    format: wgpu::TextureFormat,
    path: PathBuf,
    properties: Vec<(String, serde_json::Value)>,
) {
    let fallback = sender.clone();
    let spawned = std::thread::Builder::new()
        .name("wallpaper-load".to_owned())
        .spawn(move || {
            let result = Wallpaper::prepare(&gpu, format, &assets, &path, &properties)
                .map_err(|e| format!("loading {}: {e}", path.display()));
            // The loop may be gone (exiting).
            let _ = sender.send(Done { id, result });
        });
    if let Err(e) = spawned {
        let _ = fallback.send(Done {
            id,
            result: Err(format!("cannot start loading: {e}")),
        });
    }
}

impl App {
    /// Start loading `spec` for its output, superseding a load in flight
    /// there. `surface` is the new surface to show it on when the output
    /// has no screen (one waiting for a superseded load is reused).
    pub fn start_load(&mut self, spec: LoadSpec, surface: Option<NewSurface>) {
        let Some(gpu) = self.gpu.clone() else {
            if let Some(request) = &spec.request {
                request.borrow_mut().answer(Response::error("no GPU yet"));
            }
            return;
        };
        let mut surface = surface;
        if let Some(i) = self
            .loads
            .pending
            .iter()
            .position(|l| l.spec.output == spec.output)
        {
            let old = &mut self.loads.pending[i];
            tracing::info!(
                "{}: loading {} instead of {}",
                spec.name,
                spec.path.display(),
                old.spec.path.display()
            );
            if surface.is_none() {
                surface = old.surface.take();
            }
            match old.spec.request.clone() {
                Some(request) => self.cancel_request(&request, "superseded by a later set request"),
                None => {
                    self.loads.pending.remove(i);
                }
            }
        }
        let format = match (&surface, self.screen_for(&spec.output)) {
            (Some(s), _) => s.format(),
            (None, Some(i)) => self.screens[i].config.format,
            (None, None) => {
                if let Some(request) = &spec.request {
                    request
                        .borrow_mut()
                        .answer(Response::error(format!("{}: no surface", spec.name)));
                }
                return;
            }
        };
        let id = self.loads.next_id;
        self.loads.next_id += 1;
        spawn_prepare(
            gpu,
            self.assets.clone(),
            self.loads.sender.clone(),
            id,
            format,
            spec.path.clone(),
            spec.properties.clone(),
        );
        self.loads.pending.push(PendingLoad {
            id,
            spec,
            surface,
            started: Instant::now(),
            result: None,
        });
    }

    /// Drop every load of `request` and answer it with `error`.
    fn cancel_request(&mut self, request: &Rc<RefCell<SetRequest>>, error: &str) {
        self.loads
            .pending
            .retain(|l| !same_request(&l.spec.request, request));
        request.borrow_mut().answer(Response::error(error));
    }

    /// Forget the loads for `output` (it went away).
    pub fn cancel_loads(&mut self, output: &WlOutput) {
        let requests: Vec<_> = self
            .loads
            .pending
            .iter()
            .filter(|l| l.spec.output == *output)
            .filter_map(|l| l.spec.request.clone())
            .collect();
        for request in requests {
            self.cancel_request(&request, "the output went away");
        }
        self.loads.pending.retain(|l| l.spec.output != *output);
    }

    /// A worker finished.
    pub fn load_done(&mut self, done: Done) {
        let Some(i) = self.loads.pending.iter().position(|l| l.id == done.id) else {
            tracing::debug!("dropping a superseded load");
            return;
        };
        let load = &mut self.loads.pending[i];
        if let Err(e) = &done.result {
            tracing::warn!("{}: {e}", load.spec.name);
        }
        load.result = Some(done.result);
        match load.spec.request.clone() {
            None => {
                let load = self.loads.pending.remove(i);
                let name = load.spec.name.clone();
                match load.result {
                    Some(Ok(_)) => self.install(load),
                    Some(Err(e)) => {
                        // A wallpaper named on the command line that doesn't
                        // load is fatal at startup; later, the output stays
                        // as it is.
                        if self.args.wallpaper.is_some() && !self.ready {
                            self.error = Some(anyhow::anyhow!("{name}: {e}"));
                            self.exit = true;
                        }
                    }
                    None => unreachable!("just set"),
                }
            }
            Some(request) => {
                let mine = |l: &PendingLoad| same_request(&l.spec.request, &request);
                if self
                    .loads
                    .pending
                    .iter()
                    .any(|l| mine(l) && l.result.is_none())
                {
                    return;
                }
                let (loads, rest): (Vec<_>, Vec<_>) =
                    self.loads.pending.drain(..).partition(|l| mine(l));
                self.loads.pending = rest;
                let errors: Vec<String> = loads
                    .iter()
                    .filter_map(|l| match &l.result {
                        Some(Err(e)) => Some(format!("{}: {e}", l.spec.name)),
                        _ => None,
                    })
                    .collect();
                if errors.is_empty() {
                    for load in loads {
                        self.install(load);
                    }
                    self.save_state();
                    request.borrow_mut().answer(Response::ok());
                } else {
                    request
                        .borrow_mut()
                        .answer(Response::error(errors.join("; ")));
                }
            }
        }
        if !self.ready && self.screens.is_empty() && self.loads.is_empty() && !self.exit {
            self.ready = true;
            self.update_status();
            crate::systemd::notify(&[crate::systemd::NotifyState::Ready]);
            tracing::info!("ready, with no wallpaper showing");
        }
    }

    /// Finish a prepared wallpaper and show it.
    fn install(&mut self, mut load: PendingLoad) {
        let Some(Ok(prepared)) = load.result.take() else {
            return;
        };
        let Some(gpu) = self.gpu.clone() else { return };
        let prepare = prepared.elapsed;
        let finished = Instant::now();
        let mut wallpaper = prepared.finish(&gpu);
        wallpaper.set_volume(self.settings.volume as f32 / 100.0);
        let spec = load.spec;
        tracing::info!(
            "loaded \"{}\" ({}) in {:.1?}: prepared in {prepare:.1?} on a worker, finished in {:.1?}",
            wallpaper.title,
            spec.path.display(),
            load.started.elapsed(),
            finished.elapsed()
        );
        tracing::info!("{}: playing \"{}\"", spec.name, wallpaper.title);
        match (self.screen_for(&spec.output), load.surface.take()) {
            (Some(i), _) => self.screens[i].replace_wallpaper(wallpaper, spec.path.clone()),
            (None, Some(surface)) => {
                wallpaper.fill = fill_mode(spec.fill);
                let screen = Screen::new(
                    surface.into_parts(),
                    wallpaper,
                    spec.path.clone(),
                    spec.output.clone(),
                    spec.name.clone(),
                    spec.fill,
                );
                screen.set_passthrough(&self.compositor, self.settings.passthrough);
                // The initial commit, without a buffer; the configure follows.
                screen.layer.commit();
                self.screens.push(screen);
            }
            (None, None) => {
                tracing::warn!("{}: no surface for the wallpaper", spec.name);
                return;
            }
        }
        if spec.request.is_some() {
            self.state.set_wallpaper(&spec.name, &spec.path);
        }
        self.apply_audio();
        self.update_status();
        self.schedule_all();
    }
}
