//! Video textures: MP4 payloads inside `.tex` files, played by libmpv.
//!
//! mpv reads the video straight from memory through a custom stream
//! protocol, loops it, and paces frames at the video's own rate. Frames
//! reach the layer texture one of two ways:
//!
//! - [`gl`]: mpv renders with OpenGL (hardware decoding, no CPU copies) into
//!   textures shared with wgpu as dma-bufs; each frame is a GPU copy.
//! - [`cpu`]: mpv's software renderer writes RGBA into memory, which is
//!   uploaded. The fallback when the GPU path is unavailable.
//!
//! `WPE_VIDEO=cpu` forces the fallback; `WPE_MPV_OPTS=key=value,...` passes
//! extra options to mpv.

mod cpu;
mod gl;

use std::ffi::{c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use libmpv2::Mpv;
use libmpv2::protocol::Protocol;
use libmpv2_sys as sys;
use wpe_render::{Gpu, GpuTexture};

#[derive(Debug, thiserror::Error)]
pub enum VideoError {
    #[error("starting mpv: {0}")]
    Mpv(#[from] libmpv2::Error),
    #[error("creating mpv's {0} renderer: error {1}")]
    Render(&'static str, i32),
    #[error("setting up OpenGL: {0}")]
    Gl(String),
    #[error("sharing video frames with the renderer: {0}")]
    Share(String),
}

/// Woken by mpv when a frame is due, and by [`VideoStream`] to stop.
#[derive(Default)]
struct Wake {
    pending: Mutex<bool>,
    cond: Condvar,
}

impl Wake {
    fn notify(&self) {
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.cond.notify_one();
    }

    fn wait(&self, timeout: Duration) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if !*pending {
            pending = self
                .cond
                .wait_timeout(pending, timeout)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        *pending = false;
    }
}

unsafe extern "C" fn on_update(ctx: *mut c_void) {
    // Runs on an mpv thread; must not call back into mpv.
    let wake = unsafe { &*(ctx as *const Wake) };
    wake.notify();
}

/// An mpv render context; freed before the core it renders for.
struct RenderContext {
    ctx: *mut sys::mpv_render_context,
}

/// Build an `mpv_render_param` list, terminated as mpv expects.
fn params<const N: usize>(
    list: [(sys::mpv_render_param_type, *mut c_void); N],
) -> Vec<sys::mpv_render_param> {
    list.into_iter()
        .chain([(
            sys::mpv_render_param_type_MPV_RENDER_PARAM_INVALID,
            ptr::null_mut(),
        )])
        .map(|(type_, data)| sys::mpv_render_param { type_, data })
        .collect()
}

impl RenderContext {
    /// Create a render context of `api` with the extra creation `params`.
    fn new(
        mpv: &Mpv,
        wake: &Arc<Wake>,
        api: &'static str,
        mut params: Vec<sys::mpv_render_param>,
    ) -> Result<Self, VideoError> {
        let mut ctx = ptr::null_mut();
        let err = unsafe {
            sys::mpv_render_context_create(&mut ctx, mpv.ctx.as_ptr(), params.as_mut_ptr())
        };
        if err < 0 {
            return Err(VideoError::Render(api, err));
        }
        unsafe {
            sys::mpv_render_context_set_update_callback(
                ctx,
                Some(on_update),
                Arc::as_ptr(wake) as *mut c_void,
            );
        }
        Ok(Self { ctx })
    }

    /// Whether a new frame is due.
    fn frame_due(&self) -> bool {
        let flags = unsafe { sys::mpv_render_context_update(self.ctx) };
        flags & sys::mpv_render_update_flag_MPV_RENDER_UPDATE_FRAME as u64 != 0
    }

    fn render(&self, mut params: Vec<sys::mpv_render_param>) -> bool {
        unsafe { sys::mpv_render_context_render(self.ctx, params.as_mut_ptr()) >= 0 }
    }
}

impl Drop for RenderContext {
    fn drop(&mut self) {
        unsafe { sys::mpv_render_context_free(self.ctx) };
    }
}

/// How the playback thread produces frames.
trait Output {
    /// Render the frame mpv has due. Returns whether one was produced.
    fn render(&mut self) -> bool;
}

/// A read cursor over the shared video bytes, for mpv's stream callbacks.
struct Cursor {
    data: Arc<[u8]>,
    pos: usize,
}

fn open(data: &mut Arc<[u8]>, _uri: &str) -> Cursor {
    Cursor {
        data: data.clone(),
        pos: 0,
    }
}

fn close(_: Box<Cursor>) {}

fn read(c: &mut Cursor, buf: &mut [c_char]) -> i64 {
    let rest = &c.data[c.pos.min(c.data.len())..];
    let n = rest.len().min(buf.len());
    for (dst, &src) in buf.iter_mut().zip(&rest[..n]) {
        *dst = src as c_char;
    }
    c.pos += n;
    n as i64
}

fn seek(c: &mut Cursor, offset: i64) -> i64 {
    c.pos = offset.clamp(0, c.data.len() as i64) as usize;
    c.pos as i64
}

fn size(c: &mut Cursor) -> i64 {
    c.data.len() as i64
}

/// Name of the stream protocol serving the video bytes.
const PROTOCOL: &str = "wpevideo";

fn create_mpv(hwdec: &str) -> Result<Mpv, VideoError> {
    Ok(Mpv::with_initializer(|init| {
        for (name, value) in [
            ("vo", "libmpv"),
            ("hwdec", hwdec),
            ("loop-file", "inf"),
            // At the end of a non-looping video, hold the last frame.
            ("keep-open", "yes"),
            ("audio", "no"),
            ("keepaspect", "no"),
            ("terminal", "no"),
            ("idle", "yes"),
        ] {
            init.set_option(name, value)?;
        }
        if let Ok(extra) = std::env::var("WPE_MPV_OPTS") {
            for kv in extra.split(',') {
                if let Some((k, v)) = kv.split_once('=') {
                    init.set_option(k, v)?;
                }
            }
        }
        Ok(())
    })?)
}

/// Where the renderer picks up frames.
enum Frames {
    Cpu(cpu::Frames),
    Gpu(gl::Frames),
}

pub struct VideoStream {
    mpv: Arc<Mpv>,
    frames: Frames,
    started: Arc<AtomicBool>,
    /// Playback was at the end when last checked.
    at_end: AtomicBool,
    /// Length in seconds, known once mpv has opened the file.
    duration: std::sync::OnceLock<f64>,
    wake: Arc<Wake>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl VideoStream {
    /// Start playing `mp4` into `target`, scaled to its size.
    pub fn start(gpu: &Gpu, mp4: &[u8], target: &GpuTexture) -> Result<Self, VideoError> {
        let data: Arc<[u8]> = mp4.into();
        if std::env::var("WPE_VIDEO").as_deref() != Ok("cpu") {
            match Self::start_gl(gpu, data.clone(), target) {
                Ok(stream) => return Ok(stream),
                Err(e) => tracing::warn!("GPU video path unavailable, decoding on the CPU: {e}"),
            }
        }
        let size = target.texture.size();
        let (width, height) = (size.width, size.height);
        let latest = cpu::Frames::default();
        let shared = latest.clone();
        Self::spawn(data, "no", Frames::Cpu(latest), move |mpv, wake| {
            cpu::Output::new(mpv, wake, width, height, shared)
                .map(|o| (Box::new(o) as Box<dyn Output>, ()))
        })
        .map(|(stream, ())| stream)
    }

    fn start_gl(gpu: &Gpu, data: Arc<[u8]>, target: &GpuTexture) -> Result<Self, VideoError> {
        let format = gl::check_target(gpu, target)?;
        let size = target.texture.size();
        let (width, height) = (size.width, size.height);
        let render_node = gl::render_node(gpu);
        let slots = gl::Slots::shared();
        let shared = slots.clone();
        let (mut stream, buffers) = Self::spawn(
            data,
            "vaapi,auto-safe",
            Frames::Cpu(cpu::Frames::default()),
            move |mpv, wake| {
                let (output, buffers) =
                    gl::Output::new(mpv, wake, width, height, shared, render_node)?;
                Ok((Box::new(output) as Box<dyn Output>, buffers))
            },
        )?;
        let frames = gl::Frames::import(gpu, buffers, width, height, format, slots)?;
        stream.frames = Frames::Gpu(frames);
        Ok(stream)
    }

    /// Run playback on a thread that owns mpv's renderer, set up there by
    /// `output` (an OpenGL context must stay on the thread that made it).
    fn spawn<T: Send + 'static>(
        data: Arc<[u8]>,
        hwdec: &str,
        frames: Frames,
        output: impl FnOnce(&Mpv, &Arc<Wake>) -> Result<(Box<dyn Output>, T), VideoError>
        + Send
        + 'static,
    ) -> Result<(Self, T), VideoError> {
        let mpv = Arc::new(create_mpv(hwdec)?);
        let wake = Arc::new(Wake::default());
        let started = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));

        let (core, waker, playing, stopping) =
            (mpv.clone(), wake.clone(), started.clone(), stop.clone());
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("video playback".to_owned())
            .spawn(move || {
                // The protocol borrows the core, so it lives on this thread.
                let protocol = unsafe {
                    Protocol::new(
                        &core,
                        PROTOCOL.to_owned(),
                        data,
                        open,
                        close,
                        read,
                        Some(seek),
                        Some(size),
                    )
                };
                let setup = output(&core, &waker).and_then(|(output, extra)| {
                    protocol.register()?;
                    core.command("loadfile", &[&format!("{PROTOCOL}://video")])?;
                    Ok((output, extra))
                });
                let mut output = match setup {
                    Ok((output, extra)) => {
                        let _ = ready_tx.send(Ok(extra));
                        output
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                while !stopping.load(Ordering::Relaxed) {
                    waker.wait(Duration::from_millis(100));
                    if output.render() {
                        playing.store(true, Ordering::Relaxed);
                    }
                }
                drop(output);
                drop(protocol);
            })
            .expect("spawn video thread");
        let mut stream = Self {
            mpv,
            frames,
            started,
            at_end: AtomicBool::new(false),
            duration: std::sync::OnceLock::new(),
            wake,
            stop,
            thread: Some(thread),
        };
        match ready_rx.recv() {
            Ok(Ok(extra)) => Ok((stream, extra)),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                stream.thread.take().map(JoinHandle::join);
                Err(VideoError::Mpv(libmpv2::Error::Null))
            }
        }
    }

    /// Bring `target` up to the newest decoded frame, if there is one.
    pub fn update(&self, gpu: &Gpu, target: &GpuTexture) {
        match &self.frames {
            Frames::Cpu(frames) => {
                if let Some(frame) = frames.take() {
                    target.write_rgba(gpu, &frame);
                }
            }
            Frames::Gpu(frames) => frames.copy_latest(gpu, target),
        }
    }

    /// Block until the first frame is decoded or `timeout` passes.
    pub fn wait_started(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline && !self.started.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Playback control, as SceneScript's video texture API exposes it.
impl VideoStream {
    fn set(&self, name: &str, value: impl libmpv2::SetData, what: &str) {
        if let Err(e) = self.mpv.set_property(name, value) {
            tracing::warn!("{what}: {e}");
        }
    }

    /// Play the video's sound at `volume` (0..1), or none (the default).
    pub fn set_audio(&self, volume: Option<f32>) {
        match volume {
            Some(volume) => {
                self.set(
                    "volume",
                    f64::from(volume.clamp(0.0, 1.0)) * 100.0,
                    "setting volume",
                );
                self.set("aid", "auto", "enabling video sound");
                tracing::debug!(
                    "video sound: track {:?}",
                    self.mpv.get_property::<String>("aid").ok()
                );
            }
            None => self.set("aid", "no", "muting video"),
        }
    }

    /// Scripts often call this every frame; unchanged state isn't resent.
    pub fn set_paused(&self, paused: bool) {
        if self.mpv.get_property::<bool>("pause").ok() != Some(paused) {
            self.set("pause", paused, "pausing video");
        }
    }

    pub fn is_playing(&self) -> bool {
        !self.mpv.get_property::<bool>("pause").unwrap_or(false)
            && !self
                .mpv
                .get_property::<bool>("eof-reached")
                .unwrap_or(false)
    }

    /// Pause and rewind to the start.
    pub fn stop(&self) {
        self.set_paused(true);
        self.seek(0.0);
    }

    /// Jump to `seconds` from the start.
    pub fn seek(&self, seconds: f64) {
        self.duration();
        if let Err(e) = self
            .mpv
            .command("seek", &[&seconds.max(0.0).to_string(), "absolute+exact"])
        {
            tracing::warn!("seeking video: {e}");
        }
    }

    pub fn rate(&self) -> f64 {
        self.mpv.get_property("speed").unwrap_or(1.0)
    }

    pub fn set_rate(&self, rate: f64) {
        // mpv's speed range.
        let rate = rate.clamp(0.01, 100.0);
        if self.rate() != rate {
            self.set("speed", rate, "setting video speed");
        }
    }

    pub fn looping(&self) -> bool {
        self.mpv
            .get_property::<String>("loop-file")
            .is_ok_and(|v| v != "no")
    }

    pub fn set_looping(&self, looping: bool) {
        self.set(
            "loop-file",
            if looping { "inf" } else { "no" },
            "setting video looping",
        );
    }

    /// Playback position in seconds.
    pub fn position(&self) -> f64 {
        self.mpv.get_property("time-pos").unwrap_or(0.0)
    }

    /// Length in seconds. Scripts ask right after loading, so this waits
    /// (briefly) for mpv to open the file.
    pub fn duration(&self) -> f64 {
        if let Some(&d) = self.duration.get() {
            return d;
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Ok(d) = self.mpv.get_property::<f64>("duration") {
                return *self.duration.get_or_init(|| d);
            }
            if Instant::now() >= deadline {
                return 0.0;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Whether playback reached the end (of a non-looping video) since the
    /// last call. mpv then holds the last frame, paused.
    pub fn take_ended(&self) -> bool {
        let at_end = self
            .mpv
            .get_property::<bool>("eof-reached")
            .unwrap_or(false);
        let was = self.at_end.swap(at_end, Ordering::Relaxed);
        at_end && !was
    }
}

impl Drop for VideoStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.wake.notify();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
