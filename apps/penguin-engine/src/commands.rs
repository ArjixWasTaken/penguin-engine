//! What the page can ask for (`invoke(name, args)`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::RwLock;
use wpe_ipc::{Client, PropertyInfo, Request, Response};
use wpe_library::folders::{self, Folder};
use wpe_library::locale::{self, Locale};
use wpe_library::presets::{self, Preset};
use wpe_library::{Item, Sources, steam};

/// The scanned library and Wallpaper Engine's UI strings, loaded on first
/// use.
#[derive(Default)]
pub struct Library(
    RwLock<Option<Arc<Loaded>>>,
    tokio::sync::OnceCell<Arc<Resolutions>>,
);

/// Wallpaper sizes by item id, for those whose size is known.
type Resolutions = HashMap<String, (u32, u32)>;

pub struct Loaded {
    pub items: Vec<Item>,
    by_id: HashMap<String, usize>,
    locale: Locale,
}

impl Loaded {
    pub fn get(&self, id: &str) -> Option<&Item> {
        self.by_id.get(id).map(|&i| &self.items[i])
    }

    fn item(&self, id: &str) -> Result<&Item, String> {
        self.get(id).ok_or_else(|| format!("no wallpaper {id}"))
    }
}

impl Library {
    /// The library, scanned now if it hasn't been (or `refresh`).
    pub async fn loaded(&self, refresh: bool) -> Arc<Loaded> {
        if !refresh && let Some(loaded) = &*self.0.read().await {
            return loaded.clone();
        }
        let mut slot = self.0.write().await;
        if !refresh && let Some(loaded) = &*slot {
            return loaded.clone();
        }
        let loaded = tauri::async_runtime::spawn_blocking(scan)
            .await
            .expect("scanning the library");
        *slot = Some(loaded.clone());
        loaded
    }
}

fn scan() -> Arc<Loaded> {
    let sources = Sources::discover(&[]);
    let library = match wpe_library::default_cache_path() {
        Some(cache) => wpe_library::Library::load_or_scan(&sources, &cache),
        None => wpe_library::Library::scan(&sources),
    };
    let lang = locale::env_lang();
    let locale = match steam::we_dir() {
        Some(dir) => Locale::load(&dir, &lang),
        None => Locale::from_strings(&lang, HashMap::new()),
    };
    let by_id = library
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| (item.id.clone(), i))
        .collect();
    Arc::new(Loaded {
        items: library.items,
        by_id,
        locale,
    })
}

/// Every wallpaper. Previews are at `thumb://localhost/<id>` (a small
/// still) and `preview://localhost/<id>` (the original, animated).
#[tauri::command]
pub async fn library(
    state: State<'_, Library>,
    refresh: Option<bool>,
) -> Result<Vec<Item>, String> {
    Ok(state.loaded(refresh.unwrap_or(false)).await.items.clone())
}

/// Loading a heavy wallpaper can take a while.
const TIMEOUT: Duration = Duration::from_secs(60);

/// Send one request to the daemon (the protocol's JSON, e.g.
/// `{cmd: "set", wallpaper: "/path"}`) and get its response. Fails when
/// the daemon isn't running or answers with an error.
#[tauri::command]
pub async fn daemon(request: Request) -> Result<Response, String> {
    request_daemon(request).await
}

pub async fn request_daemon(request: Request) -> Result<Response, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut client = Client::connect_default().map_err(|e| connect_error(&e))?;
        client
            .set_timeout(Some(TIMEOUT))
            .map_err(|e| e.to_string())?;
        client
            .request(&request)
            .map_err(|e| e.to_string())?
            .into_result()
    })
    .await
    .map_err(|e| e.to_string())?
}

pub fn connect_error(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            "penguind isn't running".to_owned()
        }
        _ => format!("connecting to penguind: {e}"),
    }
}

/// A user property with its label in the user's language.
#[derive(Serialize)]
pub struct Property {
    #[serde(flatten)]
    info: PropertyInfo,
    /// Plain text (no HTML), translated.
    label: String,
}

