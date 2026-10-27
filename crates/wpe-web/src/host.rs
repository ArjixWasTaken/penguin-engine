//! The thread Servo runs on. Servo isn't `Send`, so it lives here with
//! every page's WebView and rendering context, driven by [`Cmd`]s.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dpi::PhysicalSize;
use embedder_traits::user_contents::UserStyleSheet;
use euclid::Scale;
use http::{HeaderValue, StatusCode};
use servo::{
    ConsoleLogLevel, CreateNewWebViewRequest, DevicePoint, EventLoopWaker, InputEvent, LoadStatus,
    MouseButton, MouseButtonAction, MouseButtonEvent, MouseMoveEvent, NavigationRequest,
    PermissionRequest, RenderingContext, Servo, ServoBuilder, UserContentManager, UserScript,
    WebResourceLoad, WebResourceResponse, WebView, WebViewBuilder, WebViewDelegate, WebViewId,
    WheelDelta, WheelEvent, WheelMode,
};
use url::Url;

use crate::context::PageContext;
use crate::gpu::Ring;
use crate::local::{self, LocalFiles};
use crate::policy::{self, NetworkPolicy, Verdict};
use crate::{Button, Frame, Input, PageOptions, Shared, WebError};

/// Clicks select nothing: Servo selects a clicked `<canvas>` and paints its
/// whole box in the selection colour, which Chromium (and so Wallpaper
/// Engine) doesn't. A user style, so a page's own `user-select` still wins.
const USER_CSS: &str = "* { user-select: none; }";

pub enum Cmd {
    Open {
        id: u64,
        options: Box<PageOptions>,
        shared: Arc<Shared>,
        ready: Sender<Result<(), WebError>>,
    },
    Resize {
        id: u64,
        size: (u32, u32),
    },
    Scale {
        id: u64,
        scale: f32,
    },
    Input {
        id: u64,
        input: Input,
    },
    Js {
        id: u64,
        script: String,
    },
    /// Changed user properties, `{name: {type, value}}`.
    Properties {
        id: u64,
        changed: serde_json::Value,
    },
    Paused {
        id: u64,
        paused: bool,
    },
    Close {
        id: u64,
    },
    /// Whether `host` resolved to a local address, for the loads waiting on it.
    Resolved {
        host: String,
        local: bool,
    },
    /// Servo has work to do.
    Wake,
    Shutdown(Sender<()>),
}

struct Host {
    tx: Mutex<Sender<Cmd>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

static HOST: OnceLock<Host> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn host(profile_dir: Option<&std::path::Path>) -> &'static Host {
    HOST.get_or_init(|| {
        let (tx, rx) = channel();
        let waker = tx.clone();
        let profile_dir = profile_dir.map(ToOwned::to_owned);
        let thread = std::thread::Builder::new()
            .name("servo".to_owned())
            .spawn(move || run(rx, waker, profile_dir))
            .expect("spawn the servo thread");
        Host {
            tx: Mutex::new(tx),
            thread: Mutex::new(Some(thread)),
        }
    })
}

pub fn send(cmd: Cmd) {
    if let Some(host) = HOST.get() {
        let _ = host.tx.lock().unwrap().send(cmd);
    }
}

/// Open a page on the Servo thread (starting it on first use) and wait
/// for it to be ready, at most `timeout`.
pub fn open(options: PageOptions, shared: Arc<Shared>, timeout: Duration) -> Result<u64, WebError> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let (ready, ready_rx) = channel();
    let host = host(options.profile_dir.as_deref());
    host.tx
        .lock()
        .unwrap()
        .send(Cmd::Open {
            id,
            options: Box::new(options),
            shared,
            ready,
        })
        .map_err(|_| WebError::HostGone)?;
    match ready_rx.recv_timeout(timeout) {
        Ok(Ok(())) => Ok(id),
        Ok(Err(e)) => Err(e),
        Err(RecvTimeoutError::Timeout) => {
            tracing::warn!("page {id} still loading after {timeout:?}; showing it anyway");
            Ok(id)
        }
        Err(RecvTimeoutError::Disconnected) => Err(WebError::HostGone),
    }
}

/// Close every page and stop Servo, waiting at most `timeout`. Apps call
/// this before exiting, after dropping their pages.
pub fn shutdown(timeout: Duration) {
    let Some(host) = HOST.get() else {
        return;
    };
    let (done, done_rx) = channel();
    if host.tx.lock().unwrap().send(Cmd::Shutdown(done)).is_err() {
        return;
    }
    if done_rx.recv_timeout(timeout).is_ok() {
        if let Some(thread) = host.thread.lock().unwrap().take() {
            let _ = thread.join();
        }
    } else {
        tracing::warn!("servo didn't stop within {timeout:?}");
    }
}

