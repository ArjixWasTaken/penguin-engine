//! Fixes for HLSL semantics WE shaders rely on but GLSL rejects (implicit
//! conversions, scalar/vector broadcasts, ...), applied to preprocessed source.
//!
//! Three layers:
//! - [`fix`]: unconditional per-stage rewrites that change rendering, not
//!   just compilation.
//! - [`reconcile`]: cross-stage interface fixes (varyings and uniforms whose
//!   types differ between stages; HLSL interpolators are 4-wide).
//! - [`repair`]: error-driven fixes. Given glslang's log for a failing stage
//!   it rewrites only the offending statement, so shaders that already
//!   compile are never touched and fixes compose across retries.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::glslang::Stage;
use crate::rewrite::{StageIo, TEXTURE_SUFFIX};

/// Apply every unconditional fix to one preprocessed stage.
pub fn fix(stage: Stage, src: &str) -> String {
    let src = clamp_audio_reactive_shift(src);
    if stage != Stage::Fragment {
        return src;
    }
    let src = fix_combine_alpha(&src);
    let src = fix_effect_alpha(&src);
    fix_fragment_gl_position(&src)
}

static AUDIO_SHIFT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(u_\w*[Oo]ffset)\s*\*\s*v_AudioShift\b").expect("valid regex"));

/// Cap `u_*Offset * v_AudioShift` terms at a shift of 1.0; stacked
/// chromatic-aberration passes otherwise compound unbounded audio peaks.
fn clamp_audio_reactive_shift(src: &str) -> String {
    AUDIO_SHIFT
        .replace_all(src, "$1 * min(v_AudioShift, 1.0)")
        .into_owned()
}

static COMBINE_ALPHA: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(\w+)\.a\s*=\s*\(clamp\(\s*(\w+)\.a\s*\+\s*\w+\.a\s*,\s*0\.0\s*,\s*1\.0\s*\)\)\s*;",
    )
    .expect("valid regex")
});

/// Drop `x.a = saturate(x.a + effect.a);` in combine shaders: the added
/// effect alpha grows the object's silhouette and leaves a translucent fringe
/// around puppets. Only fires when both `x` are the same variable.
fn fix_combine_alpha(src: &str) -> String {
    COMBINE_ALPHA
        .replace_all(src, |c: &regex::Captures| {
            if c[1] == c[2] {
                format!(
                    "// {} // removed: effect alpha must not extend the object",
                    &c[0]
                )
            } else {
                c[0].to_owned()
            }
        })
        .into_owned()
}

static OUT_FULL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bglOutColor\s*=[^=]").expect("valid regex"));
static OUT_COMPONENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bglOutColor\s*\.").expect("valid regex"));
static OUT_ALPHA: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bglOutColor\s*\.\s*\w*[aw]").expect("valid regex"));
static TEXTURE0: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bsampler2D\s+g_Texture0\s*;").expect("valid regex"));
static TEXCOORD_IN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bin\s+(?:\w+\s+)*?vec[234]\s+v_TexCoord\s*;").expect("valid regex")
});

/// Effects that only write some colour channels leave alpha undefined in
/// GLSL; D3D's output keeps the input's alpha, so copy it from `g_Texture0`.
fn fix_effect_alpha(src: &str) -> String {
    if OUT_FULL.is_match(src) || !OUT_COMPONENT.is_match(src) || OUT_ALPHA.is_match(src) {
        return src.to_owned();
    }
    if !TEXTURE0.is_match(src) || !TEXCOORD_IN.is_match(src) {
        return src.to_owned();
    }
    let Some(close) = main_close_brace(src) else {
        return src.to_owned();
    };
    let mut out = src.to_owned();
    out.insert_str(
        close,
        "    glOutColor.a = texture(g_Texture0, v_TexCoord.xy).a;\n",
    );
    out
}

static GL_POSITION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bgl_Position\b").expect("valid regex"));

/// HLSL pixel shaders read `SV_Position` as window coordinates, which WE's
/// sources spell `gl_Position`; GLSL's fragment equivalent is `gl_FragCoord`.
fn fix_fragment_gl_position(src: &str) -> String {
    GL_POSITION.replace_all(src, "gl_FragCoord").into_owned()
}

static MAIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bvoid\s+main\s*\(\s*(?:void)?\s*\)\s*\{").expect("valid regex"));

/// Byte offset of the `}` closing `main`.
fn main_close_brace(src: &str) -> Option<usize> {
    let m = MAIN.find(src)?;
    let open = m.end() - 1;
    match_forward(src.as_bytes(), open)
}

// ---------------------------------------------------------------------------
// Cross-stage reconciliation
// ---------------------------------------------------------------------------

/// Width of a float scalar/vector type name.
fn float_width(ty: &str) -> Option<usize> {
    match ty {
        "float" => Some(1),
        "vec2" => Some(2),
        "vec3" => Some(3),
        "vec4" => Some(4),
        _ => None,
    }
}

fn float_type(width: usize) -> &'static str {
    ["float", "float", "vec2", "vec3", "vec4"][width.min(4)]
}

fn decl_type(decl: &str) -> Option<&str> {
    let mut words = decl.trim_end_matches(';').split_whitespace().rev();
    words.next()?;
    words.next()
}

static UNIFORM_DECL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*uniform\s+(?:(?:highp|mediump|lowp)\s+)?(float|vec[234])\s+(\w+)\s*;")
        .expect("valid regex")
});

/// Make the vertex outputs and fragment inputs agree, and loose uniforms the
/// stages share. HLSL interpolators are 4-wide, so a varying written as
/// `float4` and read as `float2` is fine there; here the narrower stage gets
/// the wider type and keeps working on a narrow private copy.
pub fn reconcile(vert: &str, frag: &str, vert_io: &StageIo, frag_io: &StageIo) -> (String, String) {
    let (mut vert, mut frag) = (vert.to_owned(), frag.to_owned());
    for (name, in_decl) in &frag_io.inputs {
        let Some(out_decl) = vert_io.outputs.get(name) else {
            continue;
        };
        let (Some(out_ty), Some(in_ty)) = (decl_type(out_decl), decl_type(in_decl)) else {
            continue;
        };
        let (Some(ow), Some(iw)) = (float_width(out_ty), float_width(in_ty)) else {
            continue;
        };
        if ow < iw
            && let Some(v) = shadow_varying(&vert, name, in_ty)
        {
            vert = v;
        } else if iw < ow
            && let Some(f) = shadow_varying(&frag, name, out_ty)
        {
            frag = f;
        }
    }

    let uniforms = |src: &str| -> HashMap<String, String> {
        UNIFORM_DECL
            .captures_iter(src)
            .map(|c| (c[2].to_owned(), c[1].to_owned()))
            .collect()
    };
    let (vu, fu) = (uniforms(&vert), uniforms(&frag));
    for (name, vty) in &vu {
        let Some(fty) = fu.get(name) else { continue };
        let (Some(vw), Some(fw)) = (float_width(vty), float_width(fty)) else {
            continue;
        };
        if vw < fw {
            vert = widen_uniform(&vert, name, vw, fty);
        } else if fw < vw {
            frag = widen_uniform(&frag, name, fw, vty);
        }
    }
    (vert, frag)
}

