//! Conversions from MPRIS D-Bus property values into tracker types.

use std::collections::HashMap;

use zbus::zvariant::{OwnedValue, Value};

use crate::event::PlaybackState;
use crate::tracker::{PlayerUpdate, TrackInfo};

pub const BUS_PREFIX: &str = "org.mpris.MediaPlayer2.";
pub const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
pub const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

fn unwrap<'a, 'b>(v: &'b Value<'a>) -> &'b Value<'a> {
    match v {
        Value::Value(inner) => unwrap(inner),
        v => v,
    }
}

fn as_string(v: &Value<'_>) -> Option<String> {
    match unwrap(v) {
        Value::Str(s) => Some(s.to_string()),
        Value::ObjectPath(p) => Some(p.to_string()),
        _ => None,
    }
}

/// `as` per spec, but some players send a single string.
fn as_strings(v: &Value<'_>) -> Vec<String> {
    match unwrap(v) {
        Value::Array(a) => a
            .iter()
            .filter_map(as_string)
            .filter(|s| !s.is_empty())
            .collect(),
        other => as_string(other)
            .filter(|s| !s.is_empty())
            .into_iter()
            .collect(),
    }
}

/// `x` per spec; players also send `t`, `i`, `u` or `d`.
fn as_i64(v: &Value<'_>) -> Option<i64> {
    match unwrap(v) {
        Value::I64(n) => Some(*n),
        Value::U64(n) => i64::try_from(*n).ok(),
        Value::I32(n) => Some(*n as i64),
        Value::U32(n) => Some(*n as i64),
        Value::F64(n) if n.is_finite() => Some(*n as i64),
        _ => None,
    }
}

/// Parses the `Metadata` property (`a{sv}`).
pub fn parse_metadata(v: &Value<'_>) -> TrackInfo {
    let mut t = TrackInfo::default();
    let Value::Dict(dict) = unwrap(v) else {
        return t;
    };
    for (k, val) in dict.iter() {
        let Some(key) = as_string(k) else { continue };
        match key.as_str() {
            "xesam:title" => t.title = as_string(val).unwrap_or_default(),
            "xesam:artist" => t.artists = as_strings(val),
            "xesam:album" => t.album = as_string(val).unwrap_or_default(),
            "xesam:albumArtist" => t.album_artists = as_strings(val),
            "xesam:genre" => t.genres = as_strings(val),
            "xesam:url" => t.url = as_string(val).unwrap_or_default(),
            "mpris:artUrl" => t.art_url = as_string(val).unwrap_or_default(),
            "mpris:length" => t.length_us = as_i64(val),
            _ => {}
        }
    }
    t
}

/// Turns a `GetAll`/`PropertiesChanged` property map of the Player
/// interface into an update.
pub fn parse_player_props(props: &HashMap<String, OwnedValue>) -> PlayerUpdate {
    PlayerUpdate {
        playback: props
            .get("PlaybackStatus")
            .and_then(|v| as_string(v))
            .map(|s| PlaybackState::from_mpris(&s)),
        track: props.get("Metadata").map(|v| parse_metadata(v)),
        position_us: props.get("Position").and_then(|v| as_i64(v)),
    }
}

/// Parses a position reply / `Seeked` value.
pub fn parse_position(v: &Value<'_>) -> Option<i64> {
    as_i64(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(v: Value<'_>) -> OwnedValue {
        v.try_to_owned().unwrap()
    }

    #[test]
    fn metadata_and_props() {
        let mut md: HashMap<&str, Value<'_>> = HashMap::new();
        md.insert("xesam:title", Value::from("Song"));
        md.insert("xesam:artist", Value::from(vec!["A", "B"]));
        md.insert("xesam:albumArtist", Value::from("Solo"));
        md.insert("xesam:album", Value::from("Album"));
        md.insert("xesam:genre", Value::from(vec!["Rock", ""]));
        md.insert(
            "mpris:length",
            Value::Value(Box::new(Value::from(123_000_000u64))),
        );
        md.insert("mpris:artUrl", Value::from("file:///a%20b.png"));
        md.insert(
            "mpris:trackid",
            Value::from(zbus::zvariant::ObjectPath::try_from("/t/1").unwrap()),
        );
        let mut props = HashMap::new();
        props.insert("Metadata".to_string(), owned(Value::from(md)));
        props.insert("PlaybackStatus".to_string(), owned(Value::from("Paused")));
        props.insert("Position".to_string(), owned(Value::from(5_000_000i64)));
        props.insert("Volume".to_string(), owned(Value::from(1.0f64)));

        let u = parse_player_props(&props);
        assert_eq!(u.playback, Some(PlaybackState::Paused));
        assert_eq!(u.position_us, Some(5_000_000));
        let t = u.track.unwrap();
        assert_eq!(t.title, "Song");
        assert_eq!(t.artists, vec!["A", "B"]);
        assert_eq!(t.album_artists, vec!["Solo"]);
        assert_eq!(t.album, "Album");
        assert_eq!(t.genres, vec!["Rock"]);
        assert_eq!(t.length_us, Some(123_000_000));
        assert_eq!(t.art_url, "file:///a%20b.png");
        assert!(t.is_meaningful());
    }

    #[test]
    fn partial_props_leave_fields_unchanged() {
        let mut props = HashMap::new();
        props.insert("PlaybackStatus".to_string(), owned(Value::from("Playing")));
        let u = parse_player_props(&props);
        assert_eq!(u.playback, Some(PlaybackState::Playing));
        assert_eq!(u.track, None);
        assert_eq!(u.position_us, None);
        assert_eq!(parse_metadata(&Value::from(5u32)), TrackInfo::default());
    }
}