#[derive(Clone)]
struct Waker(Sender<Cmd>);

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }
    fn wake(&self) {
        let _ = self.0.send(Cmd::Wake);
    }
}

struct Page {
    id: u64,
    webview: WebView,
    context: Rc<PageContext>,
    shared: Arc<Shared>,
    root: std::path::PathBuf,
    origin: Url,
    network: NetworkPolicy,
    ready: Option<Sender<Result<(), WebError>>>,
    loaded: bool,
    paused: bool,
    export_frames: bool,
    /// The exported textures frames are copied into.
    ring: Option<Ring>,
    /// Local files the page may read.
    local: LocalFiles,
}

impl Page {
    /// Hand the painted frame over: copied into an exported texture, or
    /// read back as pixels.
    fn deliver_frame(&mut self) {
        if self.export_frames
            && !self.shared.readback.load(Ordering::Acquire)
            && let Some(fbo) = self.context.gpu_framebuffer()
        {
            let gl = self.context.glow_gl_api();
            let size = self.context.size();
            let size = (size.width, size.height);
            if self.ring.as_ref().is_some_and(|r| r.size() != size)
                && let Some(mut old) = self.ring.take()
            {
                old.delete(&gl);
            }
            if self.ring.is_none() {
                match Ring::new(&gl, size) {
                    Ok((ring, frames)) => {
                        *self.shared.gpu_frames.lock().unwrap() = Some(frames);
                        self.ring = Some(ring);
                    }
                    Err(e) => {
                        tracing::warn!("page {}: can't share frames on the GPU: {e}", self.id);
                        self.shared.readback.store(true, Ordering::Release);
                    }
                }
            }
            if let Some(ring) = &self.ring {
                ring.copy(&gl, fbo);
                self.shared.fresh.store(true, Ordering::Release);
                return;
            }
        }
        if let Some(image) = self.context.read_frame() {
            let (width, height) = image.dimensions();
            *self.shared.frame.lock().unwrap() = Some(Frame {
                width,
                height,
                pixels: image.into_raw(),
            });
            self.shared.fresh.store(true, Ordering::Release);
        }
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        if let Some(mut ring) = self.ring.take() {
            let _ = self.context.make_current();
            ring.delete(&self.context.glow_gl_api());
        }
    }
}

struct Pages {
    by_webview: RefCell<HashMap<WebViewId, Page>>,
    /// Back to the Servo thread's queue.
    tx: Sender<Cmd>,
    /// Whether hosts resolve to local addresses, once known.
    resolved: RefCell<HashMap<String, bool>>,
    /// Loads waiting for their host to resolve.
    waiting: RefCell<HashMap<String, Vec<(WebResourceLoad, Url)>>>,
}

impl Pages {
    fn with<R>(&self, webview: &WebView, f: impl FnOnce(&mut Page) -> R) -> Option<R> {
        self.by_webview.borrow_mut().get_mut(&webview.id()).map(f)
    }

    fn by_id<R>(&self, id: u64, f: impl FnOnce(&mut Page) -> R) -> Option<R> {
        self.by_webview
            .borrow_mut()
            .values_mut()
            .find(|p| p.id == id)
            .map(f)
    }

    /// Page `id`'s WebView, to call into Servo with nothing borrowed.
    fn webview(&self, id: u64) -> Option<WebView> {
        self.by_id(id, |p| p.webview.clone())
    }
}

impl WebViewDelegate for Pages {
    fn notify_new_frame_ready(&self, webview: WebView) {
        // Servo may call back into the delegate while painting, so nothing
        // stays borrowed across it.
        let Some((context, paused)) = self.with(&webview, |p| (p.context.clone(), p.paused)) else {
            return;
        };
        if paused {
            return;
        }
        let _ = context.make_current();
        webview.paint();
        self.with(&webview, |page| {
            page.deliver_frame();
            if page.loaded && page.ready.is_some() {
                page.became_ready();
            }
        });
        context.present();
    }

    fn notify_load_status_changed(&self, webview: WebView, status: LoadStatus) {
        if status == LoadStatus::Complete {
            self.with(&webview, |page| {
                page.loaded = true;
                tracing::debug!("page {} loaded", page.id);
                // The first frame after load marks it ready.
                if page.shared.fresh.load(Ordering::Acquire) {
                    page.became_ready();
                }
            });
        }
    }

