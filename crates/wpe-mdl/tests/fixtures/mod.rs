//! Byte-level `.mdl` builder for tests.

#![allow(dead_code)]

/// A growable little-endian byte buffer.
#[derive(Default)]
pub struct Bytes(pub Vec<u8>);

impl Bytes {
    pub fn mdlv(version: u32) -> Self {
        let mut b = Self::default();
        b.tag("MDLV", version);
        b
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    pub fn floats(&mut self, v: &[f32]) -> &mut Self {
        for &f in v {
            self.f32(f);
        }
        self
    }
    pub fn str(&mut self, s: &str) -> &mut Self {
        self.0.extend(s.as_bytes());
        self.0.push(0);
        self
    }
    pub fn tag(&mut self, kind: &str, version: u32) -> &mut Self {
        self.str(&format!("{kind}{version:04}"))
    }
    pub fn translated(&mut self, x: f32, y: f32, z: f32) -> &mut Self {
        self.floats(&[1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., x, y, z, 1.])
    }
    pub fn identity(&mut self) -> &mut Self {
        self.translated(0., 0., 0.)
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    /// Start a section: tag plus a placeholder end offset; returns the
    /// placeholder's position for [`Self::end_section`].
    pub fn section(&mut self, kind: &str, version: u32) -> usize {
        self.tag(kind, version);
        let at = self.len();
        self.u32(0);
        at
    }
    /// Patch a section's end offset to the current length.
    pub fn end_section(&mut self, at: usize) {
        let end = (self.len() as u32).to_le_bytes();
        self.0[at..at + 4].copy_from_slice(&end);
    }
}

pub const STD_PUPPET: u32 = 0x0180_0009;
pub const ALT_PUPPET: u32 = 0x0180_000F;

/// One puppet vertex in format `STD_PUPPET` (or `ALT_PUPPET` when `alt`).
pub fn puppet_vertex(
    b: &mut Bytes,
    pos: [f32; 3],
    bones: [u32; 4],
    w: [f32; 4],
    uv: [f32; 2],
    alt: bool,
) {
    b.floats(&pos);
    if alt {
        b.floats(&[0., 0., 1., 1., 0., 0., 1.]);
    }
    for i in bones {
        b.u32(i);
    }
    b.floats(&w).floats(&uv);
}

/// A bone record.
pub fn bone(b: &mut Bytes, parent: i32, t: [f32; 3], simulation: &str) {
    b.str("")
        .i32(1)
        .i32(parent)
        .u32(64)
        .translated(t[0], t[1], t[2])
        .str(simulation);
}

/// A bone frame (position, euler angles, scale).
pub fn frame(b: &mut Bytes, p: [f32; 3], angle: [f32; 3], s: [f32; 3]) {
    b.floats(&p).floats(&angle).floats(&s);
}
