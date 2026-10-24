use std::io::Cursor;

use binrw::{BinRead, BinReaderExt, NullString, binread};
use glam::{Mat4, Vec3};

use crate::{
    AnimationEvent, Attachment, Bone, BoneFrame, Mdl, MdlAnimation, MdlError, MdlMesh, PlayMode,
    Skeleton, VertexFormat,
};

const BONE_FRAME_BYTES: u32 = 36;
const MIN_BONE_BYTES: usize = 78;
const MAX_TAG_SCAN: usize = 1 << 24;

#[binread]
#[br(little)]
struct BoneRecord {
    name: NullString,
    _unk: i32,
    parent: i32,
    matrix_bytes: u32,
    #[br(if(matrix_bytes == 64))]
    matrix: [f32; 16],
    #[br(if(matrix_bytes == 64))]
    simulation: NullString,
}

#[binread]
#[br(little)]
struct AttachmentRecord {
    bone: u16,
    name: NullString,
    matrix: [f32; 16],
}

#[derive(BinRead)]
#[br(little)]
struct FrameRecord {
    position: [f32; 3],
    angle: [f32; 3],
    scale: [f32; 3],
}

struct Reader<'a> {
    cur: Cursor<&'a [u8]>,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            cur: Cursor::new(bytes),
        }
    }

    fn data(&self) -> &'a [u8] {
        self.cur.get_ref()
    }

    fn len(&self) -> u64 {
        self.data().len() as u64
    }

    fn pos(&self) -> u64 {
        self.cur.position()
    }

    fn seek(&mut self, pos: u64) {
        self.cur.set_position(pos);
    }

    fn skip(&mut self, n: u64) -> Result<(), MdlError> {
        let to = self.pos().saturating_add(n);
        if to > self.len() {
            return Err(self.malformed(format!("skip of {n} bytes runs past the end")));
        }
        self.seek(to);
        Ok(())
    }

    fn remaining(&self) -> u64 {
        self.len().saturating_sub(self.pos())
    }

    fn read<T>(&mut self) -> Result<T, MdlError>
    where
        T: for<'b> BinRead<Args<'b> = ()>,
    {
        Ok(self.cur.read_le::<T>()?)
    }

    fn u8(&mut self) -> Result<u8, MdlError> {
        self.read()
    }

    fn u16(&mut self) -> Result<u16, MdlError> {
        self.read()
    }

    fn u32(&mut self) -> Result<u32, MdlError> {
        self.read()
    }

    fn i32(&mut self) -> Result<i32, MdlError> {
        self.read()
    }

    fn f32(&mut self) -> Result<f32, MdlError> {
        self.read()
    }

    fn cstr(&mut self) -> Result<String, MdlError> {
        let s: NullString = self.read()?;
        Ok(String::from_utf8_lossy(&s.0).into_owned())
    }

    fn mat4(&mut self) -> Result<Mat4, MdlError> {
        Ok(Mat4::from_cols_array(&self.read::<[f32; 16]>()?))
    }

    fn take(&mut self, n: u64) -> Result<&'a [u8], MdlError> {
        let start = self.pos() as usize;
        self.skip(n)?;
        Ok(&self.data()[start..start + n as usize])
    }

    fn ensure_count(&self, count: u64, min_bytes: u64, what: &str) -> Result<(), MdlError> {
        if count.saturating_mul(min_bytes) > self.remaining() {
            return Err(self.malformed(format!("{what} count {count} exceeds the data")));
        }
        Ok(())
    }

    fn malformed(&self, what: impl Into<String>) -> MdlError {
        MdlError::Malformed {
            offset: self.pos(),
            what: what.into(),
        }
    }
}

/// A section tag `XXXXnnnn` (four ASCII capitals, four digits).
fn section_tag(tag: &str) -> Option<(&str, u32)> {
    let b = tag.as_bytes();
    if b.len() != 8
        || !b[..4].iter().all(u8::is_ascii_uppercase)
        || !b[4..].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    Some((&tag[..4], tag[4..].parse().ok()?))
}