    fn load_web_resource(&self, webview: WebView, load: WebResourceLoad) {
        let Some((root, origin, network, shared)) = self.with(&webview, |p| {
            (
                p.root.clone(),
                p.origin.clone(),
                p.network.clone(),
                p.shared.clone(),
            )
        }) else {
            return;
        };
        let url = load.request().url.clone();
        if url.origin() == origin.origin() {
            match url.path().strip_prefix("/__wpe/rpc/") {
                Some(method) => {
                    let method = method.to_owned();
                    rpc(load, &shared, &method, url)
                }
                None => serve(load, root, url),
            }
            return;
        }
        tracing::trace!("request for {url}");
        if url.scheme() == "file" {
            match self.with(&webview, |p| p.local.allowed(&url)).flatten() {
                Some(path) => serve_file(load, &path, url),
                None => block(load, &url),
            }
            return;
        }
        match network.check(&url) {
            Verdict::Allow => {}
            Verdict::Block => block(load, &url),
            Verdict::Resolve(host, port) => {
                if let Some(&local) = self.resolved.borrow().get(&host) {
                    if local {
                        block(load, &url);
                    }
                    return;
                }
                let mut waiting = self.waiting.borrow_mut();
                let queue = waiting.entry(host.clone()).or_default();
                queue.push((load, url));
                if queue.len() == 1 {
                    // Resolving may take a while; keep Servo's thread free.
                    let tx = self.tx.clone();
                    std::thread::spawn(move || {
                        let local = policy::resolves_local(&host, port);
                        let _ = tx.send(Cmd::Resolved { host, local });
                    });
                }
            }
        }
    }

    fn request_navigation(&self, webview: WebView, request: NavigationRequest) {
        let same_origin = self
            .with(&webview, |p| request.url.origin() == p.origin.origin())
            .unwrap_or(false);
        if same_origin || request.url.scheme() == "about" || request.url.scheme() == "data" {
            request.allow();
        } else {
            tracing::info!("blocked navigation to {}", request.url);
            request.deny();
        }
    }

    fn request_create_new(&self, _parent: WebView, _request: CreateNewWebViewRequest) {
        // No popups: ignoring the request opens nothing.
    }

    fn request_permission(&self, _webview: WebView, request: PermissionRequest) {
        request.deny();
    }

    fn show_console_message(&self, webview: WebView, level: ConsoleLogLevel, message: String) {
        let id = self.with(&webview, |p| p.id).unwrap_or(0);
        match level {
            ConsoleLogLevel::Error => {
                tracing::warn!(target: "wpe_web::console", "[{id}] {message}")
            }
            ConsoleLogLevel::Warn => tracing::info!(target: "wpe_web::console", "[{id}] {message}"),
            _ => tracing::debug!(target: "wpe_web::console", "[{id}] {message}"),
        }
    }

    fn notify_crashed(&self, webview: WebView, reason: String, _backtrace: Option<String>) {
        tracing::warn!("page crashed: {reason}");
        webview.reload();
    }
}

impl Page {
    fn became_ready(&mut self) {
        self.shared.ready.store(true, Ordering::Release);
        if let Some(ready) = self.ready.take() {
            let _ = ready.send(Ok(()));
        }
    }
}

fn block(load: WebResourceLoad, url: &Url) {
    tracing::info!("blocked a request to {url}");
    load.intercept(WebResourceResponse::new(url.clone()))
        .cancel();
}

/// Calls from the shim: `fetch("/__wpe/rpc/<method>")`.
fn rpc(load: WebResourceLoad, shared: &Shared, method: &str, url: Url) {
    match method {
        "audio-registered" => shared.audio_registered.store(true, Ordering::Release),
        other => tracing::debug!("unknown page call {other}"),
    }
    let mut head = WebResourceResponse::new(url);
    head.status_code = StatusCode::NO_CONTENT;
    head.status_message = b"No Content".to_vec();
    load.intercept(head).finish();
}

fn range(load: &WebResourceLoad) -> Option<String> {
    load.request()
        .headers
        .get(http::header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned)
}

/// Answer a request on the page's own origin from its directory.
fn serve(load: WebResourceLoad, root: std::path::PathBuf, url: Url) {
    let response = crate::serve::get(&root, url.path(), range(&load).as_deref());
    if response.status == 404 {
        tracing::debug!("not found: {}", url.path());
    }
    respond(load, url, response);
}

fn origin(load: &WebResourceLoad) -> Option<String> {
    load.request()
        .headers
        .get(http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned)
}

fn is_local_origin(origin: &str) -> bool {
    let Ok(url) = Url::parse(origin) else {
        return false;
    };
    if url.scheme() != "http" {
        return false;
    }
    matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
}

