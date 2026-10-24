//! Minimal safe wrapper over glslang's C API.
//!
//! The `glslang` crate's `Program` links inside `compile`, so `map_io`
//! (cross-stage location and binding assignment) can never run after
//! linking; this drives the C API directly instead.

use std::ffi::{CStr, CString};
use std::ptr::NonNull;
use std::sync::Mutex;

use glslang_sys as sys;

/// glslang's keyword tables are not safe for concurrent use.
static LOCK: Mutex<()> = Mutex::new(());

/// Name of the uniform block glslang gathers loose uniforms into.
pub const GLOBALS_BLOCK: &str = "WeGlobals";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Vertex,
    Geometry,
    Fragment,
}

impl Stage {
    fn sys(self) -> sys::glslang_stage_t {
        match self {
            Self::Vertex => sys::glslang_stage_t::Vertex,
            Self::Geometry => sys::glslang_stage_t::Geometry,
            Self::Fragment => sys::glslang_stage_t::Fragment,
        }
    }
}

fn messages() -> sys::glslang_messages_t {
    sys::glslang_messages_t(
        sys::glslang_messages_t::DEFAULT.0
            | sys::glslang_messages_t::SPV_RULES.0
            | sys::glslang_messages_t::VULKAN_RULES.0
            | sys::glslang_messages_t::RELAXED_ERRORS.0
            | sys::glslang_messages_t::SUPPRESS_WARNINGS.0,
    )
}

fn init() {
    glslang::Compiler::acquire().expect("glslang failed to initialize");
}

fn input(stage: Stage, code: &CStr, default_version: i32) -> sys::glslang_input_t {
    let limits: *const glslang::limits::ResourceLimits = &glslang::limits::DEFAULT_LIMITS;
    sys::glslang_input_t {
        language: sys::glslang_source_t::GLSL,
        stage: stage.sys(),
        client: sys::glslang_client_t::Vulkan,
        client_version: sys::glslang_target_client_version_t::Vulkan1_1,
        target_language: sys::glslang_target_language_t::SPIRV,
        target_language_version: sys::glslang_target_language_version_t::SPIRV1_3,
        code: code.as_ptr(),
        default_version,
        default_profile: sys::glslang_profile_t::Core,
        force_default_version_and_profile: 0,
        forward_compatible: 0,
        messages: messages(),
        // `ResourceLimits` is `#[repr(transparent)]` over `glslang_resource_t`.
        resource: limits.cast(),
        callbacks: sys::glsl_include_callbacks_t {
            include_system: None,
            include_local: None,
            free_include_result: None,
        },
        callbacks_ctx: std::ptr::null_mut(),
    }
}

/// Owns a `glslang_shader_t` together with the input it was created from;
/// glslang keeps pointers into both until the shader is deleted.
struct ShaderHandle {
    raw: NonNull<sys::glslang_shader_t>,
    input: Box<sys::glslang_input_t>,
    _code: CString,
}

impl ShaderHandle {
    fn new(stage: Stage, code: &str, default_version: i32) -> Result<Self, String> {
        let code = CString::new(code).map_err(|_| "shader source contains NUL".to_owned())?;
        let input = Box::new(input(stage, &code, default_version));
        // SAFETY: `input` and `code` are boxed/owned here and outlive the shader.
        let raw = unsafe { sys::glslang_shader_create(&*input) };
        let raw = NonNull::new(raw).ok_or("glslang_shader_create failed")?;
        Ok(Self {
            raw,
            input,
            _code: code,
        })
    }

    fn log(&self) -> String {
        // SAFETY: the handle is live; glslang returns a NUL-terminated string.
        let log = unsafe { CStr::from_ptr(sys::glslang_shader_get_info_log(self.raw.as_ptr())) };
        log.to_string_lossy().trim().to_owned()
    }

    fn preprocess(&self) -> Result<(), String> {
        // SAFETY: the handle and its input are live.
        if unsafe { sys::glslang_shader_preprocess(self.raw.as_ptr(), &*self.input) } == 0 {
            return Err(self.log());
        }
        Ok(())
    }

    fn preprocessed_code(&self) -> String {
        // SAFETY: called after a successful preprocess.
        let code =
            unsafe { CStr::from_ptr(sys::glslang_shader_get_preprocessed_code(self.raw.as_ptr())) };
        code.to_string_lossy().into_owned()
    }

    fn parse(&self) -> Result<(), String> {
        // SAFETY: the handle and its input are live.
        if unsafe { sys::glslang_shader_parse(self.raw.as_ptr(), &*self.input) } == 0 {
            return Err(self.log());
        }
        Ok(())
    }
}

impl Drop for ShaderHandle {
    fn drop(&mut self) {
        // SAFETY: we own the handle; programs referencing it are dropped first.
        unsafe { sys::glslang_shader_delete(self.raw.as_ptr()) }
    }
}

