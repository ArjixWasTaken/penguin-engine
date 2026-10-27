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

const APP_DIR: &str = wpe_library::APP_DIR;

/// `$XDG_STATE_HOME/penguin-engine/state.json` (`~/.local/state/...`).
pub fn state_path() -> Option<PathBuf> {
    Some(dirs::state_dir()?.join(APP_DIR).join("state.json"))
}

/// `$XDG_CONFIG_HOME/penguin-engine/config.toml` (`~/.config/...`).
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
    /// The wallpaper's own volume in percent of the global one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<u32>,
}

impl SavedOutput {
    pub fn new(wallpaper: PathBuf) -> Self {
        Self {
            wallpaper,
            properties: BTreeMap::new(),
            fill: None,
            volume: None,
        }
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
        let mut temp = tempfile::Builder::new()
            .prefix(".state-")
            .suffix(".tmp")
            .tempfile_in(dir)
            .with_context(|| format!("creating tempfile in {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = temp
                .as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600));
        }
        serde_json::to_writer_pretty(&mut temp, self)?;
        temp.write_all(b"\n")?;
        temp.as_file().sync_all()?;
        temp.persist(path)
            .map_err(|e| e.error)
            .with_context(|| format!("persisting {}", path.display()))?;
        Ok(())
    }

    /// What output `name` should show: its own wallpaper, or else the one
    /// set last.
    pub fn for_output(&self, name: &str) -> Option<&SavedOutput> {
        self.outputs.get(name).or(self.last.as_ref())
    }

    /// Output `name` now shows `wallpaper`, with `properties` changed from
    /// its defaults.
    pub fn set_wallpaper(
        &mut self,
        name: &str,
        wallpaper: &Path,
        properties: BTreeMap<String, Value>,
    ) {
        let fill = self.outputs.get(name).and_then(|o| o.fill);
        let entry = SavedOutput {
            fill,
            properties,
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
        let entry = self.entry(name, wallpaper);
        entry.properties.insert(property.to_owned(), value);
        let properties = entry.properties.clone();
        // New outputs showing the same wallpaper get the same look.
        if let Some(last) = &mut self.last
            && last.wallpaper == wallpaper
        {
            last.properties = properties;
        }
    }

    /// Forget the property changes on output `name` (which shows `wallpaper`).
    pub fn clear_properties(&mut self, name: &str, wallpaper: &Path) {
        if let Some(entry) = self.outputs.get_mut(name) {
            entry.properties.clear();
        }
        if let Some(last) = &mut self.last
            && last.wallpaper == wallpaper
        {
            last.properties.clear();
        }
    }

    /// Output `name` (showing `wallpaper`) plays it at `volume` percent.
    pub fn set_volume(&mut self, name: &str, wallpaper: &Path, volume: u32) {
        let volume = (volume != 100).then_some(volume);
        self.entry(name, wallpaper).volume = volume;
        // New outputs showing the same wallpaper sound the same.
        if let Some(last) = &mut self.last
            && last.wallpaper == wallpaper
        {
            last.volume = volume;
        }
    }

    /// The volume output `name` plays `wallpaper` at, in percent.
    pub fn volume(&self, name: &str, wallpaper: &Path) -> u32 {
        self.for_output(name)
            .filter(|saved| saved.wallpaper == wallpaper)
            .and_then(|saved| saved.volume)
            .unwrap_or(100)
    }

    pub fn set_fill(&mut self, name: &str, wallpaper: &Path, fill: Option<Fill>) {
        self.entry(name, wallpaper).fill = fill;
    }

    /// Output `name`'s own entry, made from what it shows (`wallpaper`,
    /// with the last one's properties when it is that one) if it has none.
    fn entry(&mut self, name: &str, wallpaper: &Path) -> &mut SavedOutput {
        let last = self
            .last
            .as_ref()
            .filter(|last| last.wallpaper == wallpaper);
        let inherited = last.map(|last| last.properties.clone()).unwrap_or_default();
        let volume = last.and_then(|last| last.volume);
        self.outputs
            .entry(name.to_owned())
            .or_insert_with(|| SavedOutput {
                properties: inherited,
                volume,
                ..SavedOutput::new(wallpaper.to_owned())
            })
    }

    /// The property changes output `name` (showing `wallpaper`) has.
    pub fn overrides(&self, name: &str, wallpaper: &Path) -> BTreeMap<String, Value> {
        self.for_output(name)
            .filter(|saved| saved.wallpaper == wallpaper)
            .map(|saved| saved.properties.clone())
            .unwrap_or_default()
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
        state.set_wallpaper("DP-1", Path::new("/w/1"), BTreeMap::new());
        state.set_property("DP-1", Path::new("/w/1"), "speed", json!(0.5));
        state.set_fill("DP-1", Path::new("/w/1"), Some(Fill::Fit));
        state.set_wallpaper(
            "HDMI-A-1",
            Path::new("/w/2"),
            BTreeMap::from([("rain".to_owned(), json!(false))]),
        );
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
        assert_eq!(new.properties["rain"], json!(false));

        // Setting a new wallpaper keeps the output's fill, drops properties.
        state.set_wallpaper("DP-1", Path::new("/w/3"), BTreeMap::new());
        let dp = state.for_output("DP-1").unwrap();
        assert!(dp.properties.is_empty());
        assert_eq!(dp.fill, Some(Fill::Fit));

        // Resetting forgets the overrides, here and for new outputs.
        state.clear_properties("HDMI-A-1", Path::new("/w/2"));
        assert!(state.outputs["HDMI-A-1"].properties.is_empty());
        assert!(state.last.as_ref().unwrap().properties.is_empty());

        std::fs::write(&path, "{broken").unwrap();
        assert_eq!(State::load(&path), State::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wallpaper_volume_stays_with_the_wallpaper() {
        let mut state = State::default();
        let (one, two) = (Path::new("/w/1"), Path::new("/w/2"));
        state.set_wallpaper("DP-1", one, BTreeMap::new());
        assert_eq!(state.volume("DP-1", one), 100);
        state.set_volume("DP-1", one, 40);
        assert_eq!(state.volume("DP-1", one), 40);
        // A new output showing the same wallpaper sounds the same.
        assert_eq!(state.volume("HDMI-A-1", one), 40);
        // Another wallpaper starts at full volume.
        state.set_wallpaper("DP-1", two, BTreeMap::new());
        assert_eq!(state.volume("DP-1", two), 100);
        state.set_volume("DP-1", two, 100);
        assert_eq!(state.outputs["DP-1"].volume, None);
    }

    #[test]
    fn everywhere_replaces_every_output() {
        let mut state = State::default();
        state.set_wallpaper("DP-1", Path::new("/w/1"), BTreeMap::new());
        state.set_everywhere(SavedOutput::new("/w/9".into()));
        assert_eq!(
            state.for_output("DP-1").unwrap().wallpaper,
            Path::new("/w/9")
        );
        // Property changes follow onto new outputs showing that wallpaper,
        // and keep the ones it was set with.
        state
            .last
            .as_mut()
            .unwrap()
            .properties
            .insert("y".into(), json!(1));
        state.set_property("DP-1", Path::new("/w/9"), "x", json!(true));
        let dp2 = state.for_output("DP-2").unwrap();
        assert_eq!(dp2.properties["x"], json!(true));
        assert_eq!(dp2.properties["y"], json!(1));
        assert_eq!(state.overrides("DP-1", Path::new("/w/9")).len(), 2);
        assert!(state.overrides("DP-1", Path::new("/w/8")).is_empty());
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
