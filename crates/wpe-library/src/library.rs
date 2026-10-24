//! Scanning the wallpaper directories into [`Item`]s, and the scan cache.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wpe_assets::Project;

use crate::acf::{self, WorkshopItem};
use crate::{Item, Kind, Rating, Source, stable_hash, steam};

/// The directories wallpapers are looked for in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sources {
    /// Workshop content directories (one wallpaper per id-named subdirectory).
    pub workshop: Vec<PathBuf>,
    /// Wallpaper Engine's `projects/defaultprojects` and `projects/myprojects`.
    pub projects: Vec<PathBuf>,
    /// Directories the user added: each a wallpaper, or a folder of them.
    pub user: Vec<PathBuf>,
}

impl Sources {
    /// The Steam install's workshop and project directories, plus `user_dirs`.
    pub fn discover(user_dirs: &[PathBuf]) -> Self {
        let projects = steam::we_dir()
            .map(|we| {
                ["defaultprojects", "myprojects"]
                    .into_iter()
                    .map(|p| we.join("projects").join(p))
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default();
        Self {
            workshop: steam::workshop_dirs(),
            projects,
            user: user_dirs.to_vec(),
        }
    }
}

/// The installed wallpapers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Library {
    /// Sorted by title.
    pub items: Vec<Item>,
}

/// `$XDG_CACHE_HOME/penguin-engine/library.json`.
pub fn default_cache_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("penguin-engine/library.json"))
}

/// Bumped when [`Item`] or the scan changes, so old caches are rescanned.
const CACHE_VERSION: u32 = 1;

/// What a directory looked like when it was scanned: `project.json`'s and
/// the directory's modification times (nanoseconds) and `project.json`'s size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Stamp {
    project: u128,
    dir: u128,
    len: u64,
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    entries: Vec<CacheEntry>,
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    stamp: Stamp,
    item: Item,
}

/// A wallpaper directory found in the sources.
struct Candidate {
    dir: PathBuf,
    source: Source,
    stamp: Stamp,
}

fn nanos(t: std::io::Result<SystemTime>) -> u128 {
    t.ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos())
}

fn stamp(dir: &Path) -> Option<Stamp> {
    let project = std::fs::metadata(dir.join("project.json")).ok()?;
    if !project.is_file() {
        return None;
    }
    let dir_meta = std::fs::metadata(dir).ok()?;
    Some(Stamp {
        project: nanos(project.modified()),
        dir: nanos(dir_meta.modified()),
        len: project.len(),
    })
}

/// Every wallpaper directory in `sources`, workshop first; each directory once.
fn candidates(sources: &Sources) -> Vec<Candidate> {
    let mut dirs: Vec<(PathBuf, Source)> = Vec::new();
    let children = |root: &Path| -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(root) else {
            return Vec::new();
        };
        let mut children: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() || t.is_symlink()))
            .map(|e| e.path())
            .collect();
        children.sort();
        children
    };
    let roots = [
        (&sources.workshop, Source::Workshop),
        (&sources.projects, Source::Projects),
        (&sources.user, Source::User),
    ];
    for (roots, source) in roots {
        for root in roots {
            let root = root.canonicalize().unwrap_or_else(|_| root.clone());
            if source == Source::User && root.join("project.json").is_file() {
                dirs.push((root, source));
            } else {
                dirs.extend(children(&root).into_iter().map(|d| (d, source)));
            }
        }
    }
    let mut seen = HashSet::new();
    dirs.retain(|(d, _)| seen.insert(d.clone()));
    dirs.into_par_iter()
        .filter_map(|(dir, source)| {
            let stamp = stamp(&dir)?;
            Some(Candidate { dir, source, stamp })
        })
        .collect()
}

/// Steam's records for the workshop items of `sources`.
fn workshop_records(sources: &Sources) -> HashMap<String, WorkshopItem> {
    let mut records = HashMap::new();
    for dir in &sources.workshop {
        if let Some(acf) = steam::acf_for(dir) {
            records.extend(acf::load(&acf));
        }
    }
    records
}

fn kind_of(project: &Project, raw: &Value) -> Kind {
    if preset_dependency(raw).is_some() {
        return Kind::Preset;
    }
    if let Some(kind) = project.kind.as_deref() {
        match kind.to_ascii_lowercase().as_str() {
            "scene" => return Kind::Scene,
            "video" => return Kind::Video,
            "web" => return Kind::Web,
            "application" => return Kind::Application,
            "preset" => return Kind::Preset,
            _ => {}
        }
    }
    // No (known) type: guess from the file, as old stock projects need.
    let ext = project
        .file
        .as_deref()
        .and_then(|f| Path::new(f).extension())
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    match ext.as_deref() {
        Some("json" | "pkg") => Kind::Scene,
        Some("mp4" | "webm" | "mkv" | "avi" | "mov" | "m4v" | "wmv") => Kind::Video,
        Some("html" | "htm") => Kind::Web,
        Some("exe") => Kind::Application,
        _ => Kind::Unknown,
    }
}

