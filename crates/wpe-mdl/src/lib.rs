//! Wallpaper Engine `.mdl` models: static meshes, puppet-warp skeletons and
//! their skeletal animations.
//!
//! # File layout
//!
//! ```text
//! "MDLVnnnn\0"  u32 format  u32 skin_count (usually 1)  u32 submesh_count
//! submesh × submesh_count:
//!     cstr material × skin_count   u32 index_kind (0 = u16, 1 = u32)
//!     [v ≥ 17] f32×6 bounding box
//!     [v ≥ 16] u32 vertex format (else the header format applies)
//!     u32 vertex_bytes  vertices   u32 index_bytes  indices
//!     [v ≥ 21] u8 has_morphs [u32 n, n × (u32 bytes, vec3 × vertex_count)]
//!              u8 has_parts  [u32 bytes, (u32 part, u32, u32 first, u32 count)…]
//!     [v ≥ 23] u32 (unknown, zero)
//! section × n: "MDLSnnnn\0" | "MDATnnnn\0" | "MDLAnnnn\0" | "MDLEnnnn\0" | …
//!     u32 absolute end offset of the section, body
//! "\0"  (or end of file)
//! ```
//!
//! # Blend shapes
//!
//! Blend shapes (morph targets, SceneScript `get/setBlendShapeWeight`) are
//! stored per submesh (v21+) and, in the corpus, only on puppet warp meshes:
//!
//! ```text
//! submesh trailer:  u8 has_morphs = 1, u32 n,
//!                   n × (u32 bytes = 12 × vertex_count, f32×3 per vertex)
//! "MDLE0002\0"      u32 end, u32 bytes = 64 × bone_count,
//!                   f32×16 per bone (column-major, parent-relative)
//! MDLA v6 animation, after the bone tracks:
//!                   u32 tracks, tracks × (u32 unknown (0), u32 bytes,
//!                   f32 weight per frame), 31 bytes (zero)
//! ```
//!
//! A target is not an offset but the full shaped mesh *as posed by the
//! `MDLE` skeleton* ([`Mdl::morph_pose`]): skinning the unshaped vertices with
//! `pose_world · bind_world⁻¹` reproduces the target to float precision
//! except on the vertices the shape moves (verified on every corpus file).
//! [`MorphTarget::deltas`] undoes that pose, giving offsets in the mesh's
//! space that Wallpaper Engine's shaders (`MORPHING` combo) add to the
//! position, scaled by `g_MorphWeights`, before skinning. Names are not
//! stored. Uncertain: whether `MDLE` holds one pose per target (every corpus
//! file has a single target; a multiple of `bone_count` matrices is read as
//! one pose per target), and the per-track `u32` before the weight data
//! (always 0; tracks are taken to map to targets by position). The shaders
//! also know per-target vertex alpha, normal deltas and bone "modifier"
//! masks; no file stores data recognisable as those.
//!
//! The vertex format is a bit set ([`VertexFormat`]); attributes are stored
//! interleaved in the order position, normal, tangent, blend indices, blend
//! weights, texcoord (vec2, or vec4 holding two texcoord sets).
//!
//! # Drawing a puppet
//!
//! An image object whose model JSON names a `puppet` replaces its quad with
//! [`Mdl::meshes`]`[0]` (any further submesh is auxiliary, e.g. a
//! `*_channelmap.json` material with [`VertexFormat::TEXCOORD_VEC4`]; the
//! reference ignores them). The reference renderer uploads the mesh unchanged:
//!
//! * **Positions** are in the layer's local pixel space, the same space as
//!   the image quad (`±size/2` around the object origin, +Y up, z mostly 0).
//!   They are *not* scaled by the layer size or the texture size; the
//!   object's model matrix (origin/scale/angles) applies on top, exactly like
//!   the quad. Across the corpus the unanimated mesh covers the layer's
//!   `width × height` rectangle.
//! * **Texcoords** are 0..1 over the authored (unpadded) image, +V down; in
//!   every corpus puppet `u = x / width + 0.5`, `v = 0.5 − y / height` at
//!   bind pose. The reference uploads them as-is, without the padded-texture
//!   UV rate it applies to the generated quad; when the layer texture is
//!   padded to a power of two, scale them by the same rate as the quad.
//! * The shader is the layer's material shader with combos `SKINNING=1` and
//!   `BONECOUNT=`[`Puppet::bone_count`] and attributes `a_Position` (vec3),
//!   `a_TexCoord` (vec2), `a_BlendIndices` (uvec4) and `a_BlendWeights`
//!   (vec4); `genericimage4` additionally reads `a_Normal` (vec3) under
//!   `SKINNING`, for which [`MdlMesh::normals`] or `(0, 0, 1)` can be bound.
//! * `g_Bones` (`uniform mat4x3 g_Bones[BONECOUNT]`) is filled from
//!   [`Puppet::bones`] each frame.
//! * When the layer has effects, the reference renders the image (and its
//!   effect chain) on the ordinary quad and applies the puppet mesh in the
//!   final composite pass, so the mesh then samples the effect output.

