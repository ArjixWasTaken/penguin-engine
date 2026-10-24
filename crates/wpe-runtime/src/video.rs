//! Video textures: MP4 payloads inside `.tex` files, decoded by an `ffmpeg`
//! subprocess that paces frames at the video's own rate and loops forever.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The most recent decoded frame, taken by the renderer.
#[derive(Default)]
struct Latest {
    frame: Option<Vec<u8>>,
    /// Some frame has been decoded since start.
    started: bool,
}

pub struct VideoStream {
    latest: Arc<Mutex<Latest>>,
    child: Child,
}

#[derive(Debug, thiserror::Error)]
pub enum VideoError {
    #[error("writing the video to {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("starting ffmpeg: {0}")]
    Spawn(std::io::Error),
}

fn cache_path(mp4: &[u8]) -> PathBuf {
    let digest = sha1_smol::Sha1::from(mp4).digest().to_string();
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("wallpaper-engine-rs/video")
        .join(format!("{digest}.mp4"))
}

impl VideoStream {
    /// Start decoding `mp4`, scaled to `width × height` RGBA frames.
    pub fn start(mp4: &[u8], width: u32, height: u32) -> Result<Self, VideoError> {
        let path = cache_path(mp4);
        if !path.is_file() {
            let write = |p: &PathBuf| -> std::io::Result<()> {
                std::fs::create_dir_all(p.parent().expect("cache path has a parent"))?;
                std::fs::write(p, mp4)
            };
            write(&path).map_err(|source| VideoError::Write {
                path: path.clone(),
                source,
            })?;
        }
        let mut child = Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-loglevel",
                "error",
                "-re",
                "-stream_loop",
                "-1",
                "-i",
            ])
            .arg(&path)
            .args([
                "-an",
                "-vf",
                &format!("scale={width}:{height},format=rgba"),
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "pipe:1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(VideoError::Spawn)?;

        let mut stdout = child.stdout.take().expect("stdout is piped");
        let latest = Arc::new(Mutex::new(Latest::default()));
        let shared = latest.clone();
        let frame_len = width as usize * height as usize * 4;
        std::thread::Builder::new()
            .name("video decode".to_owned())
            .spawn(move || {
                let mut buf = vec![0u8; frame_len];
                while stdout.read_exact(&mut buf).is_ok() {
                    let mut l = shared.lock().unwrap_or_else(|e| e.into_inner());
                    l.frame = Some(std::mem::replace(&mut buf, vec![0u8; frame_len]));
                    l.started = true;
                }
            })
            .expect("spawn video thread");
        Ok(Self { latest, child })
    }

    /// The newest frame decoded since the last call.
    pub fn take_frame(&self) -> Option<Vec<u8>> {
        self.latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .frame
            .take()
    }

    /// Block until the first frame is decoded or `timeout` passes.
    pub fn wait_started(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self
                .latest
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .started
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for VideoStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
