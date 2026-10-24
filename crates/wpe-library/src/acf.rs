//! Steam's `.acf` files (Valve's KeyValues text format, "VDF"), and what
//! `appworkshop_431960.acf` says about each workshop item.

use std::collections::HashMap;
use std::path::Path;

/// A KeyValues value: a string or a block of keyed values (in file order;
/// keys may repeat).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Vdf {
    Str(String),
    Block(Vec<(String, Vdf)>),
}

impl Vdf {
    /// The first value under `key` (case-insensitive, as Steam reads them).
    pub fn get(&self, key: &str) -> Option<&Vdf> {
        match self {
            Self::Block(entries) => entries
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v),
            Self::Str(_) => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            Self::Block(_) => None,
        }
    }

    /// A block's entries; nothing for a string.
    pub fn entries(&self) -> &[(String, Vdf)] {
        match self {
            Self::Block(entries) => entries,
            Self::Str(_) => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Str(String),
    Open,
    Close,
}

fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => tokens.push(Token::Open),
            '}' => tokens.push(Token::Close),
            '"' => {
                let mut s = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(other) => s.push(other),
                            None => break,
                        },
                        _ => s.push(c),
                    }
                }
                tokens.push(Token::Str(s));
            }
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            // Platform conditionals (`[$WIN32]`) are ignored.
            '[' => {
                for c in chars.by_ref() {
                    if c == ']' {
                        break;
                    }
                }
            }
            c if c.is_whitespace() => {}
            c => {
                let mut s = String::from(c);
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || matches!(c, '{' | '}' | '"') {
                        break;
                    }
                    s.push(c);
                    chars.next();
                }
                tokens.push(Token::Str(s));
            }
        }
    }
    tokens
}

fn parse_block(tokens: &mut std::vec::IntoIter<Token>) -> Vec<(String, Vdf)> {
    let mut entries = Vec::new();
    while let Some(token) = tokens.next() {
        let key = match token {
            Token::Str(key) => key,
            Token::Close => break,
            Token::Open => continue,
        };
        match tokens.next() {
            Some(Token::Str(value)) => entries.push((key, Vdf::Str(value))),
            Some(Token::Open) => entries.push((key, Vdf::Block(parse_block(tokens)))),
            Some(Token::Close) | None => break,
        }
    }
    entries
}

/// Parse a KeyValues document: its top-level entries as one block. Lenient:
/// a truncated file gives what was read.
pub fn parse(text: &str) -> Vdf {
    let mut tokens = tokenize(text).into_iter();
    Vdf::Block(parse_block(&mut tokens))
}

/// What Steam records about one installed workshop item.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkshopItem {
    /// Bytes on disk.
    pub size: Option<u64>,
    /// Unix seconds of the item's last update.
    pub updated: Option<u64>,
    /// Unix seconds Steam last touched it (subscribed or downloaded).
    pub touched: Option<u64>,
}

/// The items of an `appworkshop_<app>.acf`, by workshop id.
pub fn workshop_items(text: &str) -> HashMap<String, WorkshopItem> {
    let doc = parse(text);
    let Some(root) = doc.get("AppWorkshop") else {
        return HashMap::new();
    };
    let num = |block: &Vdf, key: &str| block.get(key)?.as_str()?.trim().parse::<u64>().ok();
    let mut items: HashMap<String, WorkshopItem> = HashMap::new();
    if let Some(installed) = root.get("WorkshopItemsInstalled") {
        for (id, block) in installed.entries() {
            let item = items.entry(id.clone()).or_default();
            item.size = num(block, "size");
            item.updated = num(block, "timeupdated");
        }
    }
    if let Some(details) = root.get("WorkshopItemDetails") {
        for (id, block) in details.entries() {
            let item = items.entry(id.clone()).or_default();
            item.touched = num(block, "timetouched");
            item.updated = item.updated.or_else(|| num(block, "timeupdated"));
        }
    }
    items
}

/// [`workshop_items`] of the file at `path`; nothing if it can't be read.
pub fn load(path: &Path) -> HashMap<String, WorkshopItem> {
    match std::fs::read_to_string(path) {
        Ok(text) => workshop_items(&text),
        Err(e) => {
            tracing::debug!("{}: {e}", path.display());
            HashMap::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACF: &str = r#""AppWorkshop"
{
	"appid"		"431960"
	"SizeOnDisk"		"123"
	"WorkshopItemsInstalled"
	{
		"111"
		{
			"size"		"1000"
			"timeupdated"		"1500000000"
			"manifest"		"42"
		}
		"222"
		{
			"size"		"2000"
			"timeupdated"		"1550000000"
		}
	}
	// a comment
	"WorkshopItemDetails"
	{
		"111"
		{
			"manifest"		"42"
			"timeupdated"		"1500000000"
			"timetouched"		"1700000000"
		}
		"333"
		{
			"timeupdated"		"5"
			"timetouched"		"6"
		}
	}
}
"#;

    #[test]
    fn parses_keyvalues() {
        let doc = parse(r#""a" { "b" "x \"y\"" c [$WIN32] { "d" "1" } } "#);
        let a = doc.get("A").unwrap();
        assert_eq!(a.get("b").unwrap().as_str(), Some("x \"y\""));
        assert_eq!(a.get("c").unwrap().get("d").unwrap().as_str(), Some("1"));
        assert!(a.get("missing").is_none());
        // Truncated input keeps what was read.
        let doc = parse(r#""a" { "b" "1" "c" {"#);
        assert_eq!(doc.get("a").unwrap().get("b").unwrap().as_str(), Some("1"));
    }

    #[test]
    fn workshop_items_merge_sections() {
        let items = workshop_items(ACF);
        assert_eq!(items.len(), 3);
        assert_eq!(
            items["111"],
            WorkshopItem {
                size: Some(1000),
                updated: Some(1500000000),
                touched: Some(1700000000),
            }
        );
        assert_eq!(items["222"].touched, None);
        assert_eq!(items["333"].size, None);
        assert_eq!(items["333"].updated, Some(5));
        assert!(workshop_items("garbage").is_empty());
    }
}
