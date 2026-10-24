//! Times `ParticleSystem::update` and mesh generation over a wallpaper's particle objects.
//!
//! `cargo run --release -p wpe-particles --example particle_bench -- <assets> <wallpaper dir>...`

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;
use wpe_particles::{ParticleMesh, ParticleSystem, UpdateContext};
use wpe_vfs::{PhysicalFs, PkgFs, Vfs};

const WARMUP_FRAMES: usize = 90;
const FRAMES: usize = 300;

fn bench(assets: &Path, dir: &Path) -> Option<()> {
    let mut vfs = Vfs::new();
    vfs.mount("assets", PhysicalFs::new(assets).ok()?);
    vfs.mount("wallpaper", PhysicalFs::new(dir).ok()?);
    let pkg = dir.join("scene.pkg");
    if pkg.is_file() {
        vfs.mount("pkg", PkgFs::open(&pkg).ok()?);
    }
    let scene: Value = wpe_assets::load_json(&vfs, "scene.json").ok()?;
    let mut systems = Vec::new();
    for obj in scene["objects"].as_array()? {
        let Some(path) = obj.get("particle").and_then(Value::as_str) else {
            continue;
        };
        if let Ok(mut s) = ParticleSystem::load(&vfs, path, obj.get("instanceoverride")) {
            s.set_seed(obj.get("id").and_then(Value::as_u64).unwrap_or(0));
            systems.push(s);
        }
    }
    let ctx = UpdateContext::default();
    for _ in 0..WARMUP_FRAMES {
        for s in &mut systems {
            s.update(1.0 / 30.0, &ctx);
        }
    }
    let (mut update, mut mesh_time) = (Duration::ZERO, Duration::ZERO);
    let mut mesh = ParticleMesh::default();
    let mut particles = 0usize;
    for _ in 0..FRAMES {
        let t = Instant::now();
        for s in &mut systems {
            s.update(1.0 / 30.0, &ctx);
        }
        update += t.elapsed();
        let t = Instant::now();
        for s in &systems {
            for sub in s.subsystems() {
                sub.mesh(s.view(), &mut mesh);
            }
        }
        mesh_time += t.elapsed();
        particles += systems
            .iter()
            .map(ParticleSystem::particle_count)
            .sum::<usize>();
    }
    let checksum: f64 = systems
        .iter()
        .flat_map(|s| s.subsystems())
        .flat_map(|s| s.particles())
        .map(|p| (p.position.x + p.position.y + p.alpha) as f64)
        .sum();
    let per = |d: Duration| d.as_secs_f64() * 1000.0 / FRAMES as f64;
    println!(
        "{}: {} systems, {} live particles, update {:.3} ms/frame, mesh {:.3} ms/frame, checksum {checksum:.3}",
        dir.file_name()?.to_string_lossy(),
        systems.len(),
        particles / FRAMES,
        per(update),
        per(mesh_time),
    );
    Some(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (assets, dirs) = args
        .split_first()
        .expect("usage: particle_bench <assets> <dir>...");
    for d in dirs {
        if bench(Path::new(assets), Path::new(d)).is_none() {
            eprintln!("{d}: could not load");
        }
    }
}
