use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use wpe_assets::scene::SceneObject;
use wpe_particles::{ParticleMesh, ParticleSystem, UpdateContext};
use wpe_runtime::WallpaperFiles;
use wpe_vfs::PhysicalFs;

#[derive(Default)]
struct Stats {
    objects: usize,
    loaded: usize,
    subsystems: usize,
    renderers: BTreeMap<String, usize>,
    sprite_sheets: usize,
    /// Message → (count, examples).
    failures: BTreeMap<String, (usize, Vec<String>)>,
    unknown: BTreeMap<String, (usize, Vec<String>)>,
    warnings: BTreeMap<String, (usize, Vec<String>)>,
    non_finite: Vec<String>,
    particles: usize,
    vertices: u64,
    slowest: Vec<(Duration, String)>,
}

fn record(map: &mut BTreeMap<String, (usize, Vec<String>)>, key: String, example: &str) {
    let e = map.entry(key).or_default();
    e.0 += 1;
    if e.1.len() < 3 {
        e.1.push(example.to_owned());
    }
}

fn print_group(title: &str, map: &BTreeMap<String, (usize, Vec<String>)>, verbose: bool) {
    if map.is_empty() {
        return;
    }
    println!("{title}:");
    let mut rows: Vec<_> = map.iter().collect();
    rows.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
    for (k, (n, ex)) in rows {
        println!("  {n:>5}  {k}");
        if verbose {
            for e in ex {
                println!("           e.g. {e}");
            }
        }
    }
}

/// Load and simulate every particle object of every workshop scene.
pub fn scan_particles(
    assets: &Path,
    workshop: &Path,
    seconds: f32,
    limit: Option<usize>,
    verbose: bool,
) -> Result<()> {
    let assets = Arc::new(PhysicalFs::new(assets)?);
    let mut dirs: Vec<_> = std::fs::read_dir(workshop)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("scene.pkg").is_file() || p.join("project.json").is_file())
        .collect();
    dirs.sort();
    let mut stats = Stats::default();
    let mut scenes = 0usize;
    let mut mesh = ParticleMesh::default();
    let frames = (seconds * 30.0).round().max(1.0) as usize;
    let ctx = UpdateContext::default();
    for dir in dirs.into_iter().take(limit.unwrap_or(usize::MAX)) {
        let id = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let Ok(files) = WallpaperFiles::open(assets.clone(), &dir) else {
            continue;
        };
        let mut any = false;
        for obj in &files.scene.objects {
            let SceneObject::Particle(p) = obj else {
                continue;
            };
            any = true;
            stats.objects += 1;
            let label = format!("{id}/{}", p.particle);
            let mut sys =
                match ParticleSystem::load(&files.vfs, &p.particle, p.instanceoverride.as_ref()) {
                    Ok(s) => s,
                    Err(e) => {
                        let msg = e.to_string();
                        let key = msg
                            .split_once(": ")
                            .map_or(msg.as_str(), |(_, rest)| rest)
                            .to_owned();
                        record(&mut stats.failures, key, &label);
                        continue;
                    }
                };
            stats.loaded += 1;
            stats.subsystems += sys.subsystems().len();
            for u in sys.unknown_features() {
                record(&mut stats.unknown, u.to_owned(), &label);
            }
            for w in sys.warnings() {
                let key = w.split(": ").next().unwrap_or(w).to_owned();
                record(&mut stats.warnings, key, &format!("{label}: {w}"));
            }
            sys.set_seed(p.common.id as u64);
            let start = Instant::now();
            for _ in 0..frames {
                sys.update(1.0 / 30.0, &ctx);
            }
            let mut finite = true;
            for s in sys.subsystems() {
                let kind = s
                    .renderer()
                    .map_or("spawner-only".to_owned(), |k| format!("{k:?}"));
                *stats.renderers.entry(kind).or_default() += 1;
                stats.sprite_sheets += s.combos().contains_key("SPRITESHEET") as usize;
                s.mesh(sys.view(), &mut mesh);
                stats.vertices += mesh.vertex_count as u64;
                finite &= mesh
                    .attributes
                    .iter()
                    .all(|a| a.2.iter().all(|f| f.is_finite()));
            }
            let took = start.elapsed();
            stats.slowest.push((took, label.clone()));
            stats.particles += sys.particle_count();
            if !finite {
                stats.non_finite.push(label);
            }
        }
        scenes += any as usize;
    }
    stats.slowest.sort_by_key(|s| std::cmp::Reverse(s.0));
    println!(
        "{} particle objects in {scenes} scenes: {} loaded, {} failed; {} subsystems",
        stats.objects,
        stats.loaded,
        stats.objects - stats.loaded,
        stats.subsystems
    );
    println!(
        "after {seconds}s: {} live particles, {} vertices",
        stats.particles, stats.vertices
    );
    println!("sprite-sheet subsystems: {}", stats.sprite_sheets);
    println!("renderers:");
    for (k, n) in &stats.renderers {
        println!("  {n:>5}  {k}");
    }
    print_group("load failures", &stats.failures, verbose);
    print_group("unsupported entries (objects)", &stats.unknown, verbose);
    print_group("warnings", &stats.warnings, verbose);
    if !stats.non_finite.is_empty() {
        println!("non-finite vertex data: {}", stats.non_finite.len());
        for l in stats.non_finite.iter().take(10) {
            println!("  {l}");
        }
    }
    println!("slowest simulations:");
    for (d, l) in stats.slowest.iter().take(5) {
        println!("  {:>8.1} ms  {l}", d.as_secs_f64() * 1000.0);
    }
    Ok(())
}
