//! MPRIS media integration for Wallpaper Engine scenes.
//!
//! [`MediaMonitor`] watches the desktop's media players over the D-Bus
//! session bus (MPRIS) on background threads and turns their state into the
//! events SceneScript delivers to wallpapers (`mediaStatusChanged`,
//! `mediaPlaybackChanged`, `mediaPropertiesChanged`, `mediaThumbnailChanged`,
//! `mediaTimelineChanged`), plus the decoded album art for the
//! `$mediaThumbnail` system texture.
//!
//! ```no_run
//! let mut media = wpe_media::MediaMonitor::start()?;
//! for ev in media.poll() {
//!     println!("{}({})", ev.handler_name(), ev.to_json());
//! }
//! if let Some(art) = media.thumbnail() {
//!     println!("art {}x{} gen {}", art.width, art.height, art.generation);
//! }
//! # Ok::<(), wpe_media::MediaError>(())
//! ```

pub mod art;
pub mod colors;
mod event;
mod monitor;
pub mod mpris;
pub mod tracker;

pub use art::{MAX_THUMBNAIL_SIDE, Thumbnail};
pub use colors::extract_colors;
pub use event::{
    MediaEvent, MediaEventKind, MediaProperties, PlaybackState, ThumbnailColors, coalesce,
};
pub use monitor::{MediaMonitor, POSITION_POLL};

/// Errors from the media monitor.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    /// No session bus, or a D-Bus call failed.
    #[error("D-Bus: {0}")]
    Bus(#[from] zbus::Error),
    /// Album art couldn't be parsed, fetched or decoded.
    #[error("album art: {0}")]
    Art(String),
    /// A background thread couldn't be spawned.
    #[error("media monitor thread: {0}")]
    Thread(String),
}
