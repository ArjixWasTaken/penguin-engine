//! Web wallpapers rendered with Servo.
//!
//! One Servo runs per process, on its own thread ([`host`]); a [`WebPage`]
//! is a handle to one page on it, which any thread may hold. The page's
//! files are served from its directory under a made-up origin
//! ([`serve`]), Wallpaper Engine's JavaScript API is provided by a user
//! script ([`shim`]), and painted frames come back as RGBA pixels.

mod context;
mod gpu;
mod host;
mod policy;
mod serve;
mod shim;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use gpu::{Dmabuf, GpuFrames, Slots};
pub use host::shutdown;
pub use policy::NetworkPolicy;

#[derive(Debug, thiserror::Error)]
pub enum WebError {
    #[error("no rendering context for the page: {0}")]
    Context(String),
    #[error("{0} is not in the wallpaper directory")]
    BadFile(String),
    #[error("the web engine stopped")]
    HostGone,
}

/// What to open and how.
#[derive(Clone, Debug)]
pub struct PageOptions {
    /// The wallpaper directory, served as the page's origin.
    pub root: PathBuf,
    /// The page under `root` (`project.json`'s `file`, usually `index.html`).
    pub file: String,
    /// Names the origin (`http://<name>.wallpaper.localhost/`), so each
    /// wallpaper keeps its own local storage: the workshop id, or a hash.
    pub name: String,
    /// Viewport in device pixels.
    pub size: (u32, u32),
    /// Device pixels per CSS pixel.
    pub scale: f32,
    /// User properties in Wallpaper Engine's form, `{name: {type, value}}`.
    pub properties: serde_json::Value,
    /// General properties (`{fps: 30}`), for `applyGeneralProperties`.
    pub general: serde_json::Value,
    /// Which hosts the page may reach.
    pub network: NetworkPolicy,
    /// Where Servo keeps local storage and caches.
    pub profile_dir: Option<PathBuf>,
    /// Hand frames over as dma-bufs ([`WebPage::take_gpu_frames`]) rather
    /// than as pixels, when the page paints on the GPU.
    pub export_frames: bool,
}

/// A painted frame: `width`×`height` RGBA8, rows top-down.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Mouse buttons a page sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, Debug)]
pub enum Input {
    /// The pointer is at this device pixel.
    Move(f32, f32),
    Button(Button, bool, f32, f32),
    /// Scrolled by this many pixels at a point; positive is down/right.
    Wheel {
        dx: f32,
        dy: f32,
        x: f32,
        y: f32,
    },
}

/// What the page thread shares with the handle.
#[derive(Default)]
struct Shared {
    frame: Mutex<Option<Frame>>,
    /// Set when a frame is stored, cleared when it is taken.
    fresh: AtomicBool,
    /// The page finished loading and painted once.
    ready: AtomicBool,
    /// Newly exported textures, until the renderer takes them.
    gpu_frames: Mutex<Option<GpuFrames>>,
    /// The renderer couldn't use exported textures: send pixels.
    readback: AtomicBool,
    pacer: Arc<context::Pacer>,
    /// The renderer ticks the page; until then it ticks itself.
    driven: AtomicBool,
}

/// One open page.
pub struct WebPage {
    id: u64,
    shared: Arc<Shared>,
    size: (u32, u32),
    scale: f32,
}

impl WebPage {
    /// Open a page and wait (up to `timeout`) for it to load and paint.
    /// A page that is still loading by then is returned anyway.
    pub fn open(options: PageOptions, timeout: Duration) -> Result<Self, WebError> {
        let (size, scale) = (options.size, options.scale);
        let shared = Arc::new(Shared::default());
        let id = host::open(options, shared.clone(), timeout)?;
        Ok(Self {
            id,
            shared,
            size,
            scale,
        })
    }

    /// The viewport in device pixels.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The newest frame, if one was painted since the last call.
    pub fn take_frame(&self) -> Option<Frame> {
        if !self.shared.fresh.swap(false, Ordering::AcqRel) {
            return None;
        }
        self.shared.frame.lock().unwrap().take()
    }

    /// The textures frames now arrive in, after the page started exporting
    /// them or was resized. Their slots say which one is newest.
    pub fn take_gpu_frames(&self) -> Option<GpuFrames> {
        self.shared.gpu_frames.lock().unwrap().take()
    }

    /// Switch to pixel frames ([`Self::take_frame`]), for when exported
    /// textures can't be imported.
    pub fn use_readback(&self) {
        self.shared.readback.store(true, Ordering::Release);
    }

    /// Start the page's next frame: run its animations and
    /// `requestAnimationFrame` callbacks. Call once per frame drawn; a page
    /// never ticked runs on its own at 60 Hz.
    pub fn tick(&self) {
        self.shared.driven.store(true, Ordering::Relaxed);
        self.shared.pacer.tick();
    }

    /// Whether the page has loaded and painted.
    pub fn ready(&self) -> bool {
        self.shared.ready.load(Ordering::Acquire)
    }

    /// Change the viewport (device pixels).
    pub fn resize(&mut self, size: (u32, u32)) {
        if size != self.size && size.0 > 0 && size.1 > 0 {
            self.size = size;
            host::send(host::Cmd::Resize { id: self.id, size });
        }
    }

    /// Device pixels per CSS pixel (the output's scale).
    pub fn set_scale(&mut self, scale: f32) {
        if scale > 0.0 && scale != self.scale {
            self.scale = scale;
            host::send(host::Cmd::Scale { id: self.id, scale });
        }
    }

    pub fn input(&self, input: Input) {
        host::send(host::Cmd::Input { id: self.id, input });
    }

    /// Changed user properties, `{name: {type, value}}`, for the page's
    /// `applyUserProperties`.
    pub fn apply_properties(&self, changed: &serde_json::Value) {
        self.eval(format!("window.__wpe && __wpe.apply({changed})"));
    }

    /// New general properties (`{fps: 30}`) for `applyGeneralProperties`.
    pub fn apply_general(&self, general: &serde_json::Value) {
        self.eval(format!("window.__wpe && __wpe.general({general})"));
    }

    /// Pause or resume: the page hears `setPaused`, its media pause, and
    /// it stops painting.
    pub fn set_paused(&self, paused: bool) {
        host::send(host::Cmd::Paused {
            id: self.id,
            paused,
        });
    }

    /// Run `script` in the page.
    pub fn eval(&self, script: String) {
        host::send(host::Cmd::Js {
            id: self.id,
            script,
        });
    }
}

impl Drop for WebPage {
    fn drop(&mut self) {
        host::send(host::Cmd::Close { id: self.id });
    }
}
