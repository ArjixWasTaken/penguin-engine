//! Parses every `.mdl` in the local Wallpaper Engine install, when present.
//! Set `WPE_ASSETS` / `WPE_WORKSHOP` to override the default Steam paths.

use std::path::{Path, PathBuf};

use glam::{Mat4, Vec3};
use wpe_mdl::Mdl;
use wpe_vfs::PkgFs;

fn steam(rel: &str) -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_default();
    Path::new(&home)
        .join(".local/share/Steam/steamapps")
        .join(rel)
}

fn corpus() -> Vec<(String, Vec<u8>)> {
    let assets = std::env::var_os("WPE_ASSETS")
        .map_or_else(|| steam("common/wallpaper_engine/assets"), PathBuf::from);
    let workshop = std::env::var_os("WPE_WORKSHOP")
        .map_or_else(|| steam("workshop/content/431960"), PathBuf::from);
    let mut out = Vec::new();
    let mut stack = vec![assets];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "mdl")
                && let Ok(b) = std::fs::read(&p)
            {
                out.push((p.display().to_string(), b));
            }
        }
    }
    for e in std::fs::read_dir(&workshop).into_iter().flatten().flatten() {
        let Ok(pkg) = PkgFs::open(e.path().join("scene.pkg")) else {
            continue;
        };
        for entry in pkg.entries() {
            if entry.path.to_ascii_lowercase().ends_with(".mdl") {
                let name = format!("{}/{}", e.file_name().to_string_lossy(), entry.path);
                out.push((name, pkg.entry_bytes(entry).to_vec()));
            }
        }
    }
    out
}

/// Every corpus file parses; blend shape targets match the unshaped mesh
/// posed by `MDLE` on all but a few vertices (the ones the shape moves).
#[test]
fn corpus_blend_shapes() {
    let files = corpus();
    if files.is_empty() {
        eprintln!("no Wallpaper Engine install found; skipped");
        return;
    }
    let (mut shaped, mut tracked) = (0, 0);
    for (name, bytes) in &files {
        let mdl = Mdl::parse(bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        if mdl.animations.iter().any(|a| !a.morph_tracks.is_empty()) {
            tracked += 1;
        }
        if mdl.blend_shapes().is_empty() {
            assert!(
                mdl.morph_pose.is_empty(),
                "{name}: MDLE without blend shapes"
            );
            continue;
        }
        shaped += 1;
        let mesh = &mdl.meshes[0];
        assert_eq!(mdl.morph_pose.len(), mdl.bone_count(), "{name}");
        for a in &mdl.animations {
            assert!(a.morph_tracks.len() <= mesh.morphs.len(), "{name}");
            for t in &a.morph_tracks {
                assert_eq!(t.len(), a.length as usize + 1, "{name}: {}", a.name);
            }
        }
        for morph in &mesh.morphs {
            assert_eq!(morph.deltas.len(), mesh.vertex_count(), "{name}");
            let moved = morph
                .deltas
                .iter()
                .filter(|d| Vec3::from(**d).length() > 0.01)
                .count();
            assert!(
                moved * 4 < mesh.vertex_count(),
                "{name}: {moved} of {} vertices moved",
                mesh.vertex_count()
            );
            // Reposing the shaped mesh reproduces the stored target.
            let shaped_mesh = mesh.morphed_positions(&[1.0]);
            let bones = &mdl.skeleton.as_ref().unwrap().bones;
            let world = |local: &dyn Fn(usize) -> Mat4| {
                let mut w: Vec<Mat4> = Vec::new();
                for (i, b) in bones.iter().enumerate() {
                    w.push(b.parent.map_or(local(i), |p| w[p] * local(i)));
                }
                w
            };
            let pose = world(&|i| mdl.morph_pose[i]);
            let bind = world(&|i| bones[i].bind);
            for (v, p) in shaped_mesh.iter().enumerate() {
                let m: Mat4 = (0..4)
                    .map(|k| {
                        let b = mesh.blend_indices[v][k] as usize;
                        pose[b] * bind[b].inverse() * mesh.blend_weights[v][k]
                    })
                    .sum();
                let err = (m.transform_point3(Vec3::from(*p)) - Vec3::from(morph.positions[v]))
                    .abs()
                    .max_element();
                assert!(err < 0.05, "{name}: vertex {v} off by {err}");
            }
        }
    }
    eprintln!(
        "{} .mdl files; {shaped} with blend shapes, {tracked} with weight tracks",
        files.len()
    );
}