/// Redeclare a loose uniform with a wider type and swizzle every bare use
/// back to its original width.
fn widen_uniform(src: &str, name: &str, width: usize, wide_ty: &str) -> String {
    let decl = Regex::new(&format!(
        r"(?m)^(\s*uniform\s+(?:(?:highp|mediump|lowp)\s+)?)(?:float|vec[234])(\s+{}\s*;)",
        regex::escape(name)
    ))
    .expect("valid regex");
    let Some(m) = decl.find(src) else {
        return src.to_owned();
    };
    let swizzle = [".x", ".x", ".xy", ".xyz"][width.min(3)];
    let head = swizzle_bare_uses(&src[..m.start()], name, swizzle);
    let tail = swizzle_bare_uses(&src[m.end()..], name, swizzle);
    let decl = decl.replace(m.as_str(), format!("${{1}}{wide_ty}$2"));
    format!("{head}{decl}{tail}")
}

fn swizzle_bare_uses(src: &str, name: &str, swizzle: &str) -> String {
    let re = Regex::new(&format!(r"\b{}\b", regex::escape(name))).expect("valid regex");
    let mut out = String::with_capacity(src.len());
    let mut last = 0;
    for m in re.find_iter(src) {
        out.push_str(&src[last..m.end()]);
        if !src[m.end()..].trim_start().starts_with(['.', '(']) {
            out.push_str(swizzle);
        }
        last = m.end();
    }
    out.push_str(&src[last..]);
    out
}

/// Replace a stage input/output with a private copy of the declared type,
/// redeclaring the interface variable as `iface_ty` and copying between the
/// two around `main`. Used for interface width mismatches and for writes to
/// inputs (HLSL inputs are ordinary mutable parameters).
fn shadow_varying(src: &str, name: &str, iface_ty: &str) -> Option<String> {
    let decl = Regex::new(&format!(
        r"(?m)^([ \t]*(?:\w+[ \t]+)*?)(in|out)([ \t]+(?:\w+[ \t]+)*?)(\w+)[ \t]+{}[ \t]*;",
        regex::escape(name)
    ))
    .expect("valid regex");
    let c = decl.captures(src)?;
    let whole = c.get(0).expect("whole match");
    let (dir, local_ty) = (&c[2], &c[4]);
    let local = format!("_we_{name}");
    let iface = format!(
        "{}{dir}{}{iface_ty} {name};\n{local_ty} {local};",
        &c[1], &c[3]
    );

    let uses = Regex::new(&format!(r"\b{}\b", regex::escape(name))).expect("valid regex");
    let head = uses.replace_all(&src[..whole.start()], local.as_str());
    let tail = uses.replace_all(&src[whole.end()..], local.as_str());
    let body = format!("{head}{iface}{tail}");

    let copy = if dir == "in" {
        if local_ty == iface_ty {
            format!("    {local} = {name};\n")
        } else {
            format!("    {local} = {local_ty}({name});\n")
        }
    } else {
        let pad = float_width(iface_ty)?.saturating_sub(float_width(local_ty)?);
        format!("    {name} = {iface_ty}({local}{});\n", ", 0.0".repeat(pad))
    };
    let inner = (0..)
        .map(|i| format!("_we_io_main{i}"))
        .find(|n| !src.contains(n.as_str()))
        .expect("unbounded");
    let m = MAIN.find(&body)?;
    let renamed = format!(
        "{}void {inner}() {{{}",
        &body[..m.start()],
        &body[m.end()..]
    );
    let (before, after) = if dir == "in" {
        (copy, String::new())
    } else {
        (String::new(), copy)
    };
    Some(format!(
        "{renamed}\nvoid main() {{\n{before}    {inner}();\n{after}}}\n"
    ))
}

// ---------------------------------------------------------------------------
// Error-driven repair
// ---------------------------------------------------------------------------

/// First error of a glslang log.
#[derive(Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub stage: Stage,
    /// 1-based source line.
    pub line: usize,
    pub token: String,
    pub message: String,
}

static ERROR_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"ERROR: \d+:(\d+): '([^']*)' :\s*(.*)").expect("valid regex"));

/// Parse the first error out of a `Vertex: ...` / `Fragment: ...` log as
/// returned by [`crate::glslang::compile`].
pub fn diagnostic(log: &str) -> Option<Diagnostic> {
    let stage = if log.starts_with("Vertex:") {
        Stage::Vertex
    } else if log.starts_with("Fragment:") {
        Stage::Fragment
    } else if log.starts_with("Geometry:") {
        Stage::Geometry
    } else {
        return None;
    };
    let c = ERROR_LINE.captures(log)?;
    Some(Diagnostic {
        stage,
        line: c[1].parse().ok()?,
        token: c[2].to_owned(),
        message: c[3].trim().to_owned(),
    })
}

/// Rewrite the statement `diag` points at so it means what it means in HLSL.
/// `None` when the error is not one we know how to repair.
pub fn repair(src: &str, diag: &Diagnostic) -> Option<String> {
    let msg = diag.message.as_str();
    if let Some(name) = msg
        .strip_prefix("l-value required \"")
        .and_then(|m| m.split('"').next())
        .filter(|_| msg.contains("can't modify shader input"))
    {
        let ty = shadow_type(src, name)?;
        return shadow_varying(src, name, &ty);
    }

    let (start, end) = statement_lines(src, diag.line)?;
    let region = &src[start..end];
    let syms = Symbols::scan(src);
    let fixed = if let Some(types) = msg.strip_prefix("cannot convert from ") {
        let (from, to) = conversion_types(types)?;
        fix_conversion(region, &syms, &diag.token, &from, &to)
    } else if msg.starts_with("cannot convert return value to function return type") {
        let to = return_type(&src[..start])?;
        fix_conversion(region, &syms, "return", "", &to)
    } else if msg.starts_with("no matching overloaded function found") {
        fix_overload(src, region, &diag.token)
    } else if diag.token == "%" && msg.starts_with("wrong operand types") {
        Some(fix_float_modulo(region))
    } else if msg.starts_with("wrong operand types") {
        let (left, right) = operand_types(msg)?;
        fix_vector_operands(region, &syms, &left, &right)
    } else if diag.token == "!" && msg.starts_with("wrong operand type") {
        Some(fix_float_not(region, &syms))
    } else if diag.token == "[]" && msg.starts_with("scalar integer expression required") {
        Some(fix_float_index(region))
    } else if msg.starts_with("left of '[' is not of type array") {
        fix_packed_spectrum(region)
    } else if diag.token == "non-constant initializer" {
        Some(CONST.replace_all(region, "").into_owned())
    } else if msg.starts_with("boolean expression expected") {
        fix_float_condition(region)
    } else {
        None
    }?;
    if fixed == region {
        return None;
    }
    let mut out = format!("{}{fixed}{}", &src[..start], &src[end..]);
    for (name, helper) in HELPERS.iter() {
        if fixed.contains(&format!("{name}(")) && !src.contains(&format!("{name}(")) {
            out = insert_helper(&out, helper);
        }
    }
    Some(out)
}

