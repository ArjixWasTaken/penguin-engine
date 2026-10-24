//! Rules that hold wallpapers back while something else goes on: another
//! application fullscreen or playing sound, the laptop on battery, no input
//! for a while. A rule pauses, mutes or stops the wallpaper, or caps its
//! frame rate or particle count. (Protocol 2.)
//!
//! In the config file:
//!
//! ```toml
//! [[rules]]
//! when = { if = "window", state = "fullscreen" }
//! playback = "pause"
//!
//! [[rules]]
//! when = { if = "battery" }
//! fps = 30
//! particles = 50
//! ```

use serde::{Deserialize, Serialize};

/// What a wallpaper does while a rule holds, mildest first: of several
/// rules holding at once the strongest wins.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Playback {
    /// Keep playing.
    #[default]
    Run,
    /// Keep playing without sound.
    Mute,
    /// Freeze it (it stays on screen, using no CPU or GPU time).
    Pause,
    /// Unload it, freeing its memory: the desktop's own wallpaper shows.
    /// It loads again when the rule stops holding.
    Stop,
}

/// What another application's window is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowState {
    /// It has a window (on the current virtual desktop or not, minimized
    /// or not): the application runs.
    Open,
    /// It has the keyboard focus.
    Focused,
    /// Maximized or fullscreen.
    Maximized,
    Fullscreen,
}

/// Something about the session a rule waits for.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "if", rename_all = "kebab-case")]
pub enum Condition {
    /// A window of another application is in `state`, on the output of the
    /// wallpaper (unless the rule applies to [`Rule::all_outputs`]). The
    /// desktop, panels and this project's own app don't count. `app`
    /// narrows it to one application: its app id or process name (case
    /// does not matter).
    Window {
        state: WindowState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        app: Option<String>,
    },
    /// Another application plays sound; `app` as for [`Self::Window`] (or
    /// the name it gives its sound).
    Audio {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        app: Option<String>,
    },
    /// Running on battery; with `below`, only while the charge (in percent)
    /// is under it.
    Battery {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        below: Option<u32>,
    },
    /// No keyboard or pointer input for `seconds`.
    Idle { seconds: u32 },
}

/// When [`Rule::when`] holds, the wallpaper's playback and limits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rule {
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub enabled: bool,
    pub when: Condition,
    #[serde(default, skip_serializing_if = "is_run")]
    pub playback: Playback,
    /// Frame rate cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
    /// Particles, in percent of what the wallpaper asks for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub particles: Option<u32>,
    /// A window condition applies to every output, not only the one the
    /// window is on.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all_outputs: bool,
}

fn yes() -> bool {
    true
}

fn is_yes(v: &bool) -> bool {
    *v
}

fn is_run(p: &Playback) -> bool {
    *p == Playback::Run
}

impl Rule {
    /// A rule that sets `playback` when `when` holds.
    pub fn new(when: Condition, playback: Playback) -> Self {
        Self {
            enabled: true,
            when,
            playback,
            fps: None,
            particles: None,
            all_outputs: false,
        }
    }

    /// The rules of a new installation: pause behind a fullscreen window,
    /// as Wallpaper Engine does.
    pub fn defaults() -> Vec<Self> {
        vec![Self::new(
            Condition::Window {
                state: WindowState::Fullscreen,
                app: None,
            },
            Playback::Pause,
        )]
    }
}

/// What the rules holding for an output make of its wallpaper, in
/// [`crate::OutputStatus::limits`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Limits {
    #[serde(default, skip_serializing_if = "is_run")]
    pub playback: Playback,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
    /// In percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub particles: Option<u32>,
    /// Why, for people: one entry per rule holding ("firefox is
    /// fullscreen", "on battery").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub because: Vec<String>,
}

impl Limits {
    pub fn is_none(&self) -> bool {
        *self == Self::default()
    }

    /// Add what `rule` asks for (the strongest playback, the lowest caps).
    pub fn add(&mut self, rule: &Rule, why: String) {
        self.playback = self.playback.max(rule.playback);
        let lower = |a: Option<u32>, b: Option<u32>| match (a, b) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.fps = lower(self.fps, rule.fps);
        self.particles = lower(self.particles, rule.particles);
        if !self.because.contains(&why) {
            self.because.push(why);
        }
    }

    /// The same limits, reasons aside.
    pub fn same_effect(&self, other: &Self) -> bool {
        (self.playback, self.fps, self.particles) == (other.playback, other.fps, other.particles)
    }
}

/// What the daemon sees of the session, which the rules are checked
/// against: [`crate::Response::signals`]. (Protocol 2.)
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Signals {
    /// Other applications' windows; None when the compositor doesn't tell
    /// (window rules then never hold).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<Vec<WindowInfo>>,
    /// Applications playing sound now; None when the sound server can't
    /// be asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<Vec<String>>,
    /// None without a battery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery: Option<Battery>,
    /// The longest idle rule's time without input reached, in seconds (0
    /// while there is input); None when the compositor doesn't tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WindowInfo {
    /// App id (often the desktop file name), else empty.
    pub app: String,
    /// Process name, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    pub title: String,
    /// The output it is mostly on (its centre's); None when on no output,
    /// minimized or on another virtual desktop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub focused: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub maximized: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fullscreen: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub minimized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Battery {
    /// Not plugged in.
    pub discharging: bool,
    /// Charge in percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Config {
        rules: Vec<Rule>,
    }

    #[test]
    fn rules_in_toml() {
        let text = r#"
            [[rules]]
            when = { if = "window", state = "fullscreen" }
            playback = "pause"

            [[rules]]
            when = { if = "window", state = "focused", app = "steam" }
            playback = "stop"
            all_outputs = true

            [[rules]]
            when = { if = "battery", below = 20 }
            fps = 30
            particles = 50

            [[rules]]
            enabled = false
            when = { if = "idle", seconds = 300 }
            playback = "mute"
        "#;
        let config: Config = toml::from_str(text).unwrap();
        assert_eq!(config.rules[0], Rule::defaults()[0]);
        assert_eq!(
            config.rules[1].when,
            Condition::Window {
                state: WindowState::Focused,
                app: Some("steam".into())
            }
        );
        assert!(config.rules[1].all_outputs);
        assert_eq!(config.rules[2].fps, Some(30));
        assert_eq!(config.rules[2].playback, Playback::Run);
        assert!(!config.rules[3].enabled);
        let back: Config = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(back, config);
    }

    #[test]
    fn limits_take_the_strongest() {
        let mut limits = Limits::default();
        let mut slow = Rule::new(Condition::Battery { below: None }, Playback::Mute);
        slow.fps = Some(30);
        slow.particles = Some(50);
        limits.add(&slow, "on battery".into());
        let mut pause = Rule::new(Condition::Idle { seconds: 60 }, Playback::Pause);
        pause.fps = Some(60);
        limits.add(&pause, "idle".into());
        limits.add(&pause, "idle".into());
        assert_eq!(limits.playback, Playback::Pause);
        assert_eq!(limits.fps, Some(30));
        assert_eq!(limits.particles, Some(50));
        assert_eq!(limits.because, ["on battery", "idle"]);
        assert!(Playback::Stop > Playback::Pause && Playback::Mute > Playback::Run);
    }
}
