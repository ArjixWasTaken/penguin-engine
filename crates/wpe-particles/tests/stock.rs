use std::path::PathBuf;

use wpe_particles::{ParticleMesh, ParticleSystem, UpdateContext};
use wpe_vfs::{PhysicalFs, Vfs};

fn stock_assets() -> Option<PathBuf> {
    let p = std::env::var_os("WPE_ASSETS")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| {
                PathBuf::from(h).join(".local/share/Steam/steamapps/common/wallpaper_engine/assets")
            })
        })?;
    p.is_dir().then_some(p)
}

/// Wallpaper Engine's element preview scenes cover one feature each; every one must
/// load, simulate and mesh to finite data.
#[test]
fn stock_element_previews_simulate() {
    let Some(assets) = stock_assets() else {
        eprintln!("stock assets not found, skipping");
        return;
    };
    let previews = assets.join("scenes/particleelementpreviews");
    let mut checked = 0;
    for entry in std::fs::read_dir(&previews).unwrap() {
        let dir = entry.unwrap().path();
        let Ok(files) = std::fs::read_dir(dir.join("particles")) else {
            continue;
        };
        let mut vfs = Vfs::new();
        vfs.mount("preview", PhysicalFs::new(&dir).unwrap());
        vfs.mount("assets", PhysicalFs::new(&assets).unwrap());
        for f in files {
            let name = f.unwrap().file_name().to_string_lossy().into_owned();
            let path = format!("particles/{name}");
            let mut sys = ParticleSystem::load(&vfs, &path, None)
                .unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
            let unknown: Vec<_> = sys.unknown_features().collect();
            let element = dir.file_name().unwrap().to_string_lossy().into_owned();
            let unsupported = ["collisionmodel", "layerimage"];
            if !unsupported.contains(&element.as_str()) {
                assert!(unknown.is_empty(), "{element}: {unknown:?}");
            }
            for _ in 0..60 {
                sys.update(1.0 / 30.0, &UpdateContext::default());
            }
            let mut mesh = ParticleMesh::default();
            for s in sys.subsystems() {
                s.mesh(sys.view(), &mut mesh);
                for a in &mesh.attributes {
                    assert!(a.2.iter().all(|v| v.is_finite()), "{element} {}", a.0);
                }
            }
            checked += 1;
        }
    }
    assert!(checked > 40, "only {checked} previews");
}
