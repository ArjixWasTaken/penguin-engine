//! Rasterisation of Wallpaper Engine text layers.
//!
//! A text object (`SceneObject::Text`) is turned into a [`TextStyle`] once,
//! then [`TextRenderer::render`] lays its string out and rasterises it into a
//! straight-alpha RGBA8 [`TextImage`] that the renderer draws as a textured
//! card at the object's origin, offset by [`TextImage::anchor_offset`].
//!
//! Layout follows Wallpaper Engine's conventions: `pointsize` is in points at
//! 300 DPI ([`POINT_TO_UNITS`] scene units per point), lines are spaced by the
//! font's `hhea` line height (ascender - descender + line gap), the card is
//! the laid-out text block plus `padding` on every side, and
//! `horizontalalign` / `verticalalign` both align lines inside the block and
//! pin the matching card edge to the object origin.
//!
//! Shaping, word wrapping, ellipsis and glyph fallback go through
//! `cosmic-text` (HarfBuzz-compatible shaping, swash rasterisation, fontdb
//! system font discovery), so CJK, emoji and other scripts the wallpaper's
//! font lacks fall back to installed system fonts.

mod fonts;
mod render;
mod style;

pub use render::{TextImage, TextRenderer};
pub use style::{DropShadow, HAlign, TextStyle, VAlign, resolve_text};

/// Scene units per typographic point: Wallpaper Engine lays text out at 300 DPI.
pub const POINT_TO_UNITS: f32 = 300.0 / 72.0;

/// Largest texture edge [`TextRenderer::render`] produces; bigger cards are
/// rasterised at a reduced [`TextStyle::raster_scale`].
pub const MAX_TEXTURE_SIZE: u32 = 8192;

/// Errors from [`TextRenderer::render`].
#[derive(Debug, thiserror::Error)]
pub enum TextError {
    /// Neither the requested font nor any system fallback could be loaded.
    #[error("no usable font for {0:?} and no system fallback font is installed")]
    NoFont(String),
    /// The style describes a degenerate layout.
    #[error("invalid text style: {0}")]
    InvalidStyle(&'static str),
}
