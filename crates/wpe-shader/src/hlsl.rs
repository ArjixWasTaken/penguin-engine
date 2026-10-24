//! Fixes for HLSL semantics WE shaders rely on but GLSL rejects (implicit
//! conversions, scalar/vector broadcasts, ...), applied to preprocessed source.

use crate::glslang::Stage;

/// Apply every fix to one preprocessed stage.
pub fn fix(_stage: Stage, src: &str) -> String {
    src.to_owned()
}
