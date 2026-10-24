use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;
use wpe_assets::{Project, load_json, resolve_scene_file};
use wpe_script::{
    CursorEvent, CursorEventKind, CursorState, LayerId, LayerInfo, LayerKind, LogLevel, SceneHost,
    ScriptError, ScriptOptions, ScriptRuntime, ScriptValue,
};
use wpe_vfs::{PhysicalFs, PkgFs, Vfs};

/// Scene stand-in backed by the scene JSON: property reads and writes hit a map.
struct ScanHost {
    layers: Vec<LayerInfo>,
    props: HashMap<(LayerId, String), ScriptValue>,
    canvas: [f32; 2],
    next_id: LayerId,
    logs: usize,
}

impl ScanHost {
    fn new(scene: &Value) -> Self {
        let ortho = &scene["general"]["orthogonalprojection"];
        let canvas = [
            ortho["width"].as_f64().unwrap_or(1920.0) as f32,
            ortho["height"].as_f64().unwrap_or(1080.0) as f32,
        ];
        let mut host = Self {
            layers: Vec::new(),
            props: HashMap::new(),
            canvas,
            next_id: 1_000_000,
            logs: 0,
        };
        for obj in scene["objects"].as_array().into_iter().flatten() {
            let Some(id) = obj["id"].as_i64() else {
                continue;
            };
            let id = id as LayerId;
            let kind = if obj.get("text").is_some() {
                LayerKind::Text
            } else if obj.get("particle").is_some() {
                LayerKind::Particle
            } else if obj.get("sound").is_some() {
                LayerKind::Sound
            } else if obj.get("light").is_some() {
                LayerKind::Light
            } else if obj.get("model").is_some() {
                LayerKind::Model
            } else {
                LayerKind::Image
            };
            let effects = obj["effects"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|e| {
                    e["name"]
                        .as_str()
                        .or_else(|| e["file"].as_str())
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect();
            host.layers.push(LayerInfo {
                id,
                name: obj["name"].as_str().unwrap_or_default().to_owned(),
                kind,
                parent: obj["parent"].as_i64().map(|p| p as LayerId),
                effects,
            });
            for prop in [
                "origin",
                "scale",
                "angles",
                "visible",
                "alpha",
                "color",
                "text",
                "size",
                "pointsize",
                "parallaxDepth",
            ] {
                if let Some(v) = obj.get(prop) {
                    let v = literal(v);
                    host.props
                        .insert((id, prop.to_owned()), ScriptValue::from_scene_json(prop, v));
                }
            }
        }
        host
    }
}

impl SceneHost for ScanHost {
    fn layers(&self) -> Vec<LayerInfo> {
        self.layers.clone()
    }
    fn get(&self, layer: LayerId, property: &str) -> Option<ScriptValue> {
        self.props.get(&(layer, property.to_owned())).cloned()
    }
    fn set(&mut self, layer: LayerId, property: &str, value: ScriptValue) {
        self.props.insert((layer, property.to_owned()), value);
    }
    fn create_layer(&mut self, config: &Value) -> Option<LayerId> {
        let id = self.next_id;
        self.next_id += 1;
        self.layers.push(LayerInfo {
            id,
            name: config["name"].as_str().unwrap_or("created").to_owned(),
            ..Default::default()
        });
        Some(id)
    }
    fn destroy_layer(&mut self, layer: LayerId) {
        self.layers.retain(|l| l.id != layer);
    }
    fn sort_layer(&mut self, layer: LayerId, index: usize) {
        if let Some(pos) = self.layers.iter().position(|l| l.id == layer) {
            let l = self.layers.remove(pos);
            let index = index.min(self.layers.len());
            self.layers.insert(index, l);
        }
    }
    fn canvas_size(&self) -> [f32; 2] {
        self.canvas
    }
    fn audio_spectrum(&self, left: &mut [f32], right: &mut [f32]) {
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            *l = 1.0 / (1.0 + i as f32);
            *r = *l * 0.5;
        }
    }
    fn log(&mut self, _level: LogLevel, _message: &str) {
        self.logs += 1;
    }
}

/// The literal of a possibly-bound scene value.
fn literal(v: &Value) -> &Value {
    match v.as_object() {
        Some(o) if o.contains_key("value") => &o["value"],
        _ => v,
    }
}

struct Found {
    layer: LayerId,
    property: String,
    source: String,
    script_properties: Option<Value>,
    initial: ScriptValue,
}

fn collect(v: &Value, path: &mut Vec<String>, layer: LayerId, out: &mut Vec<Found>) {
    match v {
        Value::Object(map) => {
            if let Some(Value::String(src)) = map.get("script") {
                let property = path.join("/");
                let initial = ScriptValue::from_scene_json(
                    &property,
                    map.get("value").unwrap_or(&Value::Null),
                );
                out.push(Found {
                    layer,
                    property,
                    source: src.clone(),
                    script_properties: map.get("scriptproperties").cloned(),
                    initial,
                });
            }
            for (k, child) in map {
                path.push(k.clone());
                collect(child, path, layer, out);
                path.pop();
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                path.push(i.to_string());
                collect(child, path, layer, out);
                path.pop();
            }
        }
        _ => {}
    }
}

#[derive(Default)]
struct Group {
    count: usize,
    wallpapers: Vec<String>,
    example: Option<String>,
}

#[derive(Default)]
struct Stats {
    wallpapers: usize,
    scripted: usize,
    scripts: usize,
    loaded: usize,
    clean: usize,
    disabled: usize,
    changed: usize,
    by_property: BTreeMap<String, usize>,
    groups: BTreeMap<String, Group>,
    unreadable: Vec<String>,
}

impl Stats {
    fn record(&mut self, key: String, wallpaper: &str, location: Option<&str>) {
        let g = self.groups.entry(key).or_default();
        if g.example.is_none() {
            g.example = location.map(|l| format!("{wallpaper}: {l}"));
        }
        g.count += 1;
        if g.wallpapers.len() < 4 && !g.wallpapers.iter().any(|w| w == wallpaper) {
            g.wallpapers.push(wallpaper.to_owned());
        }
    }
}

fn scene_json(dir: &Path) -> Result<(Value, Value)> {
    let mut vfs = Vfs::new();
    vfs.mount("wallpaper", PhysicalFs::new(dir)?);
    let pkg = dir.join("scene.pkg");
    if pkg.is_file() {
        vfs.mount("pkg", PkgFs::open(&pkg)?);
    }
    let project_raw: Value = load_json(&vfs, "project.json").unwrap_or(Value::Null);
    let project: Project = load_json(&vfs, "project.json").unwrap_or_default();
    let scene_file = resolve_scene_file(&project, &vfs);
    let scene: Value = load_json(&vfs, &scene_file).with_context(|| scene_file.clone())?;
    Ok((scene, project_raw))
}

/// Load every SceneScript in the workshop against a JSON-backed host, run
/// init, 120 updates and a click on each interactive layer, then group errors.
pub fn scan_scripts(
    assets: &Path,
    workshop: &Path,
    builtin_classes: bool,
    verbose: bool,
) -> Result<()> {
    let base_classes = if builtin_classes {
        None
    } else {
        std::fs::read_to_string(assets.join("scripts/jsclasses/baseclasses.js")).ok()
    };
    println!(
        "base classes: {}",
        if base_classes.is_some() {
            "Wallpaper Engine baseclasses.js"
        } else {
            "built-in"
        }
    );
    let mut dirs: Vec<_> = std::fs::read_dir(workshop)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    let mut stats = Stats::default();
    for dir in dirs {
        let id = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let (scene, project) = match scene_json(&dir) {
            Ok(v) => v,
            Err(_) => {
                if dir.join("scene.pkg").is_file() {
                    stats.unreadable.push(id);
                }
                continue;
            }
        };
        stats.wallpapers += 1;
        let mut found = Vec::new();
        for obj in scene["objects"].as_array().into_iter().flatten() {
            let layer = obj["id"].as_i64().unwrap_or(-1) as LayerId;
            collect(obj, &mut Vec::new(), layer, &mut found);
        }
        if found.is_empty() {
            continue;
        }
        stats.scripted += 1;
        let mut host = ScanHost::new(&scene);
        let mut rt = ScriptRuntime::with_options(ScriptOptions {
            base_classes: base_classes.clone(),
            ..Default::default()
        })?;
        rt.set_user_properties(&project["general"]["properties"]);
        let mut ids = Vec::new();
        for f in found {
            stats.scripts += 1;
            *stats
                .by_property
                .entry(property_group(&f.property))
                .or_default() += 1;
            match rt.add_property_script(
                &mut host,
                f.layer,
                &f.property,
                &f.source,
                f.script_properties.as_ref(),
                f.initial.clone(),
            ) {
                Ok(sid) => ids.push((sid, f.initial)),
                Err(e) => {
                    let key = match &e {
                        ScriptError::Compile(m) => format!("compile: {}", strip_location(m)),
                        ScriptError::Load(m) => format!("load: {}", strip_location(m)),
                        other => other.to_string(),
                    };
                    stats.record(key, &id, None);
                }
            }
        }
        stats.loaded += ids.len();
        rt.init(&mut host);
        let mut failed: Vec<_> = rt.take_failures();
        for frame in 0..120 {
            let t = frame as f64 / 60.0;
            let cursor = CursorState {
                world: [host.canvas[0] * 0.5, host.canvas[1] * 0.5],
                screen: [960.0, 540.0],
                left_down: false,
            };
            rt.update(&mut host, t, 1.0 / 60.0, Some(cursor));
            if frame == 60 {
                let hits = rt.interactive_layers();
                let at = [host.canvas[0] * 0.5, host.canvas[1] * 0.5];
                for kind in [
                    CursorEventKind::Move,
                    CursorEventKind::Down,
                    CursorEventKind::Up,
                ] {
                    rt.cursor_event(
                        &mut host,
                        CursorEvent::new(kind, at, [960.0, 540.0], hits.clone()),
                    );
                }
            }
            failed.extend(rt.take_failures());
        }
        let mut bad = std::collections::HashSet::new();
        for f in &failed {
            bad.insert(f.script);
            let key = if f.timeout {
                format!("{}: timeout", f.phase)
            } else {
                format!("{}: {}", f.phase, f.message)
            };
            stats.record(key, &id, f.location.as_deref());
        }
        if verbose {
            for (sid, initial) in ids.iter().filter(|(_, i)| i.as_str().is_some()).take(3) {
                if let Some(v) = rt.value(*sid).filter(|v| *v != initial) {
                    println!(
                        "{id}: text -> {}",
                        truncate(&format!("{:?}", v.to_json()), 100)
                    );
                }
            }
        }
        stats.disabled += ids.iter().filter(|(s, _)| rt.is_disabled(*s)).count();
        stats.clean += ids.iter().filter(|(s, _)| !bad.contains(s)).count();
        stats.changed += ids
            .iter()
            .filter(|(s, initial)| rt.value(*s).is_some_and(|v| v != initial))
            .count();
    }

    println!(
        "{} wallpapers read, {} with scripts; {} scripts, {} loaded, {} ran without errors, {} disabled, {} changed their value",
        stats.wallpapers,
        stats.scripted,
        stats.scripts,
        stats.loaded,
        stats.clean,
        stats.disabled,
        stats.changed
    );
    if !stats.unreadable.is_empty() {
        println!("{} scene packages unreadable", stats.unreadable.len());
    }
    println!("\nscripted properties:");
    for (p, n) in &stats.by_property {
        println!("{n:>6}  {p}");
    }
    let mut groups: Vec<_> = stats.groups.into_iter().collect();
    groups.sort_by_key(|g| std::cmp::Reverse(g.1.count));
    let total: usize = groups.iter().map(|g| g.1.count).sum();
    println!("\n{} errors in {} groups:", total, groups.len());
    let shown = if verbose { groups.len() } else { 40 };
    for (key, g) in groups.iter().take(shown) {
        println!(
            "{:>6}  {}  [{}]",
            g.count,
            truncate(key, 150),
            g.wallpapers.join(", ")
        );
        if let (true, Some(e)) = (verbose, &g.example) {
            println!("        e.g. {e}");
        }
    }
    if groups.len() > shown {
        println!("   ... {} more groups (--verbose)", groups.len() - shown);
    }
    Ok(())
}

fn property_group(property: &str) -> String {
    let parts: Vec<&str> = property
        .split('/')
        .map(|s| if s.parse::<usize>().is_ok() { "#" } else { s })
        .collect();
    match parts.iter().position(|p| *p == "constantshadervalues") {
        Some(i) => parts[..=i].join("/") + "/*",
        None => parts.join("/"),
    }
}

fn strip_location(m: &str) -> &str {
    m.split(" (at ").next().unwrap_or(m)
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}
