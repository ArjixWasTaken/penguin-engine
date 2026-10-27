//! Wallpaper Engine `.tex` (TEXV/TEXI/TEXB) textures.
//!
//! Layout, little-endian. Version tags are 9 bytes: 8 ASCII chars + NUL,
//! e.g. `TEXV0005\0`.
//!
//! ```text
//! tag  TEXV, tag TEXI
//! u32  format, u32 flags, u32 width, height, map_width, map_height
//! [flags & VOLUME] u32 depth
//! u32  unknown
//! tag  TEXB (1..=4)
//! u32  image_count
//! [TEXB>=3] i32 image_type        FreeImage format of embedded files, -1 = raw
//! [TEXB>=4] u32 n, n × {i32 1, i32 index, i32 0, NUL-terminated JSON condition}
//! image_count × {
//!     u32 mip_count
//!     mip_count × {
//!         u32 width, height, [flags & VOLUME] u32 depth
//!         [TEXB>=2] u32 is_lz4, u32 decompressed_size
//!         u32 size, u8[size]
//!         [variants] variant patch trailer
//!     }
//! }
//! [flags & SPRITE] tag TEXS (1..=3), u32 frame_count, [TEXS==3] u32 w, h,
//!     frame_count × {u32 image, f32 frametime, 6 × (i32 if TEXS==1 else f32)}
//! ```

use std::io::Cursor;

use binrw::{BinRead, NullString, binread};
use bitflags::bitflags;

const MAX_IMAGES: u32 = 256;
const MAX_MIPS: u32 = 24;
const MAX_DIM: u32 = 16384;
const MAX_ALLOC: u32 = 1024 * 1024 * 1024;
const MAX_TOTAL: usize = 2 * 1024 * 1024 * 1024;
const MAX_VARIANTS: u32 = 64;
const MAX_FRAMES: u32 = 1 << 16;
const FREEIMAGE_TARGA: i32 = 17;