static CONST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bconst\s+").expect("valid regex"));

/// Type an input written to by the shader keeps when shadowed (its own).
fn shadow_type(src: &str, name: &str) -> Option<String> {
    let decl = Regex::new(&format!(
        r"(?m)^[ \t]*(?:\w+[ \t]+)*?in[ \t]+(?:\w+[ \t]+)*?(\w+)[ \t]+{}[ \t]*;",
        regex::escape(name)
    ))
    .expect("valid regex");
    Some(decl.captures(src)?[1].to_owned())
}

/// Byte range of the whole lines making up the statement(s) on `line`.
fn statement_lines(src: &str, line: usize) -> Option<(usize, usize)> {
    let lines: Vec<(usize, &str)> = src
        .split_inclusive('\n')
        .scan(0, |off, l| {
            let start = *off;
            *off += l.len();
            Some((start, l))
        })
        .collect();
    let idx = line.checked_sub(1)?;
    lines.get(idx)?;
    let ends = |l: &str| {
        let t = l.trim();
        t.ends_with([';', '{', '}']) || t.starts_with('#')
    };
    let mut first = idx;
    while first > 0 && idx - first < 12 && !ends(lines[first - 1].1) {
        first -= 1;
    }
    let mut last = idx;
    while last + 1 < lines.len() && last - idx < 12 && !ends(lines[last].1) {
        last += 1;
    }
    let (s, _) = lines[first];
    let (e, l) = lines[last];
    let e = e + l.trim_end_matches(['\n', '\r']).len();
    Some((s, e))
}

// --- GLSL types as glslang prints them --------------------------------------

/// A GLSL type parsed from glslang's description, e.g.
/// `' temp highp 2-component vector of float'` → `vec2`.
fn glsl_type(desc: &str) -> Option<String> {
    let desc = desc.trim().trim_matches('\'').trim();
    let words: Vec<&str> = desc.split_whitespace().collect();
    let base = *words.last()?;
    let prefix = match base {
        "float" => "",
        "int" => "i",
        "uint" => "u",
        "bool" => "b",
        _ => return None,
    };
    if let Some(i) = words.iter().position(|w| *w == "vector") {
        let n = words.get(i.checked_sub(1)?)?.strip_suffix("-component")?;
        return Some(format!("{prefix}vec{n}"));
    }
    if let Some(i) = words.iter().position(|w| *w == "matrix") {
        let dims = words.get(i.checked_sub(1)?)?;
        let (c, r) = dims.split_once('X')?;
        return Some(if c == r {
            format!("mat{c}")
        } else {
            format!("mat{c}x{r}")
        });
    }
    Some(base.to_owned())
}

/// `(scalar kind, component count)`; matrices are `None`.
fn shape(ty: &str) -> Option<(char, usize)> {
    match ty {
        "float" => Some(('f', 1)),
        "int" => Some(('i', 1)),
        "uint" => Some(('u', 1)),
        "bool" => Some(('b', 1)),
        _ => {
            let (prefix, n) = ty.split_once("vec")?;
            let kind = match prefix {
                "" => 'f',
                "i" => 'i',
                "u" => 'u',
                "b" => 'b',
                _ => return None,
            };
            Some((kind, n.parse().ok()?))
        }
    }
}

fn vector_type(kind: char, n: usize) -> String {
    let scalar = match kind {
        'i' => "int",
        'u' => "uint",
        'b' => "bool",
        _ => "float",
    };
    if n <= 1 {
        return scalar.to_owned();
    }
    let prefix = match kind {
        'i' => "i",
        'u' => "u",
        'b' => "b",
        _ => "",
    };
    format!("{prefix}vec{n}")
}

/// `' A' to ' B'` → `(A, B)`.
fn conversion_types(s: &str) -> Option<(String, String)> {
    let (from, to) = s.split_once("' to '")?;
    Some((glsl_type(from)?, glsl_type(to)?))
}

static OPERANDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"left-hand operand of type '([^']*)' and a right operand of type '([^']*)'")
        .expect("valid regex")
});

fn operand_types(msg: &str) -> Option<(String, String)> {
    let c = OPERANDS.captures(msg)?;
    Some((glsl_type(&c[1])?, glsl_type(&c[2])?))
}

// --- Symbols ------------------------------------------------------------------

static DECL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(float|int|uint|bool|[biu]?vec[234]|mat[234](?:x[234])?)\s+(\w+)\s*([\[;,=)])")
        .expect("valid regex")
});

/// Declared type of every name; names declared with different types in
/// different scopes are ambiguous and left out.
struct Symbols(HashMap<String, Option<String>>);

impl Symbols {
    fn scan(src: &str) -> Self {
        let mut map: HashMap<String, Option<String>> = HashMap::new();
        for c in DECL.captures_iter(src) {
            let (ty, name) = (c[1].to_owned(), c[2].to_owned());
            map.entry(name)
                .and_modify(|t| {
                    if t.as_deref() != Some(ty.as_str()) {
                        *t = None;
                    }
                })
                .or_insert(Some(ty));
        }
        Self(map)
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name)?.as_deref()
    }

    /// Type of an l-value like `a`, `a.rgb` or `a[i].x`.
    fn lvalue(&self, lv: &str) -> Option<String> {
        let lv = lv.trim();
        let base_end = lv
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(lv.len());
        let base = self.get(&lv[..base_end])?;
        let rest = &lv[base_end..];
        let rest = match rest.find(']') {
            Some(i) if rest.starts_with('[') => &rest[i + 1..],
            _ => rest,
        };
        if rest.is_empty() {
            return Some(base.to_owned());
        }
        let swizzle = rest.strip_prefix('.')?;
        if swizzle.contains(['.', '[']) {
            return None;
        }
        let (kind, _) = shape(base)?;
        Some(vector_type(kind, swizzle.len()))
    }
}

