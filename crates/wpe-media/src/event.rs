//! Script-facing media events and their JSON shape.
//!
//! Field names and constant values follow Wallpaper Engine's own SceneScript
//! typings (`ui/dist/monaco/autocomplete/lib.sceneScript.d.ts` and
//! `assets/scripts/jsclasses/baseclasses.js` in the WE install), which define
//! `MediaStatusEvent`, `MediaPlaybackEvent`, `MediaPropertiesEvent`,
//! `MediaThumbnailEvent` and `MediaTimelineEvent`.

use serde_json::{Value, json};

/// `MediaPlaybackEvent.state`. The discriminants are WE's constants
/// (`PLAYBACK_STOPPED = 0`, `PLAYBACK_PLAYING = 1`, `PLAYBACK_PAUSED = 2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum PlaybackState {
    #[default]
    Stopped = 0,
    Playing = 1,
    Paused = 2,
}

impl PlaybackState {
    /// The numeric value scripts compare against.
    pub fn as_number(self) -> u8 {
        self as u8
    }

    /// Parses MPRIS `PlaybackStatus` ("Playing" / "Paused" / "Stopped").
    /// Anything unknown counts as stopped.
    pub fn from_mpris(status: &str) -> Self {
        match status {
            "Playing" => Self::Playing,
            "Paused" => Self::Paused,
            _ => Self::Stopped,
        }
    }
}

/// `MediaPropertiesEvent`. MPRIS has no sub-title, so `sub_title` is
/// normally empty; `genres` is comma-separated as WE documents.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MediaProperties {
    pub title: String,
    pub artist: String,
    pub sub_title: String,
    pub album_title: String,
    pub album_artist: String,
    pub genres: String,
    pub content_type: String,
}

/// The five `MediaThumbnailEvent` colours, normalized RGB.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThumbnailColors {
    pub primary: [f32; 3],
    pub secondary: [f32; 3],
    pub tertiary: [f32; 3],
    /// Readable against `primary`; secondary/tertiary when they qualify.
    pub text: [f32; 3],
    /// Black or white, whichever contrasts more with `primary`.
    pub high_contrast: [f32; 3],
}

impl ThumbnailColors {
    /// Colours reported when there is no album art: black primary, greys
    /// for secondary/tertiary, white text. (WE's own values for this case are
    /// not documented; this is a guess that keeps text readable.)
    pub const NEUTRAL: Self = Self {
        primary: [0.0, 0.0, 0.0],
        secondary: [0.25, 0.25, 0.25],
        tertiary: [0.5, 0.5, 0.5],
        text: [1.0, 1.0, 1.0],
        high_contrast: [1.0, 1.0, 1.0],
    };
}

impl Default for ThumbnailColors {
    fn default() -> Self {
        Self::NEUTRAL
    }
}

/// One event for the script runtime.
#[derive(Debug, Clone, PartialEq)]
pub enum MediaEvent {
    /// `mediaStatusChanged`: is there a player to follow at all?
    Status { enabled: bool },
    /// `mediaPlaybackChanged`.
    Playback { state: PlaybackState },
    /// `mediaPropertiesChanged`.
    Properties(MediaProperties),
    /// `mediaThumbnailChanged`. `generation` matches
    /// [`crate::Thumbnail::generation`] of the art current when this event
    /// was produced (0 when `has_thumbnail` is false).
    Thumbnail {
        has_thumbnail: bool,
        colors: ThumbnailColors,
        generation: u64,
    },
    /// `mediaTimelineChanged`, in seconds.
    Timeline { position: f64, duration: f64 },
}

/// Discriminant used for coalescing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaEventKind {
    Status,
    Playback,
    Properties,
    Thumbnail,
    Timeline,
}

fn vec3(c: [f32; 3]) -> Value {
    json!({ "x": c[0], "y": c[1], "z": c[2] })
}

impl MediaEvent {
    pub fn kind(&self) -> MediaEventKind {
        match self {
            Self::Status { .. } => MediaEventKind::Status,
            Self::Playback { .. } => MediaEventKind::Playback,
            Self::Properties(_) => MediaEventKind::Properties,
            Self::Thumbnail { .. } => MediaEventKind::Thumbnail,
            Self::Timeline { .. } => MediaEventKind::Timeline,
        }
    }

