//! Render the text layers of workshop wallpapers to PNGs for visual checks.
//!
//! ```text
//! cargo run --release -p wpe-text --example render_corpus -- <out dir> [workshop id ...]
//! ```
//!
//! Without ids every wallpaper under the Steam workshop directory is visited.
//! Each layer is written as `<id>_<object id>.png` (straight-alpha RGBA) next
//! to `<id>_<object id>_preview.png`, the same image over a dark backdrop.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use wpe_assets::scene::SceneObject;
use wpe_assets::{Project, Scene, load_json, resolve_scene_file};
use wpe_text::{TextRenderer, TextStyle, resolve_text};
use wpe_vfs::{PhysicalFs, PkgFs, Vfs};

fn steam() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME")).join(".local/share/Steam/steamapps")
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(
        args.next()
            .expect("usage: render_corpus <out dir> [ids...]"),
    );
    std::fs::create_dir_all(&out).expect("create output dir");
    let ids: Vec<String> = args.collect();
    let root = steam().join("workshop/content/431960");
    let dirs: Vec<PathBuf> = if ids.is_empty() {
        let mut d: Vec<_> = std::fs::read_dir(&root)
            .expect("workshop dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        d.sort();
        d
    } else {
        ids.iter().map(|i| root.join(i)).collect()
    };
    let assets = Arc::new(
        PhysicalFs::new(steam().join("common/wallpaper_engine/assets")).expect("assets dir"),
    );
    let mut renderer = TextRenderer::new();
    let (mut ok, mut failed) = (0, 0);
    for dir in dirs {
        let Some(vfs) = mount(&assets, &dir) else {
            continue;
        };
        let project: Project = load_json(&vfs, "project.json").unwrap_or_default();
        let Ok(scene) = load_json::<Scene>(&vfs, &resolve_scene_file(&project, &vfs)) else {
            continue;
        };
        let user = |name: &str| {
            project
                .general
                .properties
                .get(name)
                .map(|p| p.value.clone())
        };
        let id = dir.file_name().unwrap().to_string_lossy().into_owned();
        for obj in &scene.objects {
            let SceneObject::Text(t) = obj else { continue };
            let style = TextStyle::from_object(t, user);
            let text = resolve_text(t, user);
            let start = std::time::Instant::now();
            match renderer.render(&vfs, &style, &text) {
                Ok(img) => {
                    ok += 1;
                    let name = format!("{id}_{}", t.common.id);
                    println!(
                        "{name}: {}x{} card {:?} authored {:?} anchor {:?} lines {} in {:?} {:?} {:?}",
                        img.width,
                        img.height,
                        img.card_size,
                        style.authored_size,
                        img.anchor_offset,
                        img.lines,
                        start.elapsed(),
                        style.font,
                        text.chars().take(40).collect::<String>()
                    );
                    save(&out, &name, &img);
                }
                Err(e) => {
                    failed += 1;
                    println!("{id}_{}: {e}", t.common.id);
                }
            }
        }
    }
    println!("rendered {ok}, failed {failed}");
}

fn mount(assets: &Arc<PhysicalFs>, dir: &Path) -> Option<Vfs> {
    let mut vfs = Vfs::new();
    vfs.mount("assets", assets.clone());
    vfs.mount("wallpaper", PhysicalFs::new(dir).ok()?);
    let pkg = dir.join("scene.pkg");
    if pkg.is_file() {
        vfs.mount("pkg", PkgFs::open(&pkg).ok()?);
    }
    Some(vfs)
}

fn save(out: &Path, name: &str, img: &wpe_text::TextImage) {
    let rgba = image::RgbaImage::from_raw(img.width, img.height, img.pixels.clone()).unwrap();
    rgba.save(out.join(format!("{name}.png"))).unwrap();
    let mut preview = image::RgbImage::new(img.width, img.height);
    for (p, s) in preview.pixels_mut().zip(rgba.pixels()) {
        let a = u16::from(s[3]);
        let bg = [40u16, 44, 52];
        for i in 0..3 {
            p[i] = ((u16::from(s[i]) * a + bg[i] * (255 - a)) / 255) as u8;
        }
    }
    preview
        .save(out.join(format!("{name}_preview.png")))
        .unwrap();
}
