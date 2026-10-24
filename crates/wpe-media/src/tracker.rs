//! D-Bus-free model of the MPRIS players: which one to follow, and which
//! script events a state change produces.

use std::collections::BTreeMap;

use crate::event::{MediaEvent, MediaProperties, PlaybackState};

/// The parts of `org.mpris.MediaPlayer2.Player.Metadata` we use.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrackInfo {
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    pub album_artists: Vec<String>,
    pub genres: Vec<String>,
    /// `mpris:artUrl`, empty when absent.
    pub art_url: String,
    /// `mpris:length` in microseconds.
    pub length_us: Option<i64>,
    /// `xesam:url`, used to guess the content type.
    pub url: String,
}

impl TrackInfo {
    /// Players such as idle browser tabs publish empty metadata; those are
    /// not worth following.
    pub fn is_meaningful(&self) -> bool {
        !self.title.is_empty() || !self.artists.is_empty() || !self.art_url.is_empty()
    }

    /// MPRIS has no content type; guess "video" from the media URL's
    /// extension, otherwise "audio".
    pub fn content_type(&self) -> &'static str {
        const VIDEO: &[&str] = &[
            "mp4", "mkv", "webm", "avi", "mov", "m4v", "wmv", "flv", "mpg", "mpeg", "ts", "ogv",
        ];
        let path = self.url.split(['?', '#']).next().unwrap_or("");
        let ext = path.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        if !ext.contains('/') && VIDEO.iter().any(|v| ext.eq_ignore_ascii_case(v)) {
            "video"
        } else {
            "audio"
        }
    }

    pub fn to_properties(&self) -> MediaProperties {
        MediaProperties {
            title: self.title.clone(),
            artist: self.artists.join(", "),
            sub_title: String::new(),
            album_title: self.album.clone(),
            album_artist: self.album_artists.join(", "),
            genres: self.genres.join(", "),
            content_type: self.content_type().to_string(),
        }
    }
}

/// One MPRIS player as last seen on the bus.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlayerState {
    /// Unique bus name (`:1.42`) currently owning the well-known name.
    pub owner: String,
    pub playback: PlaybackState,
    pub track: TrackInfo,
    /// Microseconds.
    pub position_us: i64,
    /// Activity stamp: bumped when the player appears or starts playing.
    pub last_active: u64,
}

/// A property update for one player; `None` fields are unchanged.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlayerUpdate {
    pub playback: Option<PlaybackState>,
    pub track: Option<TrackInfo>,
    pub position_us: Option<i64>,
}

/// What the tracker wants done about album art after a [`Tracker::sync`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtChange {
    /// Load this `mpris:artUrl` (in the background).
    Load(String),
    /// No art any more.
    Clear,
}

/// Result of [`Tracker::sync`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncOutput {
    /// Status/playback/properties/timeline events. Thumbnail events come from
    /// the art loader once `art` is resolved.
    pub events: Vec<MediaEvent>,
    pub art: Option<ArtChange>,
}

/// What scripts were last told.
#[derive(Debug, Clone, PartialEq)]
struct View {
    enabled: bool,
    playback: PlaybackState,
    properties: MediaProperties,
    art_url: String,
    position_us: i64,
    length_us: i64,
}

impl View {
    fn none() -> Self {
        Self {
            enabled: false,
            playback: PlaybackState::Stopped,
            properties: MediaProperties::default(),
            art_url: String::new(),
            position_us: 0,
            length_us: 0,
        }
    }
}

/// Tracks all players, picks the one to follow, and diffs what scripts see.
#[derive(Debug, Default)]
pub struct Tracker {
    players: BTreeMap<String, PlayerState>,
    selected: Option<String>,
    clock: u64,
    shown: Option<View>,
}

