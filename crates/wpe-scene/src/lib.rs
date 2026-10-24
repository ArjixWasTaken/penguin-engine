//! Runtime scene graph, cameras and per-frame shader values.

mod animation;
mod camera;
mod light;
mod props;
mod scene;
mod transform;

pub use animation::{Animation, AnimationMode, AnimationTime};
pub use camera::{
    FillMode, OrthoCamera, PERSPECTIVE_DISTANCE, PerspectiveCamera, overlay_view_proj,
};
pub use light::{
    LightKind, LightUniforms, MAX_LIGHTS, MAX_LIGHTS_PER_KIND, SceneLight, light_uniforms,
};
pub use props::UserProperties;
pub use scene::{
    Bloom, DrawItem, ImageLayer, ModelLayer, Motion, Parallax, ParticleSource, Scene, Shake,
    apply_override, text_texture_name,
};
pub use transform::{alignment_offset, local_transform};
