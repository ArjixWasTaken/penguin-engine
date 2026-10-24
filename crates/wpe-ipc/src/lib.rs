//! Control protocol of `penguind`.
//!
//! The daemon listens on a Unix stream socket ([`socket_path`]). Clients
//! send one JSON [`Request`] per line and get one JSON [`Response`] per line
//! back, in order:
//!
//! ```text
//! > {"cmd":"set","output":"DP-1","wallpaper":"3566329932"}
//! < {"ok":true}
//! > {"cmd":"fps","fps":1000000}
//! < {"ok":false,"error":"..."}
//! ```
//!
//! After a [`Request::Subscribe`] the connection carries [`Event`] lines
//! instead, until it closes:
//!
//! ```text
//! > {"cmd":"subscribe"}
//! < {"ok":true}
//! < {"event":"status","status":{...}}
//! < {"event":"loading","output":"DP-1","wallpaper":"/.../3566329932"}
//! ```
//!
//! Version 2 of the protocol only added to version 1: requests, fields and
//! events a version 1 client never sees unless it asks.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod rules;

pub use rules::{Battery, Condition, Limits, Playback, Rule, Signals, WindowInfo, WindowState};

/// The protocol version, in [`VersionInfo::protocol`].
pub const PROTOCOL: u32 = 2;

/// User property values by name, as in `project.json` (`true`, `0.5`,
/// `"1 0 0"`, ...).
pub type Values = serde_json::Map<String, Value>;

/// File name of the control socket in `$XDG_RUNTIME_DIR`.
pub const SOCKET_NAME: &str = "penguin-engine.sock";

/// `$XDG_RUNTIME_DIR/penguin-engine.sock`.
pub fn socket_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty())?;
    Some(PathBuf::from(dir).join(SOCKET_NAME))
}

/// How a wallpaper fills an output of another aspect ratio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fill {
    /// Crop, but fit a wallpaper whose orientation differs from the
    /// output's (portrait on landscape), as Wallpaper Engine does.
    #[default]
    Auto,
    /// Cover the output, cutting off what doesn't fit.
    Crop,
    /// Show all of it, with bars.
    Fit,
    /// Distort it to the output's aspect ratio.
    Stretch,
}

/// The layer-shell layer the wallpaper's surfaces are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    #[default]
    Background,
    Bottom,
}

macro_rules! names {
    ($ty:ty { $($variant:ident = $name:literal),* $(,)? }) => {
        impl $ty {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $name),* }
            }
        }
        impl FromStr for $ty {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, String> {
                match s.to_ascii_lowercase().as_str() {
                    $($name => Ok(Self::$variant),)*
                    _ => Err(format!(
                        "unknown value {s:?}, expected one of: {}",
                        [$($name),*].join(", ")
                    )),
                }
            }
        }
        impl std::fmt::Display for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

names!(Fill {
    Auto = "auto",
    Crop = "crop",
    Fit = "fit",
    Stretch = "stretch"
});
names!(Layer {
    Background = "background",
    Bottom = "bottom"
});

/// The daemon's global settings (also the format of its config file).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Frame rate cap; 0 follows the display.
    pub fps: u32,
    /// Play sound and react to system audio (on the first output only).
    pub audio: bool,
    /// Sound volume in percent, 0..=100.
    pub volume: u32,
    /// Default fill mode of the outputs.
    pub fill: Fill,
    pub layer: Layer,
    /// Let pointer input through to what is underneath.
    pub passthrough: bool,
    /// Particles, in percent of what wallpapers ask for. (Protocol 2.)
    pub particles: u32,
    /// What holds wallpapers back, and when. (Protocol 2.)
    pub rules: Vec<Rule>,
}

impl Settings {
    /// These settings with `patch` applied.
    pub fn patched(&self, patch: &SettingsPatch) -> Self {
        Self {
            fps: patch.fps.unwrap_or(self.fps),
            audio: patch.audio.unwrap_or(self.audio),
            volume: patch.volume.unwrap_or(self.volume),
            fill: patch.fill.unwrap_or(self.fill),
            layer: patch.layer.unwrap_or(self.layer),
            passthrough: patch.passthrough.unwrap_or(self.passthrough),
            particles: patch.particles.unwrap_or(self.particles),
            rules: patch.rules.clone().unwrap_or_else(|| self.rules.clone()),
        }
    }
}

