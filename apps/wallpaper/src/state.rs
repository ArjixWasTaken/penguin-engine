//! What the daemon remembers between runs (`state.json`: the wallpaper of
//! each output, property overrides, settings changed over the socket) and
//! the user's settings file (`config.toml`).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wpe_ipc::{Fill, Settings};

const APP_DIR: &str = "wallpaper-engine-rs";

/// `$XDG_STATE_HOME/wallpaper-engine-rs/state.json` (`~/.local/state/...`).
pub fn state_path() -> Option<PathBuf> {
    Some(dirs::state_dir()?.join(APP_DIR).join("state.json"))
}

/// `$XDG_CONFIG_HOME/wallpaper-engine-rs/config.toml` (`~/.config/...`).
pub fn config_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join(APP_DIR).join("config.toml"))
}

/// The settings in the config file at `path`; the defaults without one.
pub fn load_config(path: &Path) -> Result<Settings> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("{}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// One output's wallpaper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedOutput {
    pub wallpaper: PathBuf,
    /// User properties changed from the wallpaper's defaults.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, Value>,
    /// Fill mode of this output, instead of the global one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<Fill>,
}

impl SavedOutput {
    pub fn new(wallpaper: PathBuf) -> Self {
        Self {
            wallpaper,
            properties: BTreeMap::new(),
            fill: None,
        }
    }

    /// The property overrides as `Wallpaper::load_with_properties` takes them.
    pub fn property_list(&self) -> Vec<(String, Value)> {
        self.properties
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// Settings changed over the socket; they win over the config file
    /// until it is reloaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<Settings>,
    /// By output name.
    #[serde(default)]
    pub outputs: BTreeMap<String, SavedOutput>,
    /// The wallpaper set last, for outputs without their own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<SavedOutput>,
}

impl State {
    /// The state saved at `path`; empty without one (or when it doesn't
    /// parse, which is logged).
    pub fn load(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("{}: {e}", path.display());
                }
                return Self::default();
            }
        };
        serde_json::from_str(&text)
            .inspect_err(|e| tracing::warn!("{}: {e}; starting afresh", path.display()))
            .unwrap_or_default()
    }

    /// Write the state to `path` atomically (a temporary file renamed over it).
    pub fn save(&self, path: &Path) -> Result<()> {
        let dir = path.parent().context("state path without a directory")?;
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        let write = || -> std::io::Result<()> {
            let mut file = std::fs::File::create(&tmp)?;
            serde_json::to_writer_pretty(&mut file, self)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            std::fs::rename(&tmp, path)
        };
        write().inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })?;
        Ok(())
    }

    /// What output `name` should show: its own wallpaper, or else the one
    /// set last.
    pub fn for_output(&self, name: &str) -> Option<&SavedOutput> {
        self.outputs.get(name).or(self.last.as_ref())
    }

    /// Output `name` now shows `wallpaper`, with its default properties.
    pub fn set_wallpaper(&mut self, name: &str, wallpaper: &Path) {
        let fill = self.outputs.get(name).and_then(|o| o.fill);
        let entry = SavedOutput {
            fill,
            ..SavedOutput::new(wallpaper.to_owned())
        };
        self.last = Some(SavedOutput {
            fill: None,
            ..entry.clone()
        });
        self.outputs.insert(name.to_owned(), entry);
    }

    /// Show `wallpaper` everywhere: on every output, now and new.
    pub fn set_everywhere(&mut self, wallpaper: SavedOutput) {
        self.outputs.clear();
        self.last = Some(wallpaper);
    }

    /// Remember a user property change on output `name` (which shows `wallpaper`).
    pub fn set_property(&mut self, name: &str, wallpaper: &Path, property: &str, value: Value) {
        let entry = self
            .outputs
            .entry(name.to_owned())
            .or_insert_with(|| SavedOutput::new(wallpaper.to_owned()));
        entry.properties.insert(property.to_owned(), value);
        // New outputs showing the same wallpaper get the same look.
        if let Some(last) = &mut self.last
            && last.wallpaper == wallpaper
        {
            last.properties = entry.properties.clone();
        }
    }

    pub fn set_fill(&mut self, name: &str, wallpaper: &Path, fill: Option<Fill>) {
        self.outputs
            .entry(name.to_owned())
            .or_insert_with(|| SavedOutput::new(wallpaper.to_owned()))
            .fill = fill;
    }
}

