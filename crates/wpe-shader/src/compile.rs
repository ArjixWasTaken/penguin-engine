use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, LazyLock, Mutex};

use wpe_vfs::Vfs;

use crate::glslang::{self, Stage};
use crate::hlsl;
use crate::reflect::{self, ReflectError, Reflection};
use crate::rewrite;
use crate::source::{self, ShaderInfo, TexSlot};

#[derive(Debug, thiserror::Error)]
pub enum ShaderError {
    #[error("shader {0} not found")]
    Missing(String),
    #[error("geometry shaders are not supported")]
    Geometry,
    #[error("{stage:?} preprocessing failed: {message}")]
    Preprocess { stage: Stage, message: String },
    #[error("compilation failed: {0}")]
    Compile(String),
    #[error(transparent)]
    Reflect(#[from] ReflectError),
}

/// What to compile: a WE shader name plus the scene state that selects its variant.
#[derive(Debug, Clone, Copy)]
pub struct ShaderRequest<'a> {
    /// Name under `shaders/`, e.g. `genericimage2`.
    pub name: &'a str,
    /// Material combos; they override the shader's annotated defaults.
    pub combos: &'a BTreeMap<String, String>,
    /// What is bound to each `g_Texture<N>`.
    pub slots: &'a [TexSlot],
    /// `TEX<N>FORMAT` value per slot (e.g. `FORMAT_DXT5`).
    pub formats: &'a [Option<&'a str>],
}

/// A compiled vertex + fragment program and what it expects to be bound.
#[derive(Debug)]
pub struct CompiledShader {
    pub info: ShaderInfo,
    /// Every combo the program was compiled with.
    pub combos: BTreeMap<String, String>,
    pub vertex: Vec<u32>,
    pub fragment: Vec<u32>,
    pub reflection: Reflection,
}

type Memo = HashMap<String, Arc<Result<(Vec<u32>, Vec<u32>, Reflection), String>>>;

/// Compiled programs keyed by the SHA-1 of their final sources; scenes use
/// the same shader variant from many materials.
static MEMO: LazyLock<Mutex<Memo>> = LazyLock::new(Default::default);

fn read(vfs: &Vfs, path: &str) -> Option<String> {
    vfs.read_to_string(path).ok()
}

fn load_sources(vfs: &Vfs, name: &str) -> Result<(String, String), ShaderError> {
    let base = format!("shaders/{name}");
    let missing = || ShaderError::Missing(name.to_owned());
    let vert = read(vfs, &format!("{base}.vert")).ok_or_else(missing)?;
    let frag = read(vfs, &format!("{base}.frag")).ok_or_else(missing)?;
    // A `.geom` stage is never used: WE shaders that have one also have a
    // `GS_ENABLED=0` path, which callers expand on the CPU instead.
    Ok((vert, frag))
}

/// Only gather a shader's annotations (combos, aliases, defaults, default
/// textures), without compiling it.
pub fn annotations(vfs: &Vfs, name: &str, slots: &[TexSlot]) -> Result<ShaderInfo, ShaderError> {
    let (vert, frag) = load_sources(vfs, name)?;
    let mut info = ShaderInfo::default();
    source::prepare(vfs, &vert, &mut info, slots);
    source::prepare(vfs, &frag, &mut info, slots);
    Ok(info)
}

