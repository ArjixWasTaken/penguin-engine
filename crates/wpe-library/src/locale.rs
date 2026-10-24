//! Property labels: a wallpaper's own translations
//! (`general.localization`), Wallpaper Engine's UI strings
//! (`locale/ui_<lang>.json`, which hold the `ui_browse_properties_*` keys
//! stock properties use), or the text as written.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

/// The language everything falls back to.
pub const FALLBACK: &str = "en-us";

/// Wallpaper Engine's UI strings in one language (with English behind).
#[derive(Debug, Clone, Default)]
pub struct Locale {
    lang: String,
    strings: HashMap<String, String>,
    fallback: HashMap<String, String>,
}

/// Lowercase, `-`-separated (`en_US.UTF-8` → `en-us`); `default` or empty
/// means English.
pub fn normalize_lang(lang: &str) -> String {
    let lang = lang.split(['.', '@']).next().unwrap_or("");
    let lang = lang.trim().to_ascii_lowercase().replace('_', "-");
    match lang.as_str() {
        "" | "default" | "c" | "posix" => FALLBACK.to_owned(),
        "zh-cn" | "zh-sg" | "zh-hans" | "zh" => "zh-chs".to_owned(),
        "zh-tw" | "zh-hk" | "zh-mo" | "zh-hant" => "zh-cht".to_owned(),
        _ => lang,
    }
}

/// The user's language from the environment (`LC_ALL`, `LC_MESSAGES`,
/// `LANG`), normalized; English if unset.
pub fn env_lang() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty())
        .map_or_else(|| FALLBACK.to_owned(), |v| normalize_lang(&v))
}

fn read_strings(path: &Path) -> Option<HashMap<String, String>> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: Value = wpe_assets::parse_json(&text)
        .map_err(|e| tracing::debug!("{}: {e}", path.display()))
        .ok()?;
    let Value::Object(map) = value else {
        return None;
    };
    Some(
        map.into_iter()
            .filter_map(|(k, v)| Some((k, v.as_str()?.to_owned())))
            .collect(),
    )
}

/// The `ui_<lang>.json` in `dir` for `lang`, or one of the same language
/// (`de` → `ui_de-de.json`).
fn find_file(dir: &Path, lang: &str) -> Option<(String, std::path::PathBuf)> {
    let exact = dir.join(format!("ui_{lang}.json"));
    if exact.is_file() {
        return Some((lang.to_owned(), exact));
    }
    let base = lang.split('-').next()?;
    let mut candidates: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let code = name.strip_prefix("ui_")?.strip_suffix(".json")?;
            (code.split('-').next() == Some(base)).then(|| code.to_owned())
        })
        .collect();
    candidates.sort();
    // Prefer `de-de` over `de-at` and the like.
    let code = candidates
        .iter()
        .find(|c| c.split_once('-').is_some_and(|(a, b)| a == b))
        .or(candidates.first())?
        .clone();
    let path = dir.join(format!("ui_{code}.json"));
    Some((code, path))
}

impl Locale {
    /// Wallpaper Engine's strings for `lang` from `we_dir/locale`. Missing
    /// files give an empty locale (labels are then the text itself).
    pub fn load(we_dir: &Path, lang: &str) -> Self {
        let dir = we_dir.join("locale");
        let lang = normalize_lang(lang);
        let fallback = read_strings(&dir.join(format!("ui_{FALLBACK}.json"))).unwrap_or_default();
        let (lang, strings) = match find_file(&dir, &lang) {
            Some((code, _)) if code == FALLBACK => (code, HashMap::new()),
            Some((code, path)) => (code, read_strings(&path).unwrap_or_default()),
            None => (lang, HashMap::new()),
        };
        Self {
            lang,
            strings,
            fallback,
        }
    }

    /// A locale from strings in memory (tests, or no Wallpaper Engine install).
    pub fn from_strings(lang: &str, strings: HashMap<String, String>) -> Self {
        Self {
            lang: normalize_lang(lang),
            strings,
            fallback: HashMap::new(),
        }
    }

    /// The language labels are looked up in (`en-us`, `zh-chs`, ...).
    pub fn lang(&self) -> &str {
        &self.lang
    }

    /// Wallpaper Engine's string for `key`, if it has one.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.strings
            .get(key)
            .or_else(|| self.fallback.get(key))
            .map(String::as_str)
    }

    /// The label for `text` (a property's `text`, a combo option's
    /// `label`): the wallpaper's translation (`project_localization` is
    /// its `general.localization`), else Wallpaper Engine's, else `text`.
    /// May contain HTML; see [`plain`].
    pub fn label(&self, text: &str, project_localization: Option<&Value>) -> String {
        if let Some(loc) = project_localization.and_then(Value::as_object) {
            let in_lang = |lang: &str| {
                loc.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(lang))
                    .and_then(|(_, strings)| strings.get(text)?.as_str())
            };
            if let Some(s) = in_lang(&self.lang).or_else(|| in_lang(FALLBACK)) {
                return s.to_owned();
            }
        }
        self.get(text).unwrap_or(text).to_owned()
    }
}

