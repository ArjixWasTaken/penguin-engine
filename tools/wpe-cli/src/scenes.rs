use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use wpe_assets::effect::EffectPass;
use wpe_assets::scene::{ImageEffect, ObjectKind, SceneObject};
use wpe_assets::{
    AssetError, Effect, ImageModel, Material, Project, Scene, load_json, parse_json,
    resolve_scene_file,
};
use wpe_vfs::{FileSystem, PhysicalFs, PkgFs, Vfs, VfsError};

/// Lets one indexed assets directory back every wallpaper's [`Vfs`].
struct SharedFs(Arc<PhysicalFs>);

impl FileSystem for SharedFs {
    fn contains(&self, path: &str) -> bool {
        self.0.contains(path)
    }
    fn read(&self, path: &str) -> wpe_vfs::Result<Cow<'_, [u8]>> {
        self.0.read(path)
    }
    fn files(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        self.0.files()
    }
}

#[derive(Default)]
struct Stats {
    wallpapers: usize,
    clean: usize,
    files_ok: BTreeMap<&'static str, usize>,
    kinds: BTreeMap<ObjectKind, usize>,
    /// Error group → (count, example locations).
    failures: BTreeMap<String, (usize, Vec<String>)>,
    font_images: usize,
    models_without_material: usize,
}

impl Stats {
    fn fail(&mut self, stage: &str, wallpaper: &str, err: &AssetError) {
        let (key, example) = match err {
            AssetError::Vfs(VfsError::NotFound(p)) => (
                format!("{stage}: file not found"),
                format!("{wallpaper}: {p}"),
            ),
            AssetError::Json { path, source } => {
                let msg = source.to_string();
                let msg = msg.split(" at line ").next().unwrap_or(&msg).to_owned();
                (format!("{stage}: {msg}"), format!("{wallpaper}: {path}"))
            }
            other => (format!("{stage}: {other}"), wallpaper.to_owned()),
        };
        let entry = self.failures.entry(key).or_default();
        entry.0 += 1;
        if entry.1.len() < 3 {
            entry.1.push(example);
        }
    }
}

/// Parses every file of one wallpaper, recording each distinct path once.
struct Scan<'a> {
    vfs: Vfs,
    id: String,
    seen: HashSet<String>,
    failed: bool,
    stats: &'a mut Stats,
}

impl Scan<'_> {
    fn load<T: DeserializeOwned>(&mut self, stage: &'static str, path: &str) -> Option<T> {
        match load_json::<T>(&self.vfs, path) {
            Ok(v) => {
                *self.stats.files_ok.entry(stage).or_default() += 1;
                Some(v)
            }
            Err(e) => {
                self.failed = true;
                self.stats.fail(stage, &self.id, &e);
                None
            }
        }
    }

    fn first_visit(&mut self, path: &str) -> bool {
        self.seen.insert(wpe_vfs::normalize(path))
    }

    fn image(&mut self, path: &str) {
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".ttf") || lower.ends_with(".otf") {
            self.stats.font_images += 1;
            return;
        }
        if !self.first_visit(path) {
            return;
        }
        let Some(model) = self.load::<ImageModel>("model", path) else {
            return;
        };
        match model.material {
            Some(m) if self.first_visit(&m) => {
                self.load::<Material>("material", &m);
            }
            Some(_) => {}
            None => self.stats.models_without_material += 1,
        }
    }

    fn effect(&mut self, fx: &ImageEffect) {
        if !self.first_visit(&fx.file) {
            return;
        }
        let Some(effect) = self.load::<Effect>("effect", &fx.file) else {
            return;
        };
        for pass in &effect.passes {
            if let EffectPass::Material(p) = pass
                && self.first_visit(&p.material)
            {
                self.load::<Material>("effect material", &p.material);
            }
        }
    }

    fn run(&mut self, dir: &Path) {
        let project = std::fs::read_to_string(dir.join("project.json"))
            .map_err(|e| {
                AssetError::Vfs(VfsError::Io {
                    path: "project.json".into(),
                    source: e,
                })
            })
            .and_then(|text| {
                parse_json::<Project>(&text).map_err(|source| AssetError::Json {
                    path: "project.json".into(),
                    source,
                })
            });
        let project = match project {
            Ok(p) => {
                *self.stats.files_ok.entry("project").or_default() += 1;
                p
            }
            Err(e) => {
                self.failed = true;
                self.stats.fail("project", &self.id, &e);
                Project::default()
            }
        };
        let scene_file = resolve_scene_file(&project, &self.vfs);
        let Some(scene) = self.load::<Scene>("scene", &scene_file) else {
            return;
        };
        for obj in &scene.objects {
            *self.stats.kinds.entry(obj.kind()).or_default() += 1;
            if let SceneObject::Image(img) = obj
                && let Some(path) = &img.image
            {
                self.image(path);
            }
            for fx in obj.effects() {
                self.effect(fx);
            }
        }
    }
}