fn find_known_section(data: &[u8], from: usize) -> Option<usize> {
    let end = data.len().min(from.saturating_add(MAX_TAG_SCAN));
    (from..end.saturating_sub(8)).find(|&i| {
        let w = &data[i..];
        matches!(&w[..4], b"MDLS" | b"MDAT" | b"MDLA")
            && w[4..8].iter().all(u8::is_ascii_digit)
            && w[8] == 0
    })
}

pub(crate) fn parse(bytes: &[u8]) -> Result<Mdl, MdlError> {
    if bytes.len() < 9 || &bytes[..4] != b"MDLV" || !bytes[4..8].iter().all(u8::is_ascii_digit) {
        return Err(MdlError::BadMagic);
    }
    let version: u32 = std::str::from_utf8(&bytes[4..8])
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or(MdlError::BadMagic)?;
    let mut r = Reader::new(bytes);
    r.seek(9);
    let mut mdl = Mdl {
        version,
        format: r.u32()?,
        ..Mdl::default()
    };
    let _unk = r.u32()?;
    let submeshes = r.u32()?;
    r.ensure_count(submeshes.into(), 10, "submesh")?;

    let mut resync_from = None;
    for index in 0..submeshes as usize {
        let after_indices = submesh(&mut r, &mut mdl, index)?;
        if version >= 21 {
            match post_submesh(&mut r, version) {
                Ok(masks) => {
                    for mask in masks {
                        mdl.warnings.push(format!(
                            "submesh {index}: clipping mask {mask} is not applied"
                        ));
                    }
                }
                Err(_) => {
                    resync_from = Some(after_indices);
                    break;
                }
            }
        }
    }

    let start = r.pos();
    if resync_from.is_some() || !section_start_ok(&mut r) {
        let from = resync_from.unwrap_or(start) as usize;
        match find_known_section(bytes, from) {
            Some(at) => {
                mdl.warnings.push(format!(
                    "section data did not follow the mesh at {start}; resynced at {at}"
                ));
                r.seek(at as u64);
            }
            None => r.seek(r.len()),
        }
    }
    sections(&mut r, &mut mdl)?;
    finish_skinning(&mut mdl);
    Ok(mdl)
}

fn section_start_ok(r: &mut Reader) -> bool {
    let pos = r.pos();
    let ok = match r.cstr() {
        Ok(tag) => tag.is_empty() || section_tag(&tag).is_some(),
        Err(_) => false,
    };
    r.seek(pos);
    ok || pos >= r.len()
}

struct SubmeshHeader {
    wide_flag: u32,
    bounds: Option<([f32; 3], [f32; 3])>,
    format: u32,
    vertex_bytes: u32,
}

fn submesh_header(
    r: &mut Reader,
    version: u32,
    file_format: u32,
    extra: u64,
) -> Result<SubmeshHeader, MdlError> {
    let wide_flag = r.u32()?;
    r.skip(extra)?;
    let bounds = if version >= 17 {
        let b: [f32; 6] = r.read()?;
        Some(([b[0], b[1], b[2]], [b[3], b[4], b[5]]))
    } else {
        None
    };
    let format = if version >= 16 { r.u32()? } else { file_format };
    let vertex_bytes = r.u32()?;
    Ok(SubmeshHeader {
        wide_flag,
        bounds,
        format,
        vertex_bytes,
    })
}

fn header_plausible(r: &mut Reader, h: &SubmeshHeader) -> bool {
    let pos = r.pos();
    let ok = h.format & VertexFormat::POSITION.bits() != 0
        && u64::from(h.vertex_bytes) + 4 <= r.remaining()
        && {
            r.seek(pos + u64::from(h.vertex_bytes));
            r.u32()
                .is_ok_and(|ib| u64::from(ib) <= r.remaining() && ib % 2 == 0)
        };
    r.seek(pos);
    ok
}

