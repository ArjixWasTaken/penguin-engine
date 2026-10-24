//! Audio for wallpapers: playing sound layers and analysing what the system
//! plays for audio-reactive shaders.

mod capture;
mod player;
mod spectrum;

pub use capture::{AudioCapture, CaptureError};
pub use player::{Playback, PlaybackError, SoundOutput, TrackId};
pub use spectrum::{Analyzer, AudioSpectrum, FFT_SIZE};
