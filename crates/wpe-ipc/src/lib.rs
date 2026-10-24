//! Control protocol of the `wallpaper` daemon.
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

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// File name of the control socket in `$XDG_RUNTIME_DIR`.
pub const SOCKET_NAME: &str = "wallpaper-engine-rs.sock";

/// `$XDG_RUNTIME_DIR/wallpaper-engine-rs.sock`.
pub fn socket_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty())?;
    Some(PathBuf::from(dir).join(SOCKET_NAME))
}

/// How a wallpaper fills an output of another aspect ratio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fill {
    /// Cover the output, cutting off what doesn't fit.
    #[default]
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

names!(Fill { Crop = "crop", Fit = "fit", Stretch = "stretch" });
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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fps: 0,
            audio: false,
            volume: 100,
            fill: Fill::Crop,
            layer: Layer::Background,
            passthrough: false,
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
    Set {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        wallpaper: String,
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
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WallpaperInfo {
    pub path: String,
    pub workshop_id: Option<String>,
    pub title: String,
    /// `project.json` `type`: `scene` or `video`.
    #[serde(rename = "type")]
    pub kind: String,
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
    fn requests_on_the_wire() {
        round_trip(Request::Status, json!({"cmd": "status"}));
        round_trip(
            Request::Set {
                output: Some("DP-1".into()),
                wallpaper: "3566329932".into(),
            },
            json!({"cmd": "set", "output": "DP-1", "wallpaper": "3566329932"}),
        );
        round_trip(
            Request::Set {
                output: None,
                wallpaper: "/w".into(),
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
                    }),
                    paused: false,
                    fps: 60.0,
                    fill: Fill::Crop,
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
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