fn rating_of(project: &Project) -> Rating {
    match project
        .contentrating
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("everyone") => Rating::Everyone,
        Some("questionable") => Rating::Questionable,
        Some("mature") => Rating::Mature,
        _ => Rating::Unknown,
    }
}

/// `name` in `dir`, matched case-insensitively, if it exists.
pub(crate) fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let name = name.trim_start_matches(['/', '\\']).replace('\\', "/");
    if name.is_empty() {
        return None;
    }
    let exact = dir.join(&name);
    if exact.is_file() {
        return Some(exact);
    }
    // Walk the components, matching each case-insensitively.
    let mut path = dir.to_owned();
    for part in name.split('/').filter(|p| !p.is_empty()) {
        let found = std::fs::read_dir(&path)
            .ok()?
            .flatten()
            .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(part))?;
        path.push(found.file_name());
    }
    path.is_file().then_some(path)
}

fn string_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The workshop id a preset item configures: its `dependency`, when it has
/// no `file` of its own (as the daemon decides).
pub(crate) fn preset_dependency(raw: &Value) -> Option<String> {
    let has_file = raw
        .get("file")
        .and_then(string_of)
        .is_some_and(|f| !f.trim().is_empty());
    if has_file {
        return None;
    }
    raw.get("dependency")
        .and_then(string_of)
        .map(|d| d.trim().to_owned())
        .filter(|d| !d.is_empty())
}

