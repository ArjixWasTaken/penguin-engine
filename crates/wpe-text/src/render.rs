use cosmic_text::{
    Buffer, CacheKey, Color, Ellipsize, EllipsizeHeightLimit, FontSystem, LayoutRun, Metrics,
    Shaping, SwashCache, Wrap,
};
use wpe_vfs::Vfs;

use crate::fonts::{Face, FontCache};
use crate::style::TextStyle;
use crate::{MAX_TEXTURE_SIZE, TextError};

/// A rasterised text layer.
#[derive(Debug, Clone, PartialEq)]
pub struct TextImage {
    pub width: u32,
    pub height: u32,
    /// `width * height` RGBA8 texels, straight alpha, rows top to bottom.
    pub pixels: Vec<u8>,
    /// Card size in scene units (texture size / [`Self::raster_scale`]).
    pub card_size: [f32; 2],
    /// Card centre relative to the object origin, in scene units with +y up:
    /// alignment pins the matching card edge to the origin.
    pub anchor_offset: [f32; 2],
    /// Texture pixels per scene unit actually used (lowered when the card
    /// would exceed [`MAX_TEXTURE_SIZE`]).
    pub raster_scale: f32,
    /// Number of laid-out lines after wrapping and row limits.
    pub lines: usize,
}

struct PlacedGlyph {
    key: CacheKey,
    x: i32,
    y: i32,
}

struct Layout {
    glyphs: Vec<PlacedGlyph>,
    width: u32,
    height: u32,
    lines: usize,
}

/// Lays out and rasterises text layers; owns the font database and glyph cache
/// so re-rendering changed text (clocks, media titles) is cheap.
pub struct TextRenderer {
    fonts: FontSystem,
    glyphs: SwashCache,
    cache: FontCache,
}

impl Default for TextRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for TextRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextRenderer")
            .field("faces", &self.fonts.db().len())
            .finish_non_exhaustive()
    }
}

/// Advance width of a laid-out line, ignoring trailing whitespace.
fn ink_width(run: &LayoutRun) -> f32 {
    run.glyphs
        .iter()
        .filter(|g| {
            !run.text
                .get(g.start..g.end)
                .is_some_and(|s| s.chars().all(char::is_whitespace))
        })
        .map(|g| g.x + g.w)
        .fold(0.0, f32::max)
}

