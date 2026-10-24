use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use wpe_assets::effect::EffectPass;
use wpe_assets::material::{Material, MaterialPass};
use wpe_assets::{Effect, load_json};
use wpe_runtime::{TextureStore, WallpaperFiles, material};
use wpe_scene::{Scene, apply_override};
use wpe_vfs::PhysicalFs;

#[derive(Default)]
struct Stats {
    ok: usize,
    failed: usize,
    /// First line of each error → (count, example).
    errors: BTreeMap<String, (usize, String)>,
    seen: HashSet<String>,
}

impl Stats {
    fn compile(
        &mut self,
        files: &WallpaperFiles,
        store: &mut TextureStore,
        pass: &MaterialPass,
        label: &str,
    ) {
        let key = format!("{}|{:?}|{:?}", pass.shader, pass.combos, pass.textures);
        if !self.seen.insert(key) {
            return;
        }
        match material::prepare(
            &files.vfs,
            store,
            &mut Default::default(),
            pass,
            &BTreeMap::new(),
        ) {
            Ok(_) => self.ok += 1,
            Err(e) => {
                self.failed += 1;
                let msg = e.to_string();
                let first = msg
                    .lines()
                    .find(|l| l.contains("ERROR"))
                    .unwrap_or(msg.lines().next().unwrap_or_default());
                let normalized = normalize(first);
                let entry = self.errors.entry(normalized).or_insert((0, String::new()));
                entry.0 += 1;
                if entry.1.is_empty() {
                    entry.1 = format!("{label} [{}]: {first}", pass.shader);
                }
            }
        }
    }
}

/// Strip line numbers and identifiers in quotes so similar errors group together.
fn normalize(line: &str) -> String {
    let re = regex::Regex::new(r"0:\d+|'[^']*'|\d+").expect("valid regex");
    re.replace_all(line, "_").trim().to_owned()
}

fn first_pass(files: &WallpaperFiles, path: &str) -> Option<MaterialPass> {
    let m: Material = load_json(&files.vfs, path).ok()?;
    m.passes.into_iter().next()
}

pub fn scan_shaders(
    assets: &Path,
    workshop: &Path,
    limit: Option<usize>,
    verbose: bool,
) -> Result<()> {
    let assets = Arc::new(PhysicalFs::new(assets)?);
    let mut stats = Stats::default();
    let mut dirs: Vec<_> = std::fs::read_dir(workshop)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("scene.pkg").is_file())
        .collect();
    dirs.sort();
    for dir in dirs.into_iter().take(limit.unwrap_or(usize::MAX)) {
        let id = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let files = match WallpaperFiles::open(assets.clone(), &dir) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("{id}: {e}");
                continue;
            }
        };
        let mut store = TextureStore::default();
        let vfs = &files.vfs;
        let props = wpe_scene::UserProperties::from_project(&files.project);
        let scene = Scene::build(vfs, &files.scene, &props, |n| store.size(vfs, n));
        for layer in &scene.layers {
            let label = format!("{id}/{}", layer.name);
            stats.compile(&files, &mut store, &layer.material, &label);
            for fx in &layer.effects {
                let Ok(effect) = load_json::<Effect>(vfs, &fx.file) else {
                    continue;
                };
                let mut material_index = 0;
                for pass in &effect.passes {
                    let EffectPass::Material(mp) = pass else {
                        continue;
                    };
                    let Some(mut base) = first_pass(&files, &mp.material) else {
                        continue;
                    };
                    apply_override(&mut base, &mp.settings);
                    if let Some(over) = fx.passes.get(material_index) {
                        apply_override(&mut base, over);
                    }
                    material_index += 1;
                    stats.compile(&files, &mut store, &base, &format!("{label}/{}", fx.file));
                }
            }
        }
    }

    let total = stats.ok + stats.failed;
    println!(
        "{} of {total} shader variants compiled ({:.1}%)",
        stats.ok,
        100.0 * stats.ok as f64 / total.max(1) as f64
    );
    let mut errors: Vec<_> = stats.errors.into_iter().collect();
    errors.sort_by_key(|e| std::cmp::Reverse(e.1.0));
    for (normalized, (count, example)) in errors.iter().take(if verbose { usize::MAX } else { 40 })
    {
        println!("{count:5}  {normalized}\n       e.g. {example}");
    }
    Ok(())
}
