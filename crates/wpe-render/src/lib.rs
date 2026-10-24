//! wgpu renderer for Wallpaper Engine scenes.

mod gpu;
mod image_pipeline;
mod material;
mod renderer;
mod shadow;
mod targets;
mod texture;

pub use gpu::{Gpu, GpuError};
pub use image_pipeline::BlendMode;
pub use material::{
    Camera, CardMesh, CullMode, FrameGlobals, MaterialDraw, Mesh, MeshData, ProgramError,
    RasterState, ShaderProgram, SpriteAnimation, SpriteFrame, TexRef,
};
pub use renderer::{Draw, ImageDraw, SceneRenderer, Step};
pub use shadow::{
    DirectionalShadow, PointShadow, ShadowLayout, ShadowLights, SpotShadow, point_compensation,
    point_projection_info,
};
pub use targets::{DEFAULT_FORMAT, HDR_FORMAT, TargetDesc, TargetId, TargetSize};
pub use texture::{GpuTexture, upload_texture};

pub use wgpu;