mod parse;
mod puppet;

use bitflags::bitflags;
use glam::{Mat4, Vec3};

pub use puppet::{AnimationLayer, LayerPose, Puppet};

/// Errors from [`Mdl::parse`].
#[derive(Debug, thiserror::Error)]
pub enum MdlError {
    /// The data does not start with an `MDLVnnnn` tag.
    #[error("not an MDL file (missing MDLV header)")]
    BadMagic,
    /// The data ended in the middle of a structure.
    #[error("truncated mdl: {0}")]
    Truncated(#[from] binrw::Error),
    /// A structure is inconsistent with the data around it.
    #[error("malformed mdl at offset {offset}: {what}")]
    Malformed {
        /// Byte offset where the problem was detected.
        offset: u64,
        /// What was wrong.
        what: String,
    },
}

bitflags! {
    /// Per-vertex attributes present in a submesh.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
    pub struct VertexFormat: u32 {
        /// `vec3` position.
        const POSITION = 1 << 0;
        /// `vec3` normal.
        const NORMAL = 1 << 1;
        /// `vec4` tangent (xyz + handedness).
        const TANGENT = 1 << 2;
        /// `vec2` primary texcoord.
        const TEXCOORD = 1 << 3;
        /// `vec4` texcoord pair (primary in xy, secondary/lightmap in zw),
        /// used instead of [`Self::TEXCOORD`].
        const TEXCOORD_VEC4 = 1 << 5;
        /// `uvec4` bone indices.
        const BLEND_INDICES = 1 << 23;
        /// `vec4` bone weights.
        const BLEND_WEIGHTS = 1 << 24;
    }
}

impl VertexFormat {
    /// Bytes per interleaved vertex, or `None` if the format has bits this
    /// crate does not know the size of.
    pub fn stride(self) -> Option<usize> {
        if self.bits() & !Self::all().bits() != 0 {
            return None;
        }
        let floats = [
            (Self::POSITION, 3),
            (Self::NORMAL, 3),
            (Self::TANGENT, 4),
            (Self::BLEND_INDICES, 4),
            (Self::BLEND_WEIGHTS, 4),
            (Self::TEXCOORD, 2),
            (Self::TEXCOORD_VEC4, 4),
        ]
        .iter()
        .filter(|(f, _)| self.contains(*f))
        .map(|(_, n)| n)
        .sum::<usize>();
        Some(floats * 4)
    }
}

/// A parsed `.mdl` file.
#[derive(Debug, Clone, Default)]
pub struct Mdl {
    /// `MDLV` version (13–23 seen in the wild).
    pub version: u32,
    /// Vertex format declared in the file header.
    pub format: u32,
    /// Submeshes, in file order. A puppet's warp mesh is the first one.
    pub meshes: Vec<MdlMesh>,
    /// Bone hierarchy (`MDLS` section); `None` for static models.
    pub skeleton: Option<Skeleton>,
    /// Named attachment points (`MDAT` section).
    pub attachments: Vec<Attachment>,
    /// Skeletal animations (`MDLA` section).
    pub animations: Vec<MdlAnimation>,
    /// `MDLA` section version, if present.
    pub animation_version: Option<u32>,
    /// `MDLE` section: parent-relative bone transforms (one per bone, in
    /// bone order) of the pose the blend shape targets were captured in.
    /// Empty without blend shapes.
    pub morph_pose: Vec<Mat4>,
    /// Recoverable problems: skipped submeshes, dropped triangles or
    /// animations, unknown sections.
    pub warnings: Vec<String>,
}

impl Mdl {
    /// Parse an `.mdl` file.
    pub fn parse(bytes: &[u8]) -> Result<Self, MdlError> {
        parse::parse(bytes)
    }

