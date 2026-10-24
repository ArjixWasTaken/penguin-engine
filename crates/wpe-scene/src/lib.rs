//! Runtime scene graph, cameras and per-frame shader values.

mod animation;
mod camera;
mod camera_path;
mod fog;
mod light;
mod props;
mod scene;
mod transform;

pub use animation::{Animation, AnimationMode, AnimationTime};
pub use camera::{
    FillMode, OrthoCamera, PERSPECTIVE_DISTANCE, PerspectiveCamera, overlay_view_proj,
};
pub use camera_path::{CAMERA_FADE_SECONDS, CameraKeyframe, CameraPath, CameraPathPlayer};
pub use fog::{Fog, FogRange};
pub use light::{
    COOKIE_ALIAS, DEFAULT_CASCADES, DEFAULT_COOKIE, LightKind, LightUniforms, LightsByKind,
    MAX_LIGHTS, MAX_LIGHTS_PER_KIND, SHADOW_QUALITY, SceneLight, apply_light_extras, cookie_light,
    light_uniforms,
};
pub use props::UserProperties;
pub use scene::{
    Bloom, DrawItem, HdrBloom, ImageLayer, ModelLayer, Motion, Parallax, ParticleSource, Scene,
    Shake, apply_override, hdr_intent, text_texture_name,
};
pub use transform::{alignment_offset, local_transform};