/// Composite straight `rgb` at coverage `a` beneath premultiplied `dst`.
fn under(dst: &mut [f32; 4], rgb: [f32; 3], a: f32) {
    let keep = (1.0 - dst[3]) * a;
    for (d, c) in dst.iter_mut().zip(rgb) {
        *d += c.clamp(0.0, 1.0) * keep;
    }
    dst[3] += keep;
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Two box-blur passes per axis over a single channel.
fn blur(src: &[f32], w: usize, h: usize, radius: usize) -> Vec<f32> {
    let mut a = src.to_vec();
    if radius == 0 {
        return a;
    }
    let mut b = vec![0.0; a.len()];
    let norm = 1.0 / (2 * radius + 1) as f32;
    for _ in 0..2 {
        for y in 0..h {
            let row = &a[y * w..(y + 1) * w];
            let mut sum: f32 = row[..radius.min(w)].iter().sum();
            for x in 0..w {
                if x + radius < w {
                    sum += row[x + radius];
                }
                b[y * w + x] = sum * norm;
                if x >= radius {
                    sum -= row[x - radius];
                }
            }
        }
        for x in 0..w {
            let mut sum: f32 = (0..radius.min(h)).map(|y| b[y * w + x]).sum();
            for y in 0..h {
                if y + radius < h {
                    sum += b[(y + radius) * w + x];
                }
                a[y * w + x] = sum * norm;
                if y >= radius {
                    sum -= b[(y - radius) * w + x];
                }
            }
        }
    }
    a
}

impl TextRenderer {
    /// A renderer with every installed system font available for fallback.
    /// Scanning system fonts takes a moment; create one and reuse it.
    pub fn new() -> Self {
        Self::with_font_system(FontSystem::new())
    }

    /// A renderer over an explicit font database (no system scan); fallback
    /// is limited to the faces in `db`.
    pub fn with_font_db(db: cosmic_text::fontdb::Database) -> Self {
        Self::with_font_system(FontSystem::new_with_locale_and_db("en-US".into(), db))
    }

    fn with_font_system(fonts: FontSystem) -> Self {
        Self {
            fonts,
            glyphs: SwashCache::new(),
            cache: FontCache::default(),
        }
    }

    /// The underlying font database, e.g. to register extra fallback fonts.
    pub fn font_db_mut(&mut self) -> &mut cosmic_text::fontdb::Database {
        self.fonts.db_mut()
    }

    /// Drop cached glyph bitmaps (fonts stay loaded).
    pub fn clear_glyph_cache(&mut self) {
        self.glyphs = SwashCache::new();
    }

    /// Lay out and rasterise `text` with `style`, loading the font from `vfs`.
    pub fn render(
        &mut self,
        vfs: &Vfs,
        style: &TextStyle,
        text: &str,
    ) -> Result<TextImage, TextError> {
        let font_size = style.font_size();
        if !(font_size.is_finite() && font_size > 0.0) {
            return Err(TextError::InvalidStyle("pointsize must be positive"));
        }
        if !(style.raster_scale.is_finite() && style.raster_scale > 0.0) {
            return Err(TextError::InvalidStyle("raster_scale must be positive"));
        }
        let face = self
            .cache
            .resolve(&mut self.fonts, vfs, &style.font)
            .ok_or_else(|| TextError::NoFont(style.font.clone()))?;

        let mut scale = style.raster_scale;
        let mut layout = self.layout(&face, style, text, scale);
        for _ in 0..4 {
            let edge = layout.width.max(layout.height);
            if edge <= MAX_TEXTURE_SIZE {
                break;
            }
            scale *= MAX_TEXTURE_SIZE as f32 / edge as f32 * 0.98;
            layout = self.layout(&face, style, text, scale);
        }
        let (w, h) = (
            layout.width.min(MAX_TEXTURE_SIZE),
            layout.height.min(MAX_TEXTURE_SIZE),
        );
        let pixels = self.raster(&layout, style, scale, w, h);
        let card = [w as f32 / scale, h as f32 / scale];
        Ok(TextImage {
            width: w,
            height: h,
            pixels,
            card_size: card,
            anchor_offset: [
                (0.5 - style.halign.factor()) * card[0],
                (style.valign.factor() - 0.5) * card[1],
            ],
            raster_scale: scale,
            lines: layout.lines,
        })
    }

    fn layout(&mut self, face: &Face, style: &TextStyle, text: &str, scale: f32) -> Layout {
        let fs = &mut self.fonts;
        let font_px = (style.font_size() * scale).max(0.5);
        let line_px = (face.line_height * font_px + style.spacing[1] * scale)
            .round()
            .max(1.0);
        let ascent_px = face.ascent * font_px;
        let pad = style.padding.map(|p| p * scale);

        let mut buffer = Buffer::new(fs, Metrics::new(font_px, line_px));
        let max_width = style.max_width.map(|w| (w * scale).max(font_px));
        buffer.set_wrap(if max_width.is_some() {
            Wrap::WordOrGlyph
        } else {
            Wrap::None
        });
        buffer.set_size(max_width, None);
        if let (true, Some(rows), Some(_)) = (style.ellipsis, style.max_rows, max_width) {
            buffer.set_ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(rows as usize)));
        }
        let mut attrs = face.attrs();
        if style.spacing[0] != 0.0 {
            attrs = attrs.letter_spacing(style.spacing[0] / style.font_size());
        }
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(fs, false);

        let max_rows = style.max_rows.map_or(usize::MAX, |r| r as usize);
        let runs: Vec<_> = buffer.layout_runs().take(max_rows).collect();
        let widths: Vec<f32> = runs.iter().map(ink_width).collect();
        let content_w = widths.iter().copied().fold(0.0, f32::max);
        let lines = runs.len().max(1);
        let content_h = lines as f32 * line_px;

        let mut glyphs = Vec::new();
        for (i, run) in runs.iter().enumerate() {
            let x0 = pad[0] + (content_w - widths[i]) * style.halign.factor();
            let baseline = (pad[1] + i as f32 * line_px + ascent_px).round();
            for g in run.glyphs {
                let p = g.physical((x0, baseline), 1.0);
                glyphs.push(PlacedGlyph {
                    key: p.cache_key,
                    x: p.x,
                    y: p.y,
                });
            }
        }
        Layout {
            glyphs,
            width: ((content_w + 2.0 * pad[0]).ceil() as u32).max(1),
            height: ((content_h + 2.0 * pad[1]).ceil() as u32).max(1),
            lines,
        }
    }

    fn raster(
        &mut self,
        layout: &Layout,
        style: &TextStyle,
        scale: f32,
        w: u32,
        h: u32,
    ) -> Vec<u8> {
        let (w, h) = (w as usize, h as usize);
        // Premultiplied RGBA.
        let mut text = vec![[0.0f32; 4]; w * h];
        let [r, g, b] = style.color.map(to_u8);
        let base = Color::rgb(r, g, b);
        for glyph in &layout.glyphs {
            self.glyphs
                .with_pixels(&mut self.fonts, glyph.key, base, |dx, dy, c| {
                    let (x, y) = (glyph.x + dx, glyph.y + dy);
                    if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
                        return;
                    }
                    let a = f32::from(c.a()) / 255.0;
                    if a == 0.0 {
                        return;
                    }
                    let src = [
                        f32::from(c.r()) / 255.0 * a,
                        f32::from(c.g()) / 255.0 * a,
                        f32::from(c.b()) / 255.0 * a,
                        a,
                    ];
                    let dst = &mut text[y as usize * w + x as usize];
                    for (d, s) in dst.iter_mut().zip(src) {
                        *d = s + *d * (1.0 - a);
                    }
                });
        }

        if let Some(shadow) = style.drop_shadow {
            let alpha: Vec<f32> = text.iter().map(|p| p[3]).collect();
            let blurred = blur(&alpha, w, h, (shadow.size * scale * 0.5).round() as usize);
            let (ox, oy) = (
                (shadow.offset[0] * scale).round() as isize,
                (shadow.offset[1] * scale).round() as isize,
            );
            let opacity = shadow.opacity.clamp(0.0, 1.0);
            for y in 0..h as isize {
                for x in 0..w as isize {
                    let (sx, sy) = (x - ox, y - oy);
                    if sx < 0 || sy < 0 || sx >= w as isize || sy >= h as isize {
                        continue;
                    }
                    let sa = blurred[sy as usize * w + sx as usize] * opacity;
                    under(&mut text[y as usize * w + x as usize], shadow.color, sa);
                }
            }
        }

        if let Some(bg) = style.background {
            for p in &mut text {
                under(p, bg, 1.0);
            }
        }

        let mut out = Vec::with_capacity(w * h * 4);
        for p in &text {
            let a = p[3].clamp(0.0, 1.0);
            if a <= 0.0 {
                out.extend_from_slice(&[r, g, b, 0]);
            } else {
                out.extend_from_slice(&[
                    to_u8(p[0] / a),
                    to_u8(p[1] / a),
                    to_u8(p[2] / a),
                    to_u8(a),
                ]);
            }
        }
        out
    }
}
