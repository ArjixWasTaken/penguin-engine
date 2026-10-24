//! Wallpaper Engine shader preprocessing and GLSL to SPIR-V compilation.

mod compile;
pub mod glslang;
pub mod hlsl;
pub mod preamble;
pub mod reflect;
pub mod rewrite;
pub mod source;

pub use compile::{CompiledShader, ShaderError, ShaderRequest, annotations, compile};
pub use reflect::Reflection;
pub use source::{ShaderInfo, TexSlot};