/// Bytes under `dir` and its modification time (Unix seconds).
fn walk_size(dir: &Path) -> u64 {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// The [`Item`] for the wallpaper in `dir`; `None` (logged) if its
/// `project.json` can't be read.
fn read_item(dir: &Path, source: Source, stamp: Stamp) -> Option<Item> {
    let path = dir.join("project.json");
    let text = std::fs::read(&path)
        .map_err(|e| tracing::debug!("{}: {e}", path.display()))
        .ok()?;
    let text = String::from_utf8_lossy(&text);
    let raw: Value = wpe_assets::parse_json(&text)
        .map_err(|e| tracing::debug!("{}: {e}", path.display()))
        .ok()?;
    let project = <Project as serde::Deserialize>::deserialize(&raw)
        .map_err(|e| tracing::debug!("{}: {e}", path.display()))
        .ok()?;

    let dir_name = dir.file_name().map(|n| n.to_string_lossy().into_owned());
    let workshop_dir_id = dir_name
        .as_deref()
        .filter(|n| source == Source::Workshop && steam::is_workshop_id(n))
        .map(ToOwned::to_owned);
    let id = workshop_dir_id
        .clone()
        .unwrap_or_else(|| format!("h{:016x}", stable_hash(dir.as_os_str().as_encoded_bytes())));
    let workshop_id = workshop_dir_id.or_else(|| {
        project
            .workshopid
            .clone()
            .filter(|w| steam::is_workshop_id(w) && w != "0")
    });
    let kind = kind_of(&project, &raw);
    let preset_values = raw.get("preset").and_then(Value::as_object);
    let has_properties = match kind {
        Kind::Preset => preset_values.is_some_and(|p| !p.is_empty()),
        _ => !project.general.properties.is_empty(),
    };
    let tags = project.tags.iter().filter_map(string_of).collect();
    let title = project
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .or(dir_name)
        .unwrap_or_default();
    Some(Item {
        id,
        path: dir.to_owned(),
        title,
        description: project.description.clone().filter(|d| !d.is_empty()),
        kind,
        tags,
        rating: rating_of(&project),
        preview: project.preview.as_deref().and_then(|p| find_file(dir, p)),
        size: 0,
        updated: Some((stamp.dir.max(stamp.project) / 1_000_000_000) as u64),
        installed: None,
        workshop_id,
        source,
        audio_reactive: project.general.supportsaudioprocessing.unwrap_or(false),
        has_properties,
        dependency: preset_dependency(&raw),
    })
}

/// Fill in what Steam knows about a workshop item, or walk the directory.
fn apply_records(item: &mut Item, records: &HashMap<String, WorkshopItem>, walk: bool) {
    let record = (item.source == Source::Workshop)
        .then(|| records.get(&item.id))
        .flatten();
    match record {
        Some(r) => {
            if let Some(size) = r.size {
                item.size = size;
            } else if walk {
                item.size = walk_size(&item.path);
            }
            item.updated = r.updated.or(item.updated);
            item.installed = r.touched;
        }
        None if walk => item.size = walk_size(&item.path),
        None => {}
    }
}

fn sort_items(items: &mut Vec<Item>) {
    let mut seen = HashSet::new();
    // Candidates come workshop first; a duplicate id keeps that one.
    items.retain(|i| seen.insert(i.id.clone()));
    items.sort_by_cached_key(|i| (i.title.to_lowercase(), i.id.clone()));
}

impl Library {
    /// Scan every directory of `sources` (in parallel).
    pub fn scan(sources: &Sources) -> Self {
        let records = workshop_records(sources);
        let mut items: Vec<Item> = candidates(sources)
            .into_par_iter()
            .filter_map(|c| {
                let mut item = read_item(&c.dir, c.source, c.stamp)?;
                apply_records(&mut item, &records, true);
                Some(item)
            })
            .collect();
        sort_items(&mut items);
        Self { items }
    }

    /// The library from `cache`, rescanning only directories that changed
    /// or are new and dropping ones that are gone; the cache is then
    /// updated. Steam's metadata is always reread.
    pub fn load_or_scan(sources: &Sources, cache: &Path) -> Self {
        let cached: HashMap<PathBuf, CacheEntry> = std::fs::read(cache)
            .ok()
            .and_then(|bytes| {
                serde_json::from_slice::<CacheFile>(&bytes)
                    .map_err(|e| tracing::debug!("{}: {e}", cache.display()))
                    .ok()
            })
            .filter(|c| c.version == CACHE_VERSION)
            .map(|c| {
                c.entries
                    .into_iter()
                    .map(|e| (e.item.path.clone(), e))
                    .collect()
            })
            .unwrap_or_default();

        let records = workshop_records(sources);
        let found = candidates(sources);
        let mut rescanned = 0usize;
        let entries: Vec<CacheEntry> = found
            .into_par_iter()
            .filter_map(|c| {
                if let Some(entry) = cached.get(&c.dir)
                    && entry.stamp == c.stamp
                    && entry.item.source == c.source
                {
                    let mut item = entry.item.clone();
                    apply_records(&mut item, &records, false);
                    return Some(CacheEntry {
                        stamp: c.stamp,
                        item,
                    });
                }
                let mut item = read_item(&c.dir, c.source, c.stamp)?;
                apply_records(&mut item, &records, true);
                Some(CacheEntry {
                    stamp: c.stamp,
                    item,
                })
            })
            .collect();
        rescanned += entries
            .iter()
            .filter(|e| cached.get(&e.item.path).is_none_or(|c| c.stamp != e.stamp))
            .count();
        tracing::debug!("library: {} items, {rescanned} rescanned", entries.len());

        let file = CacheFile {
            version: CACHE_VERSION,
            entries,
        };
        let changed = rescanned > 0 || file.entries.len() != cached.len();
        if changed {
            match serde_json::to_vec(&file) {
                Ok(json) => {
                    if let Err(e) = crate::write_atomic(cache, &json) {
                        tracing::warn!("{}: {e}", cache.display());
                    }
                }
                Err(e) => tracing::warn!("library cache: {e}"),
            }
        }
        let mut items: Vec<Item> = file.entries.into_iter().map(|e| e.item).collect();
        sort_items(&mut items);
        Self { items }
    }

    /// The item with `id`.
    pub fn get(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_project(dir: &Path, json: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("project.json"), json).unwrap();
    }

    /// A fake Steam layout: `workshop/content/431960/<id>` and its acf.
    fn fixture(root: &Path) -> Sources {
        let content = root.join("steamapps/workshop/content/431960");
        write_project(
            &content.join("111"),
            r#"{"title": "Beta", "type": "Video", "file": "a.mp4", "preview": "Preview.JPG",
                "contentrating": "Everyone", "tags": ["Nature", 3],
                "general": {"supportsaudioprocessing": true,
                            "properties": {"speed": {"type": "slider", "value": 1}}}}"#,
        );
        std::fs::write(content.join("111/preview.jpg"), b"x").unwrap();
        write_project(
            &content.join("222"),
            r#"{"title": "alpha", "dependency": "111", "preset": {"speed": 2},
                "contentrating": "Questionable"}"#,
        );
        write_project(&content.join("333"), "{ not json");
        std::fs::create_dir_all(content.join("444")).unwrap();
        std::fs::write(
            root.join("steamapps/workshop/appworkshop_431960.acf"),
            r#""AppWorkshop" { "WorkshopItemsInstalled" { "111" { "size" "1000" "timeupdated" "50" } }
               "WorkshopItemDetails" { "111" { "timetouched" "60" } } }"#,
        )
        .unwrap();
        let user = root.join("user");
        write_project(
            &user.join("mine"),
            r#"{"title": "Gamma", "file": "scene.json", "workshopid": "999"}"#,
        );
        std::fs::write(user.join("mine/scene.json"), b"0123456789").unwrap();
        let single = root.join("single");
        write_project(
            &single,
            r#"{"title": "Delta", "type": "web", "file": "index.html"}"#,
        );
        Sources {
            workshop: vec![content],
            projects: vec![root.join("missing")],
            user: vec![user, single],
        }
    }

    #[test]
    fn scans_sources() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::scan(&fixture(dir.path()));
        let titles: Vec<&str> = lib.items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["alpha", "Beta", "Delta", "Gamma"]);

        let video = lib.get("111").unwrap();
        assert_eq!(video.kind, Kind::Video);
        assert_eq!(video.rating, Rating::Everyone);
        assert_eq!(video.tags, ["Nature", "3"]);
        assert_eq!(video.size, 1000);
        assert_eq!(video.updated, Some(50));
        assert_eq!(video.installed, Some(60));
        assert_eq!(video.workshop_id.as_deref(), Some("111"));
        assert!(video.audio_reactive && video.has_properties);
        assert_eq!(
            video.preview.as_deref().unwrap().file_name().unwrap(),
            "preview.jpg"
        );

        let preset = lib.get("222").unwrap();
        assert_eq!(preset.kind, Kind::Preset);
        assert_eq!(preset.dependency.as_deref(), Some("111"));
        assert!(preset.has_properties && preset.preview.is_none());

        let gamma = &lib.items[3];
        assert_eq!(gamma.kind, Kind::Scene);
        assert_eq!(gamma.source, Source::User);
        assert!(gamma.id.starts_with('h') && gamma.id.len() == 17);
        assert_eq!(gamma.workshop_id.as_deref(), Some("999"));
        assert!(gamma.size >= 10);
        assert!(gamma.updated.is_some() && gamma.installed.is_none());
        assert_eq!(lib.items[2].kind, Kind::Web);

        // Ids are stable across scans.
        assert_eq!(Library::scan(&fixture(dir.path())), lib);
    }

    #[test]
    fn cache_rescans_only_changes() {
        let dir = tempfile::tempdir().unwrap();
        let sources = fixture(dir.path());
        let cache = dir.path().join("cache/library.json");
        let lib = Library::load_or_scan(&sources, &cache);
        assert_eq!(lib, Library::scan(&sources));
        assert!(cache.is_file());

        // Unchanged: served from the cache (a poisoned title shows it).
        let mut file: CacheFile = serde_json::from_slice(&std::fs::read(&cache).unwrap()).unwrap();
        for e in &mut file.entries {
            if e.item.id == "111" {
                e.item.title = "Cached".into();
            }
        }
        std::fs::write(&cache, serde_json::to_vec(&file).unwrap()).unwrap();
        let lib = Library::load_or_scan(&sources, &cache);
        assert_eq!(lib.get("111").unwrap().title, "Cached");

        // Changed project.json (size differs even if mtime is coarse): rescanned.
        let content = &sources.workshop[0];
        write_project(
            &content.join("111"),
            r#"{"title": "Beta 2", "type": "video", "file": "a.mp4"}"#,
        );
        // New item, removed item.
        write_project(
            &content.join("555"),
            r#"{"title": "Epsilon", "type": "scene"}"#,
        );
        std::fs::remove_dir_all(content.join("222")).unwrap();
        let lib = Library::load_or_scan(&sources, &cache);
        assert_eq!(lib.get("111").unwrap().title, "Beta 2");
        assert!(lib.get("555").is_some());
        assert!(lib.get("222").is_none());
        assert_eq!(lib, Library::scan(&sources));

        // A broken or outdated cache is a full scan.
        std::fs::write(&cache, b"garbage").unwrap();
        assert_eq!(
            Library::load_or_scan(&sources, &cache),
            Library::scan(&sources)
        );
    }

    #[test]
    fn finds_files_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Sub")).unwrap();
        std::fs::write(dir.path().join("Sub/Prev.GIF"), b"").unwrap();
        let found = find_file(dir.path(), "sub\\prev.gif").unwrap();
        assert!(found.ends_with("Sub/Prev.GIF"));
        assert!(find_file(dir.path(), "missing.gif").is_none());
        assert!(find_file(dir.path(), "").is_none());
    }

    /// Scan the real Steam library (`cargo test --release -- --ignored
    /// --nocapture real_library`): prints timings only.
    #[test]
    #[ignore]
    fn real_library() {
        let sources = Sources::discover(&[]);
        let t = std::time::Instant::now();
        let lib = Library::scan(&sources);
        println!("scan: {:?}", t.elapsed());
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("library.json");
        let t = std::time::Instant::now();
        Library::load_or_scan(&sources, &cache);
        println!("cold cache: {:?}", t.elapsed());
        let t = std::time::Instant::now();
        let warm = Library::load_or_scan(&sources, &cache);
        println!("warm cache: {:?}", t.elapsed());
        assert_eq!(warm, lib);
        assert!(!lib.items.is_empty());
    }
}
