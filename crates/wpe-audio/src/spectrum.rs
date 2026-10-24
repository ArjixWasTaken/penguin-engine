//! Audio spectrum as Wallpaper Engine shaders see it in
//! `g_AudioSpectrum{16,32,64}{Left,Right}`.

use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex};

/// FFT window in frames (~10.7 ms at 48 kHz).
pub const FFT_SIZE: usize = 512;
const BINS: usize = FFT_SIZE / 2 + 1;
const MIN_FREQ: f32 = 20.0;
const MAX_FREQ: f32 = 20_000.0;
const GAIN: f32 = 15.0;
const ATTACK: f32 = 0.8;
const DECAY: f32 = 0.4;

/// Smoothed band magnitudes for both channels, roughly 0..1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioSpectrum {
    pub left64: [f32; 64],
    pub right64: [f32; 64],
    pub left32: [f32; 32],
    pub right32: [f32; 32],
    pub left16: [f32; 16],
    pub right16: [f32; 16],
}

impl Default for AudioSpectrum {
    fn default() -> Self {
        Self {
            left64: [0.0; 64],
            right64: [0.0; 64],
            left32: [0.0; 32],
            right32: [0.0; 32],
            left16: [0.0; 16],
            right16: [0.0; 16],
        }
    }
}

impl AudioSpectrum {
    /// A moving test pattern, for exercising audio-reactive wallpapers
    /// without sound.
    pub fn synthetic(time: f32) -> Self {
        let band = |i: usize, n: usize, phase: f32| {
            let x = i as f32 / n as f32;
            (0.5 + 0.5 * (time * 3.0 + x * 12.0 + phase).sin()) * (1.0 - 0.6 * x)
        };
        let mut s = Self::default();
        for i in 0..64 {
            (s.left64[i], s.right64[i]) = (band(i, 64, 0.0), band(i, 64, 1.0));
        }
        for i in 0..32 {
            (s.left32[i], s.right32[i]) = (band(i, 32, 0.0), band(i, 32, 1.0));
        }
        for i in 0..16 {
            (s.left16[i], s.right16[i]) = (band(i, 16, 0.0), band(i, 16, 1.0));
        }
        s
    }

    /// `(uniform name, values)` for every spectrum uniform.
    pub fn uniforms(&self) -> [(&'static str, &[f32]); 6] {
        [
            ("g_AudioSpectrum16Left", &self.left16),
            ("g_AudioSpectrum16Right", &self.right16),
            ("g_AudioSpectrum32Left", &self.left32),
            ("g_AudioSpectrum32Right", &self.right32),
            ("g_AudioSpectrum64Left", &self.left64),
            ("g_AudioSpectrum64Right", &self.right64),
        ]
    }
}

/// Turns stereo PCM windows into an [`AudioSpectrum`] with the reference
/// renderer's banding, gain, saturation and attack/decay smoothing.
pub struct Analyzer {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    /// Bin edges of the 64 logarithmic bands.
    edges: [usize; 65],
    spectrum: AudioSpectrum,
}

fn band_edges(sample_rate: u32) -> [usize; 65] {
    let per_bin = sample_rate as f32 / FFT_SIZE as f32;
    let (lo, hi) = (MIN_FREQ.log10(), MAX_FREQ.log10());
    let mut edges = [0usize; 65];
    for (b, edge) in edges.iter_mut().enumerate() {
        let freq = 10f32.powf(lo + (hi - lo) * b as f32 / 64.0);
        *edge = ((freq / per_bin + 0.5) as usize).min(BINS - 1);
    }
    for b in 0..64 {
        if edges[b + 1] <= edges[b] {
            edges[b + 1] = (edges[b] + 1).min(BINS - 1);
        }
    }
    edges
}

fn soft_saturate(x: f32) -> f32 {
    let x = x.max(0.0).sqrt();
    x / (1.0 + x * 0.4)
}

impl Analyzer {
    pub fn new(sample_rate: u32) -> Self {
        let window = (0..FFT_SIZE)
            .map(|i| {
                0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos())
            })
            .collect();
        Self {
            fft: RealFftPlanner::new().plan_fft_forward(FFT_SIZE),
            window,
            edges: band_edges(sample_rate),
            spectrum: AudioSpectrum::default(),
        }
    }

