//! What the detail panel shows beyond [`Item`]: the picture size and the
//! user properties, read when asked for.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value};
use wpe_assets::Project;
use wpe_assets::scene::OrthogonalProjection;
use wpe_ipc::{PropertyChoice, PropertyInfo};
use wpe_vfs::{PkgFs, Vfs};

use crate::library::{find_file, preset_dependency};
use crate::{Item, steam};

/// `project.json` of the wallpaper in `dir`, raw and parsed.
fn read_project(dir: &Path) -> Option<(Value, Project)> {
    let path = dir.join("project.json");
    let bytes = std::fs::read(&path).ok()?;
    let raw: Value = wpe_assets::parse_json(&String::from_utf8_lossy(&bytes))
        .map_err(|e| tracing::debug!("{}: {e}", path.display()))
        .ok()?;
    let project = Project::deserialize(&raw).ok()?;
    Some((raw, project))
}

/// The directory of the wallpaper preset `dir` configures (`dependency`): a
/// sibling directory, else an installed workshop item.
fn dependency_dir(dir: &Path, dependency: &str) -> Option<PathBuf> {
    if !steam::is_workshop_id(dependency) {
        return None;
    }
    let sibling = dir.parent()?.join(dependency);
    if sibling.join("project.json").is_file() {
        return Some(sibling);
    }
    steam::resolve_wallpaper(dependency).ok()
}

/// For a preset: its values and the wallpaper it configures.
fn preset_target(dir: &Path, raw: &Value) -> Option<(Map<String, Value>, PathBuf)> {
    let dependency = preset_dependency(raw)?;
    let values = raw
        .get("preset")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    Some((values, dependency_dir(dir, &dependency)?))
}

/// The user properties of `project` for a settings form, in the order
/// Wallpaper Engine shows them; `current` gives a property's value now
/// (the daemon's `property_infos`, so both return the same).
pub fn property_infos(
    project: &Project,
    current: impl Fn(&str) -> Option<Value>,
) -> Vec<PropertyInfo> {
    let mut list: Vec<PropertyInfo> = project
        .general
        .properties
        .iter()
        .map(|(name, p)| PropertyInfo {
            name: name.clone(),
            kind: p.kind.clone().unwrap_or_default(),
            text: p.text.clone(),
            value: current(name).unwrap_or_else(|| p.value.clone()),
            default: p.value.clone(),
            options: p
                .options
                .iter()
                .map(|o| PropertyChoice {
                    label: o.label.clone(),
                    value: o.value.clone(),
                })
                .collect(),
            min: p.min,
            max: p.max,
            step: p.step,
            order: p.order,
            condition: p.condition.clone(),
        })
        .collect();
    list.sort_by(|a, b| {
        let order = |p: &PropertyInfo| p.order.unwrap_or(f64::MAX);
        order(a)
            .total_cmp(&order(b))
            .then_with(|| a.name.cmp(&b.name))
    });
    list
}

/// The user properties of the wallpaper in `dir` with the values
/// `project.json` declares. For a preset: the properties of the wallpaper
/// it configures, with the preset's values as their values and defaults
/// (a `null` keeps the wallpaper's), as the daemon reports them.
pub fn properties(dir: &Path) -> Vec<PropertyInfo> {
    let Some((raw, project)) = read_project(dir) else {
        return Vec::new();
    };
    if let Some((values, target)) = preset_target(dir, &raw) {
        let Some((_, mut target)) = read_project(&target) else {
            return Vec::new();
        };
        // As the daemon loads it: the preset's values are the defaults; a
        // file shipped with the preset is relative to it.
        for (name, value) in values {
            let Some(p) = target.general.properties.get_mut(&name) else {
                continue;
            };
            if value.is_null() {
                continue;
            }
            p.value = match value.as_str() {
                Some(rel)
                    if matches!(p.kind.as_deref(), Some("file" | "directory"))
                        && !rel.is_empty()
                        && !Path::new(rel).is_absolute()
                        && dir.join(rel).exists() =>
                {
                    Value::String(dir.join(rel).display().to_string())
                }
                _ => value,
            };
        }
        return property_infos(&target, |_| None);
    }
    property_infos(&project, |_| None)
}

/// The wallpaper's label translations (`general.localization`, for
/// [`crate::Locale::label`]); a preset's come from the wallpaper it configures.
pub fn localization(dir: &Path) -> Option<Value> {
    let (raw, project) = read_project(dir)?;
    if let Some((_, target)) = preset_target(dir, &raw) {
        return read_project(&target)?.1.general.localization;
    }
    project.general.localization
}

/// The picture size of `item`: a scene's `orthogonalprojection`, a video's
/// track size. `None` for web pages (sized by the screen), 3D scenes and
/// what can't be read. Reads files; call it when needed.
pub fn resolution(item: &Item) -> Option<(u32, u32)> {
    resolution_of(&item.path, 0)
}