/// Some of the [`Settings`], to change them in one request.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SettingsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<u32>,
    /// The global fill; outputs' own fill modes are cleared, as with
    /// [`Request::Fill`] without an output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<Fill>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<Layer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passthrough: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub particles: Option<u32>,
    /// All the rules, replacing the old ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<Rule>>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fps: 0,
            audio: false,
            volume: 100,
            fill: Fill::Auto,
            layer: Layer::Background,
            passthrough: false,
            particles: 100,
            rules: Rule::defaults(),
        }
    }
}

/// A request to the daemon. `output` is an output (connector) name such as
/// `DP-1`; without one a request applies to every output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Request {
    /// The outputs and settings: [`Response::status`].
    Status,
    /// Show a wallpaper: a path (absolute, as the daemon sees it) or the id
    /// of an installed workshop wallpaper.
    ///
    /// `properties` are user property values to load it with (the rest
    /// keep their defaults), saved as the output's overrides.
    Set {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        wallpaper: String,
        #[serde(default, skip_serializing_if = "Values::is_empty")]
        properties: Values,
    },
    /// The wallpapers' user properties: [`Response::properties`].
    Properties {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    /// Change a user property; `value` as in `project.json` (`true`, `0.5`,
    /// `"1 0 0"`, ...).
    Property {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        name: String,
        value: Value,
    },
    Pause {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    Resume {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    Audio {
        enabled: bool,
    },
    /// Volume in percent, 0..=100.
    Volume {
        volume: u32,
    },
    /// The volume of the wallpaper an output shows, in percent (0..=100) of
    /// the global volume; kept until another wallpaper is set there.
    /// (Protocol 2.)
    WallpaperVolume {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        volume: u32,
    },
    /// Frame rate cap; 0 follows the display.
    Fps {
        fps: u32,
    },
    Fill {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        fill: Fill,
    },
    /// Re-read the config file (as SIGHUP does).
    ReloadSettings,
    Quit,
    /// The protocol version and what the daemon supports:
    /// [`Response::version`]. (Protocol 2.)
    Version,
    /// Answered with `ok`; then the connection carries [`Event`] lines,
    /// starting with the current status, until it closes. Further requests
    /// on it are ignored. (Protocol 2.)
    Subscribe,
    /// Change several user properties at once (one rebuild per output).
    /// Names a wallpaper doesn't have are skipped; without `output`, each
    /// output takes the values its wallpaper has. (Protocol 2.)
    SetProperties {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        values: Values,
    },
    /// Back to the wallpaper's own values (a preset item's preset), the
    /// saved overrides cleared. (Protocol 2.)
    ResetProperties {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    /// Change several settings at once, saved as the single-setting
    /// requests are. (Protocol 2.)
    Settings {
        #[serde(flatten)]
        patch: SettingsPatch,
    },
    /// What the rules are checked against: [`Response::signals`].
    /// (Protocol 2.)
    Signals,
}

/// The daemon's answer to one request. `ok` tells whether it succeeded;
/// `error` says why not. The other fields answer `status` and `properties`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<Vec<OutputProperties>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<VersionInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signals: Option<Signals>,
}

/// The answer to [`Request::Version`].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VersionInfo {
    /// [`PROTOCOL`] of the daemon.
    pub protocol: u32,
    /// The daemon's version.
    pub daemon: String,
    /// Optional features built in: `web` (web wallpapers).
    #[serde(default)]
    pub features: Vec<String>,
}

impl Response {
    pub fn ok() -> Self {
        Self {
            ok: true,
            ..Default::default()
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(message.into()),
            ..Default::default()
        }
    }

    /// `Ok` as is, an error message as [`Self::error`].
    pub fn from_result(result: Result<Self, String>) -> Self {
        result.unwrap_or_else(Self::error)
    }

    /// The error as a `Result`.
    pub fn into_result(self) -> Result<Self, String> {
        if self.ok {
            Ok(self)
        } else {
            Err(self.error.unwrap_or_else(|| "request failed".to_owned()))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Status {
    pub settings: Settings,
    pub outputs: Vec<OutputStatus>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OutputStatus {
    pub name: String,
    /// Size in logical pixels.
    pub width: u32,
    pub height: u32,
    /// The scale buffers are rendered at.
    pub scale: f32,
    /// None when the output shows no wallpaper.
    pub wallpaper: Option<WallpaperInfo>,
    pub paused: bool,
    /// Frames per second over the last few seconds.
    pub fps: f32,
    pub fill: Fill,
    /// The wallpaper being loaded for it (a `set`), if any. (Protocol 2.)
    #[serde(default)]
    pub loading: Option<String>,
    /// What the rules holding make of its wallpaper. With
    /// [`Playback::Stop`] the wallpaper is unloaded (`wallpaper` still
    /// tells which). (Protocol 2.)
    #[serde(default, skip_serializing_if = "Limits::is_none")]
    pub limits: Limits,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WallpaperInfo {
    pub path: String,
    pub workshop_id: Option<String>,
    pub title: String,
    /// `project.json` `type`: `scene` or `video`.
    #[serde(rename = "type")]
    pub kind: String,
    /// For a preset item (a workshop item that is another wallpaper with
    /// its own property values), the wallpaper actually playing; `path`,
    /// `workshop_id` and `title` are the preset item's. (Protocol 2.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency: Option<String>,
    /// It plays sound (sound layers, a video's sound track, or a web page,
    /// which may). (Protocol 2.)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sound: bool,
    /// Its own volume in percent, on top of the global one. (Protocol 2.)
    #[serde(default = "full_volume", skip_serializing_if = "is_full_volume")]
    pub volume: u32,
}

fn full_volume() -> u32 {
    100
}

fn is_full_volume(v: &u32) -> bool {
    *v == 100
}

/// One output's wallpaper's user properties.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OutputProperties {
    pub output: String,
    pub wallpaper: String,
    pub properties: Vec<PropertyInfo>,
}

/// A user property (`project.json` `general.properties`), for a settings form.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PropertyInfo {
    pub name: String,
    /// Widget kind: `bool`, `slider`, `combo`, `color`, `textinput`, `text`, ...
    #[serde(rename = "type")]
    pub kind: String,
    /// Label (often a localization key or HTML).
    pub text: Option<String>,
    /// Current value.
    pub value: Value,
    /// Value declared in `project.json`.
    pub default: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<PropertyChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<f64>,
    /// When the settings panel shows it (a JavaScript-like expression).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

/// One entry of a `combo` property.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PropertyChoice {
    pub label: Option<String>,
    pub value: Value,
}

/// What a subscribed connection ([`Request::Subscribe`]) gets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum Event {
    /// The status changed (once per change, however many things changed
    /// together); also the first event after subscribing.
    Status { status: Status },
    /// A wallpaper started loading for `output`.
    Loading { output: String, wallpaper: String },
    /// Loading `wallpaper` for `output` failed (or another output of the
    /// same `set` failed); the output shows what it showed before.
    LoadFailed {
        output: String,
        wallpaper: String,
        error: String,
    },
    /// The property values of `output`'s `wallpaper` now, all of them: after
    /// a property change and after a wallpaper loaded.
    Properties {
        output: String,
        wallpaper: String,
        values: Values,
    },
}

impl Event {
    /// The `event` names this version knows; others (from a newer daemon)
    /// are skipped by [`Client::subscribe`].
    pub const NAMES: &[&str] = &["status", "loading", "load-failed", "properties"];
}

/// `value` as one protocol line (JSON and a newline).
pub fn encode_line<T: Serialize>(value: &T) -> String {
    let mut line = serde_json::to_string(value).expect("protocol types serialize");
    line.push('\n');
    line
}

/// A blocking connection to the daemon.
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Client {
    /// Connect to the daemon's socket at [`socket_path`].
    pub fn connect_default() -> io::Result<Self> {
        let path = socket_path()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))?;
        Self::connect(&path)
    }

    pub fn connect(path: &Path) -> io::Result<Self> {
        let stream = UnixStream::connect(path)?;
        Ok(Self {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
        })
    }

    /// Give up on a response after `timeout` (loading a wallpaper can take a
    /// while; the default is to wait forever).
    pub fn set_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.writer.set_read_timeout(timeout)
    }

    /// Send `request` and wait for its response.
    pub fn request(&mut self, request: &Request) -> io::Result<Response> {
        self.writer.write_all(encode_line(request).as_bytes())?;
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the daemon closed the connection",
            ));
        }
        serde_json::from_str(&line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Subscribe to the daemon's events: the iterator yields them as they
    /// come and ends when the daemon closes the connection. Events newer
    /// than this crate are skipped. A read timeout ([`Self::set_timeout`])
    /// still applies; the default waits forever.
    pub fn subscribe(mut self) -> io::Result<impl Iterator<Item = io::Result<Event>>> {
        self.request(&Request::Subscribe)?
            .into_result()
            .map_err(io::Error::other)?;
        let mut reader = self.reader;
        Ok(std::iter::from_fn(move || {
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => return None,
                    Ok(_) => {}
                    Err(e) => return Some(Err(e)),
                }
                if line.trim().is_empty() {
                    continue;
                }
                match parse_event(&line) {
                    Ok(Some(event)) => return Some(Ok(event)),
                    Ok(None) => continue,
                    Err(e) => return Some(Err(e)),
                }
            }
        }))
    }
}