#[derive(Debug, thiserror::Error)]
pub enum TexError {
    #[error("malformed tex: {0}")]
    Malformed(#[from] binrw::Error),
    #[error("lz4 decompression failed: {0}")]
    Lz4(#[from] lz4_flex::block::DecompressError),
    #[error("lz4 output size {got} differs from declared {want}")]
    Lz4Size { got: usize, want: usize },
    #[error("embedded image failed to decode: {0}")]
    Image(#[from] image::ImageError),
    #[error("invalid mip: {0}")]
    InvalidMip(String),
    #[error("texture exceeds the {MAX_TOTAL}-byte decode budget")]
    TooLarge,
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct TexFlags: u32 {
        /// Nearest-neighbour sampling.
        const NO_INTERPOLATION = 1 << 0;
        /// Clamp UVs instead of repeating.
        const CLAMP_UVS = 1 << 1;
        /// Animated sprite sheet; a TEXS frame table follows the images.
        const SPRITE = 1 << 2;
        /// 3D texture (e.g. colour LUTs); header and mips carry a depth.
        const VOLUME = 1 << 6;
        const COMPO1 = 1 << 20;
        const COMPO2 = 1 << 21;
        const COMPO3 = 1 << 22;
    }
}

/// Pixel format of mip data as stored (after LZ4 decompression).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TexFormat {
    Rgba8,
    Bc1,
    Bc2,
    Bc3,
    Bc6h,
    Bc7,
    Rg8,
    R8,
    Rgba16f,
    Rg16f,
    R16f,
    Rgb565,
    Rgba1010102,
}

impl TexFormat {
    fn from_raw(raw: u32) -> Self {
        match raw {
            0 => Self::Rgba8,
            4 => Self::Bc3,
            6 => Self::Bc2,
            7 => Self::Bc1,
            8 => Self::Rg8,
            9 => Self::R8,
            10 => Self::Rgba16f,
            11 => Self::Rg16f,
            12 => Self::Bc7,
            13 => Self::R16f,
            14 => Self::Bc6h,
            15 => Self::Rgb565,
            16 => Self::Rgba1010102,
            other => {
                tracing::warn!("unknown tex format {other}, assuming RGBA8");
                Self::Rgba8
            }
        }
    }

    /// Side of the square stored as one unit: 4 for block-compressed formats.
    pub fn block_dim(self) -> u32 {
        match self {
            Self::Bc1 | Self::Bc2 | Self::Bc3 | Self::Bc6h | Self::Bc7 => 4,
            _ => 1,
        }
    }

    pub fn bytes_per_unit(self) -> usize {
        match self {
            Self::Bc1 => 8,
            Self::Bc2 | Self::Bc3 | Self::Bc6h | Self::Bc7 => 16,
            Self::Rgba8 | Self::Rg16f | Self::Rgba1010102 => 4,
            Self::Rg8 | Self::R16f | Self::Rgb565 => 2,
            Self::R8 => 1,
            Self::Rgba16f => 8,
        }
    }

    pub fn is_compressed(self) -> bool {
        self.block_dim() > 1
    }

    /// Bytes a tightly packed `w × h` region occupies, rounding partial blocks up.
    pub fn region_bytes(self, w: u32, h: u32) -> usize {
        let bd = self.block_dim();
        w.div_ceil(bd) as usize * h.div_ceil(bd) as usize * self.bytes_per_unit()
    }
}

fn parse_tag(bytes: [u8; 9]) -> Result<u32, String> {
    let text = &bytes[..8];
    if !text.starts_with(b"TEX") {
        return Err(format!(
            "bad version tag {:?}",
            String::from_utf8_lossy(text)
        ));
    }
    std::str::from_utf8(&text[4..])
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("bad version number {:?}", String::from_utf8_lossy(text)))
}

#[binread]
#[br(little)]
struct VariantRecord {
    tag: i32,
    index: i32,
    pad: i32,
    json: NullString,
}

#[binread]
#[br(little)]
struct VariantTable {
    #[br(temp, assert(count <= MAX_VARIANTS, "variant count {}", count))]
    count: u32,
    #[br(count = count)]
    records: Vec<VariantRecord>,
}

#[binread]
#[br(little)]
struct RawHeader {
    #[br(try_map = parse_tag)]
    _texv: u32,
    #[br(try_map = parse_tag)]
    _texi: u32,
    #[br(map = TexFormat::from_raw)]
    format: TexFormat,
    #[br(map = TexFlags::from_bits_retain)]
    flags: TexFlags,
    width: u32,
    height: u32,
    map_width: u32,
    map_height: u32,
    #[br(if(flags.contains(TexFlags::VOLUME), 1))]
    depth: u32,
    _unknown: u32,
    #[br(try_map = parse_tag)]
    texb: u32,
    #[br(assert(image_count <= MAX_IMAGES, "image count {}", image_count))]
    image_count: u32,
    #[br(if(texb >= 3, -1))]
    image_type: i32,
    #[br(if(texb >= 4))]
    variants: Option<VariantTable>,
}

impl RawHeader {
    fn has_variant_trailers(&self) -> bool {
        self.variants
            .as_ref()
            .is_some_and(|v| !v.records.is_empty())
    }
}

#[binread]
#[br(little)]
struct RawPatch {
    tag: i32,
    index: i32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    kind: i32,
    #[br(temp, assert(size <= MAX_ALLOC, "patch size {}", size))]
    size: u32,
    #[br(count = size)]
    data: Vec<u8>,
}

#[binread]
#[br(little)]
struct VariantTrailer {
    lead: i32,
    #[br(temp, assert(n <= MAX_VARIANTS, "patch count {}", n))]
    n: u32,
    #[br(count = n)]
    patches: Vec<RawPatch>,
}

#[binread]
#[br(little, import(texb: u32, has_variants: bool, volume: bool))]
struct RawMip {
    #[br(assert(width <= MAX_DIM, "mip width {}", width))]
    width: u32,
    #[br(assert(height <= MAX_DIM, "mip height {}", height))]
    height: u32,
    #[br(if(volume, 1), assert(depth <= MAX_DIM, "mip depth {}", depth))]
    depth: u32,
    #[br(if(texb >= 2))]
    is_lz4: u32,
    #[br(if(texb >= 2), assert(decompressed_size <= MAX_ALLOC))]
    decompressed_size: u32,
    #[br(temp, assert(size <= MAX_ALLOC, "mip size {}", size))]
    size: u32,
    #[br(count = size)]
    data: Vec<u8>,
    #[br(if(has_variants))]
    trailer: Option<VariantTrailer>,
}

#[binread]
#[br(little, import(texb: u32, has_variants: bool, volume: bool))]
struct RawImage {
    #[br(temp, assert(mip_count <= MAX_MIPS, "mip count {}", mip_count))]
    mip_count: u32,
    #[br(count = mip_count, args { inner: (texb, has_variants, volume) })]
    mips: Vec<RawMip>,
}

#[binread]
#[br(little, import(texs: u32))]
struct RawFrame {
    image: u32,
    frametime: f32,
    #[br(if(texs == 1))]
    int_coords: Option<[i32; 6]>,
    #[br(if(texs != 1))]
    float_coords: Option<[f32; 6]>,
}

#[binread]
#[br(little)]
struct RawSprite {
    #[br(temp, try_map = parse_tag)]
    texs: u32,
    #[br(temp, assert(frame_count <= MAX_FRAMES, "frame count {}", frame_count))]
    frame_count: u32,
    #[br(if(texs == 3))]
    _dims: Option<[u32; 2]>,
    #[br(count = frame_count, args { inner: (texs,) })]
    frames: Vec<RawFrame>,
}

#[binread]
#[br(little)]
struct RawTex {
    header: RawHeader,
    #[br(count = header.image_count, args {
        inner: (header.texb, header.has_variant_trailers(), header.flags.contains(TexFlags::VOLUME)),
    })]
    images: Vec<RawImage>,
    #[br(if(header.flags.contains(TexFlags::SPRITE)))]
    sprite: Option<RawSprite>,
}