fn resolution_of(dir: &Path, depth: u8) -> Option<(u32, u32)> {
    let (raw, project) = read_project(dir)?;
    if depth == 0
        && let Some((_, target)) = preset_target(dir, &raw)
    {
        return resolution_of(&target, 1);
    }
    if project.is_video() {
        let file = find_file(dir, project.file.as_deref()?)?;
        return video_size(&file);
    }
    if project.is_scene()
        || project
            .file
            .as_deref()
            .is_some_and(|f| f.ends_with(".json"))
    {
        return scene_size(dir, &project);
    }
    None
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SceneHead {
    general: SceneGeneral,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SceneGeneral {
    orthogonalprojection: Option<OrthogonalProjection>,
}

/// The scene's `general.orthogonalprojection` size, from its package or a
/// loose file (the package wins, as when it plays).
fn scene_size(dir: &Path, project: &Project) -> Option<(u32, u32)> {
    let stem = project
        .file
        .as_deref()
        .and_then(|f| f.rsplit(['/', '\\']).next())
        .and_then(|f| f.rsplit_once('.').map(|(stem, _)| stem.to_owned()));
    let pkg = stem
        .iter()
        .map(|stem| dir.join(format!("{stem}.pkg")))
        .chain([dir.join("scene.pkg")])
        .find(|p| p.is_file());
    let text = pkg
        .and_then(|pkg| {
            let mut vfs = Vfs::new();
            vfs.mount("pkg", PkgFs::open(&pkg).ok()?);
            let name = wpe_assets::resolve_scene_file(project, &vfs);
            vfs.read_to_string(&name).ok()
        })
        .or_else(|| {
            let names = stem
                .iter()
                .map(|s| format!("{s}.json"))
                .chain(["scene.json".to_owned()]);
            names
                .filter_map(|n| find_file(dir, &n))
                .find_map(|p| std::fs::read_to_string(p).ok())
        })?;
    let head: SceneHead = wpe_assets::parse_json(&text).ok()?;
    let ortho = head.general.orthogonalprojection?;
    let (w, h) = (
        u32::try_from(ortho.width).ok()?,
        u32::try_from(ortho.height).ok()?,
    );
    (w > 0 && h > 0).then_some((w, h))
}

/// Largest `moov` box read into memory.
const MAX_MOOV: u64 = 64 << 20;

/// The `ftyp` and `moov` boxes of an MP4, found by seeking over the rest
/// (the media data can be gigabytes).
fn mp4_header(file: &Path) -> Option<Vec<u8>> {
    let mut f = std::fs::File::open(file).ok()?;
    let len = f.metadata().ok()?.len();
    let mut out = Vec::new();
    let mut pos = 0u64;
    while pos + 8 <= len {
        f.seek(SeekFrom::Start(pos)).ok()?;
        let mut head = [0u8; 16];
        f.read_exact(&mut head[..8]).ok()?;
        let mut size = u64::from(u32::from_be_bytes(head[..4].try_into().ok()?));
        let kind: [u8; 4] = head[4..8].try_into().ok()?;
        let mut header_len = 8;
        if size == 1 {
            f.read_exact(&mut head[8..16]).ok()?;
            size = u64::from_be_bytes(head[8..16].try_into().ok()?);
            header_len = 16;
        } else if size == 0 {
            size = len - pos;
        }
        if size < header_len || pos + size > len {
            return None;
        }
        if &kind == b"ftyp" || &kind == b"moov" {
            if size > MAX_MOOV {
                return None;
            }
            f.seek(SeekFrom::Start(pos)).ok()?;
            let start = out.len();
            out.resize(start + size as usize, 0);
            f.read_exact(&mut out[start..]).ok()?;
            if &kind == b"moov" {
                return Some(out);
            }
        }
        pos += size;
    }
    None
}

/// The picture size of the MP4 `file` as shown (rotation applied), from its
/// first video track; the same as the runtime's `video_wallpaper::dimensions`.
fn video_size(file: &Path) -> Option<(u32, u32)> {
    let header = mp4_header(file)?;
    let context = mp4parse::read_mp4(&mut std::io::Cursor::new(header)).ok()?;
    context.tracks.iter().find_map(|track| {
        if track.track_type != mp4parse::TrackType::Video {
            return None;
        }
        let tkhd = track.tkhd.as_ref();
        // `tkhd` sizes are 16.16 fixed point; the sample entry has the coded size.
        let presented = tkhd
            .map(|t| (t.width >> 16, t.height >> 16))
            .filter(|&(w, h)| w > 0 && h > 0);
        let coded = track.stsd.as_ref().and_then(|stsd| {
            stsd.descriptions.iter().find_map(|d| match d {
                mp4parse::SampleEntry::Video(v) => Some((u32::from(v.width), u32::from(v.height))),
                _ => None,
            })
        });
        let (w, h) = presented.or(coded).filter(|&(w, h)| w > 0 && h > 0)?;
        // A quarter turn swaps the sides (mpv applies the rotation).
        let quarter_turn = tkhd.is_some_and(|t| t.matrix.a == 0 && t.matrix.d == 0);
        Some(if quarter_turn { (h, w) } else { (w, h) })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Kind, Rating, Source};

    fn item(path: &Path, kind: Kind) -> Item {
        Item {
            id: "x".into(),
            path: path.to_owned(),
            title: String::new(),
            description: None,
            kind,
            tags: Vec::new(),
            rating: Rating::Unknown,
            preview: None,
            size: 0,
            updated: None,
            installed: None,
            workshop_id: None,
            source: Source::User,
            audio_reactive: false,
            has_properties: false,
            dependency: None,
        }
    }

    const PROJECT: &str = r#"{"type": "scene", "file": "scene.json", "general": {
        "localization": {"en-us": {"ui_a": "A"}},
        "properties": {
            "speed": {"type": "slider", "value": 1, "min": 0, "max": 5, "order": 2, "text": "ui_a"},
            "on": {"type": "bool", "value": true, "order": 1, "condition": "speed.value > 0"},
            "mode": {"type": "combo", "value": "1", "options": [{"label": "One", "value": "1"}]},
            "color": {"type": "color", "value": "1 0 0", "order": 2},
            "bg": {"type": "file", "value": "", "order": 9}
        }}}"#;

    #[test]
    fn properties_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("111");
        std::fs::create_dir(&base).unwrap();
        std::fs::write(base.join("project.json"), PROJECT).unwrap();
        let props = properties(&base);
        let names: Vec<&str> = props.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["on", "color", "speed", "bg", "mode"]);
        assert_eq!(props[2].max, Some(5.0));
        assert_eq!(props[0].condition.as_deref(), Some("speed.value > 0"));
        assert_eq!(props[4].options[0].label.as_deref(), Some("One"));
        assert_eq!(props[2].value, props[2].default);

        // A preset: the dependency's properties with its values.
        let preset = dir.path().join("222");
        std::fs::create_dir(&preset).unwrap();
        std::fs::write(
            preset.join("project.json"),
            r#"{"title": "p", "dependency": "111",
                "preset": {"speed": 3, "on": null, "gone": 1, "bg": "files/a.jpg"}}"#,
        )
        .unwrap();
        std::fs::create_dir(preset.join("files")).unwrap();
        std::fs::write(preset.join("files/a.jpg"), b"").unwrap();
        let props = properties(&preset);
        assert_eq!(props.len(), 5);
        assert_eq!(
            props[3].value,
            preset.join("files/a.jpg").display().to_string()
        );
        assert_eq!(props[2].value, 3);
        assert_eq!(props[2].default, 3);
        assert_eq!(props[0].value, true);
        assert_eq!(localization(&preset).unwrap()["en-us"]["ui_a"], "A");
        assert!(properties(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn scene_resolution_from_loose_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("project.json"), PROJECT).unwrap();
        std::fs::write(
            dir.path().join("Scene.json"),
            r#"{"general": {"orthogonalprojection": {"width": 2560, "height": 1440}}, "objects": []}"#,
        )
        .unwrap();
        assert_eq!(
            resolution(&item(dir.path(), Kind::Scene)),
            Some((2560, 1440))
        );

        std::fs::write(
            dir.path().join("Scene.json"),
            r#"{"general": {"orthogonalprojection": null}}"#,
        )
        .unwrap();
        assert_eq!(resolution(&item(dir.path(), Kind::Scene)), None);
    }

    #[test]
    fn mp4_boxes_are_walked() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.mp4");
        let mut data = Vec::new();
        data.extend_from_slice(&16u32.to_be_bytes());
        data.extend_from_slice(b"ftypisom\0\0\0\0");
        // A 64-bit sized `mdat` to skip.
        data.extend_from_slice(&1u32.to_be_bytes());
        data.extend_from_slice(b"mdat");
        data.extend_from_slice(&24u64.to_be_bytes());
        data.extend_from_slice(&[0; 8]);
        data.extend_from_slice(&8u32.to_be_bytes());
        data.extend_from_slice(b"moov");
        std::fs::write(&file, &data).unwrap();
        let header = mp4_header(&file).unwrap();
        assert_eq!(&header[4..8], b"ftyp");
        assert_eq!(&header[20..24], b"moov");
        assert_eq!(header.len(), 24);
        // No video track.
        assert_eq!(video_size(&file), None);

        std::fs::write(&file, &data[..20]).unwrap();
        assert!(mp4_header(&file).is_none());
    }

    /// Resolutions over the real library (`cargo test --release -- --ignored
    /// --nocapture real_resolutions`): prints timing and how many resolved.
    #[test]
    #[ignore]
    fn real_resolutions() {
        let lib = crate::Library::scan(&crate::Sources::discover(&[]));
        let t = std::time::Instant::now();
        let mut found = 0;
        for item in &lib.items {
            if resolution(item).is_some() {
                found += 1;
            }
        }
        println!("{found}/{} resolved in {:?}", lib.items.len(), t.elapsed());
        let t = std::time::Instant::now();
        let props: usize = lib.items.iter().map(|i| properties(&i.path).len()).sum();
        println!("{props} properties in {:?}", t.elapsed());
    }
}
