//! Video wallpapers (project type `video`): the project's `.mp4` shown over
//! the whole output. It is made into a one-layer scene, so fill modes,
//! playback and audio come from the scene runtime.

use serde_json::json;
use wpe_assets::tex::{Mip, TexFlags, TexFormat, TexImage, Texture};
use wpe_vfs::MemoryFs;

/// Texture name the video plays into.
pub const TEXTURE: &str = "__video";
/// The scene file the made-up documents are mounted under.
pub const SCENE_FILE: &str = "__video/scene.json";
const MODEL: &str = "__video/model.json";
const MATERIAL: &str = "__video/material.json";

/// The picture size of `mp4`, as it is shown (rotation applied), from its
/// first video track.
pub fn dimensions(mp4: &[u8]) -> Option<(u32, u32)> {
    let context = mp4parse::read_mp4(&mut std::io::Cursor::new(mp4)).ok()?;
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

/// The scene, model and material a `width`×`height` video wallpaper is
/// shown with.
pub fn documents(width: u32, height: u32) -> MemoryFs {
    let (w, h) = (width as f32, height as f32);
    let scene = json!({
        "camera": {
            "center": format!("{} {} -1", w / 2.0, h / 2.0),
            "eye": format!("{} {} 0", w / 2.0, h / 2.0),
            "up": "0 1 0",
        },
        "general": {
            "orthogonalprojection": { "width": width, "height": height },
            "clearcolor": "0 0 0",
            "clearenabled": true,
            "bloom": false,
            "zoom": 1.0,
        },
        "objects": [{
            "id": 1,
            "name": "video",
            "image": MODEL,
            "origin": format!("{} {} 0", w / 2.0, h / 2.0),
            "angles": "0 0 0",
            "scale": "1 1 1",
            "size": format!("{w} {h}"),
            "visible": true,
        }],
    });
    let model = json!({ "material": MATERIAL });
    let material = json!({
        "passes": [{
            "blending": "normal",
            "cullmode": "nocull",
            "depthtest": "disabled",
            "depthwrite": "disabled",
            "shader": "genericimage2",
            "textures": [TEXTURE],
        }],
    });
    let mut fs = MemoryFs::new();
    for (path, doc) in [(SCENE_FILE, scene), (MODEL, model), (MATERIAL, material)] {
        fs.insert(path, doc.to_string());
    }
    fs
}

/// A video texture playing `mp4` (shown black until the first frame).
pub fn texture(mp4: Vec<u8>, width: u32, height: u32) -> Texture {
    Texture {
        format: TexFormat::Rgba8,
        flags: TexFlags::empty(),
        texb_version: 0,
        width,
        height,
        map_width: width,
        map_height: height,
        depth: 1,
        images: vec![TexImage {
            mips: vec![Mip {
                width,
                height,
                depth: 1,
                data: vec![0; width as usize * height as usize * 4],
            }],
        }],
        sprite_frames: Vec::new(),
        variants: Vec::new(),
        video: Some(mp4),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_make_a_one_layer_scene() {
        let mut vfs = wpe_vfs::Vfs::new();
        vfs.mount("video", documents(1280, 720));
        let scene: wpe_assets::Scene = wpe_assets::load_json(&vfs, SCENE_FILE).unwrap();
        assert_eq!(scene.objects.len(), 1);
        let model: serde_json::Value = wpe_assets::load_json(&vfs, MODEL).unwrap();
        assert_eq!(model["material"], MATERIAL);
        let material: wpe_assets::material::Material =
            wpe_assets::load_json(&vfs, MATERIAL).unwrap();
        assert_eq!(material.passes[0].textures[0].as_deref(), Some(TEXTURE));
    }

    #[test]
    fn corpus_video_sizes() {
        let dir = dirs::home_dir()
            .unwrap_or_default()
            .join(".local/share/Steam/steamapps/workshop/content/431960");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        let mut checked = 0;
        for entry in entries.flatten().take(400) {
            let Ok(text) = std::fs::read_to_string(entry.path().join("project.json")) else {
                continue;
            };
            let Ok(project) = wpe_assets::parse_json::<wpe_assets::Project>(&text) else {
                continue;
            };
            let Some(file) = project.file.as_deref().filter(|_| project.is_video()) else {
                continue;
            };
            let Ok(mp4) = std::fs::read(entry.path().join(file)) else {
                continue;
            };
            let size = dimensions(&mp4);
            assert!(size.is_some(), "{}: no video size", entry.path().display());
            checked += 1;
            if checked == 20 {
                break;
            }
        }
    }
}