#[derive(Serialize)]
pub struct Properties {
    /// The output the values are live on, or null for the project's own.
    output: Option<String>,
    properties: Vec<Property>,
}

/// A wallpaper's properties: live from `output` when given (it must show
/// that wallpaper), else the project's (with a preset item's values).
#[tauri::command]
pub async fn properties(
    state: State<'_, Library>,
    id: String,
    output: Option<String>,
) -> Result<Properties, String> {
    let loaded = state.loaded(false).await;
    let dir = loaded.item(&id)?.path.clone();
    let infos = match &output {
        Some(name) => request_daemon(Request::Properties {
            output: Some(name.clone()),
        })
        .await?
        .properties
        .and_then(|list| list.into_iter().find(|p| &p.output == name))
        .map(|p| p.properties)
        .ok_or("no properties in the reply")?,
        None => {
            let dir = dir.clone();
            tauri::async_runtime::spawn_blocking(move || wpe_library::properties(&dir))
                .await
                .map_err(|e| e.to_string())?
        }
    };
    let properties = labelled(&loaded.locale, &dir, infos);
    Ok(Properties { output, properties })
}

fn labelled(locale: &Locale, dir: &std::path::Path, infos: Vec<PropertyInfo>) -> Vec<Property> {
    let localization = wpe_library::localization(dir);
    let label = |text: &str| locale::plain(&locale.label(text, localization.as_ref()));
    infos
        .into_iter()
        .map(|mut info| {
            for option in &mut info.options {
                option.label = option.label.as_deref().map(label);
            }
            let text = info.text.as_deref().unwrap_or(&info.name);
            let label = Some(label(text))
                .filter(|l| !l.trim().is_empty())
                .unwrap_or_else(|| info.name.clone());
            Property { info, label }
        })
        .collect()
}

/// Which properties show (`condition` by property name) with these values.
#[tauri::command]
pub fn conditions(
    conditions: HashMap<String, String>,
    values: HashMap<String, Value>,
) -> HashMap<String, bool> {
    conditions
        .into_iter()
        .map(|(name, expr)| {
            let visible = wpe_library::condition::visible(&expr, &values);
            (name, visible)
        })
        .collect()
}

/// Every wallpaper's size in pixels, by id (those that have one). Reads
/// each wallpaper's files once (a second or two), then answers at once.
#[tauri::command]
pub async fn resolutions(state: State<'_, Library>) -> Result<Arc<Resolutions>, String> {
    let loaded = state.loaded(false).await;
    let sizes = state
        .1
        .get_or_init(|| async move {
            tauri::async_runtime::spawn_blocking(move || {
                use rayon::prelude::*;
                Arc::new(
                    loaded
                        .items
                        .par_iter()
                        .filter_map(|item| Some((item.id.clone(), wpe_library::resolution(item)?)))
                        .collect(),
                )
            })
            .await
            .expect("measuring wallpapers")
        })
        .await;
    Ok(sizes.clone())
}

/// A wallpaper's size in pixels, if known (reads its files).
#[tauri::command]
pub async fn resolution(
    state: State<'_, Library>,
    id: String,
) -> Result<Option<(u32, u32)>, String> {
    let loaded = state.loaded(false).await;
    let item = loaded.item(&id)?.clone();
    tauri::async_runtime::spawn_blocking(move || wpe_library::resolution(&item))
        .await
        .map_err(|e| e.to_string())
}

/// The user's folders: Wallpaper Engine's own, as its `config.json` has
/// them now; without Wallpaper Engine, the ones saved here.
#[tauri::command]
pub fn folders() -> Result<Vec<Folder>, String> {
    let we_config = steam::we_dir()
        .map(|dir| dir.join("config.json"))
        .filter(|c| c.is_file());
    if let Some(config) = we_config {
        return folders::import_we_folders(&config).map_err(|e| e.to_string());
    }
    match folders::default_path().filter(|p| p.exists()) {
        Some(path) => folders::load(&path).map_err(|e| e.to_string()),
        None => Ok(Vec::new()),
    }
}