/// Run only the preprocessor, resolving `#if`s and macros.
pub fn preprocess(stage: Stage, code: &str) -> Result<String, String> {
    init();
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let shader = ShaderHandle::new(stage, code, 330)?;
    shader.preprocess()?;
    Ok(shader.preprocessed_code())
}

/// Compile and link `units` as one program, assigning locations and bindings
/// across stages, and return each stage's SPIR-V in the same order.
///
/// Loose uniforms are gathered into the [`GLOBALS_BLOCK`] uniform block at
/// set 0, binding 0.
pub fn compile(units: &[(Stage, &str)]) -> Result<Vec<Vec<u32>>, String> {
    init();
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let block = CString::new(GLOBALS_BLOCK).expect("no NUL in block name");

    let mut shaders = Vec::with_capacity(units.len());
    for &(stage, code) in units {
        let shader = ShaderHandle::new(stage, code, 330)?;
        // SAFETY: the handle is live; `block` is copied by glslang.
        unsafe {
            sys::glslang_shader_set_options(
                shader.raw.as_ptr(),
                sys::glslang_shader_options_t::AUTO_MAP_BINDINGS.0
                    | sys::glslang_shader_options_t::AUTO_MAP_LOCATIONS.0
                    | sys::glslang_shader_options_t::VULKAN_RULES_RELAXED.0,
            );
            sys::glslang_shader_set_default_uniform_block_name(shader.raw.as_ptr(), block.as_ptr());
            sys::glslang_shader_set_default_uniform_block_set_and_binding(
                shader.raw.as_ptr(),
                0,
                0,
            );
        }
        shader
            .preprocess()
            .and_then(|()| shader.parse())
            .map_err(|e| format!("{stage:?}: {e}"))?;
        shaders.push((stage, shader));
    }

    let program = Program::new()?;
    for (_, shader) in &shaders {
        // SAFETY: both handles are live; `program` is dropped before `shaders`.
        unsafe { sys::glslang_program_add_shader(program.0.as_ptr(), shader.raw.as_ptr()) }
    }
    // SAFETY: the program and all attached shaders are live.
    unsafe {
        if sys::glslang_program_link(program.0.as_ptr(), messages().0) == 0 {
            return Err(format!("link: {}", program.log()));
        }
        if sys::glslang_program_map_io(program.0.as_ptr()) == 0 {
            return Err(format!("map io: {}", program.log()));
        }
    }

    let spirv = shaders
        .iter()
        .map(|(stage, _)| {
            // SAFETY: the program is linked; the buffer is sized from glslang.
            unsafe {
                sys::glslang_program_SPIRV_generate(program.0.as_ptr(), stage.sys());
                let size = sys::glslang_program_SPIRV_get_size(program.0.as_ptr());
                let mut words = vec![0u32; size];
                sys::glslang_program_SPIRV_get(program.0.as_ptr(), words.as_mut_ptr());
                words
            }
        })
        .collect();
    drop(program);
    Ok(spirv)
}

struct Program(NonNull<sys::glslang_program_t>);

impl Program {
    fn new() -> Result<Self, String> {
        // SAFETY: plain constructor.
        NonNull::new(unsafe { sys::glslang_program_create() })
            .map(Self)
            .ok_or_else(|| "glslang_program_create failed".to_owned())
    }

    fn log(&self) -> String {
        // SAFETY: the handle is live; glslang returns a NUL-terminated string.
        let log = unsafe { CStr::from_ptr(sys::glslang_program_get_info_log(self.0.as_ptr())) };
        log.to_string_lossy().trim().to_owned()
    }
}

impl Drop for Program {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        unsafe { sys::glslang_program_delete(self.0.as_ptr()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preprocess_resolves_conditionals() {
        let out = preprocess(
            Stage::Fragment,
            "#define A 1\n#if A\nfloat yes;\n#else\nfloat no;\n#endif\n",
        )
        .unwrap();
        assert!(out.contains("yes"));
        assert!(!out.contains("no;"));
    }

    #[test]
    fn compiles_loose_uniforms_with_matched_varyings() {
        let vs = "#version 330\nuniform mat4 u_Mvp;\nin vec3 a_Position;\nout vec2 v_A;\nout vec3 v_B;\n\
                  void main() { v_A = a_Position.xy; v_B = a_Position; gl_Position = u_Mvp * vec4(a_Position, 1.0); }\n";
        let fs = "#version 330\nuniform vec4 u_Color;\nin vec3 v_B;\nin vec2 v_A;\nout vec4 o;\n\
                  void main() { o = u_Color * vec4(v_B, v_A.x); }\n";
        let spv = compile(&[(Stage::Vertex, vs), (Stage::Fragment, fs)]).unwrap();
        assert_eq!(spv.len(), 2);
        assert!(spv.iter().all(|s| s.first() == Some(&0x0723_0203)));
    }

    #[test]
    fn reports_errors() {
        let err =
            compile(&[(Stage::Fragment, "#version 330\nvoid main() { nope; }\n")]).unwrap_err();
        assert!(err.contains("nope"), "{err}");
    }
}
