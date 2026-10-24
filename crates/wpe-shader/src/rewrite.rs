//! Text rewrites applied to preprocessed GLSL before compilation.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::glslang::Stage;
use crate::preamble::PLACEHOLDER;

/// Suffixes of the separate texture and sampler a combined sampler is split into.
pub const TEXTURE_SUFFIX: &str = "_wet";
pub const SAMPLER_SUFFIX: &str = "_wes";

/// Stage inputs and outputs, name → full declaration.
#[derive(Debug, Default, Clone)]
pub struct StageIo {
    pub inputs: BTreeMap<String, String>,
    pub outputs: BTreeMap<String, String>,
}

static IO_DECL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)(?:^|\s)(in|out)\s[\s\w]+?\s(\w+)\s*(?:\[\])?\s*;").expect("valid regex")
});

/// Top-level `in`/`out` declarations of preprocessed source.
pub fn scan_io(src: &str) -> StageIo {
    let mut io = StageIo::default();
    for line in src.lines() {
        // Function parameters (`in vec2 uv,`) never end in `;` on their own line,
        // but `out vec4 glOutColor;` and friends do.
        let Some(c) = IO_DECL.captures(line) else {
            continue;
        };
        if line.contains('(') {
            continue;
        }
        let decl = c.get(0).expect("whole match").as_str().trim().to_owned();
        let name = c[2].to_owned();
        if name == "glOutColor" {
            continue;
        }
        match &c[1] {
            "in" => io.inputs.insert(name, decl),
            _ => io.outputs.insert(name, decl),
        };
    }
    io
}

fn needs_flat(decl: &str) -> bool {
    let d = decl.trim_start_matches("flat ").trim_start();
    ["int ", "uint ", "ivec", "uvec"]
        .iter()
        .any(|t| d.contains(&format!(" {t}")) || d.starts_with(t))
        && !d.contains("flat ")
}

/// Fill the placeholder with varyings a neighbouring stage declares but this
/// stage lacks, so interfaces match.
pub fn fill_placeholder(
    src: &str,
    own: &StageIo,
    prev: Option<&StageIo>,
    next: Option<&StageIo>,
) -> String {
    let mut decls = String::new();
    if let Some(prev) = prev {
        for (name, decl) in &prev.outputs {
            if !own.inputs.contains_key(name) {
                let d = decl.replacen("out ", "in ", 1);
                if needs_flat(&d) {
                    decls.push_str("flat ");
                }
                decls.push_str(&d);
                decls.push('\n');
            }
        }
    }
    if let Some(next) = next {
        for (name, decl) in &next.inputs {
            if !own.outputs.contains_key(name) {
                let d = decl.replacen("in ", "out ", 1);
                if needs_flat(&d) {
                    decls.push_str("flat ");
                }
                decls.push_str(&d);
                decls.push('\n');
            }
        }
    }
    src.replacen(PLACEHOLDER, &decls, 1)
}

static CLIP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bclip\s*\(([^;]*)\)\s*;").expect("valid regex"));

/// HLSL `clip(x);` → `if ((x) < 0.0) discard;`.
pub fn translate_clip(src: &str) -> String {
    CLIP.replace_all(src, "if (($1) < 0.0) discard;")
        .into_owned()
}

static COMBINED_SAMPLER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?m)^\s*uniform\s+(?:(?:highp|mediump|lowp)\s+)?sampler(2D|3D|Cube|2DArray)\s+(\w+)\s*;",
    )
    .expect("valid regex")
});

/// Split each `uniform samplerXD name;` into a texture and a sampler (naga's
/// SPIR-V frontend has no combined image-samplers), with a macro rebuilding
/// the combined sampler at each use.
pub fn split_samplers(src: &str) -> String {
    COMBINED_SAMPLER
        .replace_all(src, |c: &regex::Captures| {
            let (dim, name) = (&c[1], &c[2]);
            format!(
                "uniform texture{dim} {name}{TEXTURE_SUFFIX};\n\
                 uniform sampler {name}{SAMPLER_SUFFIX};\n\
                 #define {name} sampler{dim}({name}{TEXTURE_SUFFIX}, {name}{SAMPLER_SUFFIX})"
            )
        })
        .into_owned()
}

/// Drop `#endif`/`#else`/`#elif` lines with no open `#if`; some workshop
/// shaders ship them and glslang truncates its output there.
pub fn strip_stray_endifs(src: &str) -> String {
    let mut depth = 0usize;
    let mut out = String::with_capacity(src.len());
    for line in src.split_inclusive('\n') {
        let t = line.trim_start();
        if t.starts_with("#if") {
            depth += 1;
        } else if t.starts_with("#endif") {
            if depth == 0 {
                continue;
            }
            depth -= 1;
        } else if (t.starts_with("#else") || t.starts_with("#elif")) && depth == 0 {
            continue;
        }
        out.push_str(line);
    }
    out
}

static REQUIRE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^#require\s+(\S+)\s*$").expect("valid regex"));