impl Tracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a player or changes its owner. `active` stamps it as recently
    /// active (use `false` for players found during the start-up scan).
    pub fn upsert(&mut self, name: &str, owner: &str, active: bool) {
        let stamp = if active { self.tick() } else { 0 };
        let p = self.players.entry(name.to_string()).or_default();
        p.owner = owner.to_string();
        if active {
            p.last_active = stamp;
        }
    }

    pub fn remove(&mut self, name: &str) {
        self.players.remove(name);
    }

    pub fn contains(&self, name: &str) -> bool {
        self.players.contains_key(name)
    }

    /// Well-known names owned by a unique name (signals carry the latter).
    pub fn names_for_owner(&self, owner: &str) -> Vec<String> {
        self.players
            .iter()
            .filter(|(_, p)| p.owner == owner)
            .map(|(n, _)| n.clone())
            .collect()
    }

    pub fn update(&mut self, name: &str, update: PlayerUpdate) {
        let stamp = self.clock + 1;
        let Some(p) = self.players.get_mut(name) else {
            return;
        };
        let mut bumped = false;
        if let Some(pb) = update.playback {
            if pb == PlaybackState::Playing && p.playback != PlaybackState::Playing {
                p.last_active = stamp;
                bumped = true;
            }
            p.playback = pb;
        }
        if let Some(t) = update.track {
            if t != p.track {
                // New track: the old position no longer applies.
                p.position_us = 0;
            }
            p.track = t;
        }
        if let Some(pos) = update.position_us {
            p.position_us = pos.max(0);
        }
        if bumped {
            self.clock = stamp;
        }
    }

    pub fn player(&self, name: &str) -> Option<&PlayerState> {
        self.players.get(name)
    }

    /// The player currently followed (after the last [`Tracker::sync`]).
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Whether the followed player is playing (position needs polling).
    pub fn selected_is_playing(&self) -> bool {
        self.selected
            .as_ref()
            .and_then(|n| self.players.get(n))
            .is_some_and(|p| p.playback == PlaybackState::Playing)
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// Re-picks the followed player and returns the events needed to bring
    /// scripts up to date. The first call reports the full state.
    pub fn sync(&mut self) -> SyncOutput {
        self.selected = select_player(&self.players, self.selected.as_deref());
        let view = match self.selected.as_ref().and_then(|n| self.players.get(n)) {
            Some(p) => View {
                enabled: true,
                playback: p.playback,
                properties: p.track.to_properties(),
                art_url: p.track.art_url.clone(),
                position_us: p.position_us,
                length_us: p.track.length_us.unwrap_or(0).max(0),
            },
            None => View::none(),
        };

        let first = self.shown.is_none();
        let old = self.shown.take().unwrap_or_else(View::none);
        let mut out = SyncOutput::default();
        if first || old.enabled != view.enabled {
            out.events.push(MediaEvent::Status {
                enabled: view.enabled,
            });
        }
        if first || old.playback != view.playback {
            out.events.push(MediaEvent::Playback {
                state: view.playback,
            });
        }
        if first || old.properties != view.properties {
            out.events
                .push(MediaEvent::Properties(view.properties.clone()));
        }
        if first || old.art_url != view.art_url {
            out.art = Some(if view.art_url.is_empty() {
                ArtChange::Clear
            } else {
                ArtChange::Load(view.art_url.clone())
            });
        }
        if first || old.position_us != view.position_us || old.length_us != view.length_us {
            out.events.push(MediaEvent::Timeline {
                position: view.position_us as f64 / 1e6,
                duration: view.length_us as f64 / 1e6,
            });
        }
        self.shown = Some(view);
        out
    }
}