    fn magnitudes(&self, samples: impl Iterator<Item = f32>) -> Vec<f32> {
        let mut input: Vec<f32> = samples.zip(&self.window).map(|(s, w)| s * w).collect();
        input.resize(FFT_SIZE, 0.0);
        let mut output = self.fft.make_output_vec();
        self.fft
            .process(&mut input, &mut output)
            .expect("buffers sized by the planner");
        let norm = 2.0 / FFT_SIZE as f32;
        let mut mags: Vec<f32> = output.iter().map(|c| c.norm() * norm).collect();
        mags[0] *= 0.5;
        mags[BINS - 1] *= 0.5;
        mags
    }

    fn bands(&self, mags: &[f32], smooth: &mut [f32; 64]) {
        for (b, value) in smooth.iter_mut().enumerate() {
            let (lo, hi) = (
                self.edges[b],
                self.edges[b + 1].max(self.edges[b] + 1).min(BINS),
            );
            let mean = mags[lo..hi].iter().sum::<f32>() / (hi - lo) as f32;
            let new = soft_saturate(mean * GAIN);
            *value = if new >= *value {
                new * ATTACK + *value * (1.0 - ATTACK)
            } else {
                new * (1.0 - DECAY) + *value * DECAY
            };
        }
    }

    /// Analyse the latest `FFT_SIZE` interleaved stereo frames.
    pub fn process(&mut self, interleaved: &[f32]) -> AudioSpectrum {
        let frames = interleaved.len() / 2;
        let start = frames.saturating_sub(FFT_SIZE);
        let window = &interleaved[start * 2..frames * 2];
        let left = self.magnitudes(window.iter().step_by(2).copied());
        let right = self.magnitudes(window.iter().skip(1).step_by(2).copied());
        let mut s = self.spectrum;
        self.bands(&left, &mut s.left64);
        self.bands(&right, &mut s.right64);
        for b in 0..32 {
            s.left32[b] = (s.left64[b * 2] + s.left64[b * 2 + 1]) * 0.5;
            s.right32[b] = (s.right64[b * 2] + s.right64[b * 2 + 1]) * 0.5;
        }
        for b in 0..16 {
            s.left16[b] = (s.left32[b * 2] + s.left32[b * 2 + 1]) * 0.5;
            s.right16[b] = (s.right32[b * 2] + s.right32[b * 2 + 1]) * 0.5;
        }
        self.spectrum = s;
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_zero_and_tones_peak_in_their_band() {
        let mut a = Analyzer::new(48_000);
        let s = a.process(&vec![0.0; FFT_SIZE * 2]);
        assert!(s.left64.iter().all(|&v| v == 0.0));

        let tone: Vec<f32> = (0..FFT_SIZE)
            .flat_map(|i| {
                let v = (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48_000.0).sin();
                [v, 0.0]
            })
            .collect();
        let mut s = AudioSpectrum::default();
        for _ in 0..10 {
            s = a.process(&tone);
        }
        let peak = (0..64)
            .max_by(|&x, &y| s.left64[x].total_cmp(&s.left64[y]))
            .unwrap();
        let edges = band_edges(48_000);
        let bin = (1000.0 / (48_000.0 / FFT_SIZE as f32)).round() as usize;
        assert!(
            edges[peak] <= bin && bin <= edges[peak + 1] + 1,
            "peak band {peak}"
        );
        assert!(s.right64.iter().all(|&v| v < 1e-3));
        assert!(s.left16.iter().any(|&v| v > 0.1));
    }
}
