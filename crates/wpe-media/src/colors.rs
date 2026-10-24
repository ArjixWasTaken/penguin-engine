//! Album-art colour extraction for `MediaThumbnailEvent`.
//!
//! The algorithm is a deterministic popularity quantiser:
//!
//! 1. Downscale the art to at most 64x64 (triangle filter) and skip pixels
//!    with alpha < 128.
//! 2. Bucket every pixel into a 3-bit-per-channel RGB grid (512 bins), keeping
//!    the pixel count and the colour sum per bin.
//! 3. Rank bins by count (ties: lower bin index first). Each bin's colour is
//!    the mean of its pixels, not the bin centre.
//! 4. `primary` is the top bin. `secondary` is the next-ranked bin whose
//!    colour is at least [`MIN_DISTANCE`] (Euclidean, normalized RGB) from
//!    primary; `tertiary` is the next one at least that far from both. When
//!    the art has too few distinct colours, the missing ones are made by
//!    mixing primary towards `highContrastColor` (35% and 65%).
//! 5. `highContrastColor` is black or white, whichever has the higher WCAG
//!    contrast ratio against primary (WE documents it as exactly that).
//! 6. `textColor` is secondary, else tertiary, if its WCAG contrast ratio
//!    against primary is at least [`TEXT_CONTRAST`] (4.5, WCAG AA); otherwise
//!    it is `highContrastColor`. WE documents it as "guaranteed to have
//!    sufficient contrast with primary color. May be secondary or tertiary
//!    color when possible."

use crate::event::ThumbnailColors;

/// Minimum Euclidean distance in normalized RGB between picked colours.
pub const MIN_DISTANCE: f32 = 0.25;
/// Minimum WCAG contrast ratio for `textColor`.
pub const TEXT_CONTRAST: f32 = 4.5;

const SAMPLE_SIZE: u32 = 64;
const BITS: u32 = 3;
const BINS: usize = 1 << (BITS * 3);

/// Extracts the thumbnail colours from RGBA8 pixels.
/// Returns [`ThumbnailColors::NEUTRAL`] when there's nothing opaque to look at.
pub fn extract_colors(width: u32, height: u32, rgba: &[u8]) -> ThumbnailColors {
    let Some(img) = image::RgbaImage::from_raw(width, height, rgba.to_vec()) else {
        return ThumbnailColors::NEUTRAL;
    };
    if width == 0 || height == 0 {
        return ThumbnailColors::NEUTRAL;
    }
    let img = if width > SAMPLE_SIZE || height > SAMPLE_SIZE {
        let scale = SAMPLE_SIZE as f32 / width.max(height) as f32;
        let w = ((width as f32 * scale).round() as u32).max(1);
        let h = ((height as f32 * scale).round() as u32).max(1);
        image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle)
    } else {
        img
    };

    let mut count = vec![0u32; BINS];
    let mut sum = vec![[0u64; 3]; BINS];
    for px in img.pixels() {
        let [r, g, b, a] = px.0;
        if a < 128 {
            continue;
        }
        let shift = 8 - BITS;
        let idx = ((r as usize >> shift) << (2 * BITS))
            | ((g as usize >> shift) << BITS)
            | (b as usize >> shift);
        count[idx] += 1;
        sum[idx][0] += r as u64;
        sum[idx][1] += g as u64;
        sum[idx][2] += b as u64;
    }

    let mut ranked: Vec<(u32, usize)> = count
        .iter()
        .enumerate()
        .filter(|(_, c)| **c > 0)
        .map(|(i, c)| (*c, i))
        .collect();
    if ranked.is_empty() {
        return ThumbnailColors::NEUTRAL;
    }
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let colors: Vec<[f32; 3]> = ranked
        .iter()
        .map(|&(c, i)| {
            let n = c as f32 * 255.0;
            [
                sum[i][0] as f32 / n,
                sum[i][1] as f32 / n,
                sum[i][2] as f32 / n,
            ]
        })
        .collect();

    let primary = colors[0];
    let high_contrast = high_contrast_for(primary);
    let mut rest = colors[1..].iter().copied();
    let secondary = rest.find(|c| distance(*c, primary) >= MIN_DISTANCE);
    let tertiary = secondary.and_then(|s| {
        rest.find(|c| distance(*c, primary) >= MIN_DISTANCE && distance(*c, s) >= MIN_DISTANCE)
    });
    let secondary = secondary.unwrap_or_else(|| mix(primary, high_contrast, 0.35));
    let tertiary = tertiary.unwrap_or_else(|| mix(primary, high_contrast, 0.65));

    let text = [secondary, tertiary]
        .into_iter()
        .find(|c| contrast_ratio(*c, primary) >= TEXT_CONTRAST)
        .unwrap_or(high_contrast);

    ThumbnailColors {
        primary,
        secondary,
        tertiary,
        text,
        high_contrast,
    }
}