/// Picks the player to follow:
/// 1. Players without meaningful metadata are ignored.
/// 2. If any player is playing: keep `current` if it is one of them, else the
///    playing player that most recently became active.
/// 3. Otherwise keep `current` if it is still eligible, else the most
///    recently active player.
///
/// Ties on the activity stamp go to the lexicographically smaller name, so
/// the result is deterministic.
pub fn select_player(
    players: &BTreeMap<String, PlayerState>,
    current: Option<&str>,
) -> Option<String> {
    let eligible = || players.iter().filter(|(_, p)| p.track.is_meaningful());
    let best = |it: &mut dyn Iterator<Item = (&String, &PlayerState)>| {
        // BTreeMap iterates names ascending; `>` keeps the first on ties.
        let mut best: Option<(&String, &PlayerState)> = None;
        for (n, p) in it {
            if best.is_none_or(|(_, b)| p.last_active > b.last_active) {
                best = Some((n, p));
            }
        }
        best.map(|(n, _)| n.clone())
    };

    let is_current = |n: &String| current == Some(n.as_str());
    let mut playing = eligible().filter(|(_, p)| p.playback == PlaybackState::Playing);
    if let Some((n, _)) =
        eligible().find(|(n, p)| is_current(n) && p.playback == PlaybackState::Playing)
    {
        return Some(n.clone());
    }
    if let Some(n) = best(&mut playing) {
        return Some(n);
    }
    if let Some((n, _)) = eligible().find(|(n, _)| is_current(n)) {
        return Some(n.clone());
    }
    best(&mut eligible())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::MediaEventKind;

    fn track(title: &str) -> TrackInfo {
        TrackInfo {
            title: title.into(),
            artists: vec!["A".into(), "B".into()],
            length_us: Some(180_000_000),
            ..Default::default()
        }
    }

    fn player(pb: PlaybackState, title: &str, active: u64) -> PlayerState {
        PlayerState {
            owner: String::new(),
            playback: pb,
            track: track(title),
            position_us: 0,
            last_active: active,
        }
    }

    fn map(ps: &[(&str, PlayerState)]) -> BTreeMap<String, PlayerState> {
        ps.iter().map(|(n, p)| (n.to_string(), p.clone())).collect()
    }

    use PlaybackState::*;

    #[test]
    fn prefers_playing_then_recent() {
        let m = map(&[
            ("a", player(Paused, "x", 5)),
            ("b", player(Playing, "y", 1)),
            ("c", player(Playing, "z", 3)),
        ]);
        assert_eq!(select_player(&m, None).as_deref(), Some("c"));
        // A playing current player is kept even if another is more recent.
        assert_eq!(select_player(&m, Some("b")).as_deref(), Some("b"));
        // A paused current loses to a playing one.
        assert_eq!(select_player(&m, Some("a")).as_deref(), Some("c"));

        let m = map(&[
            ("a", player(Paused, "x", 2)),
            ("b", player(Stopped, "y", 7)),
        ]);
        assert_eq!(select_player(&m, None).as_deref(), Some("b"));
        assert_eq!(select_player(&m, Some("a")).as_deref(), Some("a"));
    }

    #[test]
    fn ignores_players_without_metadata_and_ties_are_stable() {
        let mut empty = player(Playing, "", 9);
        empty.track = TrackInfo::default();
        let m = map(&[
            ("z", empty),
            ("b", player(Paused, "y", 0)),
            ("a", player(Paused, "x", 0)),
        ]);
        assert_eq!(select_player(&m, None).as_deref(), Some("a"));
        assert_eq!(select_player(&m, Some("z")).as_deref(), Some("a"));
        assert_eq!(select_player(&BTreeMap::new(), None), None);
    }

    #[test]
    fn content_type_guess() {
        let mut t = track("x");
        assert_eq!(t.content_type(), "audio");
        t.url = "file:///v/clip.MKV".into();
        assert_eq!(t.content_type(), "video");
        t.url = "https://example.com/watch.mp4?x=1".into();
        assert_eq!(t.content_type(), "video");
        t.url = "https://example.mp4/song".into();
        assert_eq!(t.content_type(), "audio");
    }

    #[test]
    fn initial_sync_reports_everything_even_without_players() {
        let mut t = Tracker::new();
        let out = t.sync();
        let kinds: Vec<_> = out.events.iter().map(|e| e.kind()).collect();
        assert_eq!(
            kinds,
            vec![
                MediaEventKind::Status,
                MediaEventKind::Playback,
                MediaEventKind::Properties,
                MediaEventKind::Timeline
            ]
        );
        assert_eq!(out.events[0], MediaEvent::Status { enabled: false });
        assert_eq!(out.art, Some(ArtChange::Clear));
        // Nothing changed: nothing to report.
        assert_eq!(t.sync(), SyncOutput::default());
    }

    #[test]
    fn lifecycle_produces_diffs() {
        let mut t = Tracker::new();
        t.sync();

        t.upsert("org.mpris.MediaPlayer2.spotify", ":1.5", true);
        let mut tr = track("Song");
        tr.art_url = "https://art/1".into();
        t.update(
            "org.mpris.MediaPlayer2.spotify",
            PlayerUpdate {
                playback: Some(Playing),
                track: Some(tr.clone()),
                position_us: Some(1_500_000),
            },
        );
        let out = t.sync();
        assert_eq!(t.selected(), Some("org.mpris.MediaPlayer2.spotify"));
        assert!(t.selected_is_playing());
        assert_eq!(out.art, Some(ArtChange::Load("https://art/1".into())));
        assert_eq!(out.events[0], MediaEvent::Status { enabled: true });
        assert_eq!(out.events[1], MediaEvent::Playback { state: Playing });
        match &out.events[2] {
            MediaEvent::Properties(p) => {
                assert_eq!(p.title, "Song");
                assert_eq!(p.artist, "A, B");
            }
            e => panic!("{e:?}"),
        }
        assert_eq!(
            out.events[3],
            MediaEvent::Timeline {
                position: 1.5,
                duration: 180.0
            }
        );

        // Position only.
        t.update(
            "org.mpris.MediaPlayer2.spotify",
            PlayerUpdate {
                position_us: Some(2_500_000),
                ..Default::default()
            },
        );
        let out = t.sync();
        assert_eq!(out.art, None);
        assert_eq!(
            out.events,
            vec![MediaEvent::Timeline {
                position: 2.5,
                duration: 180.0
            }]
        );

        // Pause.
        t.update(
            "org.mpris.MediaPlayer2.spotify",
            PlayerUpdate {
                playback: Some(Paused),
                ..Default::default()
            },
        );
        assert_eq!(
            t.sync().events,
            vec![MediaEvent::Playback { state: Paused }]
        );
        assert!(!t.selected_is_playing());

        // A second player starts playing: we switch to it.
        t.upsert("org.mpris.MediaPlayer2.mpv", ":1.9", true);
        t.update(
            "org.mpris.MediaPlayer2.mpv",
            PlayerUpdate {
                playback: Some(Playing),
                track: Some(track("Video")),
                position_us: None,
            },
        );
        let out = t.sync();
        assert_eq!(t.selected(), Some("org.mpris.MediaPlayer2.mpv"));
        assert_eq!(out.art, Some(ArtChange::Clear));
        assert!(
            out.events
                .contains(&MediaEvent::Playback { state: Playing })
        );
        assert!(
            !out.events
                .iter()
                .any(|e| e.kind() == MediaEventKind::Status)
        );
        assert_eq!(
            t.names_for_owner(":1.9"),
            vec!["org.mpris.MediaPlayer2.mpv"]
        );

        // Both vanish: integration disabled, everything reset.
        t.remove("org.mpris.MediaPlayer2.mpv");
        t.sync();
        assert_eq!(t.selected(), Some("org.mpris.MediaPlayer2.spotify"));
        t.remove("org.mpris.MediaPlayer2.spotify");
        let out = t.sync();
        assert_eq!(t.selected(), None);
        assert_eq!(out.events[0], MediaEvent::Status { enabled: false });
        assert_eq!(out.events[1], MediaEvent::Playback { state: Stopped });
        assert_eq!(out.art, Some(ArtChange::Clear));
    }

    #[test]
    fn new_track_resets_position() {
        let mut t = Tracker::new();
        t.upsert("p", ":1", true);
        t.update(
            "p",
            PlayerUpdate {
                playback: Some(Playing),
                track: Some(track("one")),
                position_us: Some(90_000_000),
            },
        );
        t.update(
            "p",
            PlayerUpdate {
                track: Some(track("two")),
                ..Default::default()
            },
        );
        assert_eq!(t.player("p").unwrap().position_us, 0);
    }
}
