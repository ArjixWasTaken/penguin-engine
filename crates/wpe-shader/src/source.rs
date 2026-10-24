//! Source-level handling of WE shaders before the GLSL preprocessor runs:
//! `#include` expansion and the JSON annotations in comments.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Deserialize;
use wpe_vfs::Vfs;

/// Includes nested deeper than this are treated as cycles.
const MAX_INCLUDE_DEPTH: usize = 16;

/// What the scene binds to one `g_Texture<N>` slot.
#[derive(Debug, Clone, Copy, Default)]
pub struct TexSlot {
    /// A texture is bound.
    pub enabled: bool,
    /// The texture's `COMPO1..3` header flags.
    pub components: [bool; 3],
}

/// Facts gathered from a shader's annotations.
#[derive(Debug, Clone, Default)]
pub struct ShaderInfo {
    /// Combo defaults: `// [COMBO]` lines plus those implied by texture bindings.
    pub combos: BTreeMap<String, String>,
    /// Material constant name (`"material":"Brightness"`) → uniform name.
    pub aliases: HashMap<String, String>,
    /// Uniform name → declared default value.
    pub defaults: HashMap<String, Vec<f32>>,
    /// Texture slot → default texture name.
    pub default_textures: BTreeMap<usize, String>,
}

impl ShaderInfo {
    /// The uniform a material `constantshadervalues` key refers to: an alias
    /// match, or `g_<key>` when such a uniform has a default.
    pub fn uniform_for_constant(&self, key: &str) -> Option<String> {
        self.aliases.get(key).cloned().or_else(|| {
            let g = format!("g_{key}");
            self.defaults.contains_key(&g).then_some(g)
        })
    }
}

fn is_commented(line: &str, at: usize) -> bool {
    line[..at].contains("//")
}

/// Expand `#include "name"` lines recursively from `shaders/<name>`,
/// wrapping each in `//-----include` markers.
pub fn expand_includes(vfs: &Vfs, src: &str) -> String {
    expand(vfs, src, 0, &mut HashSet::new())
}

fn expand(vfs: &Vfs, src: &str, depth: usize, active: &mut HashSet<String>) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.split_inclusive('\n') {
        let Some(at) = line.find("#include").filter(|&at| !is_commented(line, at)) else {
            out.push_str(line);
            continue;
        };
        let name = match (line.find('"'), line.rfind('"')) {
            (Some(a), Some(b)) if b > a + 1 => &line[a + 1..b],
            _ => {
                tracing::info!("skipping malformed include: {}", line.trim());
                out.push_str(line);
                continue;
            }
        };
        if depth >= MAX_INCLUDE_DEPTH || !active.insert(name.to_owned()) {
            tracing::error!("#include \"{name}\" is cyclic, skipping");
            out.push_str(line);
            continue;
        }
        let text = vfs
            .read_to_string(&format!("shaders/{name}"))
            .inspect_err(|e| tracing::warn!("include {name}: {e}"))
            .unwrap_or_default();
        out.push_str(&line[..at]);
        out.push_str("\n//-----include ");
        out.push_str(name);
        out.push('\n');
        out.push_str(&expand(vfs, &text, depth + 1, active));
        out.push_str("\n//-----include end\n");
        active.remove(name);
    }
    out
}

/// Tracks `#if` nesting so a texture declared inside its own combo's guard
/// is not mistaken for one the scene bound.
#[derive(Default)]
struct ComboGuard {
    open: Vec<String>,
}

impl ComboGuard {
    fn on_line(&mut self, line: &str) {
        let st = line.trim_start_matches([' ', '\t']);
        if !st.starts_with('#') {
            return;
        }
        if st.starts_with("#endif") {
            self.open.pop();
        } else if st.starts_with("#elif") {
            match self.open.last_mut() {
                Some(last) => *last = st.to_owned(),
                None => self.open.push(st.to_owned()),
            }
        } else if st.starts_with("#if") {
            self.open.push(st.to_owned());
        }
    }