/// Settings given on the command line, which win over everything else.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Overrides {
    pub fps: Option<u32>,
    pub audio: Option<bool>,
    pub volume: Option<u32>,
    pub fill: Option<Fill>,
    pub layer: Option<wpe_ipc::Layer>,
    pub passthrough: Option<bool>,
}

impl Overrides {
    pub fn apply(&self, mut settings: Settings) -> Settings {
        settings.fps = self.fps.unwrap_or(settings.fps);
        settings.audio = self.audio.unwrap_or(settings.audio);
        settings.volume = self.volume.unwrap_or(settings.volume);
        settings.fill = self.fill.unwrap_or(settings.fill);
        settings.layer = self.layer.unwrap_or(settings.layer);
        settings.passthrough = self.passthrough.unwrap_or(settings.passthrough);
        settings
    }
}

/// The settings in effect at startup: the config file, then what was
/// changed over the socket last run, then the command line.
pub fn startup_settings(config: &Settings, state: &State, cli: &Overrides) -> Settings {
    cli.apply(state.settings.clone().unwrap_or_else(|| config.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wpe_ipc::Layer;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wpe-state-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn saves_and_restores() {
        let dir = temp_dir("save");
        let path = dir.join("nested/state.json");
        assert_eq!(State::load(&path), State::default());

        let mut state = State::default();
        state.set_wallpaper("DP-1", Path::new("/w/1"));
        state.set_property("DP-1", Path::new("/w/1"), "speed", json!(0.5));
        state.set_fill("DP-1", Path::new("/w/1"), Some(Fill::Fit));
        state.set_wallpaper("HDMI-A-1", Path::new("/w/2"));
        state.settings = Some(Settings {
            fps: 30,
            ..Default::default()
        });
        state.save(&path).unwrap();
        // Nothing but the state file is left behind.
        let files: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(files.len(), 1);

        let back = State::load(&path);
        assert_eq!(back, state);
        let dp = back.for_output("DP-1").unwrap();
        assert_eq!(dp.wallpaper, Path::new("/w/1"));
        assert_eq!(dp.properties["speed"], json!(0.5));
        assert_eq!(dp.fill, Some(Fill::Fit));
        // A new output gets the wallpaper set last, without per-output fill.
        let new = back.for_output("DP-3").unwrap();
        assert_eq!(new.wallpaper, Path::new("/w/2"));
        assert_eq!(new.fill, None);

        // Setting a new wallpaper keeps the output's fill, drops properties.
        state.set_wallpaper("DP-1", Path::new("/w/3"));
        let dp = state.for_output("DP-1").unwrap();
        assert!(dp.properties.is_empty());
        assert_eq!(dp.fill, Some(Fill::Fit));

        std::fs::write(&path, "{broken").unwrap();
        assert_eq!(State::load(&path), State::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn everywhere_replaces_every_output() {
        let mut state = State::default();
        state.set_wallpaper("DP-1", Path::new("/w/1"));
        state.set_everywhere(SavedOutput::new("/w/9".into()));
        assert_eq!(
            state.for_output("DP-1").unwrap().wallpaper,
            Path::new("/w/9")
        );
        // Property changes follow onto new outputs showing that wallpaper.
        state.set_property("DP-1", Path::new("/w/9"), "x", json!(true));
        assert_eq!(
            state.for_output("DP-2").unwrap().properties["x"],
            json!(true)
        );
    }

    #[test]
    fn config_files() {
        let dir = temp_dir("config");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        assert_eq!(load_config(&path).unwrap(), Settings::default());
        std::fs::write(&path, "fps = 30\nfill = \"fit\"\nlayer = \"bottom\"\n").unwrap();
        let config = load_config(&path).unwrap();
        assert_eq!(
            config,
            Settings {
                fps: 30,
                fill: Fill::Fit,
                layer: Layer::Bottom,
                ..Default::default()
            }
        );
        std::fs::write(&path, "fps = \"fast\"").unwrap();
        assert!(load_config(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn settings_precedence() {
        let config = Settings {
            fps: 30,
            volume: 50,
            ..Default::default()
        };
        let cli = Overrides {
            volume: Some(10),
            ..Default::default()
        };
        let mut state = State::default();
        assert_eq!(
            startup_settings(&config, &state, &cli),
            Settings {
                fps: 30,
                volume: 10,
                ..Default::default()
            }
        );
        state.settings = Some(Settings {
            fps: 60,
            ..Default::default()
        });
        assert_eq!(
            startup_settings(&config, &state, &cli),
            Settings {
                fps: 60,
                volume: 10,
                ..Default::default()
            }
        );
    }
}
