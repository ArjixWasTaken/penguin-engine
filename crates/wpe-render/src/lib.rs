//! wgpu renderer for Wallpaper Engine scenes.

mod gpu;
mod image_pipeline;
mod material;
mod renderer;
mod targets;
mod texture;

pub use gpu::{Gpu, GpuError};
pub use image_pipeline::BlendMode;
pub use material::{
    Camera, CardMesh, FrameGlobals, MaterialDraw, ProgramError, ShaderProgram, SpriteAnimation,
    SpriteFrame, TexRef,
};
pub use renderer::{Draw, ImageDraw, SceneRenderer, Step};
pub use targets::{DEFAULT_FORMAT, TargetDesc, TargetId, TargetSize};
pub use texture::{GpuTexture, upload_texture};

pub use wgpu;