/// A user-property condition that selects a texture variant.
#[derive(Debug, Clone, PartialEq)]
pub struct VariantCondition {
    pub index: u32,
    pub property: String,
    /// Expected value; non-string JSON values are kept as their JSON text.
    pub value: String,
}

impl VariantCondition {
    fn parse(rec: &VariantRecord) -> Option<Self> {
        if rec.tag != 1 || rec.pad != 0 || rec.index <= 0 {
            return None;
        }
        let json: serde_json::Value = serde_json::from_slice(&rec.json.0).ok()?;
        let cond = json.get("condition")?;
        let property = cond.get("name")?.as_str()?.to_owned();
        let value = match cond.get("condition")? {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        Some(Self {
            index: rec.index as u32,
            property,
            value,
        })
    }
}

/// One frame of an animated sprite sheet, in UV units of its image slot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpriteFrame {
    pub image: u32,
    pub frametime: f32,
    pub x: f32,
    pub y: f32,
    pub x_axis: [f32; 2],
    pub y_axis: [f32; 2],
    /// Frame size in pixels.
    pub width: f32,
    pub height: f32,
    /// `height / width`.
    pub rate: f32,
}

#[derive(Debug, Clone)]
pub struct Mip {
    pub width: u32,
    pub height: u32,
    /// Slice count of a 3D texture, 1 otherwise. Slices are stored back to back.
    pub depth: u32,
    pub data: Vec<u8>,
}

/// One image slot (cube face / sprite page): its mip chain, largest first.
#[derive(Debug, Clone, Default)]
pub struct TexImage {
    pub mips: Vec<Mip>,
}

#[derive(Debug, Clone)]
pub struct Texture {
    /// Format of every mip's `data`. Embedded PNG/JPEG/... images are
    /// decoded to RGBA8.
    pub format: TexFormat,
    pub flags: TexFlags,
    pub texb_version: u32,
    /// Container size (usually power of two).
    pub width: u32,
    pub height: u32,
    /// Real picture size inside the container; for sprites, one frame.
    pub map_width: u32,
    pub map_height: u32,
    /// Slice count of a 3D texture ([`TexFlags::VOLUME`]), 1 otherwise.
    pub depth: u32,
    pub images: Vec<TexImage>,
    pub sprite_frames: Vec<SpriteFrame>,
    pub variants: Vec<VariantCondition>,
    /// MP4 payload when the texture is actually a video. Mips then hold a
    /// black RGBA8 placeholder.
    pub video: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DecodeOptions {
    /// Variant index to apply (0 = base image). See [`Texture::select_variant`].
    pub variant: u32,
}

impl Texture {
    pub fn parse(bytes: &[u8]) -> Result<Self, TexError> {
        Self::parse_with(bytes, DecodeOptions::default())
    }