    /// Number of bones in the skeleton (0 for static models).
    pub fn bone_count(&self) -> usize {
        self.skeleton.as_ref().map_or(0, |s| s.bones.len())
    }

    /// Whether this file is a skinned puppet (has at least one bone).
    pub fn is_puppet(&self) -> bool {
        self.bone_count() > 0
    }

    /// Blend shapes of the puppet warp mesh (the first mesh).
    pub fn blend_shapes(&self) -> &[MorphTarget] {
        self.meshes.first().map_or(&[], |m| &m.morphs)
    }

    /// Look up an attachment by name.
    pub fn attachment(&self, name: &str) -> Option<&Attachment> {
        self.attachments.iter().find(|a| a.name == name)
    }
}

/// One submesh: de-interleaved vertex attributes and a triangle list.
///
/// Attribute vectors are either empty (absent from the format) or exactly
/// [`Self::vertex_count`] long. For puppets, `blend_indices` and
/// `blend_weights` are always filled (vertices of a format without them are
/// bound fully to bone 0) and every blend index is below the bone count.
#[derive(Debug, Clone, Default)]
pub struct MdlMesh {
    /// Material JSON path, e.g. `materials/foo.json`.
    pub material: Option<String>,
    /// Material JSON paths per skin when the model has several skins (the
    /// first is [`Self::material`]); empty otherwise.
    pub skins: Vec<String>,
    /// Attributes stored in the file.
    pub format: VertexFormat,
    /// Axis-aligned bounds `(min, max)` from the file (v17+; often zero for puppets).
    pub bounds: Option<([f32; 3], [f32; 3])>,
    /// Vertex positions.
    pub positions: Vec<[f32; 3]>,
    /// Vertex normals.
    pub normals: Vec<[f32; 3]>,
    /// Vertex tangents (xyz + handedness).
    pub tangents: Vec<[f32; 4]>,
    /// Primary texcoords.
    pub texcoords: Vec<[f32; 2]>,
    /// Secondary (lightmap/channel-map) texcoords, from [`VertexFormat::TEXCOORD_VEC4`].
    pub texcoords1: Vec<[f32; 2]>,
    /// Bone indices per vertex.
    pub blend_indices: Vec<[u32; 4]>,
    /// Bone weights per vertex.
    pub blend_weights: Vec<[f32; 4]>,
    /// Triangle list; every index is below [`Self::vertex_count`].
    pub indices: Vec<u32>,
    /// Whether the file stored 32-bit indices.
    pub wide_indices: bool,
    /// Blend shapes (morph targets), in file order (v21+).
    pub morphs: Vec<MorphTarget>,
}

/// One blend shape (morph target) of a mesh. See the module docs for how
/// it is stored and applied.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MorphTarget {
    /// Name. The `.mdl` does not store blend shape names, so this is empty
    /// for parsed files.
    pub name: String,
    /// Target position of every vertex as stored in the file (vertex
    /// aligned): the shaped mesh in the skeleton pose of [`Mdl::morph_pose`].
    pub positions: Vec<[f32; 3]>,
    /// Offset of every vertex from [`MdlMesh::positions`] in the space the
    /// mesh is stored in (vertex aligned), to be scaled by the blend shape's
    /// weight and added *before* skinning. Zero for vertices the shape does
    /// not move.
    pub deltas: Vec<[f32; 3]>,
}