#[tauri::command]
pub fn save_folders(folders: Vec<Folder>) -> Result<(), String> {
    let path = folders::default_path().ok_or("no config directory")?;
    folders::save(&path, &folders).map_err(|e| e.to_string())
}

/// The daemon's systemd user units (README, "Running at login").
const UNITS: [&str; 2] = ["penguin-engine.socket", "penguin-engine.service"];

/// Whether the daemon starts with the session (its units are enabled).
#[tauri::command]
pub async fn autostart() -> Result<bool, String> {
    let out = systemctl(&["is-enabled", UNITS[0]]).await?;
    Ok(out.trim() == "enabled")
}

#[tauri::command]
pub async fn set_autostart(enabled: bool) -> Result<(), String> {
    let verb = if enabled { "enable" } else { "disable" };
    systemctl(&[verb, UNITS[0], UNITS[1]]).await.map(|_| ())
}

/// `systemctl --user ...`: its output, or why it failed.
async fn systemctl(args: &[&str]) -> Result<String, String> {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let fut = tauri::async_runtime::spawn_blocking(move || {
        let out = std::process::Command::new("systemctl")
            .arg("--user")
            .args(&args)
            .output()
            .map_err(|e| format!("systemctl: {e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        // `is-enabled` exits 1 for a disabled unit: that is an answer.
        if out.status.success() || args[0] == "is-enabled" && !stdout.trim().is_empty() {
            Ok(stdout)
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        }
    });
    match tokio::time::timeout(Duration::from_secs(5), fut).await {
        Ok(res) => res.map_err(|e| e.to_string())?,
        Err(_) => Err("systemctl timed out after 5s".to_owned()),
    }
}

fn presets_dir() -> Result<PathBuf, String> {
    presets::default_dir().ok_or_else(|| "no config directory".to_owned())
}

/// The saved presets of wallpaper `id`, by name.
#[tauri::command]
pub fn presets(id: String) -> Result<Vec<Preset>, String> {
    Ok(presets::list(&presets_dir()?, &id))
}

/// Save a preset (replacing one of the same name).
#[tauri::command]
pub fn save_preset(preset: Preset) -> Result<(), String> {
    presets::save(&presets_dir()?, &preset)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_preset(id: String, name: String) -> Result<(), String> {
    presets::delete(&presets_dir()?, &id, &name).map_err(|e| e.to_string())
}

fn ui_state_path() -> Result<PathBuf, String> {
    wpe_library::config_dir()
        .map(|d| d.join("app-state.json"))
        .ok_or_else(|| "no config directory".to_owned())
}

/// What the page saved with `save_ui_state` (null at first).
#[tauri::command]
pub fn ui_state() -> Value {
    ui_state_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}

/// Keep `state` (any JSON: the search, filters, selection...) for next time.
#[tauri::command]
pub fn save_ui_state(state: Value) -> Result<(), String> {
    let path = ui_state_path()?;
    let json = serde_json::to_vec_pretty(&state).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(path.parent().expect("a config file has a directory"))
        .and_then(|()| {
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, json)?;
            std::fs::rename(&tmp, &path)
        })
        .map_err(|e| e.to_string())
}

/// Open a workshop item's Steam page in the browser.
#[tauri::command]
pub fn open_workshop(app: AppHandle, id: String) -> Result<(), String> {
    if !steam::is_workshop_id(&id) {
        return Err(format!("{id} is not a workshop id"));
    }
    let url = format!("https://steamcommunity.com/sharedfiles/filedetails/?id={id}");
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

/// Open a wallpaper's directory in the file manager.
#[tauri::command]
pub async fn open_folder(app: AppHandle, id: String) -> Result<(), String> {
    let loaded = app.state::<Library>().loaded(false).await;
    let path = loaded.item(&id)?.path.to_string_lossy().into_owned();
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| e.to_string())
}