/// Parse every stock effect, material and image model, used by a scene or not.
fn scan_stock(assets: &Arc<PhysicalFs>, stats: &mut Stats) {
    let mut vfs = Vfs::new();
    vfs.mount("assets", SharedFs(assets.clone()));
    let mut files: Vec<String> = assets
        .files()
        .filter(|p| p.ends_with(".json") && !p.contains("/preview"))
        .map(str::to_owned)
        .collect();
    files.sort_unstable();
    let mut scan = Scan {
        vfs,
        id: "assets".to_owned(),
        seen: HashSet::new(),
        failed: false,
        stats,
    };
    for path in &files {
        let parts: Vec<&str> = path.split('/').collect();
        match parts.as_slice() {
            ["effects", _, "effect.json"] => {
                scan.load::<Effect>("stock effect", path);
            }
            ["materials", ..] | ["effects", _, "materials", ..] => {
                scan.load::<Material>("stock material", path);
            }
            ["models", ..] => {
                scan.load::<ImageModel>("stock model", path);
            }
            ["scenes", .., "project.json"] => {
                scan.load::<Project>("stock project", path);
            }
            ["scenes", .., name] if name.ends_with("scene.json") => {
                scan.load::<Scene>("stock scene", path);
            }
            _ => {}
        }
    }
}

pub fn scan_scenes(assets: &Path, workshop: &Path) -> Result<()> {
    let assets = Arc::new(
        PhysicalFs::new(assets).with_context(|| format!("indexing {}", assets.display()))?,
    );
    let mut dirs: Vec<_> = std::fs::read_dir(workshop)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("scene.pkg").exists())
        .collect();
    dirs.sort();

    let mut stats = Stats::default();
    scan_stock(&assets, &mut stats);
    for dir in &dirs {
        let id = dir
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let mut vfs = Vfs::new();
        vfs.mount("assets", SharedFs(assets.clone()));
        match PkgFs::open(dir.join("scene.pkg")) {
            Ok(pkg) => vfs.mount("pkg", pkg),
            Err(e) => {
                stats.wallpapers += 1;
                stats.fail("pkg", &id, &AssetError::Vfs(e));
                continue;
            }
        }
        let mut scan = Scan {
            vfs,
            id,
            seen: HashSet::new(),
            failed: false,
            stats: &mut stats,
        };
        scan.run(dir);
        let failed = scan.failed;
        stats.wallpapers += 1;
        stats.clean += !failed as usize;
    }

    println!(
        "{} wallpapers, {} parsed cleanly, {} with failures",
        stats.wallpapers,
        stats.clean,
        stats.wallpapers - stats.clean
    );
    println!("files parsed: {:?}", stats.files_ok);
    let kinds: BTreeMap<_, _> = stats.kinds.iter().map(|(k, n)| (k.as_str(), n)).collect();
    println!("object kinds: {kinds:?}");
    println!(
        "font files referenced as images (skipped): {}",
        stats.font_images
    );
    println!(
        "image models without a material: {}",
        stats.models_without_material
    );
    let total: usize = stats.failures.values().map(|(n, _)| n).sum();
    println!("{total} failures in {} groups", stats.failures.len());
    let mut groups: Vec<_> = stats.failures.into_iter().collect();
    groups.sort_by_key(|g| std::cmp::Reverse(g.1.0));
    for (msg, (n, examples)) in groups {
        println!("{n:>5}  {msg}");
        for e in examples {
            println!("         e.g. {e}");
        }
    }
    Ok(())
}