impl MdlMesh {
    /// Number of vertices.
    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    /// Index of the blend shape called `name`.
    pub fn blend_shape_index(&self, name: &str) -> Option<usize> {
        self.morphs.iter().position(|m| m.name == name)
    }

    /// Positions with the blend shapes applied: `position + Σ weight[k] ×
    /// delta[k]` (in the mesh's own space, before skinning). Missing weights
    /// count as 0, extra ones are ignored.
    pub fn morphed_positions(&self, weights: &[f32]) -> Vec<[f32; 3]> {
        let mut out = self.positions.clone();
        for (morph, &w) in self.morphs.iter().zip(weights) {
            if w == 0.0 {
                continue;
            }
            for (p, d) in out.iter_mut().zip(&morph.deltas) {
                for k in 0..3 {
                    p[k] += w * d[k];
                }
            }
        }
        out
    }
}

/// The bone hierarchy of a puppet.
#[derive(Debug, Clone, Default)]
pub struct Skeleton {
    /// `MDLS` section version.
    pub version: u32,
    /// Bones; a bone's parent always precedes it.
    pub bones: Vec<Bone>,
}

/// One bone.
#[derive(Debug, Clone)]
pub struct Bone {
    /// Bone name (usually empty).
    pub name: String,
    /// Parent bone index; `None` for roots.
    pub parent: Option<usize>,
    /// Bind-pose transform relative to the parent: where the bone sits in the
    /// space the mesh vertices are stored in.
    pub bind: Mat4,
    /// Rest-pose transform relative to the parent, when stored separately from
    /// the bind pose (atlas-packed puppets). Animation frames are relative to
    /// the rest pose's parent space.
    pub rest: Option<Mat4>,
    /// Physics simulation settings JSON, if any.
    pub simulation: Option<String>,
}

impl Bone {
    /// The pose animation starts from: the rest pose, else the bind pose.
    pub fn rest_local(&self) -> Mat4 {
        self.rest.unwrap_or(self.bind)
    }
}

/// A named point that child objects can attach to (scene `attachment`).
#[derive(Debug, Clone)]
pub struct Attachment {
    /// Attachment name, e.g. `head`.
    pub name: String,
    /// Bone the attachment is rigged to.
    pub bone: usize,
    /// Transform in puppet space.
    pub transform: Mat4,
}

/// How an animation behaves past its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayMode {
    /// Wrap around.
    #[default]
    Loop,
    /// Play forward then backward.
    Mirror,
    /// Play once and hold the last frame.
    Single,
}

/// One skeletal animation.
#[derive(Debug, Clone, Default)]
pub struct MdlAnimation {
    /// Id referenced by scene `animationlayers[].animation`.
    pub id: i32,
    /// Display name.
    pub name: String,
    /// Playback mode.
    pub mode: PlayMode,
    /// Frames per second (> 0).
    pub fps: f32,
    /// Declared length in frames (> 0); tracks usually store `length + 1` frames.
    pub length: u32,
    /// One track per bone, in bone order.
    pub tracks: Vec<Vec<BoneFrame>>,
    /// Keyframe events (`{"frame": n, "name": …}` records).
    pub events: Vec<AnimationEvent>,
    /// Blend shape weight tracks (MDLA v6+): track `k` animates blend shape
    /// `k` of the warp mesh ([`Mdl::meshes`]`[0]`) with one weight per frame,
    /// like the bone tracks (usually `length + 1` frames).
    pub morph_tracks: Vec<Vec<f32>>,
}

/// A bone's local pose at one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoneFrame {
    /// Translation.
    pub position: Vec3,
    /// Euler angles in radians, applied as `Rz · Ry · Rx`.
    pub angle: Vec3,
    /// Scale.
    pub scale: Vec3,
}

/// An event fired when playback reaches a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimationEvent {
    /// Frame index.
    pub frame: i32,
    /// Event name passed to scripts.
    pub name: String,
}