    fn guards(&self, combo: &str) -> bool {
        !combo.is_empty()
            && self
                .open
                .iter()
                .any(|cond| mentions_identifier(cond, combo))
    }
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn mentions_identifier(text: &str, name: &str) -> bool {
    text.match_indices(name).any(|(at, _)| {
        let b = text.as_bytes();
        let left = at == 0 || !is_ident(b[at - 1]);
        let after = at + name.len();
        left && (after >= b.len() || !is_ident(b[after]))
    })
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ComboAnnotation {
    combo: Option<String>,
    default: serde_json::Value,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Component {
    combo: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct UniformAnnotation {
    material: Option<String>,
    default: Option<serde_json::Value>,
    combo: Option<String>,
    formatcombo: bool,
    format: Option<String>,
    components: Vec<Component>,
}

fn parse_floats(v: &serde_json::Value) -> Option<Vec<f32>> {
    match v {
        serde_json::Value::Number(n) => Some(vec![n.as_f64()? as f32]),
        serde_json::Value::Bool(b) => Some(vec![*b as u8 as f32]),
        serde_json::Value::String(s) => s
            .split_whitespace()
            .map(|t| t.parse().ok())
            .collect::<Option<Vec<f32>>>(),
        _ => None,
    }
}

/// Collect annotations from `src` up to `void main()`.
fn scan_annotations(src: &str, info: &mut ShaderInfo, slots: &[TexSlot]) {
    let mut guard = ComboGuard::default();
    for line in src.lines() {
        guard.on_line(line);
        if line.contains("// [COMBO]") {
            if let Some(brace) = line.find('{')
                && let Ok(c) = serde_json::from_str::<ComboAnnotation>(&line[brace..])
                && let Some(name) = c.combo
            {
                let value = c.default.as_i64().unwrap_or(0);
                info.combos.insert(name, value.to_string());
            }
        } else if line.contains("uniform ")
            && line.contains("// {")
            && let Some(brace) = line.find('{')
            && let Ok(ann) = serde_json::from_str::<UniformAnnotation>(&line[brace..])
        {
            let decl = &line[..line.find(';').unwrap_or(line.len())];
            let Some(name) = decl.split_whitespace().last() else {
                continue;
            };
            if let Some(material) = &ann.material {
                info.aliases.insert(material.clone(), name.to_owned());
            }
            if let Some(slot) = name
                .strip_prefix("g_Texture")
                .and_then(|n| n.parse::<usize>().ok())
            {
                let default = ann
                    .default
                    .as_ref()
                    .and_then(|d| d.as_str())
                    .filter(|d| !d.is_empty());
                if let Some(d) = default {
                    info.default_textures.insert(slot, d.to_owned());
                }
                let filled = slots.get(slot).is_some_and(|s| s.enabled);
                let self_guarded = ann.combo.as_deref().is_some_and(|c| guard.guards(c));
                let bound = filled || (default.is_some() && !self_guarded);
                let flag = if bound { "1" } else { "0" };
                if let Some(combo) = &ann.combo {
                    info.combos.insert(combo.clone(), flag.to_owned());
                }
                if ann.formatcombo
                    && let Some(format) = ann.format.as_deref().filter(|f| !f.is_empty())
                {
                    info.combos
                        .insert(format.to_ascii_uppercase(), flag.to_owned());
                }
                if filled {
                    let compos = slots[slot].components;
                    for (on, c) in compos.iter().zip(&ann.components) {
                        if *on {
                            info.combos.insert(c.combo.clone(), "1".to_owned());
                        }
                    }
                }
            } else {
                if let Some(v) = ann.default.as_ref().and_then(parse_floats) {
                    info.defaults.insert(name.to_owned(), v);
                }
                if let Some(combo) = ann.combo {
                    info.combos.insert(combo, "1".to_owned());
                }
            }
        }
        if line.contains("void main()") {
            break;
        }
    }
}

/// Byte offset of the line after the last top-level `attribute`, `varying`
/// or `uniform` declaration, moved past any enclosing `#if` block and never
/// after `void main(`.
fn include_insert_pos(src: &str) -> usize {
    let Some(main) = src.find("void main(") else {
        return 0;
    };
    if src[main + 2..].contains("void main(") {
        return 0;
    }
    let mut pos = 0;
    let mut offset = 0;
    for line in src.split_inclusive('\n') {
        offset += line.len();
        if ["attribute ", "varying ", "uniform "]
            .iter()
            .any(|kw| line.starts_with(kw))
        {
            pos = offset;
        }
    }
    pos = pos.min(main);

    let mut stack = Vec::new();
    let mut offset = 0;
    for line in src.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        if start > main {
            break;
        }
        let t = line.trim_start();
        if t.starts_with("#endif") {
            if let Some(if_start) = stack.pop()
                && pos > if_start
                && pos <= start
            {
                pos = offset;
            }
        } else if t.starts_with("#if") {
            stack.push(start);
        }
    }
    pos.min(main)
}

/// Prepare one stage's source: collect its includes, scan annotations in the
/// includes and the body, and re-insert the expanded includes after the
/// declarations.
pub fn prepare(vfs: &Vfs, src: &str, info: &mut ShaderInfo, slots: &[TexSlot]) -> String {
    let mut body = String::with_capacity(src.len());
    let mut includes = String::new();
    for line in src.split_inclusive('\n') {
        match line.find("#include") {
            Some(at) if !is_commented(line, at) => {
                includes.push_str(line.trim_end_matches(['\r', '\n']));
                includes.push('\n');
                // Keep byte offsets stable for diagnostics.
                body.extend(line.chars().map(|c| if c == '\n' { '\n' } else { ' ' }));
            }
            _ => body.push_str(line),
        }
    }
    if !body.ends_with('\n') {
        body.push('\n');
    }
    let includes = expand_includes(vfs, &includes);
    scan_annotations(&includes, info, slots);
    scan_annotations(&body, info, slots);
    let at = include_insert_pos(&body);
    body.insert_str(at, &includes);
    body
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;
    use wpe_vfs::{FileSystem, VfsError};

    struct MemFs(HashMap<&'static str, &'static str>);

    impl FileSystem for MemFs {
        fn contains(&self, path: &str) -> bool {
            self.0.contains_key(path)
        }
        fn read(&self, path: &str) -> wpe_vfs::Result<Cow<'_, [u8]>> {
            self.0
                .get(path)
                .map(|s| Cow::Borrowed(s.as_bytes()))
                .ok_or_else(|| VfsError::NotFound(path.to_owned()))
        }
        fn files(&self) -> Box<dyn Iterator<Item = &str> + '_> {
            Box::new(self.0.keys().copied())
        }
    }

    fn vfs(files: &[(&'static str, &'static str)]) -> Vfs {
        let mut vfs = Vfs::new();
        vfs.mount("mem", MemFs(files.iter().copied().collect()));
        vfs
    }

    #[test]
    fn includes_expand_and_cycles_stop() {
        let v = vfs(&[
            ("shaders/a.h", "#include \"b.h\"\nfloat a;\n"),
            ("shaders/b.h", "#include \"a.h\"\nfloat b;\n"),
        ]);
        let out = expand_includes(&v, "#include \"a.h\"\n// #include \"x.h\"\n");
        assert_eq!(out.matches("float a;").count(), 1);
        assert_eq!(out.matches("float b;").count(), 1);
        assert!(out.contains("// #include \"x.h\""));
    }

    #[test]
    fn annotations() {
        let src = r#"
// [COMBO] {"material":"ui","combo":"LIGHTING","default":1}
uniform sampler2D g_Texture0; // {"material":"albedo"}
uniform sampler2D g_Texture1; // {"combo":"MASK","default":"util/white"}
#if NORMALMAP
uniform sampler2D g_Texture2; // {"combo":"NORMALMAP","default":"util/flatnormal","format":"normalmap","formatcombo":true}
#endif
uniform sampler2D g_Texture3; // {"components":[{"combo":"METALLIC_MAP"},{"combo":"ROUGH"}]}
uniform float g_Brightness; // {"material":"Brightness","default":1.5}
uniform vec3 g_Tint; // {"default":"1 0.5 0","combo":"TINT"}
void main() {}
uniform float g_After; // {"default":3}
"#;
        let mut info = ShaderInfo::default();
        let slots = [
            TexSlot::default(),
            TexSlot::default(),
            TexSlot::default(),
            TexSlot {
                enabled: true,
                components: [false, true, false],
            },
        ];
        scan_annotations(src, &mut info, &slots);
        let combo = |k: &str| info.combos.get(k).map(String::as_str);
        assert_eq!(combo("LIGHTING"), Some("1"));
        assert_eq!(combo("MASK"), Some("1"), "a default texture binds the slot");
        assert_eq!(
            combo("NORMALMAP"),
            Some("0"),
            "self-guarded default does not"
        );
        assert_eq!(combo("ROUGH"), Some("1"));
        assert_eq!(combo("METALLIC_MAP"), None);
        assert_eq!(combo("TINT"), Some("1"));
        assert_eq!(info.aliases["Brightness"], "g_Brightness");
        assert_eq!(info.defaults["g_Tint"], [1.0, 0.5, 0.0]);
        assert!(!info.defaults.contains_key("g_After"));
        assert_eq!(info.default_textures[&1], "util/white");
        assert_eq!(
            info.uniform_for_constant("Brightness").as_deref(),
            Some("g_Brightness")
        );
        assert_eq!(info.uniform_for_constant("Tint").as_deref(), Some("g_Tint"));
    }

    #[test]
    fn includes_land_after_declarations_and_outside_ifs() {
        let v = vfs(&[("shaders/common.h", "float helper() { return 1.0; }\n")]);
        let src = "#include \"common.h\"\nuniform float u;\n#if X\nuniform float v;\n#endif\nvoid main() {}\n";
        let mut info = ShaderInfo::default();
        let out = prepare(&v, src, &mut info, &[]);
        let helper = out.find("float helper").unwrap();
        assert!(helper > out.find("#endif").unwrap());
        assert!(helper < out.find("void main").unwrap());
    }
}