/// Replace `#require` directives with their implementations.
pub fn expand_requires(src: &str) -> String {
    REQUIRE
        .replace_all(src, |c: &regex::Captures| match &c[1] {
            "LightingV1" => crate::preamble::LIGHTING_V1.to_owned(),
            other => format!("//#require {other}"),
        })
        .into_owned()
}

static VARYING_ARRAY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*((?:(?:flat|smooth|noperspective|highp|mediump|lowp)\s+)*)(in|out)\s+(\w+)\s+(\w+)\s*\[\s*(\d+)\s*\]\s*;")
        .expect("valid regex")
});

static MAIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bvoid\s+main\s*\(\s*(?:void)?\s*\)").expect("valid regex"));

/// Stage inputs/outputs may not be arrays for naga: turn `out vec2 v[4];`
/// into a private array plus four scalar varyings, copied by a wrapper
/// around `main`.
pub fn split_varying_arrays(src: &str) -> String {
    let mut copies_in = String::new();
    let mut copies_out = String::new();
    let replaced = VARYING_ARRAY.replace_all(src, |c: &regex::Captures| {
        let (quals, dir, ty, name) = (&c[1], &c[2], &c[3], &c[4]);
        let n: usize = c[5].parse().unwrap_or(0);
        let mut out = format!("\n{ty} {name}[{n}];\n");
        for i in 0..n {
            out.push_str(&format!("{quals}{dir} {ty} {name}_we{i};\n"));
            let copy = if dir == "in" {
                format!("    {name}[{i}] = {name}_we{i};\n")
            } else {
                format!("    {name}_we{i} = {name}[{i}];\n")
            };
            if dir == "in" {
                copies_in.push_str(&copy);
            } else {
                copies_out.push_str(&copy);
            }
        }
        out
    });
    if copies_in.is_empty() && copies_out.is_empty() {
        return src.to_owned();
    }
    let renamed = MAIN.replace(&replaced, "void _we_main()");
    format!("{renamed}\nvoid main() {{\n{copies_in}    _we_main();\n{copies_out}}}\n")
}

static DEFINES_LOG10: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:float|half|double|vec[234]|half[234])\s+log10\s*\(").expect("valid regex")
});

/// Preamble, stage defines and combo `#define`s for `src`.
pub fn header(stage: Stage, combos: &BTreeMap<String, String>, src: &str) -> String {
    let mut out = String::from(crate::preamble::COMMON);
    out.push_str(match stage {
        Stage::Vertex => crate::preamble::VERTEX,
        Stage::Fragment => crate::preamble::FRAGMENT,
        Stage::Geometry => crate::preamble::GEOMETRY,
    });
    for (name, value) in combos {
        if value.is_empty() {
            continue;
        }
        out.push_str(&format!("#define {} {value}\n", name.to_ascii_uppercase()));
    }
    if DEFINES_LOG10.is_match(src) {
        out.push_str("#undef log10\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_scan_skips_function_parameters() {
        let io = scan_io(
            "in vec2 v_TexCoord;\nflat out int v_Id;\nvoid f(in vec2 uv, out float x);\nout vec4 glOutColor;\n",
        );
        assert_eq!(io.inputs.keys().collect::<Vec<_>>(), ["v_TexCoord"]);
        assert_eq!(io.outputs.keys().collect::<Vec<_>>(), ["v_Id"]);
    }

    #[test]
    fn placeholder_adds_missing_varyings() {
        let vs = scan_io("out vec4 v_A;\nout int v_I;\n");
        let fs = scan_io("in vec4 v_A;\n");
        let filled = fill_placeholder(PLACEHOLDER, &fs, Some(&vs), None);
        assert_eq!(filled.trim(), "flat in int v_I;");
    }

    #[test]
    fn clip_and_samplers() {
        assert_eq!(
            translate_clip("clip(a - 0.5);"),
            "if ((a - 0.5) < 0.0) discard;"
        );
        let s = split_samplers("uniform sampler2D g_Texture0;\nuniform float x;\n");
        assert!(s.contains("uniform texture2D g_Texture0_wet;"));
        assert!(s.contains("#define g_Texture0 sampler2D(g_Texture0_wet, g_Texture0_wes)"));
    }

    #[test]
    fn varying_arrays_split() {
        let vs = split_varying_arrays("out vec2 v_T[2];\nvoid main() { v_T[0] = vec2(0); }\n");
        assert!(vs.contains("out vec2 v_T_we1;"));
        assert!(vs.contains("void _we_main()"));
        assert!(vs.contains("_we_main();\n    v_T_we0 = v_T[0];"));
        let fs = split_varying_arrays("in vec2 v_T[2];\nvoid main() {}\n");
        assert!(fs.contains("v_T[1] = v_T_we1;\n    _we_main();"));
    }

    #[test]
    fn stray_endifs() {
        assert_eq!(
            strip_stray_endifs("#endif\n#if A\nx\n#endif\n#else\n"),
            "#if A\nx\n#endif\n"
        );
    }
}