/// Parse one submesh; returns the offset just past its index data.
fn submesh(r: &mut Reader, mdl: &mut Mdl, index: usize) -> Result<u64, MdlError> {
    let material = r.cstr()?;
    let header_start = r.pos();
    let mut header = submesh_header(r, mdl.version, mdl.format, 0)?;
    if mdl.version >= 17 && !header_plausible(r, &header) {
        r.seek(header_start);
        if let Ok(alt) = submesh_header(r, mdl.version, mdl.format, 4)
            && header_plausible(r, &alt)
        {
            header = alt;
        } else {
            r.seek(header_start);
            header = submesh_header(r, mdl.version, mdl.format, 0)?;
        }
    }

    let format = VertexFormat::from_bits_retain(header.format);
    let material = (!material.is_empty()).then_some(material);
    let vertex_data = r.take(header.vertex_bytes.into())?;
    let index_bytes = r.u32()?;

    let Some(stride) = format
        .stride()
        .filter(|&s| s > 0 && (header.vertex_bytes as usize).is_multiple_of(s))
    else {
        r.skip(index_bytes.into())?;
        mdl.warnings.push(format!(
            "submesh {index} ({}): unsupported vertex format {:#x} with {} vertex bytes; skipped",
            material.as_deref().unwrap_or(""),
            header.format,
            header.vertex_bytes
        ));
        return Ok(r.pos());
    };
    let vertex_count = header.vertex_bytes as usize / stride;
    let mut mesh = decode_vertices(vertex_data, format, vertex_count);
    mesh.material = material;
    mesh.bounds = header.bounds;

    let wide = header.wide_flag == 1 || (vertex_count > 0xFFFF && index_bytes % 12 == 0);
    let triangle_bytes = if wide { 12 } else { 6 };
    if index_bytes % triangle_bytes != 0 {
        return Err(r.malformed(format!(
            "submesh {index}: index data size {index_bytes} is not a whole number of triangles"
        )));
    }
    let data = r.take(index_bytes.into())?;
    let indices: Vec<u32> = if wide {
        data.chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    } else {
        data.chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]).into())
            .collect()
    };
    let total = indices.len() / 3;
    mesh.indices = indices
        .chunks_exact(3)
        .filter(|t| t.iter().all(|&i| (i as usize) < vertex_count))
        .flatten()
        .copied()
        .collect();
    let dropped = total - mesh.indices.len() / 3;
    if dropped > 0 {
        mdl.warnings.push(format!(
            "submesh {index}: dropped {dropped} triangle(s) indexing past {vertex_count} vertices"
        ));
    }
    mesh.wide_indices = wide;
    mdl.meshes.push(mesh);
    Ok(r.pos())
}

fn decode_vertices(data: &[u8], format: VertexFormat, count: usize) -> MdlMesh {
    let mut mesh = MdlMesh {
        format,
        ..MdlMesh::default()
    };
    let stride = data.len().checked_div(count).unwrap_or(0);
    let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
    let u = |b: &[u8], at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
    let has = |flag| format.contains(flag);
    for v in data.chunks_exact(stride.max(1)).take(count) {
        let mut at = 0;
        let mut floats = |n: usize| {
            let start = at;
            at += n * 4;
            start
        };
        if has(VertexFormat::POSITION) {
            let o = floats(3);
            mesh.positions.push([f(v, o), f(v, o + 4), f(v, o + 8)]);
        }
        if has(VertexFormat::NORMAL) {
            let o = floats(3);
            mesh.normals.push([f(v, o), f(v, o + 4), f(v, o + 8)]);
        }
        if has(VertexFormat::TANGENT) {
            let o = floats(4);
            mesh.tangents
                .push([f(v, o), f(v, o + 4), f(v, o + 8), f(v, o + 12)]);
        }
        if has(VertexFormat::BLEND_INDICES) {
            let o = floats(4);
            mesh.blend_indices
                .push([u(v, o), u(v, o + 4), u(v, o + 8), u(v, o + 12)]);
        }
        if has(VertexFormat::BLEND_WEIGHTS) {
            let o = floats(4);
            mesh.blend_weights
                .push([f(v, o), f(v, o + 4), f(v, o + 8), f(v, o + 12)]);
        }
        if has(VertexFormat::TEXCOORD) {
            let o = floats(2);
            mesh.texcoords.push([f(v, o), f(v, o + 4)]);
        }
        if has(VertexFormat::TEXCOORD_VEC4) {
            let o = floats(4);
            mesh.texcoords.push([f(v, o), f(v, o + 4)]);
            mesh.texcoords1.push([f(v, o + 8), f(v, o + 12)]);
        }
    }
    mesh
}