// --- Expression scanning ------------------------------------------------------

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Index of the bracket closing the one at `open`.
fn match_forward(b: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, &c) in b.iter().enumerate().skip(open) {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Index of the bracket opening the one at `close`.
fn match_backward(b: &[u8], close: usize) -> Option<usize> {
    let mut depth = 0i32;
    for i in (0..=close).rev() {
        match b[i] {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Start of the postfix expression (identifier, literal, call, swizzle,
/// index, parenthesised group) ending at `end`.
fn operand_start(b: &[u8], end: usize) -> usize {
    let mut i = end;
    while i > 0 && b[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    let stop = i;
    loop {
        if i == 0 {
            break;
        }
        let c = b[i - 1];
        if c == b')' || c == b']' {
            match match_backward(b, i - 1) {
                Some(o) => i = o,
                None => break,
            }
        } else if is_ident(c) || c == b'.' {
            while i > 0 && (is_ident(b[i - 1]) || b[i - 1] == b'.') {
                i -= 1;
            }
        } else {
            break;
        }
    }
    if i == stop { end } else { i }
}

/// End of the unary/postfix expression starting at `start`.
fn operand_end(b: &[u8], start: usize) -> usize {
    let mut i = start;
    let skip_ws = |i: &mut usize| {
        while *i < b.len() && b[*i].is_ascii_whitespace() {
            *i += 1;
        }
    };
    skip_ws(&mut i);
    while i < b.len() && matches!(b[i], b'-' | b'+' | b'!' | b'~') {
        i += 1;
        skip_ws(&mut i);
    }
    loop {
        if i >= b.len() {
            break;
        }
        let c = b[i];
        if c == b'(' || c == b'[' {
            match match_forward(b, i) {
                Some(close) => i = close + 1,
                None => break,
            }
        } else if is_ident(c) || c == b'.' {
            while i < b.len() && (is_ident(b[i]) || b[i] == b'.') {
                i += 1;
            }
        } else {
            break;
        }
    }
    i
}

/// Byte ranges to replace, applied back to front.
fn apply(src: &str, mut edits: Vec<(usize, usize, String)>) -> String {
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut out = src.to_owned();
    for (s, e, text) in edits {
        out.replace_range(s..e, &text);
    }
    out
}

fn trim_span(src: &str, s: usize, e: usize) -> (usize, usize) {
    let t = &src[s..e];
    let s2 = s + (t.len() - t.trim_start().len());
    let e2 = e - (t.len() - t.trim_end().len());
    (s2, e2.max(s2))
}

// --- Assignments ----------------------------------------------------------------

struct Assign {
    lhs: (usize, usize),
    rhs: (usize, usize),
    /// Type named by the enclosing declaration, if this is an initializer.
    decl: Option<String>,
}

static TYPE_BEFORE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(float|int|uint|bool|[biu]?vec[234]|mat[234](?:x[234])?)\s*$")
        .expect("valid regex")
});

/// Every `=` / compound assignment in `region` with its operands.
fn assignments(region: &str) -> Vec<Assign> {
    let b = region.as_bytes();
    let mut out = Vec::new();
    let mut decl: Option<String> = None;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b';' {
            decl = None;
        }
        if b[i] != b'=' {
            i += 1;
            continue;
        }
        if b.get(i + 1) == Some(&b'=') {
            i += 2;
            continue;
        }
        let prev = if i > 0 { b[i - 1] } else { b' ' };
        let mut op_start = i;
        match prev {
            b'!' => {
                i += 1;
                continue;
            }
            b'<' | b'>' => {
                if i >= 2 && b[i - 2] == prev {
                    op_start = i - 2;
                } else {
                    i += 1;
                    continue;
                }
            }
            b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^' => op_start = i - 1,
            _ => {}
        }
        let lhs_start = operand_start(b, op_start);
        let lhs = trim_span(region, lhs_start, op_start);
        let before = &region[..lhs.0];
        if let Some(c) = TYPE_BEFORE.captures(before) {
            decl = Some(c[1].to_owned());
        } else if !before.trim_end().ends_with(',') {
            decl = None;
        }
        // RHS runs to `;`, a top-level `,`, or the bracket enclosing the `=`.
        let mut depth = 0i32;
        let mut j = i + 1;
        while j < b.len() {
            match b[j] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                b';' if depth == 0 => break,
                b',' if depth == 0 => break,
                _ => {}
            }
            j += 1;
        }
        let rhs = trim_span(region, i + 1, j);
        out.push(Assign {
            lhs,
            rhs,
            decl: decl.clone(),
        });
        i = j;
    }
    out
}

static FUNCTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*(\w+)\s+\w+\s*\([^)]*\)\s*\{").expect("valid regex"));

/// Return type of the last function defined in `before`.
fn return_type(before: &str) -> Option<String> {
    let ty = FUNCTION.captures_iter(before).last()?[1].to_owned();
    shape(&ty).map(|_| ty)
}

static RETURN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\breturn\s+([^;]+);").expect("valid regex"));

/// HLSL converts implicitly on assignment: truncating vectors (`float2 =
/// float4`), broadcasting scalars (`float4 = 1.0`), taking `.x` of vectors
/// for scalars, and float↔int↔bool. A GLSL constructor of the target type
/// does each of these.
fn fix_conversion(
    region: &str,
    syms: &Symbols,
    token: &str,
    from: &str,
    to: &str,
) -> Option<String> {
    if let (Some((_, fw)), Some((_, tw))) = (shape(from), shape(to))
        && fw > 1
        && tw > fw
    {
        return narrow_lvalue(region, syms, to, fw);
    }
    if token == "return" {
        return Some(
            RETURN
                .replace(region, |c: &regex::Captures| {
                    format!("return {to}({});", c[1].trim())
                })
                .into_owned(),
        );
    }
    let float_to_integer =
        matches!(shape(from), Some(('f', _))) && matches!(shape(to), Some(('i' | 'u', _)));
    let all = assignments(region);
    let typed = |a: &Assign| {
        a.decl
            .clone()
            .or_else(|| syms.lvalue(&region[a.lhs.0..a.lhs.1]))
    };
    let mut pick: Vec<&Assign> = all
        .iter()
        .filter(|a| typed(a).as_deref() == Some(to))
        .collect();
    if pick.is_empty() {
        pick = if all.len() == 1 {
            all.iter().collect()
        } else {
            all.iter().filter(|a| typed(a).is_none()).collect()
        };
    }
    if pick.is_empty() {
        return None;
    }
    let edits = pick
        .iter()
        .filter(|a| a.rhs.0 < a.rhs.1)
        .map(|a| {
            let rhs = &region[a.rhs.0..a.rhs.1];
            let op = region[a.lhs.1..a.rhs.0].trim().trim_end_matches('=');
            // `int x *= 0.5` computes in float and converts the result.
            if !op.is_empty() && float_to_integer {
                let lhs = &region[a.lhs.0..a.lhs.1];
                (
                    a.lhs.0,
                    a.rhs.1,
                    format!("{lhs} = {to}({lhs} {op} ({rhs}))"),
                )
            } else {
                (a.rhs.0, a.rhs.1, format!("{to}({rhs})"))
            }
        })
        .collect();
    Some(apply(region, edits))
}

/// `float4 x; x *= float2(...)` writes the leading components in HLSL.
fn narrow_lvalue(region: &str, syms: &Symbols, to: &str, width: usize) -> Option<String> {
    let swizzle = &"xyzw"[..width];
    let edits: Vec<_> = assignments(region)
        .iter()
        .filter(|a| a.decl.is_none())
        .filter(|a| {
            let lv = &region[a.lhs.0..a.lhs.1];
            lv.bytes().all(is_ident) && syms.get(lv) == Some(to)
        })
        .map(|a| (a.lhs.1, a.lhs.1, format!(".{swizzle}")))
        .collect();
    (!edits.is_empty()).then(|| apply(region, edits))
}

// --- Overloads --------------------------------------------------------------------

/// Calls to `name(` in `region`: `(name start, '(' index, ')' index)`.
fn calls(region: &str, name: &str) -> Vec<(usize, usize, usize)> {
    let re = Regex::new(&format!(r"\b{}\s*\(", regex::escape(name))).expect("valid regex");
    let b = region.as_bytes();
    re.find_iter(region)
        .filter(|m| m.start() == 0 || b[m.start() - 1] != b'.')
        .filter_map(|m| {
            let open = m.end() - 1;
            Some((m.start(), open, match_forward(b, open)?))
        })
        .collect()
}

/// Top-level comma-separated argument spans between `open` and `close`.
fn args(region: &str, open: usize, close: usize) -> Vec<(usize, usize)> {
    let b = region.as_bytes();
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, open + 1);
    for (i, &c) in b.iter().enumerate().take(close).skip(open + 1) {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                out.push(trim_span(region, start, i));
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = trim_span(region, start, close);
    if last.0 < last.1 || !out.is_empty() {
        out.push(last);
    }
    out
}

static TEXTURE_DECL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:texture|sampler)(2D|3D|Cube|2DArray)\s+(\w+)\s*[;,)]").expect("valid regex")
});

