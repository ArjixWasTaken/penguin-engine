use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use wpe_assets::tex::{Mip, TexFormat, Texture};
use wpe_vfs::{FileSystem, PhysicalFs, PkgFs};

pub fn tex2png(input: &str, out: &Path, pkg: Option<&Path>) -> Result<()> {
    let bytes = match pkg {
        Some(pkg) => PkgFs::open(pkg)?
            .read(&wpe_vfs::normalize(input))?
            .into_owned(),
        None => std::fs::read(input).with_context(|| format!("reading {input}"))?,
    };
    let tex = Texture::parse(&bytes)?;
    println!(
        "format {:?}, flags {:?}, texb {}, container {}x{}, map {}x{}, {} image(s), {} sprite frame(s){}",
        tex.format,
        tex.flags,
        tex.texb_version,
        tex.width,
        tex.height,
        tex.map_width,
        tex.map_height,
        tex.images.len(),
        tex.sprite_frames.len(),
        if tex.video.is_some() { ", video" } else { "" }
    );
    let mip = tex
        .images
        .first()
        .and_then(|i| i.mips.first())
        .context("texture has no mips")?;
    let rgba = to_rgba8(tex.format, mip)?;
    image::RgbaImage::from_raw(mip.width, mip.height, rgba)
        .context("pixel buffer size mismatch")?
        .save(out)?;
    println!("wrote {}x{} to {}", mip.width, mip.height, out.display());
    Ok(())
}

type BcDecoder = fn(&[u8], usize, usize, &mut [u32]) -> Result<(), &'static str>;

fn to_rgba8(format: TexFormat, mip: &Mip) -> Result<Vec<u8>> {
    let (w, h) = (mip.width as usize, mip.height as usize);
    let d = &mip.data;
    let bc = |f: BcDecoder| -> Result<Vec<u8>> {
        let mut px = vec![0u32; w * h];
        f(d, w, h, &mut px).map_err(anyhow::Error::msg)?;
        // texture2ddecoder packs pixels as BGRA.
        Ok(px
            .into_iter()
            .flat_map(|p| {
                let [b, g, r, a] = p.to_le_bytes();
                [r, g, b, a]
            })
            .collect())
    };
    let f16 = |i: usize| half::f16::from_le_bytes([d[i * 2], d[i * 2 + 1]]).to_f32();
    let unorm = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Ok(match format {
        TexFormat::Rgba8 => d.clone(),
        TexFormat::Bc1 => bc(texture2ddecoder::decode_bc1)?,
        TexFormat::Bc2 => bc(texture2ddecoder::decode_bc2)?,
        TexFormat::Bc3 => bc(texture2ddecoder::decode_bc3)?,
        TexFormat::Bc6h => bc(texture2ddecoder::decode_bc6_unsigned)?,
        TexFormat::Bc7 => bc(texture2ddecoder::decode_bc7)?,
        TexFormat::R8 => d.iter().flat_map(|&r| [r, r, r, 255]).collect(),
        TexFormat::Rg8 => d
            .chunks_exact(2)
            .flat_map(|c| [c[0], c[0], c[0], c[1]])
            .collect(),
        TexFormat::Rgba16f => (0..w * h)
            .flat_map(|p| [0, 1, 2, 3].map(|c| unorm(f16(p * 4 + c))))
            .collect(),
        TexFormat::R16f => (0..w * h)
            .flat_map(|p| {
                let v = unorm(f16(p));
                [v, v, v, 255]
            })
            .collect(),
        other => bail!("PNG conversion of {other:?} not supported"),
    })
}

#[derive(Default)]
struct Stats {
    ok: usize,
    failed: usize,
    formats: BTreeMap<String, usize>,
    texb: BTreeMap<u32, usize>,
    sprites: usize,
    videos: usize,
    variants: usize,
}

impl Stats {
    fn record(&mut self, name: &str, bytes: &[u8]) {
        match Texture::parse(bytes) {
            Ok(t) => {
                self.ok += 1;
                *self.formats.entry(format!("{:?}", t.format)).or_default() += 1;
                *self.texb.entry(t.texb_version).or_default() += 1;
                self.sprites += t.is_sprite() as usize;
                self.videos += t.video.is_some() as usize;
                self.variants += !t.variants.is_empty() as usize;
            }
            Err(e) => {
                self.failed += 1;
                eprintln!("FAIL {name}: {e}");
            }
        }
    }

    fn scan_fs(&mut self, label: &str, fs: &dyn FileSystem) {
        let mut names: Vec<&str> = fs.files().filter(|p| p.ends_with(".tex")).collect();
        names.sort_unstable();
        for name in names {
            match fs.read(name) {
                Ok(bytes) => self.record(&format!("{label}:{name}"), &bytes),
                Err(e) => {
                    self.failed += 1;
                    eprintln!("FAIL {label}:{name}: {e}");
                }
            }
        }
    }
}

pub fn scan_tex(workshop: Option<&Path>, assets: &[std::path::PathBuf]) -> Result<()> {
    let mut stats = Stats::default();
    for dir in assets {
        stats.scan_fs(&dir.display().to_string(), &PhysicalFs::new(dir)?);
    }
    if let Some(workshop) = workshop {
        for item in std::fs::read_dir(workshop)? {
            let pkg_path = item?.path().join("scene.pkg");
            if pkg_path.exists() {
                stats.scan_fs(&pkg_path.display().to_string(), &PkgFs::open(&pkg_path)?);
            }
        }
    }
    println!("{} ok, {} failed", stats.ok, stats.failed);
    println!(
        "{} sprite sheets, {} video textures, {} with variants",
        stats.sprites, stats.videos, stats.variants
    );
    println!("formats: {:?}", stats.formats);
    println!("texb versions: {:?}", stats.texb);
    Ok(())
}