/// `html` as plain text: tags dropped, `<br>` and block ends as newlines,
/// basic entities decoded, surrounding whitespace trimmed.
pub fn plain(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find(['<', '&']) {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        if rest.starts_with('<') {
            let Some(end) = rest.find('>') else {
                out.push_str(rest);
                rest = "";
                break;
            };
            let tag = rest[1..end].trim().to_ascii_lowercase();
            let name = tag
                .trim_start_matches('/')
                .split(|c: char| c.is_whitespace() || c == '/')
                .next()
                .unwrap_or("");
            let closing = tag.starts_with('/');
            if name == "br" || (closing && matches!(name, "p" | "div" | "li" | "h1" | "h2" | "h3"))
            {
                out.push('\n');
            }
            rest = &rest[end + 1..];
        } else {
            let end = rest.find(';').filter(|&end| end <= 12);
            let decoded = end.and_then(|end| {
                let entity = &rest[1..end];
                let c = match entity {
                    "amp" => '&',
                    "lt" => '<',
                    "gt" => '>',
                    "quot" => '"',
                    "apos" => '\'',
                    "nbsp" => '\u{a0}',
                    _ => {
                        let num = entity.strip_prefix('#')?;
                        let code = match num.strip_prefix(['x', 'X']) {
                            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                            None => num.parse().ok()?,
                        };
                        char::from_u32(code)?
                    }
                };
                Some((c, end))
            });
            match decoded {
                Some((c, end)) => {
                    out.push(c);
                    rest = &rest[end + 1..];
                }
                None => {
                    out.push('&');
                    rest = &rest[1..];
                }
            }
        }
    }
    out.push_str(rest);
    out.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_languages() {
        assert_eq!(normalize_lang("en_US.UTF-8"), "en-us");
        assert_eq!(normalize_lang("default"), "en-us");
        assert_eq!(normalize_lang("zh_CN.utf8"), "zh-chs");
        assert_eq!(normalize_lang("zh_TW"), "zh-cht");
        assert_eq!(normalize_lang("de_DE@euro"), "de-de");
    }

    #[test]
    fn labels_prefer_project_then_we_then_text() {
        let dir = tempfile::tempdir().unwrap();
        let locale_dir = dir.path().join("locale");
        std::fs::create_dir(&locale_dir).unwrap();
        std::fs::write(
            locale_dir.join("ui_en-us.json"),
            "{\n\"ui_browse_properties_scheme_color\" : \"Scheme color\",\n\"ui_only_en\": \"English\",\n}",
        )
        .unwrap();
        std::fs::write(
            locale_dir.join("ui_de-de.json"),
            r#"{"ui_browse_properties_scheme_color": "Farbschema"}"#,
        )
        .unwrap();

        let de = Locale::load(dir.path(), "de");
        assert_eq!(de.lang(), "de-de");
        assert_eq!(
            de.label("ui_browse_properties_scheme_color", None),
            "Farbschema"
        );
        assert_eq!(de.label("ui_only_en", None), "English");
        assert_eq!(de.label("Speed", None), "Speed");

        let project = json!({
            "en-us": {"ui_cf1": "Variation 1", "ui_browse_properties_scheme_color": "Mine"},
            "de-de": {"ui_cf1": "Variante 1"},
        });
        assert_eq!(de.label("ui_cf1", Some(&project)), "Variante 1");
        // English project strings beat Wallpaper Engine's German ones.
        assert_eq!(
            de.label("ui_browse_properties_scheme_color", Some(&project)),
            "Mine"
        );

        let en = Locale::load(dir.path(), "en_US.UTF-8");
        assert_eq!(en.label("ui_cf1", Some(&project)), "Variation 1");
        assert_eq!(
            en.label("ui_browse_properties_scheme_color", None),
            "Scheme color"
        );

        let none = Locale::load(&dir.path().join("missing"), "fr");
        assert_eq!(none.label("ui_only_en", None), "ui_only_en");
    }

    #[test]
    fn plain_strips_html() {
        assert_eq!(plain("<b>Speed</b>"), "Speed");
        assert_eq!(
            plain("Line 1<br>Line 2<br/>Line 3<BR />"),
            "Line 1\nLine 2\nLine 3"
        );
        assert_eq!(plain("<p>One</p><p>Two</p>"), "One\nTwo");
        assert_eq!(
            plain("a &amp; b &lt;c&gt; &quot;d&quot; &#39;e&#x27;"),
            "a & b <c> \"d\" 'e'"
        );
        assert_eq!(plain("AT&T &bogus; x"), "AT&T &bogus; x");
        assert_eq!(plain("<a href='x'>link</a> 1 < 2"), "link 1 < 2");
        assert_eq!(plain("  <span style=\"color:red\">red</span>  "), "red");
    }
}