/// HLSL truncates arguments to the parameter's width and broadcasts scalars;
/// GLSL wants exact overloads. `mix` (lerp) goes through broadcasting
/// helpers, `texture` gets its coordinate cut to the sampler's dimension, and
/// calls to a user function with a single definition get each argument
/// converted to its parameter type.
fn fix_overload(src: &str, region: &str, func: &str) -> Option<String> {
    if func == "mix" {
        let edits = calls(region, "mix")
            .into_iter()
            .map(|(s, open, _)| (s, open, "_wemix".to_owned()))
            .collect::<Vec<_>>();
        return (!edits.is_empty()).then(|| apply(region, edits));
    }
    let params: Vec<Option<String>> = if func == "texture" {
        Vec::new()
    } else {
        user_params(src, func)?
    };
    let mut edits = Vec::new();
    for (_, open, close) in calls(region, func) {
        let a = args(region, open, close);
        let types: Vec<Option<String>> = if func == "texture" {
            let Some(&(s, e)) = a.first() else { continue };
            let sampler = &region[s..e];
            let dim = TEXTURE_DECL.captures_iter(src).find_map(|c| {
                let n = &c[2];
                (n == sampler || n.strip_suffix(TEXTURE_SUFFIX) == Some(sampler))
                    .then(|| c[1].to_owned())
            });
            let coord = match dim.as_deref() {
                Some("2D") => "vec2",
                Some(_) => "vec3",
                None => continue,
            };
            vec![None, Some(coord.to_owned())]
        } else {
            if a.len() != params.len() {
                continue;
            }
            params.clone()
        };
        for ((s, e), ty) in a.iter().zip(&types) {
            if let Some(ty) = ty {
                let arg = &region[*s..*e];
                if !arg.starts_with(&format!("{ty}(")) {
                    edits.push((*s, *e, format!("{ty}({arg})")));
                }
            }
        }
    }
    (!edits.is_empty()).then(|| apply(region, edits))
}

/// Parameter types of `func`'s only definition (`None` for `out`/`inout`
/// and non-numeric parameters, which are left alone).
fn user_params(src: &str, func: &str) -> Option<Vec<Option<String>>> {
    let re = Regex::new(&format!(
        r"\b\w+\s+{}\s*\(([^)]*)\)\s*\{{",
        regex::escape(func)
    ))
    .expect("valid regex");
    let mut defs = re.captures_iter(src);
    let params = defs.next()?[1].to_owned();
    if defs.next().is_some() {
        return None;
    }
    Some(
        params
            .split(',')
            .filter(|p| !p.trim().is_empty() && p.trim() != "void")
            .map(|p| {
                let words: Vec<&str> = p.split_whitespace().collect();
                if words.iter().any(|w| *w == "out" || *w == "inout") {
                    return None;
                }
                let ty = words.iter().rev().nth(1)?;
                shape(ty).map(|_| (*ty).to_owned())
            })
            .collect(),
    )
}

/// `%` on floats is `fmod` in HLSL; GLSL only has integer `%`.
fn fix_float_modulo(region: &str) -> String {
    let mut out = region.to_owned();
    // Each rewrite consumes one `%`; bounded by their count.
    for _ in 0..region.matches('%').count() {
        let b = out.as_bytes();
        let Some(pos) = (0..b.len()).find(|&i| b[i] == b'%' && b.get(i + 1) != Some(&b'=')) else {
            break;
        };
        // `*`, `/` and `%` share precedence and associate left.
        let mut left = operand_start(b, pos);
        loop {
            let mut k = left;
            while k > 0 && b[k - 1].is_ascii_whitespace() {
                k -= 1;
            }
            if k > 0 && matches!(b[k - 1], b'*' | b'/') && (k < 2 || b[k - 2] != b'/') {
                let s = operand_start(b, k - 1);
                if s == k - 1 {
                    break;
                }
                left = s;
            } else {
                break;
            }
        }
        let right_end = operand_end(b, pos + 1);
        let (ls, le) = trim_span(&out, left, pos);
        let (rs, re) = trim_span(&out, pos + 1, right_end);
        if ls == le || rs == re {
            break;
        }
        let text = format!("_wefmod({}, {})", &out[ls..le], &out[rs..re]);
        out.replace_range(ls..re, &text);
    }
    out
}

static IDENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[A-Za-z_]\w*\b").expect("valid regex"));

/// HLSL truncates the wider operand of a binary operator between float
/// vectors of different widths (`float4 * float2` is a `float2`).
///
/// Mixed `uint`/`int` scalar arithmetic is done in `int` (GLSL 3.30 has no
/// implicit int→uint conversion); assignments convert the result back.
fn fix_vector_operands(region: &str, syms: &Symbols, left: &str, right: &str) -> Option<String> {
    let ((lk, lw), (rk, rw)) = (shape(left)?, shape(right)?);
    let (wide, narrow) = match (lk, rk) {
        ('f', 'f') if lw != rw && lw > 1 && rw > 1 => {
            (float_type(lw.max(rw)), float_type(lw.min(rw)))
        }
        ('u', 'i') | ('i', 'u') if lw == 1 && rw == 1 => ("uint", "int"),
        _ => return None,
    };
    let b = region.as_bytes();
    let lhs: Vec<(usize, usize)> = assignments(region).iter().map(|a| a.lhs).collect();
    let edits: Vec<_> = IDENT
        .find_iter(region)
        .filter(|m| syms.get(m.as_str()) == Some(wide))
        .filter(|m| m.start() == 0 || b[m.start() - 1] != b'.')
        .filter(|m| {
            let next = region[m.end()..].trim_start();
            !next.starts_with(['.', '(', '['])
        })
        .filter(|m| !lhs.iter().any(|&(s, e)| s <= m.start() && m.end() <= e))
        .filter(|m| !TYPE_BEFORE.is_match(&region[..m.start()]))
        .map(|m| (m.start(), m.end(), format!("{narrow}({})", m.as_str())))
        .collect();
    (!edits.is_empty()).then(|| apply(region, edits))
}