/// Morph targets, part ranges and (v23) clipping masks stored after each
/// submesh in v21+; returns the clipping-mask texture paths.
fn post_submesh(r: &mut Reader, version: u32) -> Result<Vec<String>, MdlError> {
    if r.u8()? != 0 {
        let morphs = r.u32()?;
        r.ensure_count(morphs.into(), 4, "morph target")?;
        for _ in 0..morphs {
            let bytes = r.u32()?;
            r.skip(bytes.into())?;
        }
    }
    if r.u8()? != 0 {
        let bytes = r.u32()?;
        r.skip(bytes.into())?;
    }
    let mut masks = Vec::new();
    if version >= 23 {
        let count = r.u32()?;
        r.ensure_count(count.into(), 25, "clipping mask")?;
        for _ in 0..count {
            let _id = r.u32()?;
            let _unk = r.u32()?;
            masks.push(r.cstr()?);
            let _unk = r.u32()?;
            for _ in 0..2 {
                let n = r.u32()?;
                r.skip(u64::from(n) * 4)?;
            }
        }
    }
    Ok(masks)
}

fn sections(r: &mut Reader, mdl: &mut Mdl) -> Result<(), MdlError> {
    while r.remaining() > 0 {
        let tag_pos = r.pos();
        let Ok(tag) = r.cstr() else { break };
        if tag.is_empty() {
            break;
        }
        let Some((kind, section_version)) = section_tag(&tag) else {
            match find_known_section(r.data(), tag_pos as usize + 1) {
                Some(at) => {
                    mdl.warnings
                        .push(format!("garbage at {tag_pos}; resynced at {at}"));
                    r.seek(at as u64);
                    continue;
                }
                None => {
                    mdl.warnings.push(format!("garbage at {tag_pos}; stopped"));
                    break;
                }
            }
        };
        let body = r.pos();
        let end = u64::from(r.u32()?);
        let end = (end > body && end <= r.len()).then_some(end);
        match kind {
            "MDLS" => mdl.skeleton = Some(skeleton(r, section_version, end)?),
            "MDAT" => mdl.attachments = attachments(r)?,
            "MDLA" => {
                mdl.animation_version = Some(section_version);
                animations(r, mdl, section_version, end.unwrap_or(r.len()));
            }
            other => {
                if end.is_none() {
                    mdl.warnings.push(format!(
                        "unknown section {other} without a valid end; stopped"
                    ));
                    break;
                }
            }
        }
        if let Some(end) = end {
            r.seek(end);
        }
    }
    Ok(())
}

