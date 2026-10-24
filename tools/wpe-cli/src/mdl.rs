use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use wpe_assets::scene::SceneObject;
use wpe_assets::{ImageModel, Scene, load_json};
use wpe_mdl::{Mdl, Puppet};
use wpe_vfs::{PhysicalFs, PkgFs, Vfs};

#[derive(Default)]
struct Stats {
    files: usize,
    failed: usize,
    groups: BTreeMap<String, usize>,
    failures: BTreeMap<String, Vec<String>>,
    warnings: Vec<String>,
    puppets: usize,
    puppets_animated: usize,
    puppet_failures: Vec<String>,
}

impl Stats {
    fn record(&mut self, location: &str, bytes: &[u8]) {
        self.files += 1;
        let header = bytes
            .get(..8)
            .map(|h| String::from_utf8_lossy(h).into_owned())
            .unwrap_or_default();
        let format = bytes
            .get(9..13)
            .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        match Mdl::parse(bytes) {
            Ok(mdl) => {
                let formats: Vec<String> = mdl
                    .meshes
                    .iter()
                    .map(|m| format!("{:#x}", m.format.bits()))
                    .collect();
                let key = format!(
                    "{header} header={format:#x} meshes=[{}] MDLS={} MDLA={}",
                    formats.join(","),
                    mdl.skeleton
                        .as_ref()
                        .map_or("-".into(), |s| s.version.to_string()),
                    mdl.animation_version.map_or("-".into(), |v| v.to_string()),
                );
                *self.groups.entry(key).or_default() += 1;
                for w in &mdl.warnings {
                    self.warnings.push(format!("{location}: {w}"));
                }
            }
            Err(e) => {
                self.failed += 1;
                self.failures
                    .entry(format!("{header} header={format:#x}: {e}"))
                    .or_default()
                    .push(location.to_owned());
            }
        }
    }
}