/// HLSL's `!` takes any numeric scalar (true when zero) and its result
/// converts back to float in arithmetic.
fn fix_float_not(region: &str, syms: &Symbols) -> String {
    let b = region.as_bytes();
    let mut edits = Vec::new();
    for i in 0..b.len() {
        if b[i] != b'!' || b.get(i + 1) == Some(&b'=') {
            continue;
        }
        let end = operand_end(b, i + 1);
        let (s, e) = trim_span(region, i + 1, end);
        if s == e {
            continue;
        }
        let operand = &region[s..e];
        let numeric = matches!(syms.get(operand), Some("float" | "int" | "uint"));
        let text = if numeric {
            format!("float(!bool({operand}))")
        } else {
            format!("!bool({operand})")
        };
        edits.push((i, e, text));
    }
    apply(region, edits)
}

/// HLSL truncates float array indices.
fn fix_float_index(region: &str) -> String {
    let b = region.as_bytes();
    let mut edits = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'['
            && i > 0
            && (is_ident(b[i - 1]) || b[i - 1] == b']')
            && let Some(close) = match_forward(b, i)
        {
            let (s, e) = trim_span(region, i + 1, close);
            let inner = &region[s..e];
            if !inner.is_empty() && !inner.bytes().all(|c| c.is_ascii_digit()) {
                edits.push((s, e, format!("int({inner})")));
            }
            i = close;
        }
        i += 1;
    }
    apply(region, edits)
}