/// Black or white, whichever contrasts more with `c` (ties go to white).
pub fn high_contrast_for(c: [f32; 3]) -> [f32; 3] {
    let white = [1.0; 3];
    let black = [0.0; 3];
    if contrast_ratio(white, c) >= contrast_ratio(black, c) {
        white
    } else {
        black
    }
}

/// WCAG 2 relative luminance of an sRGB colour.
pub fn relative_luminance(c: [f32; 3]) -> f32 {
    let lin = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

/// WCAG 2 contrast ratio (1..21).
pub fn contrast_ratio(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a w x h image from horizontal bands of (fraction, colour).
    fn bands(w: u32, h: u32, spec: &[(f32, [u8; 4])]) -> Vec<u8> {
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        let mut rows_done = 0u32;
        for (i, (frac, c)) in spec.iter().enumerate() {
            let rows = if i + 1 == spec.len() {
                h - rows_done
            } else {
                (h as f32 * frac).round() as u32
            };
            for _ in 0..rows * w {
                out.extend_from_slice(c);
            }
            rows_done += rows;
        }
        out
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        distance(a, b) < 0.01
    }

    #[test]
    fn ranks_by_population_and_picks_contrast() {
        let px = bands(
            40,
            100,
            &[
                (0.5, [255, 0, 0, 255]),
                (0.3, [0, 0, 255, 255]),
                (0.2, [0, 255, 0, 255]),
            ],
        );
        let c = extract_colors(40, 100, &px);
        assert!(close(c.primary, [1.0, 0.0, 0.0]), "{c:?}");
        assert!(close(c.secondary, [0.0, 0.0, 1.0]), "{c:?}");
        assert!(close(c.tertiary, [0.0, 1.0, 0.0]), "{c:?}");
        // Pure red contrasts more with black (5.25) than white (4.0).
        assert_eq!(c.high_contrast, [0.0, 0.0, 0.0]);
        // Neither blue nor green reaches 4.5:1 against red.
        assert_eq!(c.text, c.high_contrast);
        // Deterministic.
        assert_eq!(c, extract_colors(40, 100, &px));
    }

    #[test]
    fn text_color_prefers_palette_colour_when_readable() {
        let px = bands(
            30,
            30,
            &[(0.7, [10, 20, 60, 255]), (0.3, [250, 240, 200, 255])],
        );
        let c = extract_colors(30, 30, &px);
        assert!(close(c.primary, [10.0 / 255.0, 20.0 / 255.0, 60.0 / 255.0]));
        assert_eq!(c.high_contrast, [1.0, 1.0, 1.0]);
        assert_eq!(c.text, c.secondary);
        assert!(contrast_ratio(c.text, c.primary) >= TEXT_CONTRAST);
    }

    #[test]
    fn similar_shades_fall_back_to_mixes() {
        let px = bands(
            20,
            20,
            &[(0.6, [200, 0, 0, 255]), (0.4, [210, 12, 12, 255])],
        );
        let c = extract_colors(20, 20, &px);
        assert!(distance(c.secondary, c.primary) >= 0.2, "{c:?}");
        assert!(distance(c.tertiary, c.secondary) >= 0.2, "{c:?}");
        assert!(contrast_ratio(c.text, c.primary) >= TEXT_CONTRAST);
    }

    #[test]
    fn transparent_and_empty_are_neutral() {
        let px = bands(8, 8, &[(1.0, [255, 0, 0, 0])]);
        assert_eq!(extract_colors(8, 8, &px), ThumbnailColors::NEUTRAL);
        assert_eq!(extract_colors(0, 0, &[]), ThumbnailColors::NEUTRAL);
        // Mismatched buffer size.
        assert_eq!(extract_colors(4, 4, &[0; 3]), ThumbnailColors::NEUTRAL);
    }

    #[test]
    fn large_images_are_sampled() {
        let px = bands(
            1000,
            800,
            &[(0.75, [255, 255, 255, 255]), (0.25, [0, 0, 0, 255])],
        );
        let c = extract_colors(1000, 800, &px);
        assert!(close(c.primary, [1.0, 1.0, 1.0]), "{c:?}");
        assert!(close(c.secondary, [0.0, 0.0, 0.0]), "{c:?}");
        assert_eq!(c.high_contrast, [0.0, 0.0, 0.0]);
        assert_eq!(c.text, c.secondary);
    }

    #[test]
    fn wcag_reference_values() {
        assert!((contrast_ratio([1.0; 3], [0.0; 3]) - 21.0).abs() < 1e-3);
        assert!((contrast_ratio([1.0, 0.0, 0.0], [0.0; 3]) - 5.252).abs() < 1e-2);
    }
}