fn skeleton(r: &mut Reader, version: u32, end: Option<u64>) -> Result<Skeleton, MdlError> {
    let count = usize::from(r.u16()?);
    let _unk = r.u16()?;
    r.ensure_count(count as u64, MIN_BONE_BYTES as u64, "bone")?;
    let mut bones = Vec::with_capacity(count);
    for i in 0..count {
        let rec: BoneRecord = r.read()?;
        if rec.matrix_bytes != 64 {
            return Err(r.malformed(format!(
                "bone {i}: transform is {} bytes, expected 64",
                rec.matrix_bytes
            )));
        }
        let parent = usize::try_from(rec.parent).ok().filter(|&p| p < i);
        let simulation = String::from_utf8_lossy(&rec.simulation.0).into_owned();
        bones.push(Bone {
            name: String::from_utf8_lossy(&rec.name.0).into_owned(),
            parent,
            bind: Mat4::from_cols_array(&rec.matrix),
            rest: None,
            simulation: (!simulation.is_empty()).then_some(simulation),
        });
    }
    if version > 1 {
        let _unk: i16 = r.read()?;
        if r.u8()? != 0 {
            for bone in &mut bones {
                bone.rest = Some(r.mat4()?);
            }
        }
        if end.is_none() {
            let n = r.u32()?;
            r.skip(u64::from(n) * 12)?;
            let _unk = r.u32()?;
            if r.u8()? != 0 {
                r.skip(count as u64 * 76)?;
            }
            if r.u8()? != 0 {
                r.skip(count as u64 * 4)?;
            }
            if version >= 3 && r.u8()? != 0 {
                r.skip(count as u64 * 4)?;
            }
        }
    }
    Ok(Skeleton { version, bones })
}

fn attachments(r: &mut Reader) -> Result<Vec<Attachment>, MdlError> {
    let count = r.u16()?;
    r.ensure_count(count.into(), 67, "attachment")?;
    (0..count)
        .map(|_| {
            let rec: AttachmentRecord = r.read()?;
            Ok(Attachment {
                name: String::from_utf8_lossy(&rec.name.0).into_owned(),
                bone: rec.bone.into(),
                transform: Mat4::from_cols_array(&rec.matrix),
            })
        })
        .collect()
}

fn play_mode(s: &str) -> Option<PlayMode> {
    match s {
        "loop" | "" => Some(PlayMode::Loop),
        "mirror" => Some(PlayMode::Mirror),
        "single" => Some(PlayMode::Single),
        _ => None,
    }
}

/// Whether an animation header (`i32 id, i32, cstr name, cstr mode`) starts at `at`.
fn looks_like_animation(data: &[u8], at: usize) -> bool {
    let Some(head) = data.get(at..at + 8) else {
        return false;
    };
    let id = i32::from_le_bytes([head[0], head[1], head[2], head[3]]);
    if id <= 0 {
        return false;
    }
    let cstr = |from: usize| -> Option<(&[u8], usize)> {
        let rest = data.get(from..)?;
        let n = rest.iter().take(256).position(|&b| b == 0)?;
        Some((&rest[..n], from + n + 1))
    };
    let known = |s: &[u8]| matches!(s, b"loop" | b"mirror" | b"single");
    let Some((name, next)) = cstr(at + 8) else {
        return false;
    };
    if std::str::from_utf8(name).is_err() {
        return false;
    }
    match cstr(next) {
        Some((mode, _)) if known(mode) => true,
        Some((second, after)) if name.is_empty() && !second.is_empty() => {
            cstr(after).is_some_and(|(mode, _)| known(mode))
        }
        _ => false,
    }
}

fn animations(r: &mut Reader, mdl: &mut Mdl, version: u32, end: u64) {
    let Ok(count) = r.u32() else { return };
    for k in 0..count {
        let data = r.data();
        let from = r.pos() as usize;
        let limit = (end as usize).min(data.len());
        let found = if looks_like_animation(data, from) {
            Some(from)
        } else {
            (from..limit)
                .find(|&i| looks_like_animation(data, i))
                .map(|i| {
                    if looks_like_animation(data, i + 1) {
                        i + 1
                    } else {
                        i
                    }
                })
        };
        let Some(at) = found else {
            mdl.warnings.push(format!(
                "animation {k}/{count}: header not found after offset {from}"
            ));
            return;
        };
        r.seek(at as u64);
        match animation(r, version) {
            Ok(Some(anim)) => mdl.animations.push(anim),
            Ok(None) => mdl
                .warnings
                .push(format!("animation {k}: unusable timing; dropped")),
            Err(e) => {
                mdl.warnings
                    .push(format!("animation {k}: {e}; later animations dropped"));
                return;
            }
        }
    }
}