    pub fn parse_with(bytes: &[u8], opts: DecodeOptions) -> Result<Self, TexError> {
        let raw = RawTex::read(&mut Cursor::new(bytes))?;
        let h = &raw.header;
        let embedded = h.texb >= 3 && h.image_type != -1;
        let mut format = h.format;
        let mut video = None;
        let mut total = 0usize;

        let mut images = Vec::with_capacity(raw.images.len());
        for raw_image in raw.images {
            let mut mips = Vec::with_capacity(raw_image.mips.len());
            for raw_mip in raw_image.mips {
                if raw_mip.width == 0 || raw_mip.height == 0 {
                    return Err(TexError::InvalidMip(format!(
                        "{}x{}",
                        raw_mip.width, raw_mip.height
                    )));
                }
                let data = if raw_mip.is_lz4 == 1 {
                    let want = raw_mip.decompressed_size as usize;
                    let out = lz4_flex::block::decompress(&raw_mip.data, want)?;
                    if out.len() != want {
                        return Err(TexError::Lz4Size {
                            got: out.len(),
                            want,
                        });
                    }
                    out
                } else {
                    raw_mip.data
                };

                let mip = if data.len() >= 8 && &data[4..8] == b"ftyp" {
                    let placeholder = vec![0; raw_mip.width as usize * raw_mip.height as usize * 4];
                    if video.is_none() {
                        video = Some(data);
                    }
                    format = TexFormat::Rgba8;
                    Mip {
                        width: raw_mip.width,
                        height: raw_mip.height,
                        depth: 1,
                        data: placeholder,
                    }
                } else if embedded {
                    let img = decode_embedded(&data, h.image_type)?;
                    // Volume slices are stacked vertically in the embedded image.
                    let stacked_height = raw_mip.height * raw_mip.depth;
                    if img.width() > raw_mip.width || img.height() > stacked_height {
                        return Err(TexError::InvalidMip(format!(
                            "embedded image {}x{} larger than its {}x{} mip",
                            img.width(),
                            img.height(),
                            raw_mip.width,
                            stacked_height
                        )));
                    }
                    format = TexFormat::Rgba8;
                    let depth = raw_mip.depth;
                    Mip {
                        width: img.width(),
                        height: img.height() / depth,
                        depth,
                        data: img.into_raw(),
                    }
                } else {
                    let mut mip = Mip {
                        width: raw_mip.width,
                        height: raw_mip.height,
                        depth: raw_mip.depth,
                        data,
                    };
                    if opts.variant != 0
                        && let Some(trailer) = &raw_mip.trailer
                    {
                        apply_variant(&mut mip, format, trailer, opts.variant);
                    }
                    mip
                };

                total += mip.data.len();
                if total > MAX_TOTAL {
                    return Err(TexError::TooLarge);
                }
                mips.push(mip);
            }
            images.push(TexImage { mips });
        }

        let sprite_frames = raw
            .sprite
            .map(|s| sprite_frames(&s, &images))
            .unwrap_or_default();
        let variants = raw
            .header
            .variants
            .iter()
            .flat_map(|t| &t.records)
            .filter_map(VariantCondition::parse)
            .collect();

        Ok(Self {
            format,
            flags: h.flags,
            texb_version: h.texb,
            width: h.width,
            height: h.height,
            map_width: h.map_width,
            map_height: h.map_height,
            depth: h.depth,
            images,
            sprite_frames,
            variants,
            video,
        })
    }

    /// The variant the given predicate selects: the first condition that
    /// holds, else 0 (the base image).
    pub fn select_variant(
        conditions: &[VariantCondition],
        mut holds: impl FnMut(&str, &str) -> bool,
    ) -> u32 {
        conditions
            .iter()
            .find(|c| holds(&c.property, &c.value))
            .map_or(0, |c| c.index)
    }

    pub fn is_sprite(&self) -> bool {
        self.flags.contains(TexFlags::SPRITE) && !self.sprite_frames.is_empty()
    }