pub fn compile(vfs: &Vfs, req: ShaderRequest) -> Result<CompiledShader, ShaderError> {
    let (vert, frag) = load_sources(vfs, req.name)?;
    let mut info = ShaderInfo::default();
    let vert = source::prepare(vfs, &vert, &mut info, req.slots);
    let frag = source::prepare(vfs, &frag, &mut info, req.slots);

    let mut combos = info.combos.clone();
    combos.insert("GS_ENABLED".to_owned(), "0".to_owned());
    combos.extend(req.combos.iter().map(|(k, v)| (k.clone(), v.clone())));
    for (slot, format) in req.formats.iter().enumerate() {
        if let Some(format) = format {
            combos.insert(format!("TEX{slot}FORMAT"), (*format).to_owned());
        }
    }

    let stage_source = |stage: Stage, src: &str| -> Result<String, ShaderError> {
        let full = rewrite::strip_stray_endifs(&rewrite::expand_requires(&format!(
            "{}{src}",
            rewrite::header(stage, &combos, src)
        )));
        glslang::preprocess(stage, &full)
            .map_err(|message| ShaderError::Preprocess { stage, message })
    };
    let vert = stage_source(Stage::Vertex, &vert)?;
    let frag = stage_source(Stage::Fragment, &frag)?;

    let vert_io = rewrite::scan_io(&vert);
    let frag_io = rewrite::scan_io(&frag);
    let vert = rewrite::fill_placeholder(&vert, &vert_io, None, Some(&frag_io));
    let frag = rewrite::fill_placeholder(&frag, &frag_io, Some(&vert_io), None);
    let (vert, frag) = hlsl::reconcile(&vert, &frag, &vert_io, &frag_io);

    let vert = rewrite::split_samplers(&rewrite::split_varying_arrays(&hlsl::fix(
        Stage::Vertex,
        &vert,
    )));
    let frag = rewrite::split_samplers(&rewrite::split_varying_arrays(&hlsl::fix(
        Stage::Fragment,
        &rewrite::translate_clip(&frag),
    )));

    let key = {
        let mut sha = sha1_smol::Sha1::new();
        sha.update(vert.as_bytes());
        sha.update(&[0]);
        sha.update(frag.as_bytes());
        sha.digest().to_string()
    };
    if let Some(dir) = std::env::var_os("WPE_DUMP_SHADERS") {
        let stem = format!("{}_{}", req.name.replace('/', "_"), &key[..8]);
        let dir = std::path::Path::new(&dir);
        for (ext, src) in [("vert", &vert), ("frag", &frag)] {
            let path = dir.join(format!("{stem}.{ext}"));
            if let Err(e) = std::fs::write(&path, src) {
                tracing::warn!("dumping {}: {e}", path.display());
            }
        }
    }
    let cached = MEMO
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .cloned();
    let result = match cached {
        Some(r) => r,
        None => {
            let r = Arc::new(build(&vert, &frag));
            MEMO.lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, r.clone());
            r
        }
    };
    let (vertex, fragment, reflection) = result.as_ref().clone().map_err(ShaderError::Compile)?;
    Ok(CompiledShader {
        info,
        combos,
        vertex,
        fragment,
        reflection,
    })
}

/// glslang errors [`hlsl::repair`] may fix before a program is given up on.
const MAX_REPAIRS: usize = 32;

fn build(vert: &str, frag: &str) -> Result<(Vec<u32>, Vec<u32>, Reflection), String> {
    let (mut vert, mut frag) = (vert.to_owned(), frag.to_owned());
    let mut attempt = 0;
    let mut spirv = loop {
        // Repairs work on unqualified declarations; locations go on a copy.
        let (located_vert, located_frag) = rewrite::assign_varying_locations(&vert, &frag);
        match glslang::compile(&[
            (Stage::Vertex, &located_vert),
            (Stage::Fragment, &located_frag),
        ]) {
            Ok(spirv) => break spirv.into_iter(),
            Err(e) => {
                attempt += 1;
                let diag = hlsl::diagnostic(&e).filter(|_| attempt <= MAX_REPAIRS);
                let src = match diag.as_ref().map(|d| d.stage) {
                    Some(Stage::Vertex) => &mut vert,
                    Some(Stage::Fragment) => &mut frag,
                    _ => return Err(e),
                };
                let fixed = diag.and_then(|d| hlsl::repair(src, &d));
                match fixed {
                    Some(fixed) => *src = fixed,
                    None => return Err(e),
                }
            }
        }
    };
    let (vertex, fragment) = (
        spirv.next().unwrap_or_default(),
        spirv.next().unwrap_or_default(),
    );
    let mut reflection = Reflection::default();
    for (stage, words) in [(Stage::Vertex, &vertex), (Stage::Fragment, &fragment)] {
        let module = reflect::parse_spirv(words).map_err(|e| format!("{stage:?}: {e}"))?;
        reflect::validate(&module).map_err(|e| format!("{stage:?}: {e}"))?;
        reflection
            .add_stage(stage, &module)
            .map_err(|e| format!("{stage:?}: {e}"))?;
    }
    Ok((vertex, fragment, reflection))
}
