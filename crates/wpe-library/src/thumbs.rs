//! Tile thumbnails: a preview's first frame scaled down once and kept as a
//! PNG in the cache directory, named after the preview's path, size and
//! modification time.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Thumbnails are this size (square, covering, as previews mostly are):
/// twice a typical tile, for scaled outputs.
pub const WIDTH: u32 = 360;
pub const HEIGHT: u32 = 360;

/// `$XDG_CACHE_HOME/penguin-engine/thumbs`.
pub fn cache_dir() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("penguin-engine/thumbs"))
}

/// The cached thumbnail of `preview`, made now if there is none. None when
/// the preview doesn't decode or there is no cache directory.
pub fn ensure(preview: &Path) -> Option<PathBuf> {
    let cached = cache_dir()?.join(format!("{}.png", key(preview)));
    if cached.is_file() {
        return Some(cached);
    }
    let thumb = make(preview)?;
    std::fs::create_dir_all(cached.parent()?).ok()?;
    // Written under another name and renamed, so a reader never sees half.
    let partial = cached.with_extension(format!("{}.part", std::process::id()));
    if let Err(e) = thumb.save_with_format(&partial, image::ImageFormat::Png) {
        tracing::debug!("caching {}: {e}", cached.display());
        let _ = std::fs::remove_file(&partial);
        return None;
    }
    std::fs::rename(&partial, &cached).ok()?;
    Some(cached)
}

/// The thumbnail of `preview` as pixels.
pub fn rgba(preview: &Path) -> Option<image::RgbaImage> {
    match ensure(preview) {
        Some(cached) => image::open(cached).ok().map(|i| i.into_rgba8()),
        // No cache: make it anyway.
        None => make(preview),
    }
}

fn make(preview: &Path) -> Option<image::RgbaImage> {
    // `image::open` decodes the first frame of a GIF.
    let image = image::open(preview)
        .inspect_err(|e| tracing::debug!("{}: {e}", preview.display()))
        .ok()?;
    Some(
        image
            .resize_to_fill(WIDTH, HEIGHT, image::imageops::FilterType::Triangle)
            .into_rgba8(),
    )
}

fn key(preview: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    preview.hash(&mut hasher);
    if let Ok(meta) = std::fs::metadata(preview) {
        meta.len().hash(&mut hasher);
        if let Ok(time) = meta.modified() {
            time.duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .hash(&mut hasher);
        }
    }
    (WIDTH, HEIGHT).hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}
