//! Capturing what the system is playing, through the PulseAudio API
//! (PipeWire provides it), from the default output's monitor.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use libpulse_binding::def::BufferAttr;
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;

use crate::spectrum::{Analyzer, AudioSpectrum, FFT_SIZE};

const RATE: u32 = 48_000;

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("connecting to the audio server: {0}")]
    Connect(String),
}

/// Records the default output's monitor on a background thread and turns
/// the latest window into a spectrum on demand.
pub struct AudioCapture {
    /// The most recent `FFT_SIZE` interleaved stereo frames.
    latest: Arc<Mutex<Vec<f32>>>,
    stop: Arc<AtomicBool>,
    analyzer: Analyzer,
}

impl AudioCapture {
    pub fn start() -> Result<Self, CaptureError> {
        let spec = Spec {
            format: Format::F32le,
            channels: 2,
            rate: RATE,
        };
        let frame_bytes = 2 * size_of::<f32>() as u32;
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: FFT_SIZE as u32 / 2 * frame_bytes,
        };
        let simple = Simple::new(
            None,
            "Penguin Engine",
            Direction::Record,
            Some("@DEFAULT_MONITOR@"),
            "audio spectrum",
            &spec,
            None,
            Some(&attr),
        )
        .map_err(|e| CaptureError::Connect(e.to_string().unwrap_or_default()))?;

        let latest = Arc::new(Mutex::new(vec![0.0f32; FFT_SIZE * 2]));
        let stop = Arc::new(AtomicBool::new(false));
        let (shared, stopping) = (latest.clone(), stop.clone());
        std::thread::Builder::new()
            .name("audio capture".to_owned())
            .spawn(move || {
                let mut bytes = vec![0u8; FFT_SIZE / 2 * frame_bytes as usize];
                while !stopping.load(Ordering::Relaxed) {
                    if simple.read(&mut bytes).is_err() {
                        tracing::warn!("audio capture stopped");
                        return;
                    }
                    let samples = bytes
                        .chunks_exact(4)
                        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                    let mut window = shared.lock().unwrap_or_else(|e| e.into_inner());
                    window.extend(samples);
                    let excess = window.len().saturating_sub(FFT_SIZE * 2);
                    window.drain(..excess);
                }
            })
            .expect("spawn audio thread");
        Ok(Self {
            latest,
            stop,
            analyzer: Analyzer::new(RATE),
        })
    }

    /// Spectrum of the most recent audio window.
    pub fn spectrum(&mut self) -> AudioSpectrum {
        let window = self
            .latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        self.analyzer.process(&window)
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