/// Answer a `file:` request for a file the page may read.
fn serve_file(load: WebResourceLoad, path: &std::path::Path, url: Url) {
    let mut response = crate::serve::respond(path, range(&load).as_deref());
    // Only mirror the Origin header for verified local origins
    if let Some(req_origin) = origin(&load)
        && is_local_origin(&req_origin)
    {
        response
            .headers
            .push(("Access-Control-Allow-Origin", req_origin));
    }
    respond(load, url, response);
}

fn respond(load: WebResourceLoad, url: Url, response: crate::serve::Response) {
    let mut head = WebResourceResponse::new(url);
    head.status_code = StatusCode::from_u16(response.status).unwrap_or(StatusCode::OK);
    head.status_message = head
        .status_code
        .canonical_reason()
        .unwrap_or("")
        .as_bytes()
        .to_vec();
    for (name, value) in response.headers {
        if let Ok(value) = HeaderValue::from_str(&value) {
            head.headers.insert(name, value);
        }
    }
    let mut intercepted = load.intercept(head);
    if !response.body.is_empty() {
        intercepted.send_body_data(response.body);
    }
    intercepted.finish();
}

fn run(rx: Receiver<Cmd>, waker: Sender<Cmd>, profile_dir: Option<std::path::PathBuf>) {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let mut opts = servo::Opts::default();
    if let Some(dir) = &profile_dir {
        let _ = std::fs::create_dir_all(dir);
        opts.config_dir = Some(dir.clone());
    }
    let prefs = servo::Preferences {
        dom_webgl2_enabled: false,
        network_http_proxy_uri: String::new(),
        devtools_server_enabled: false,
        ..Default::default()
    };
    let servo = ServoBuilder::default()
        .opts(opts)
        .preferences(prefs)
        .event_loop_waker(Box::new(Waker(waker.clone())))
        .build();
    let pages = Rc::new(Pages {
        by_webview: Default::default(),
        tx: waker,
        resolved: Default::default(),
        waiting: Default::default(),
    });

    const FREE_RUNNING: Duration = Duration::from_micros(16_667);
    let mut last_tick = Instant::now();
    let mut shutdown = None;
    loop {
        // Pages the renderer doesn't tick (yet) run at 60 Hz.
        if last_tick.elapsed() >= FREE_RUNNING {
            last_tick = Instant::now();
            for page in pages.by_webview.borrow().values() {
                if !page.shared.driven.load(Ordering::Relaxed) {
                    page.shared.pacer.tick();
                }
            }
        }
        let cmd = match rx.recv_timeout(FREE_RUNNING) {
            Ok(cmd) => Some(cmd),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        match cmd {
            Some(Cmd::Open {
                id,
                options,
                shared,
                ready,
            }) => open_page(&servo, &pages, id, *options, shared, ready),
            Some(Cmd::Resize { id, size }) => {
                if let Some(webview) = pages.webview(id) {
                    webview.resize(PhysicalSize::new(size.0, size.1));
                }
            }
            Some(Cmd::Scale { id, scale }) => {
                if let Some(webview) = pages.webview(id) {
                    webview.set_hidpi_scale_factor(Scale::new(scale));
                }
            }
            Some(Cmd::Input { id, input }) => {
                if let Some(webview) = pages.webview(id) {
                    webview.notify_input_event(input_event(input));
                }
            }
            Some(Cmd::Properties { id, changed }) => {
                pages.by_id(id, |p| p.local.update(&changed));
                if let Some(webview) = pages.webview(id) {
                    let changed = local::for_page(&changed);
                    webview.evaluate_javascript(
                        format!("window.__wpe && __wpe.apply({changed})"),
                        |_| {},
                    );
                }
            }
            Some(Cmd::Js { id, script }) => {
                if let Some(webview) = pages.webview(id) {
                    webview.evaluate_javascript(script, move |result| {
                        if let Err(e) = result {
                            tracing::debug!("page {id}: script failed: {e:?}");
                        }
                    });
                }
            }
            Some(Cmd::Paused { id, paused }) => {
                if let Some(webview) = pages.webview(id) {
                    pages.by_id(id, |p| p.paused = paused);
                    webview.evaluate_javascript(
                        format!("window.__wpe && __wpe.setPaused({paused})"),
                        |_| {},
                    );
                    if paused {
                        webview.hide();
                    } else {
                        webview.show();
                    }
                }
            }
            Some(Cmd::Close { id }) => {
                let closed: Vec<Page> = {
                    let mut by_webview = pages.by_webview.borrow_mut();
                    let ids: Vec<WebViewId> = by_webview
                        .iter()
                        .filter(|(_, p)| p.id == id)
                        .map(|(k, _)| *k)
                        .collect();
                    ids.iter().filter_map(|k| by_webview.remove(k)).collect()
                };
                drop(closed);
            }
            Some(Cmd::Resolved { host, local }) => {
                pages.resolved.borrow_mut().insert(host.clone(), local);
                let loads = pages.waiting.borrow_mut().remove(&host).unwrap_or_default();
                for (load, url) in loads {
                    // A dropped load goes ahead.
                    if local {
                        block(load, &url);
                    }
                }
            }
            Some(Cmd::Wake) | None => {}
            Some(Cmd::Shutdown(done)) => {
                let closed: Vec<Page> = pages
                    .by_webview
                    .borrow_mut()
                    .drain()
                    .map(|(_, p)| p)
                    .collect();
                drop(closed);
                shutdown = Some(done);
            }
        }
        servo.spin_event_loop();
        if shutdown.is_some() {
            break;
        }
    }
    // Let Servo finish closing the pages before it goes.
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        servo.spin_event_loop();
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(servo);
    if let Some(done) = shutdown {
        let _ = done.send(());
    }
}

fn open_page(
    servo: &Servo,
    pages: &Rc<Pages>,
    id: u64,
    options: PageOptions,
    shared: Arc<Shared>,
    ready: Sender<Result<(), WebError>>,
) {
    let size = PhysicalSize::new(options.size.0.max(1), options.size.1.max(1));
    let context = match PageContext::new(size, shared.pacer.clone()) {
        Ok(c) => Rc::new(c),
        Err(e) => {
            let _ = ready.send(Err(WebError::Context(format!("{e:?}"))));
            return;
        }
    };
    let _ = context.make_current();
    if crate::serve::find(&options.root, &options.file).is_none() {
        let _ = ready.send(Err(WebError::BadFile(options.file.clone())));
        return;
    }
    let origin = Url::parse(&format!(
        "http://{}.wallpaper.localhost/",
        host_label(&options.name)
    ))
    .expect("a valid origin");
    let url = origin
        .join(&options.file)
        .unwrap_or_else(|_| origin.clone());
    let content = Rc::new(UserContentManager::new(servo));
    content.add_script(Rc::new(UserScript::new(
        crate::shim::script(&local::for_page(&options.properties), &options.general),
        None,
    )));
    content.add_stylesheet(Rc::new(UserStyleSheet::new(
        USER_CSS.to_owned(),
        origin.join("__wpe/user.css").expect("a valid url"),
    )));
    let mut local = LocalFiles::new(&options.root);
    local.update(&options.properties);
    let webview = WebViewBuilder::new(servo, context.clone())
        .url(url.clone())
        .hidpi_scale_factor(Scale::new(options.scale.max(0.1)))
        .user_content_manager(content)
        .delegate(pages.clone())
        .build();
    tracing::debug!("page {id}: opening {url} at {}x{}", size.width, size.height);
    pages.by_webview.borrow_mut().insert(
        webview.id(),
        Page {
            id,
            webview,
            context,
            shared,
            root: options.root,
            origin,
            network: options.network,
            ready: Some(ready),
            loaded: false,
            paused: false,
            export_frames: options.export_frames,
            ring: None,
            local,
        },
    );
}

/// `name` made into one DNS label.
fn host_label(name: &str) -> String {
    let label: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(63)
        .collect();
    let label = label.trim_matches('-');
    if label.is_empty() {
        "page".to_owned()
    } else {
        label.to_owned()
    }
}

fn input_event(input: Input) -> InputEvent {
    let point = |x: f32, y: f32| DevicePoint::new(x, y).into();
    match input {
        Input::Move(x, y) => InputEvent::MouseMove(MouseMoveEvent::new(point(x, y))),
        Input::Button(button, down, x, y) => InputEvent::MouseButton(MouseButtonEvent::new(
            if down {
                MouseButtonAction::Down
            } else {
                MouseButtonAction::Up
            },
            match button {
                Button::Left => MouseButton::Primary,
                Button::Middle => MouseButton::Auxiliary,
                Button::Right => MouseButton::Secondary,
            },
            point(x, y),
        )),
        Input::Wheel { dx, dy, x, y } => InputEvent::Wheel(WheelEvent::new(
            // Servo takes winit's sign (positive scrolls up); ours is the
            // DOM's and Wayland's (positive scrolls down).
            WheelDelta {
                x: -dx as f64,
                y: -dy as f64,
                z: 0.0,
                mode: WheelMode::DeltaPixel,
            },
            point(x, y),
        )),
    }
}