    /// Name of the exported script function this event is delivered to.
    pub fn handler_name(&self) -> &'static str {
        match self {
            Self::Status { .. } => "mediaStatusChanged",
            Self::Playback { .. } => "mediaPlaybackChanged",
            Self::Properties(_) => "mediaPropertiesChanged",
            Self::Thumbnail { .. } => "mediaThumbnailChanged",
            Self::Timeline { .. } => "mediaTimelineChanged",
        }
    }

    /// The event object exactly as scripts see it. Colours are `{x, y, z}`
    /// objects (the runtime turns them into `Vec3`, whose `r/g/b` aliases
    /// scripts also use).
    pub fn to_json(&self) -> Value {
        match self {
            Self::Status { enabled } => json!({ "enabled": enabled }),
            Self::Playback { state } => json!({ "state": state.as_number() }),
            Self::Properties(p) => json!({
                "title": p.title,
                "artist": p.artist,
                "subTitle": p.sub_title,
                "albumTitle": p.album_title,
                "albumArtist": p.album_artist,
                "genres": p.genres,
                "contentType": p.content_type,
            }),
            Self::Thumbnail {
                has_thumbnail,
                colors,
                ..
            } => json!({
                "hasThumbnail": has_thumbnail,
                "primaryColor": vec3(colors.primary),
                "secondaryColor": vec3(colors.secondary),
                "tertiaryColor": vec3(colors.tertiary),
                "textColor": vec3(colors.text),
                "highContrastColor": vec3(colors.high_contrast),
            }),
            Self::Timeline { position, duration } => {
                json!({ "position": position, "duration": duration })
            }
        }
    }
}

/// Coalesces a batch of events: for each kind only the latest event is kept,
/// and the kept events stay in the order of their last occurrence. Every event
/// carries full state, so dropping superseded ones loses nothing.
pub fn coalesce(events: Vec<MediaEvent>) -> Vec<MediaEvent> {
    let mut out: Vec<MediaEvent> = Vec::with_capacity(events.len().min(5));
    for ev in events {
        let kind = ev.kind();
        out.retain(|e| e.kind() != kind);
        out.push(ev);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_constants_match_we() {
        assert_eq!(PlaybackState::Stopped.as_number(), 0);
        assert_eq!(PlaybackState::Playing.as_number(), 1);
        assert_eq!(PlaybackState::Paused.as_number(), 2);
        assert_eq!(PlaybackState::from_mpris("Playing"), PlaybackState::Playing);
        assert_eq!(PlaybackState::from_mpris("Paused"), PlaybackState::Paused);
        assert_eq!(PlaybackState::from_mpris("Stopped"), PlaybackState::Stopped);
        assert_eq!(PlaybackState::from_mpris("bogus"), PlaybackState::Stopped);
    }

    #[test]
    fn json_shapes() {
        assert_eq!(
            MediaEvent::Status { enabled: true }.to_json(),
            json!({ "enabled": true })
        );
        assert_eq!(
            MediaEvent::Playback {
                state: PlaybackState::Paused
            }
            .to_json(),
            json!({ "state": 2 })
        );
        assert_eq!(
            MediaEvent::Timeline {
                position: 12.5,
                duration: 200.0
            }
            .to_json(),
            json!({ "position": 12.5, "duration": 200.0 })
        );
        let p = MediaProperties {
            title: "T".into(),
            artist: "A".into(),
            sub_title: String::new(),
            album_title: "Al".into(),
            album_artist: "AA".into(),
            genres: "Rock, Pop".into(),
            content_type: "audio".into(),
        };
        assert_eq!(
            MediaEvent::Properties(p).to_json(),
            json!({
                "title": "T", "artist": "A", "subTitle": "", "albumTitle": "Al",
                "albumArtist": "AA", "genres": "Rock, Pop", "contentType": "audio"
            })
        );
        let ev = MediaEvent::Thumbnail {
            has_thumbnail: true,
            colors: ThumbnailColors {
                primary: [1.0, 0.5, 0.0],
                secondary: [0.0, 0.0, 1.0],
                tertiary: [0.0, 1.0, 0.0],
                text: [0.0, 0.0, 0.0],
                high_contrast: [0.0, 0.0, 0.0],
            },
            generation: 3,
        };
        assert_eq!(
            ev.to_json(),
            json!({
                "hasThumbnail": true,
                "primaryColor": { "x": 1.0, "y": 0.5, "z": 0.0 },
                "secondaryColor": { "x": 0.0, "y": 0.0, "z": 1.0 },
                "tertiaryColor": { "x": 0.0, "y": 1.0, "z": 0.0 },
                "textColor": { "x": 0.0, "y": 0.0, "z": 0.0 },
                "highContrastColor": { "x": 0.0, "y": 0.0, "z": 0.0 },
            })
        );
        assert_eq!(ev.handler_name(), "mediaThumbnailChanged");
    }

    #[test]
    fn coalesce_keeps_latest_per_kind_in_order() {
        let t = |p| MediaEvent::Timeline {
            position: p,
            duration: 10.0,
        };
        let play = |s| MediaEvent::Playback { state: s };
        let out = coalesce(vec![
            MediaEvent::Status { enabled: true },
            t(1.0),
            play(PlaybackState::Playing),
            t(2.0),
            play(PlaybackState::Paused),
            t(3.0),
        ]);
        assert_eq!(
            out,
            vec![
                MediaEvent::Status { enabled: true },
                play(PlaybackState::Paused),
                t(3.0),
            ]
        );
    }
}