/// An event line; none for an event this version doesn't know.
fn parse_event(line: &str) -> io::Result<Option<Event>> {
    let invalid = |e| io::Error::new(io::ErrorKind::InvalidData, e);
    match serde_json::from_str::<Event>(line) {
        Ok(event) => Ok(Some(event)),
        Err(e) => {
            let value: Value = serde_json::from_str(line).map_err(invalid)?;
            match value.get("event").and_then(Value::as_str) {
                Some(name) if !Event::NAMES.contains(&name) => Ok(None),
                _ => Err(invalid(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round_trip(request: Request, wire: Value) {
        assert_eq!(serde_json::to_value(&request).unwrap(), wire);
        let back: Request = serde_json::from_value(wire).unwrap();
        assert_eq!(back, request);
    }

    #[test]
    fn wallpaper_sound_and_volume() {
        // Older daemons send neither: no sound, full volume.
        let info: WallpaperInfo = serde_json::from_value(
            json!({"path": "/w", "workshop_id": null, "title": "W", "type": "scene"}),
        )
        .unwrap();
        assert!(!info.sound);
        assert_eq!(info.volume, 100);
        let info = WallpaperInfo {
            sound: true,
            volume: 30,
            ..info
        };
        let wire = serde_json::to_value(&info).unwrap();
        assert_eq!(
            (&wire["sound"], &wire["volume"]),
            (&json!(true), &json!(30))
        );
        assert_eq!(serde_json::from_value::<WallpaperInfo>(wire).unwrap(), info);
    }

    #[test]
    fn requests_on_the_wire() {
        round_trip(Request::Status, json!({"cmd": "status"}));
        round_trip(
            Request::WallpaperVolume {
                output: Some("DP-1".into()),
                volume: 40,
            },
            json!({"cmd": "wallpaper-volume", "output": "DP-1", "volume": 40}),
        );
        round_trip(
            Request::Set {
                output: Some("DP-1".into()),
                wallpaper: "3566329932".into(),
                properties: Values::new(),
            },
            json!({"cmd": "set", "output": "DP-1", "wallpaper": "3566329932"}),
        );
        round_trip(
            Request::Set {
                output: None,
                wallpaper: "/w".into(),
                properties: Values::new(),
            },
            json!({"cmd": "set", "wallpaper": "/w"}),
        );
        round_trip(
            Request::Property {
                output: None,
                name: "speed".into(),
                value: json!(0.5),
            },
            json!({"cmd": "property", "name": "speed", "value": 0.5}),
        );
        round_trip(Request::Pause { output: None }, json!({"cmd": "pause"}));
        round_trip(
            Request::Audio { enabled: false },
            json!({"cmd": "audio", "enabled": false}),
        );
        round_trip(
            Request::Fill {
                output: Some("HDMI-A-1".into()),
                fill: Fill::Fit,
            },
            json!({"cmd": "fill", "output": "HDMI-A-1", "fill": "fit"}),
        );
        round_trip(Request::ReloadSettings, json!({"cmd": "reload-settings"}));
        round_trip(Request::Quit, json!({"cmd": "quit"}));
    }

    fn values(v: Value) -> Values {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn v2_requests_on_the_wire() {
        round_trip(Request::Version, json!({"cmd": "version"}));
        round_trip(Request::Subscribe, json!({"cmd": "subscribe"}));
        round_trip(
            Request::Set {
                output: None,
                wallpaper: "1".into(),
                properties: values(json!({"speed": 2, "rain": false})),
            },
            json!({"cmd": "set", "wallpaper": "1", "properties": {"speed": 2, "rain": false}}),
        );
        round_trip(
            Request::SetProperties {
                output: Some("DP-1".into()),
                values: values(json!({"speed": 0.5, "color": "1 0 0"})),
            },
            json!({"cmd": "set-properties", "output": "DP-1",
                   "values": {"speed": 0.5, "color": "1 0 0"}}),
        );
        round_trip(
            Request::ResetProperties { output: None },
            json!({"cmd": "reset-properties"}),
        );
        round_trip(
            Request::Settings {
                patch: SettingsPatch {
                    fps: Some(30),
                    fill: Some(Fill::Fit),
                    passthrough: Some(true),
                    ..Default::default()
                },
            },
            json!({"cmd": "settings", "fps": 30, "fill": "fit", "passthrough": true}),
        );
        round_trip(
            Request::Settings {
                patch: SettingsPatch::default(),
            },
            json!({"cmd": "settings"}),
        );
        // Settings a newer client knows are ignored.
        let newer: Request = serde_json::from_value(
            json!({"cmd": "settings", "volume": 5, "pause_on_fullscreen": true}),
        )
        .unwrap();
        assert_eq!(
            newer,
            Request::Settings {
                patch: SettingsPatch {
                    volume: Some(5),
                    ..Default::default()
                }
            }
        );
        assert!(
            serde_json::from_value::<Request>(json!({"cmd": "settings", "fill": "zoom"})).is_err()
        );
        assert!(serde_json::from_value::<Request>(json!({"cmd": "set-properties"})).is_err());
    }

    #[test]
    fn settings_patches() {
        let patch = SettingsPatch {
            audio: Some(true),
            layer: Some(Layer::Bottom),
            ..Default::default()
        };
        assert_eq!(
            Settings::default().patched(&patch),
            Settings {
                audio: true,
                layer: Layer::Bottom,
                ..Default::default()
            }
        );
        assert_eq!(
            Settings::default().patched(&SettingsPatch::default()),
            Settings::default()
        );
    }

    #[test]
    fn v2_responses_on_the_wire() {
        let version = Response {
            version: Some(VersionInfo {
                protocol: PROTOCOL,
                daemon: "0.1.0".into(),
                features: vec!["web".into()],
            }),
            ..Response::ok()
        };
        let wire = serde_json::to_value(&version).unwrap();
        assert_eq!(
            wire,
            json!({"ok": true, "version": {"protocol": 2, "daemon": "0.1.0", "features": ["web"]}})
        );
        assert_eq!(serde_json::from_value::<Response>(wire).unwrap(), version);

        // A version 1 status (no `loading`, no `dependency`) still parses.
        let v1 = json!({"name": "DP-1", "width": 1, "height": 1, "scale": 1.0,
                        "wallpaper": {"path": "/w", "workshop_id": null, "title": "W", "type": "scene"},
                        "paused": false, "fps": 0.0, "fill": "crop"});
        let output: OutputStatus = serde_json::from_value(v1).unwrap();
        assert_eq!(output.loading, None);
        assert_eq!(output.wallpaper.unwrap().dependency, None);

        let output = OutputStatus {
            name: "DP-1".into(),
            loading: Some("/w/2".into()),
            wallpaper: Some(WallpaperInfo {
                path: "/w/preset".into(),
                dependency: Some("/w/base".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let wire = serde_json::to_value(&output).unwrap();
        assert_eq!(wire["loading"], json!("/w/2"));
        assert_eq!(wire["wallpaper"]["dependency"], json!("/w/base"));
        assert_eq!(
            serde_json::from_value::<OutputStatus>(wire).unwrap(),
            output
        );
    }

    #[test]
    fn events_on_the_wire() {
        let cases = [
            (
                Event::Status {
                    status: Status::default(),
                },
                json!({"event": "status", "status": {"settings": Settings::default(), "outputs": []}}),
            ),
            (
                Event::Loading {
                    output: "DP-1".into(),
                    wallpaper: "/w/1".into(),
                },
                json!({"event": "loading", "output": "DP-1", "wallpaper": "/w/1"}),
            ),
            (
                Event::LoadFailed {
                    output: "DP-1".into(),
                    wallpaper: "/w/1".into(),
                    error: "boom".into(),
                },
                json!({"event": "load-failed", "output": "DP-1", "wallpaper": "/w/1", "error": "boom"}),
            ),
            (
                Event::Properties {
                    output: "DP-1".into(),
                    wallpaper: "/w/1".into(),
                    values: values(json!({"speed": 1})),
                },
                json!({"event": "properties", "output": "DP-1", "wallpaper": "/w/1",
                       "values": {"speed": 1}}),
            ),
        ];
        for (event, wire) in cases {
            assert_eq!(serde_json::to_value(&event).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<Event>(wire.clone()).unwrap(),
                event
            );
            let name = wire["event"].as_str().unwrap();
            assert!(Event::NAMES.contains(&name), "{name}");
        }
        assert!(
            parse_event(r#"{"event":"playlist","index":3}"#)
                .unwrap()
                .is_none()
        );
        assert!(parse_event(r#"{"event":"loading"}"#).is_err());
        assert!(parse_event("nope").is_err());
    }

    #[test]
    fn bad_requests_are_rejected() {
        for bad in [
            r#"{"cmd":"nope"}"#,
            r#"{"cmd":"set"}"#,
            r#"{"cmd":"fill","fill":"zoom"}"#,
            r#"{"wallpaper":"x"}"#,
            "not json",
        ] {
            assert!(serde_json::from_str::<Request>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn responses_on_the_wire() {
        assert_eq!(encode_line(&Response::ok()), "{\"ok\":true}\n");
        assert_eq!(
            serde_json::to_value(Response::error("boom")).unwrap(),
            json!({"ok": false, "error": "boom"})
        );
        let status = Response {
            status: Some(Status {
                settings: Settings::default(),
                outputs: vec![OutputStatus {
                    name: "DP-1".into(),
                    width: 2560,
                    height: 1440,
                    scale: 1.0,
                    wallpaper: Some(WallpaperInfo {
                        path: "/w/1".into(),
                        workshop_id: Some("1".into()),
                        title: "One".into(),
                        kind: "scene".into(),
                        dependency: None,
                        sound: false,
                        volume: 100,
                    }),
                    paused: false,
                    fps: 60.0,
                    fill: Fill::Crop,
                    loading: None,
                    limits: Limits::default(),
                }],
            }),
            properties: Some(vec![OutputProperties {
                output: "DP-1".into(),
                wallpaper: "/w/1".into(),
                properties: vec![PropertyInfo {
                    name: "speed".into(),
                    kind: "slider".into(),
                    value: json!(1),
                    default: json!(1),
                    min: Some(0.0),
                    ..Default::default()
                }],
            }]),
            ..Response::ok()
        };
        let wire = serde_json::to_string(&status).unwrap();
        assert!(wire.contains(r#""type":"scene""#), "{wire}");
        assert!(wire.contains(r#""fill":"crop""#), "{wire}");
        let back: Response = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, status);
        assert!(back.into_result().is_ok());
        assert_eq!(
            Response::error("x").into_result().unwrap_err(),
            "x".to_owned()
        );
    }

    #[test]
    fn settings_defaults_and_names() {
        let s: Settings = serde_json::from_str(r#"{"fps":30}"#).unwrap();
        assert_eq!(
            s,
            Settings {
                fps: 30,
                ..Settings::default()
            }
        );
        assert_eq!("Fit".parse::<Fill>().unwrap(), Fill::Fit);
        assert_eq!("bottom".parse::<Layer>().unwrap(), Layer::Bottom);
        assert!("zoom".parse::<Fill>().is_err());
        assert_eq!(Fill::Stretch.to_string(), "stretch");
    }

    #[test]
    fn client_talks_lines() {
        let dir = std::env::temp_dir().join(format!("wpe-ipc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.sock");
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Request = serde_json::from_str(&line).unwrap();
            assert_eq!(request, Request::Status);
            writer
                .write_all(encode_line(&Response::error("nope")).as_bytes())
                .unwrap();
        });
        let mut client = Client::connect(&path).unwrap();
        let response = client.request(&Request::Status).unwrap();
        assert_eq!(response, Response::error("nope"));
        server.join().unwrap();

        // Subscribing: `ok`, then events until the connection closes.
        let listener = std::os::unix::net::UnixListener::bind(dir.join("events.sock")).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(
                serde_json::from_str::<Request>(&line).unwrap(),
                Request::Subscribe
            );
            writer
                .write_all(encode_line(&Response::ok()).as_bytes())
                .unwrap();
            let loading = Event::Loading {
                output: "DP-1".into(),
                wallpaper: "/w".into(),
            };
            writer.write_all(encode_line(&loading).as_bytes()).unwrap();
            writer
                .write_all(b"{\"event\":\"from-the-future\"}\n\n")
                .unwrap();
            writer.write_all(encode_line(&loading).as_bytes()).unwrap();
        });
        let events: Vec<Event> = Client::connect(&dir.join("events.sock"))
            .unwrap()
            .subscribe()
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], Event::Loading { output, .. } if output == "DP-1"));
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