fn mesh_report(mdl: &Mdl) -> String {
    let Some(mesh) = mdl.meshes.first() else {
        return "no mesh".into();
    };
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in &mesh.positions {
        for i in 0..3 {
            lo[i] = lo[i].min(p[i]);
            hi[i] = hi[i].max(p[i]);
        }
    }
    let (mut ulo, mut uhi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for t in &mesh.texcoords {
        for i in 0..2 {
            ulo[i] = ulo[i].min(t[i]);
            uhi[i] = uhi[i].max(t[i]);
        }
    }
    format!(
        "{} verts {} tris, pos x[{:.1},{:.1}] y[{:.1},{:.1}] z[{:.1},{:.1}], uv u[{:.3},{:.3}] v[{:.3},{:.3}]",
        mesh.vertex_count(),
        mesh.indices.len() / 3,
        lo[0],
        hi[0],
        lo[1],
        hi[1],
        lo[2],
        hi[2],
        ulo[0],
        uhi[0],
        ulo[1],
        uhi[1]
    )
}

fn scan_puppets(vfs: &Vfs, id: &str, stats: &mut Stats, verbose: bool) {
    let Ok(scene) = load_json::<Scene>(vfs, "scene.json") else {
        return;
    };
    for obj in &scene.objects {
        let SceneObject::Image(img) = obj else {
            continue;
        };
        let Some(model_path) = &img.image else {
            continue;
        };
        let Ok(model) = load_json::<ImageModel>(vfs, model_path) else {
            continue;
        };
        let Some(puppet_path) = &model.puppet else {
            continue;
        };
        stats.puppets += 1;
        let loc = format!("{id}: {} ({puppet_path})", img.common.name);
        let mdl = match vfs.read(puppet_path) {
            Ok(bytes) => match Mdl::parse(&bytes) {
                Ok(m) => m,
                Err(e) => {
                    stats.puppet_failures.push(format!("{loc}: {e}"));
                    continue;
                }
            },
            Err(e) => {
                stats.puppet_failures.push(format!("{loc}: {e}"));
                continue;
            }
        };
        let puppet = Puppet::new(&mdl, &img.animationlayers);
        let mut floats = Vec::new();
        let mut first = Vec::new();
        let mut max_motion = 0f32;
        let mut finite = true;
        for frame in 0..=60 {
            puppet.bones(f64::from(frame) / 30.0, &mut floats);
            finite &= floats.iter().all(|f| f.is_finite());
            if frame == 0 {
                first.clone_from(&floats);
            }
            for (a, b) in floats.iter().zip(&first) {
                max_motion = max_motion.max((a - b).abs());
            }
        }
        if !finite || floats.len() != puppet.bone_count() * 16 {
            stats
                .puppet_failures
                .push(format!("{loc}: non-finite or mis-sized bone data"));
            continue;
        }
        if max_motion > 1e-3 {
            stats.puppets_animated += 1;
        }
        if verbose {
            let size = match (model.width, model.height) {
                (Some(w), Some(h)) => format!("{w}x{h}"),
                _ => img
                    .size
                    .map_or("?".into(), |s| format!("{}x{}", s.0[0], s.0[1])),
            };
            let identity = {
                let mut v = Vec::new();
                puppet.bones(0.0, &mut v);
                v.iter()
                    .enumerate()
                    .map(|(i, x)| (x - if i % 16 % 5 == 0 { 1.0 } else { 0.0 }).abs())
                    .fold(0f32, f32::max)
            };
            println!(
                "  {loc}: layer {size}, {} bones, {} anims, layers {}/{} matched, motion {max_motion:.3}, t=0 max |M-I| {identity:.3}\n    {}",
                puppet.bone_count(),
                mdl.animations.len(),
                puppet.matched_layers(),
                puppet.layers().count(),
                mesh_report(&mdl)
            );
        }
    }
}

/// Parse every `.mdl` in workshop packages and stock assets, then evaluate
/// each puppet image layer's bones for 2 seconds.
pub fn scan_mdl(assets: &Path, workshop: &Path, verbose: bool) -> Result<()> {
    let assets_fs = Arc::new(
        PhysicalFs::new(assets).with_context(|| format!("indexing {}", assets.display()))?,
    );
    let mut stats = Stats::default();
    for path in wpe_vfs::FileSystem::files(&*assets_fs)
        .filter(|p| p.ends_with(".mdl"))
        .map(str::to_owned)
        .collect::<Vec<_>>()
    {
        if let Ok(bytes) = std::fs::read(assets.join(&path)) {
            stats.record(&format!("assets/{path}"), &bytes);
        }
    }
    let mut dirs: Vec<_> = std::fs::read_dir(workshop)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("scene.pkg").exists())
        .collect();
    dirs.sort();
    for dir in &dirs {
        let id = dir
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let Ok(pkg) = PkgFs::open(dir.join("scene.pkg")) else {
            continue;
        };
        let mut any = false;
        for entry in pkg.entries() {
            if entry.path.to_ascii_lowercase().ends_with(".mdl") {
                any = true;
                stats.record(&format!("{id}/{}", entry.path), pkg.entry_bytes(entry));
            }
        }
        if !any {
            continue;
        }
        let mut vfs = Vfs::new();
        vfs.mount("assets", assets_fs.clone());
        vfs.mount("pkg", pkg);
        scan_puppets(&vfs, &id, &mut stats, verbose);
    }

    println!(
        "{} .mdl files, {} parsed, {} failed",
        stats.files,
        stats.files - stats.failed,
        stats.failed
    );
    for (key, n) in &stats.groups {
        println!("  {n:>4}  {key}");
    }
    for (key, locs) in &stats.failures {
        println!("FAIL {} × {key}", locs.len());
        for l in locs.iter().take(3) {
            println!("       {l}");
        }
    }
    println!("{} warnings", stats.warnings.len());
    for w in &stats.warnings {
        println!("  {w}");
    }
    println!(
        "{} puppet layers, {} animated over 2 s, {} failed",
        stats.puppets,
        stats.puppets_animated,
        stats.puppet_failures.len()
    );
    for f in &stats.puppet_failures {
        println!("  {f}");
    }
    Ok(())
}