fn animation(r: &mut Reader, version: u32) -> Result<Option<MdlAnimation>, MdlError> {
    let id = r.i32()?;
    let _unk = r.i32()?;
    let mut name = r.cstr()?;
    let mut mode_str = r.cstr()?;
    if name.is_empty() && play_mode(&mode_str).is_none() {
        name = std::mem::take(&mut mode_str);
        mode_str = r.cstr()?;
    }
    let mode = play_mode(&mode_str).unwrap_or_default();
    let fps = r.f32()?;
    let length = r.i32()?;
    let _unk = r.i32()?;
    let track_count = r.u32()?;
    r.ensure_count(track_count.into(), 8, "bone track")?;
    let mut tracks = Vec::with_capacity(track_count as usize);
    for t in 0..track_count {
        let _unk = r.i32()?;
        let bytes = r.u32()?;
        if bytes % BONE_FRAME_BYTES != 0 {
            return Err(r.malformed(format!("track {t}: frame data size {bytes}")));
        }
        let frames = bytes / BONE_FRAME_BYTES;
        r.ensure_count(frames.into(), BONE_FRAME_BYTES.into(), "frame")?;
        let track = (0..frames)
            .map(|_| {
                let f: FrameRecord = r.read()?;
                Ok(BoneFrame {
                    position: Vec3::from(f.position),
                    angle: Vec3::from(f.angle),
                    scale: Vec3::from(f.scale),
                })
            })
            .collect::<Result<Vec<_>, MdlError>>()?;
        tracks.push(track);
    }
    let events = if version <= 2 {
        events(r).unwrap_or_default()
    } else {
        skip_animation_trailer(r, version).ok();
        Vec::new()
    };
    if !(fps > 0.0 && fps.is_finite()) || length <= 0 {
        return Ok(None);
    }
    Ok(Some(MdlAnimation {
        id,
        name,
        mode,
        fps,
        length: length as u32,
        tracks,
        events,
    }))
}

/// Per-animation data after the bone tracks in MDLA v3+ (all zero in the
/// corpus apart from v6 morph-weight tracks).
fn skip_animation_trailer(r: &mut Reader, version: u32) -> Result<(), MdlError> {
    match version {
        3 => r.skip(9),
        5 => r.skip(34),
        6.. => {
            let tracks = r.u32()?;
            r.ensure_count(tracks.into(), 8, "morph weight track")?;
            for _ in 0..tracks {
                let _unk = r.u32()?;
                let bytes = r.u32()?;
                r.skip(bytes.into())?;
            }
            r.skip(31)
        }
        _ => Ok(()),
    }
}

fn events(r: &mut Reader) -> Result<Vec<AnimationEvent>, MdlError> {
    let count = r.u32()?;
    r.ensure_count(count.into(), 5, "event")?;
    let mut out = Vec::new();
    for _ in 0..count {
        let _time = r.f32()?;
        let json = r.cstr()?;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) else {
            continue;
        };
        let name = v.get("name").and_then(|n| n.as_str()).unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        out.push(AnimationEvent {
            frame: v
                .get("frame")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0) as i32,
            name: name.to_owned(),
        });
    }
    Ok(out)
}

/// Give every puppet vertex valid bone bindings.
fn finish_skinning(mdl: &mut Mdl) {
    let bones = mdl.bone_count() as u32;
    if bones == 0 {
        return;
    }
    for mesh in &mut mdl.meshes {
        let n = mesh.vertex_count();
        if mesh.blend_indices.len() != n {
            mesh.blend_indices = vec![[0; 4]; n];
        }
        if mesh.blend_weights.len() != n {
            mesh.blend_weights = vec![[1.0, 0.0, 0.0, 0.0]; n];
        }
        for idx in mesh.blend_indices.iter_mut().flatten() {
            if *idx >= bones {
                *idx = 0;
            }
        }
    }
}