static SPECTRUM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bg_AudioSpectrum\d+(?:Left|Right)\s*\[").expect("valid regex"));

/// HLSL sources may index the audio spectrum as packed `float4`s
/// (`s[i / 4][i % 4]`); ours is a flat `float` array.
fn fix_packed_spectrum(region: &str) -> Option<String> {
    let b = region.as_bytes();
    let edits: Vec<_> = SPECTRUM
        .find_iter(region)
        .filter_map(|m| {
            let open = m.end() - 1;
            let close = match_forward(b, open)?;
            if b.get(close + 1) != Some(&b'[') {
                return None;
            }
            let close2 = match_forward(b, close + 1)?;
            let (i, j) = (&region[open + 1..close], &region[close + 2..close2]);
            Some((open, close2 + 1, format!("[int({i}) * 4 + int({j})]")))
        })
        .collect();
    (!edits.is_empty()).then(|| apply(region, edits))
}

static IF_COND: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(if|while)\s*\(").expect("valid regex"));

/// HLSL `if (x)` and `x ? a : b` accept numeric conditions.
fn fix_float_condition(region: &str) -> Option<String> {
    let b = region.as_bytes();
    let mut edits: Vec<_> = IF_COND
        .find_iter(region)
        .filter_map(|m| {
            let open = m.end() - 1;
            let close = match_forward(b, open)?;
            let inner = &region[open + 1..close];
            Some((open + 1, close, format!("bool({inner})")))
        })
        .collect();
    for q in (0..b.len()).filter(|&i| b[i] == b'?') {
        let start = condition_start(b, q);
        let (s, e) = trim_span(region, start, q);
        if s < e && !edits.iter().any(|&(es, ee, _)| es <= s && e <= ee) {
            edits.push((s, e, format!("bool({})", &region[s..e])));
        }
    }
    (!edits.is_empty()).then(|| apply(region, edits))
}

/// Start of the condition of the `?` at `q`: back to the enclosing bracket,
/// a top-level `,`/`;`, an assignment or `return`.
fn condition_start(b: &[u8], q: usize) -> usize {
    let mut depth = 0i32;
    let mut i = q;
    while i > 0 {
        let c = b[i - 1];
        match c {
            b')' | b']' => depth += 1,
            b'(' | b'[' | b'{' => {
                if depth == 0 {
                    return i;
                }
                depth -= 1;
            }
            b',' | b';' | b'?' | b':' if depth == 0 => return i,
            b'=' if depth == 0 => {
                let p = if i >= 2 { b[i - 2] } else { b' ' };
                if !matches!(p, b'=' | b'!' | b'<' | b'>') && b.get(i) != Some(&b'=') {
                    return i;
                }
            }
            _ => {}
        }
        if depth == 0 && i >= 6 && &b[i - 6..i] == b"return" {
            return i;
        }
        i -= 1;
    }
    0
}

// --- Helpers ------------------------------------------------------------------

static HELPERS: LazyLock<Vec<(&'static str, String)>> =
    LazyLock::new(|| vec![("_wemix", mix_helpers()), ("_wefmod", fmod_helpers())]);

/// `_wemix` overloads for every float scalar/vector combination; operands
/// are converted to the narrowest vector width present, like HLSL `lerp`.
fn mix_helpers() -> String {
    let mut out = String::new();
    for a in 1..=4 {
        for b in 1..=4 {
            for t in 1..=4 {
                let n = [a, b, t].into_iter().filter(|&w| w > 1).min().unwrap_or(1);
                let ty = float_type(n);
                out.push_str(&format!(
                    "{ty} _wemix({} a, {} b, {} t) {{ return mix({ty}(a), {ty}(b), {ty}(t)); }}\n",
                    float_type(a),
                    float_type(b),
                    float_type(t)
                ));
            }
        }
    }
    out
}

/// HLSL `%` / `fmod` on floats: the remainder truncated towards zero.
fn fmod_helpers() -> String {
    let mut out =
        String::from("float _wefmod(float x, float y) { return x - y * trunc(x / y); }\n");
    for n in 2..=4 {
        let v = float_type(n);
        out.push_str(&format!(
            "{v} _wefmod({v} x, float y) {{ return x - y * trunc(x / y); }}\n\
             {v} _wefmod({v} x, {v} y) {{ return x - y * trunc(x / y); }}\n\
             {v} _wefmod(float x, {v} y) {{ return x - y * trunc(x / y); }}\n"
        ));
    }
    out
}

/// Insert helper functions after the leading `#version`/`#extension` lines.
fn insert_helper(src: &str, helper: &str) -> String {
    let mut pos = 0;
    for line in src.split_inclusive('\n') {
        let t = line.trim();
        if t.starts_with("#version") || t.starts_with("#extension") || t.is_empty() {
            pos += line.len();
        } else {
            break;
        }
    }
    format!("{}{helper}{}", &src[..pos], &src[pos..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rewrite::scan_io;

    fn diag(stage: Stage, line: usize, token: &str, message: &str) -> Diagnostic {
        Diagnostic {
            stage,
            line,
            token: token.to_owned(),
            message: message.to_owned(),
        }
    }

    fn frag(line: usize, token: &str, message: &str) -> Diagnostic {
        diag(Stage::Fragment, line, token, message)
    }

    /// Compile `vs`/`fs`, repairing until they build.
    fn build(vs: &str, fs: &str) -> Result<(String, String), String> {
        let (mut vs, mut fs) = (vs.to_owned(), fs.to_owned());
        for _ in 0..16 {
            match crate::glslang::compile(&[(Stage::Vertex, &vs), (Stage::Fragment, &fs)]) {
                Ok(_) => return Ok((vs, fs)),
                Err(e) => {
                    let d = diagnostic(&e).ok_or(e.clone())?;
                    let src = if d.stage == Stage::Vertex {
                        &mut vs
                    } else {
                        &mut fs
                    };
                    *src = repair(src, &d).ok_or(e)?;
                }
            }
        }
        Err("too many repairs".to_owned())
    }

    const VS: &str = "#version 330\nin vec3 a_Position;\nout vec2 v_TexCoord;\n\
                      void main() { v_TexCoord = a_Position.xy; gl_Position = vec4(a_Position, 1.0); }\n";

    fn fs(body: &str) -> String {
        format!(
            "#version 330\nin vec2 v_TexCoord;\nout vec4 glOutColor;\nuniform float u_F;\n\
             uniform sampler2D g_Texture0;\nvoid main() {{\n{body}\n}}\n"
        )
    }

    #[test]
    fn parses_first_error() {
        let log = "Fragment: ERROR: 0:414: '=' :  cannot convert from ' smooth in highp 4-component vector of float' to ' temp highp 2-component vector of float'\nERROR: 0:414: '' : compilation terminated";
        let d = diagnostic(log).unwrap();
        assert_eq!(d.line, 414);
        assert_eq!(d.token, "=");
        assert_eq!(
            conversion_types(d.message.strip_prefix("cannot convert from ").unwrap()),
            Some(("vec4".to_owned(), "vec2".to_owned()))
        );
        assert_eq!(glsl_type(" uniform highp float"), Some("float".to_owned()));
        assert_eq!(
            glsl_type(" temp highp 4X4 matrix of float"),
            Some("mat4".to_owned())
        );
    }

    #[test]
    fn audio_shift_is_clamped() {
        assert_eq!(
            clamp_audio_reactive_shift("uv += u_rOffset * v_AudioShift;"),
            "uv += u_rOffset * min(v_AudioShift, 1.0);"
        );
        assert_eq!(
            clamp_audio_reactive_shift("x = v_AudioShift;"),
            "x = v_AudioShift;"
        );
    }

    #[test]
    fn combine_alpha_removed_only_for_same_variable() {
        let out = fix_combine_alpha(" albedo.a = (clamp( albedo.a + rays.a , 0.0 , 1.0 )) ;\n");
        assert!(out.trim_start().starts_with("// albedo.a"), "{out}");
        let keep = " albedo.a = (clamp(prev.a + albedo.a, 0.0, 1.0));\n";
        assert_eq!(fix_combine_alpha(keep), keep);
    }

    #[test]
    fn effect_alpha_injected_for_colour_only_writes() {
        let head = "uniform sampler2D g_Texture0;\nin vec2 v_TexCoord;\n";
        let inject = "glOutColor.a = texture(g_Texture0, v_TexCoord.xy).a;";
        let out = fix_effect_alpha(&format!(
            "{head}void main() {{\nglOutColor.rgb = vec3(1.0);\n}}\nvoid f() {{}}\n"
        ));
        let at = out.find(inject).unwrap();
        assert!(at < out.find("void f").unwrap());
        for body in [
            "glOutColor = vec4(1.0);",
            "glOutColor.rgb = vec3(1.0);\nglOutColor.a = 1.0;",
            "glOutColor.rgba = vec4(1.0);",
            "glOutColor.xyzw = vec4(1.0);",
        ] {
            let src = format!("{head}void main() {{\n{body}\n}}");
            assert_eq!(fix_effect_alpha(&src), src);
        }
        let no_tex = "void main() {\nglOutColor.r = 0.5;\n}";
        assert_eq!(fix_effect_alpha(no_tex), no_tex);
    }

    #[test]
    fn fragment_gl_position_is_frag_coord() {
        assert_eq!(
            fix_fragment_gl_position("vec2 a = gl_Position.xy; float b = gl_Position.z;"),
            "vec2 a = gl_FragCoord.xy; float b = gl_FragCoord.z;"
        );
    }

    #[test]
    fn conversions_use_target_constructor() {
        let src = "float u;\nvec4 v;\nvoid main() {\n vec2 c = v; int i = u;\n}\n";
        let out = repair(
            src,
            &frag(4, "=", "cannot convert from ' smooth in highp 4-component vector of float' to ' temp highp 2-component vector of float'"),
        )
        .unwrap();
        assert!(out.contains(" vec2 c = vec2(v); int i = u;"), "{out}");
        let out = repair(
            "void main() {\n vec2 radial = 0.0, tangential = 0.0, center = (1.0 - 0.5);\n}",
            &frag(
                2,
                "=",
                "cannot convert from ' const float' to ' temp highp 2-component vector of float'",
            ),
        )
        .unwrap();
        assert!(
            out.contains(
                "vec2 radial = vec2(0.0), tangential = vec2(0.0), center = vec2((1.0 - 0.5));"
            ),
            "{out}"
        );
        let out = repair(
            "uniform float n;\nvoid main() {\n for (int i = - n; i <= n; i ++) {\n }\n}",
            &frag(
                3,
                "=",
                "cannot convert from ' temp highp float' to ' temp highp int'",
            ),
        )
        .unwrap();
        assert!(
            out.contains("for (int i = int(- n); i <= n; i ++) {"),
            "{out}"
        );
        let out = repair(
            "vec2 s;\nvoid main() {\n s.x += 1.0 - 2.0 < 0.0;\n}",
            &frag(
                3,
                "assign",
                "cannot convert from ' temp bool' to ' temp highp float'",
            ),
        )
        .unwrap();
        assert!(out.contains("s.x += float(1.0 - 2.0 < 0.0);"), "{out}");
        let out = repair(
            "int bar;\nvoid main() {\n bar *= 1.0 - f;\n}",
            &frag(
                3,
                "assign",
                "cannot convert from ' temp highp float' to ' temp highp int'",
            ),
        )
        .unwrap();
        assert!(out.contains(" bar = int(bar * (1.0 - f));"), "{out}");
    }

    #[test]
    fn return_value_converted_to_return_type() {
        let src = "vec3 f(vec2 uv)\n{\n    vec4 col = vec4(0.0);\n    return col;\n}";
        let d = frag(
            4,
            "return",
            "cannot convert return value to function return type",
        );
        assert!(
            repair(src, &d)
                .unwrap()
                .contains("    return vec3(col);\n}")
        );
    }

    #[test]
    fn widening_conversion_is_not_repaired() {
        let src = "void main() {\n vec4 c = vec2(1.0);\n}";
        let d = frag(
            2,
            "=",
            "cannot convert from ' temp highp 2-component vector of float' to ' temp highp 4-component vector of float'",
        );
        assert_eq!(repair(src, &d), None);
    }

    #[test]
    fn float_modulo_becomes_fmod() {
        assert_eq!(
            fix_float_modulo(" uint b = frequency % 64;"),
            " uint b = _wefmod(frequency, 64);"
        );
        assert_eq!(
            fix_float_modulo(" vec4 o = (k3 * (1.0 / 41.0)) % 1;"),
            " vec4 o = _wefmod((k3 * (1.0 / 41.0)), 1);"
        );
        assert_eq!(fix_float_modulo("x = a * b % c;"), "x = _wefmod(a * b, c);");
        assert_eq!(
            fix_float_modulo("x = f(a)[1] % -c.x;"),
            "x = _wefmod(f(a)[1], -c.x);"
        );
    }

    #[test]
    fn mix_broadcasts_and_truncates() {
        let src = "#version 330\nvoid main() {\n albedo.rgb = mix(albedo, n, mask);\n}";
        let out = repair(
            src,
            &frag(3, "mix", "no matching overloaded function found"),
        )
        .unwrap();
        assert!(out.contains("albedo.rgb = _wemix(albedo, n, mask);"));
        assert!(out.contains(
            "vec3 _wemix(vec4 a, vec3 b, float t) { return mix(vec3(a), vec3(b), vec3(t)); }"
        ));
        assert!(out.starts_with("#version 330\nfloat _wemix"), "{out}");
    }

    #[test]
    fn texture_coordinate_cut_to_sampler_dimension() {
        let src = "uniform texture2D g_Texture0_wet;\nvoid main() {\n vec4 s = texture(g_Texture0, v_TexCoord);\n}";
        let out = repair(
            src,
            &frag(3, "texture", "no matching overloaded function found"),
        )
        .unwrap();
        assert!(
            out.contains("texture(g_Texture0, vec2(v_TexCoord));"),
            "{out}"
        );
    }

    #[test]
    fn user_function_arguments_converted() {
        let src = "vec2 rot(vec2 v, float a) {\n return v;\n}\nvoid main() {\n vec2 c = rot(v_TexCoord, - u + 1.5) * 2.0;\n}";
        let out = repair(
            src,
            &frag(5, "rot", "no matching overloaded function found"),
        )
        .unwrap();
        assert!(
            out.contains("rot(vec2(v_TexCoord), float(- u + 1.5)) * 2.0;"),
            "{out}"
        );
    }

    #[test]
    fn wider_vector_operand_truncated() {
        let src =
            "out vec4 v_P;\nuniform vec2 g_S;\nvoid main() {\n vec2 da = v_P * g_S * 0.001;\n}";
        let d = frag(
            4,
            "*",
            "wrong operand types: no operation '*' exists that takes a left-hand operand of type ' smooth out highp 4-component vector of float' and a right operand of type ' uniform highp 2-component vector of float' (or there is no acceptable conversion)",
        );
        let out = repair(src, &d).unwrap();
        assert!(out.contains("vec2 da = vec2(v_P) * g_S * 0.001;"), "{out}");
    }

    #[test]
    fn misc_statement_fixes() {
        assert_eq!(
            fix_float_index("x = a[barID / 4][barID % 4] + b[2];"),
            "x = a[int(barID / 4)][int(barID % 4)] + b[2];"
        );
        let syms = Symbols::scan("float horizontal;");
        assert_eq!(
            fix_float_not("t = 0 == 1 ? ! horizontal : horizontal;", &syms),
            "t = 0 == 1 ? float(!bool(horizontal)) : horizontal;"
        );
        assert_eq!(
            fix_float_condition(" if (u_F) {").unwrap(),
            " if (bool(u_F)) {"
        );
        assert_eq!(
            fix_float_condition(" mask = 0 ? 1 - mask : f(a, b ? c : d);").unwrap(),
            " mask = bool(0) ? 1 - mask : f(a, bool(b) ? c : d);"
        );
        assert_eq!(
            fix_packed_spectrum("clamp(0., 1., g_AudioSpectrum64Left[int(id / 4)][int(m)]);")
                .unwrap(),
            "clamp(0., 1., g_AudioSpectrum64Left[int(int(id / 4)) * 4 + int(int(m))]);"
        );
        let syms = Symbols::scan("uint f1;\nvec4 t;\n");
        assert_eq!(
            fix_vector_operands(" uint f2 = (f1 + 1) % 32;", &syms, "uint", "int").unwrap(),
            " uint f2 = (int(f1) + 1) % 32;"
        );
        assert_eq!(
            narrow_lvalue(" t *= aspect;", &syms, "vec4", 2).unwrap(),
            " t.xy *= aspect;"
        );
        let out = repair(
            "void main() {\n const float c = cos(u);\n}",
            &frag(
                2,
                "non-constant initializer",
                "not supported for this version or the enabled extensions",
            ),
        )
        .unwrap();
        assert!(out.contains(" float c = cos(u);"));
    }

    #[test]
    fn written_input_gets_private_copy() {
        let src = fs("v_TexCoord = v_TexCoord.yx;\nglOutColor = texture(g_Texture0, v_TexCoord);");
        let (_, out) = build(VS, &src).unwrap();
        assert!(
            out.contains("in vec2 v_TexCoord;\nvec2 _we_v_TexCoord;"),
            "{out}"
        );
        assert!(out.contains("_we_v_TexCoord = _we_v_TexCoord.yx;"));
        assert!(
            out.contains("void main() {\n    _we_v_TexCoord = v_TexCoord;\n    _we_io_main0();\n}")
        );
    }

    #[test]
    fn mismatched_varyings_and_uniforms_reconciled() {
        let vs = "#version 330\nin vec3 a_Position;\nout vec2 v_TexCoord;\nuniform vec2 u_R;\n\
                  void main() { v_TexCoord = a_Position.xy * u_R; gl_Position = vec4(a_Position, 1.0); }\n";
        let fs = "#version 330\nin vec4 v_TexCoord;\nout vec4 glOutColor;\nuniform float u_R;\n\
                  void main() { glOutColor = v_TexCoord * u_R; }\n";
        let (v, f) = reconcile(vs, fs, &scan_io(vs), &scan_io(fs));
        assert!(
            v.contains("out vec4 v_TexCoord;\nvec2 _we_v_TexCoord;"),
            "{v}"
        );
        assert!(
            v.contains("v_TexCoord = vec4(_we_v_TexCoord, 0.0, 0.0);"),
            "{v}"
        );
        assert!(
            f.contains("uniform vec2 u_R;") && f.contains("v_TexCoord * u_R.x;"),
            "{f}"
        );
        build(&v, &f).unwrap();
    }

    #[test]
    fn repairs_compose_until_compiling() {
        let body = "int n = u_F;\nfloat m = texture(g_Texture0, v_TexCoord);\n\
                    uint b = u_F % 64;\nvec4 c = 1.0;\nc.rgb = mix(0.5, c, m);\nglOutColor = c;";
        build(VS, &fs(body)).unwrap();
    }
}