    /// Size of the first mip of the first image.
    pub fn mip0_size(&self) -> Option<(u32, u32)> {
        let m = self.images.first()?.mips.first()?;
        Some((m.width, m.height))
    }
}

fn decode_embedded(data: &[u8], image_type: i32) -> Result<image::RgbaImage, TexError> {
    let format = match image::guess_format(data) {
        Ok(f) => f,
        Err(_) if image_type == FREEIMAGE_TARGA => image::ImageFormat::Tga,
        Err(e) => return Err(e.into()),
    };
    Ok(image::load_from_memory_with_format(data, format)?.into_rgba8())
}

fn sprite_frames(sprite: &RawSprite, images: &[TexImage]) -> Vec<SpriteFrame> {
    let mut frames = Vec::with_capacity(sprite.frames.len());
    for f in &sprite.frames {
        let Some(mip0) = images.get(f.image as usize).and_then(|i| i.mips.first()) else {
            tracing::error!("sprite frame references missing image {}", f.image);
            return Vec::new();
        };
        let sw = (mip0.width as f32).max(1.0);
        let sh = (mip0.height as f32).max(1.0);
        let c = match (f.int_coords, f.float_coords) {
            (Some(i), _) => i.map(|v| v as f32),
            (_, Some(fl)) => fl,
            _ => unreachable!("one of the coordinate encodings is always read"),
        };
        let x_axis = [c[2], c[3]];
        let y_axis = [c[4], c[5]];
        let width = x_axis[0].hypot(x_axis[1]);
        let height = y_axis[0].hypot(y_axis[1]);
        let rate = if width > 1e-6 { height / width } else { 1.0 };
        frames.push(SpriteFrame {
            image: f.image,
            frametime: f.frametime,
            x: c[0] / sw,
            y: c[1] / sh,
            x_axis: [x_axis[0] / sw, x_axis[1] / sw],
            y_axis: [y_axis[0] / sh, y_axis[1] / sh],
            width,
            height,
            rate,
        });
    }
    frames
}

fn apply_variant(mip: &mut Mip, format: TexFormat, trailer: &VariantTrailer, variant: u32) {
    for p in &trailer.patches {
        if p.index as u32 != variant {
            continue;
        }
        if trailer.lead != 1 || p.tag != 1 || p.kind != 13 {
            tracing::info!("skipping variant patch with unknown fields");
            continue;
        }
        let want = format.region_bytes(p.w, p.h);
        let ok = lz4_flex::block::decompress(&p.data, want)
            .ok()
            .filter(|raw| raw.len() == want)
            .is_some_and(|raw| blit_patch(format, mip, &raw, p.x, p.y, p.w, p.h));
        if !ok {
            tracing::error!(
                "variant patch {}x{} at ({},{}) does not fit the {}x{} mip",
                p.w,
                p.h,
                p.x,
                p.y,
                mip.width,
                mip.height
            );
        }
    }
}

/// Copy a `w × h` patch over `(x, y)` of `mip`. The rectangle must start on a
/// block boundary; its far edge may stop mid-block only at the mip's edge.
fn blit_patch(fmt: TexFormat, mip: &mut Mip, patch: &[u8], x: u32, y: u32, w: u32, h: u32) -> bool {
    let bd = fmt.block_dim();
    if w == 0 || h == 0 || !x.is_multiple_of(bd) || !y.is_multiple_of(bd) {
        return false;
    }
    let end_x = match x.checked_add(w) {
        Some(end) if end <= mip.width => end,
        _ => return false,
    };
    let end_y = match y.checked_add(h) {
        Some(end) if end <= mip.height => end,
        _ => return false,
    };
    if (!w.is_multiple_of(bd) && end_x != mip.width)
        || (!h.is_multiple_of(bd) && end_y != mip.height)
    {
        return false;
    }
    if fmt.region_bytes(mip.width, mip.height) != mip.data.len()
        || fmt.region_bytes(w, h) != patch.len()
    {
        return false;
    }
    let dst_stride = fmt.region_bytes(mip.width, bd);
    let row_bytes = fmt.region_bytes(w, bd);
    let dst_x = (x / bd) as usize * fmt.bytes_per_unit();
    for r in 0..h.div_ceil(bd) as usize {
        let dst = (y / bd) as usize * dst_stride + r * dst_stride + dst_x;
        let patch_start = r * row_bytes;
        let patch_end = patch_start + row_bytes;
        if patch_end > patch.len() {
            return false;
        }
        let Some(dst_slice) = mip.data.get_mut(dst..dst + row_bytes) else {
            return false;
        };
        dst_slice.copy_from_slice(&patch[patch_start..patch_end]);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal little-endian writer for synthetic `.tex` fixtures.
    #[derive(Default)]
    struct W(Vec<u8>);
    impl W {
        fn tag(&mut self, s: &str) -> &mut Self {
            self.0.extend(s.as_bytes());
            self.0.push(0);
            self
        }
        fn u(&mut self, v: u32) -> &mut Self {
            self.0.extend(v.to_le_bytes());
            self
        }
        fn i(&mut self, v: i32) -> &mut Self {
            self.0.extend(v.to_le_bytes());
            self
        }
        fn f(&mut self, v: f32) -> &mut Self {
            self.0.extend(v.to_le_bytes());
            self
        }
        fn bytes(&mut self, b: &[u8]) -> &mut Self {
            self.0.extend(b);
            self
        }
    }

    fn header(w: &mut W, format: u32, flags: u32, size: (u32, u32), texb: u32, images: u32) {
        w.tag("TEXV0005").tag("TEXI0001").u(format).u(flags);
        w.u(size.0).u(size.1).u(size.0).u(size.1).u(0);
        w.tag(&format!("TEXB{texb:04}")).u(images);
    }

    #[test]
    fn raw_rgba8_texb1() {
        let mut w = W::default();
        header(&mut w, 0, TexFlags::CLAMP_UVS.bits(), (2, 1), 1, 1);
        w.u(1).u(2).u(1).u(8).bytes(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let t = Texture::parse(&w.0).unwrap();
        assert_eq!(t.format, TexFormat::Rgba8);
        assert!(t.flags.contains(TexFlags::CLAMP_UVS));
        assert_eq!(t.images[0].mips[0].data, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(t.sprite_frames.is_empty());
    }

    #[test]
    fn lz4_mip_texb2() {
        let pixels: Vec<u8> = (0..64u8).collect();
        let packed = lz4_flex::block::compress(&pixels);
        let mut w = W::default();
        header(&mut w, 0, 0, (4, 4), 2, 1);
        w.u(1)
            .u(4)
            .u(4)
            .u(1)
            .u(64)
            .u(packed.len() as u32)
            .bytes(&packed);
        let t = Texture::parse(&w.0).unwrap();
        assert_eq!(t.images[0].mips[0].data, pixels);
    }

    #[test]
    fn embedded_png_texb3() {
        let img = image::RgbaImage::from_pixel(3, 2, image::Rgba([9, 8, 7, 255]));
        let mut png = Vec::new();
        img.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let mut w = W::default();
        header(&mut w, 0, 0, (4, 2), 3, 1);
        w.i(13); // FreeImage PNG
        w.u(1).u(4).u(2).u(0).u(0).u(png.len() as u32).bytes(&png);
        let t = Texture::parse(&w.0).unwrap();
        let mip = &t.images[0].mips[0];
        assert_eq!((mip.width, mip.height), (3, 2));
        assert_eq!(&mip.data[..4], &[9, 8, 7, 255]);
    }

    #[test]
    fn bc1_with_sprite_table_texs2() {
        let mut w = W::default();
        header(&mut w, 7, TexFlags::SPRITE.bits(), (8, 4), 2, 1);
        let data = vec![0u8; TexFormat::Bc1.region_bytes(8, 4)];
        w.u(1).u(8).u(4).u(0).u(0).u(data.len() as u32).bytes(&data);
        w.tag("TEXS0002").u(2);
        for (i, x) in [0.0f32, 4.0].into_iter().enumerate() {
            w.u(0)
                .f(0.5 + i as f32)
                .f(x)
                .f(0.0)
                .f(4.0)
                .f(0.0)
                .f(0.0)
                .f(4.0);
        }
        let t = Texture::parse(&w.0).unwrap();
        assert_eq!(t.format, TexFormat::Bc1);
        assert!(t.is_sprite());
        let f = t.sprite_frames[1];
        assert_eq!(
            (f.x, f.y, f.width, f.height, f.rate),
            (0.5, 0.0, 4.0, 4.0, 1.0)
        );
        assert_eq!(f.x_axis, [0.5, 0.0]);
        assert_eq!(f.y_axis, [0.0, 1.0]);
    }

    #[test]
    fn variant_patch_texb4() {
        let base = vec![0u8; 16];
        let patch = lz4_flex::block::compress(&[0xAA; 4]);
        let json = br#"{"condition":{"name":"face","condition":"2"}}"#;
        let mut w = W::default();
        header(&mut w, 0, 0, (2, 2), 4, 1);
        w.i(-1).u(1).i(1).i(1).i(0).bytes(json).bytes(&[0]);
        w.u(1).u(2).u(2).u(0).u(0).u(16).bytes(&base);
        w.i(1).u(1); // trailer: lead, one patch
        w.i(1)
            .i(1)
            .u(1)
            .u(1)
            .u(1)
            .u(1)
            .i(13)
            .u(patch.len() as u32)
            .bytes(&patch);

        let t = Texture::parse(&w.0).unwrap();
        assert_eq!(t.variants.len(), 1);
        assert_eq!(t.variants[0].property, "face");
        assert_eq!(t.images[0].mips[0].data, base);

        let v = Texture::select_variant(&t.variants, |p, v| p == "face" && v == "2");
        assert_eq!(v, 1);
        let t = Texture::parse_with(&w.0, DecodeOptions { variant: v }).unwrap();
        let px = &t.images[0].mips[0].data;
        assert_eq!(&px[12..16], &[0xAA; 4]);
        assert_eq!(&px[..12], &[0; 12]);
    }

    #[test]
    fn volume_with_embedded_png() {
        // 2x2x3 volume stored as a 2x6 PNG of vertically stacked slices.
        let img = image::RgbaImage::from_fn(2, 6, |_, y| image::Rgba([y as u8 / 2, 0, 0, 255]));
        let mut png = Vec::new();
        img.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let mut w = W::default();
        w.tag("TEXV0005")
            .tag("TEXI0001")
            .u(0)
            .u(TexFlags::VOLUME.bits());
        w.u(2).u(2).u(6).u(2).u(3).u(0xff5555aa);
        w.tag("TEXB0004").u(1).i(13).u(0);
        w.u(1)
            .u(2)
            .u(2)
            .u(3)
            .u(0)
            .u(0)
            .u(png.len() as u32)
            .bytes(&png);
        let t = Texture::parse(&w.0).unwrap();
        assert_eq!(t.depth, 3);
        let mip = &t.images[0].mips[0];
        assert_eq!((mip.width, mip.height, mip.depth), (2, 2, 3));
        assert_eq!(
            mip.data[2 * 2 * 4 * 2],
            2,
            "third slice starts after two 2x2 slices"
        );
    }

    #[test]
    fn rejects_hostile_counts() {
        let mut w = W::default();
        header(&mut w, 0, 0, (1, 1), 1, 100_000);
        assert!(matches!(Texture::parse(&w.0), Err(TexError::Malformed(_))));

        let mut w = W::default();
        header(&mut w, 0, 0, (1, 1), 1, 1);
        w.u(1).u(1).u(1).u(u32::MAX);
        assert!(matches!(Texture::parse(&w.0), Err(TexError::Malformed(_))));
    }

    #[test]
    fn rejects_bad_tag() {
        let mut w = W::default();
        w.tag("XXXV0005");
        assert!(Texture::parse(&w.0).is_err());
    }

    #[test]
    fn blit_patch_rejects_overflow() {
        let mut mip = Mip {
            width: 16,
            height: 16,
            depth: 1,
            data: vec![0u8; 16 * 16 * 4],
        };
        let patch = vec![0u8; 4 * 4 * 4];
        // Overflow in x + w
        assert!(!blit_patch(TexFormat::Rgba8, &mut mip, &patch, u32::MAX - 2, 0, 4, 4));
        // Out of bounds without wrapping
        assert!(!blit_patch(TexFormat::Rgba8, &mut mip, &patch, 14, 0, 4, 4));
    }
}
