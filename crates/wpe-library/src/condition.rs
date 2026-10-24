//! Property visibility conditions: the JavaScript-like expressions in
//! `project.json` (`"condition": "mode.value == 2 && !simple.value"`) that
//! decide whether the settings panel shows a property.
//!
//! The subset used by wallpapers: `name.value` lookups, number, string and
//! `true`/`false`/`null`/`undefined` literals, `!`, unary `-`, `==`, `!=`,
//! `===`, `!==`, `<`, `<=`, `>`, `>=`, `&&`, `||`, parentheses and a few
//! string members (`length`, `startsWith`, `endsWith`, `includes`,
//! `indexOf`, `toLowerCase`, `toUpperCase`, `trim`), with JavaScript's
//! loose equality and truthiness. Anything else (a bare name, another
//! call, a syntax error) shows the property.

use std::collections::HashMap;

use serde_json::Value;

/// Whether a property with condition `expr` is shown, given the current
/// property values by name. Unknown syntax shows it.
pub fn visible(expr: &str, values: &HashMap<String, Value>) -> bool {
    expr.trim().is_empty() || evaluate(expr, values).unwrap_or(true)
}

/// The truthiness of `expr`; `None` if it uses syntax this doesn't know.
pub fn evaluate(expr: &str, values: &HashMap<String, Value>) -> Option<bool> {
    let tokens = tokenize(expr)?;
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
        values,
    };
    let value = parser.or()?;
    (parser.pos == tokens.len()).then(|| value.truthy())
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Num(f64),
    Str(String),
    Ident(String),
    Op(&'static str),
}

const OPS: [&str; 17] = [
    "===", "!==", "==", "!=", "<=", ">=", "&&", "||", "<", ">", "!", "(", ")", ".", ",", "-", "+",
];

fn tokenize(s: &str) -> Option<Vec<Token>> {
    let mut tokens = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    'outer: while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || (c == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            tokens.push(Token::Num(s[start..i].parse().ok()?));
            continue;
        }
        if c == b'"' || c == b'\'' {
            let mut out = String::new();
            let mut chars = s[i + 1..].char_indices();
            while let Some((j, ch)) = chars.next() {
                match ch {
                    ch if ch as u32 == u32::from(c) => {
                        tokens.push(Token::Str(out));
                        i += 1 + j + 1;
                        continue 'outer;
                    }
                    '\\' => out.push(chars.next()?.1),
                    ch => out.push(ch),
                }
            }
            return None;
        }
        if c.is_ascii_alphabetic() || c == b'_' || c == b'$' || c >= 0x80 {
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric()
                    || bytes[i] == b'_'
                    || bytes[i] == b'$'
                    || bytes[i] >= 0x80)
            {
                i += 1;
            }
            tokens.push(Token::Ident(s[start..i].to_owned()));
            continue;
        }
        let op = OPS.iter().find(|op| s[i..].starts_with(*op))?;
        tokens.push(Token::Op(op));
        i += op.len();
    }
    Some(tokens)
}

/// A JavaScript value, as far as conditions need.
#[derive(Debug, Clone, PartialEq)]
enum Js {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    /// Arrays and objects: truthy, equal to nothing.
    Object,
}

impl Js {
    fn from_json(v: &Value) -> Self {
        match v {
            Value::Null => Self::Null,
            Value::Bool(b) => Self::Bool(*b),
            Value::Number(n) => Self::Num(n.as_f64().unwrap_or(f64::NAN)),
            Value::String(s) => Self::Str(s.clone()),
            Value::Array(_) | Value::Object(_) => Self::Object,
        }
    }

    fn truthy(&self) -> bool {
        match self {
            Self::Undefined | Self::Null => false,
            Self::Bool(b) => *b,
            Self::Num(n) => *n != 0.0 && !n.is_nan(),
            Self::Str(s) => !s.is_empty(),
            Self::Object => true,
        }
    }

    fn to_number(&self) -> f64 {
        match self {
            Self::Undefined | Self::Object => f64::NAN,
            Self::Null => 0.0,
            Self::Bool(b) => f64::from(u8::from(*b)),
            Self::Num(n) => *n,
            Self::Str(s) => {
                let t = s.trim();
                if t.is_empty() {
                    0.0
                } else {
                    t.parse().unwrap_or(f64::NAN)
                }
            }
        }
    }

    fn strict_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Object, _) | (_, Self::Object) => false,
            (Self::Num(a), Self::Num(b)) => a == b,
            (a, b) => a == b,
        }
    }

    fn loose_eq(&self, other: &Self) -> bool {
        use Js::*;
        match (self, other) {
            (Undefined | Null, Undefined | Null) => true,
            (Undefined | Null, _) | (_, Undefined | Null) => false,
            (Object, _) | (_, Object) => false,
            (Bool(_), _) | (_, Bool(_)) | (Num(_), Str(_)) | (Str(_), Num(_)) => {
                if std::mem::discriminant(self) == std::mem::discriminant(other) {
                    self == other
                } else {
                    self.to_number() == other.to_number()
                }
            }
            (a, b) => a.strict_eq(b),
        }
    }

    fn compare(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Self::Str(a), Self::Str(b)) => Some(a.cmp(b)),
            (a, b) => a.to_number().partial_cmp(&b.to_number()),
        }
    }
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    values: &'a HashMap<String, Value>,
}

impl Parser<'_> {
    fn peek_op(&self) -> Option<&'static str> {
        match self.tokens.get(self.pos) {
            Some(Token::Op(op)) => Some(op),
            _ => None,
        }
    }

    fn eat(&mut self, op: &str) -> bool {
        if self.peek_op() == Some(op) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn or(&mut self) -> Option<Js> {
        let mut left = self.and()?;
        while self.eat("||") {
            let right = self.and()?;
            left = if left.truthy() { left } else { right };
        }
        Some(left)
    }

    fn and(&mut self) -> Option<Js> {
        let mut left = self.equality()?;
        while self.eat("&&") {
            let right = self.equality()?;
            left = if left.truthy() { right } else { left };
        }
        Some(left)
    }

    fn equality(&mut self) -> Option<Js> {
        let mut left = self.relational()?;
        while let Some(op @ ("==" | "!=" | "===" | "!==")) = self.peek_op() {
            self.pos += 1;
            let right = self.relational()?;
            let eq = match op {
                "==" | "!=" => left.loose_eq(&right),
                _ => left.strict_eq(&right),
            };
            left = Js::Bool(eq == op.starts_with('='));
        }
        Some(left)
    }

    fn relational(&mut self) -> Option<Js> {
        let mut left = self.unary()?;
        while let Some(op @ ("<" | "<=" | ">" | ">=")) = self.peek_op() {
            self.pos += 1;
            let right = self.unary()?;
            use std::cmp::Ordering::*;
            let ord = left.compare(&right);
            left = Js::Bool(match op {
                "<" => ord == Some(Less),
                "<=" => matches!(ord, Some(Less | Equal)),
                ">" => ord == Some(Greater),
                _ => matches!(ord, Some(Greater | Equal)),
            });
        }
        Some(left)
    }

    fn unary(&mut self) -> Option<Js> {
        if self.eat("!") {
            return Some(Js::Bool(!self.unary()?.truthy()));
        }
        if self.eat("-") {
            return Some(Js::Num(-self.unary()?.to_number()));
        }
        if self.eat("+") {
            return Some(Js::Num(self.unary()?.to_number()));
        }
        self.primary()
    }

    fn primary(&mut self) -> Option<Js> {
        let mut value = self.atom()?;
        // String members: `.length`, `.startsWith('x')` and the like.
        while self.eat(".") {
            let Some(Token::Ident(member)) = self.tokens.get(self.pos).cloned() else {
                return None;
            };
            self.pos += 1;
            let Js::Str(s) = &value else {
                return None;
            };
            if member == "length" {
                value = Js::Num(s.chars().count() as f64);
                continue;
            }
            if !self.eat("(") {
                return None;
            }
            let mut args = Vec::new();
            if !self.eat(")") {
                loop {
                    args.push(self.or()?);
                    if self.eat(")") {
                        break;
                    }
                    if !self.eat(",") {
                        return None;
                    }
                }
            }
            let arg = |i: usize| match args.get(i) {
                Some(Js::Str(a)) => Some(a.as_str()),
                _ => None,
            };
            value = match (member.as_str(), args.len()) {
                ("startsWith", 1) => Js::Bool(s.starts_with(arg(0)?)),
                ("endsWith", 1) => Js::Bool(s.ends_with(arg(0)?)),
                ("includes", 1) => Js::Bool(s.contains(arg(0)?)),
                ("indexOf", 1) => Js::Num(
                    s.find(arg(0)?)
                        .map_or(-1.0, |i| s[..i].encode_utf16().count() as f64),
                ),
                ("toLowerCase", 0) => Js::Str(s.to_lowercase()),
                ("toUpperCase", 0) => Js::Str(s.to_uppercase()),
                ("trim", 0) => Js::Str(s.trim().to_owned()),
                _ => return None,
            };
        }
        Some(value)
    }

    fn atom(&mut self) -> Option<Js> {
        let token = self.tokens.get(self.pos)?.clone();
        self.pos += 1;
        match token {
            Token::Num(n) => Some(Js::Num(n)),
            Token::Str(s) => Some(Js::Str(s)),
            Token::Op("(") => {
                let v = self.or()?;
                self.eat(")").then_some(v)
            }
            Token::Op(_) => None,
            Token::Ident(name) => match name.as_str() {
                "true" => Some(Js::Bool(true)),
                "false" => Some(Js::Bool(false)),
                "null" => Some(Js::Null),
                "undefined" => Some(Js::Undefined),
                _ => {
                    // `name.value`; a bare name is not something we know.
                    if !self.eat(".") {
                        return None;
                    }
                    match self.tokens.get(self.pos) {
                        Some(Token::Ident(field)) if field == "value" => self.pos += 1,
                        _ => return None,
                    }
                    Some(self.values.get(&name).map_or(Js::Undefined, Js::from_json))
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn values() -> HashMap<String, Value> {
        [
            ("mode", json!("dual")),
            ("clock", json!(true)),
            ("off", json!(false)),
            ("count", json!(2)),
            ("half", json!(0.5)),
            ("combo", json!("1")),
            ("color", json!("1 0 0")),
            ("list", json!([1])),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect()
    }

    fn check(expr: &str) -> bool {
        visible(expr, &values())
    }

    #[test]
    fn equality_and_truthiness() {
        assert!(check(r#"mode.value == "dual""#));
        assert!(!check("mode.value == 'stretched'"));
        assert!(check(r#"mode.value != "stretched""#));
        assert!(check("clock.value"));
        assert!(!check("off.value"));
        assert!(check("clock.value == true"));
        assert!(check("clock.value == 1"));
        assert!(check("clock.value==1"));
        assert!(!check("clock.value === 1"));
        assert!(check("clock.value === true"));
        assert!(check("combo.value == 1"));
        assert!(!check("combo.value === 1"));
        assert!(check("combo.value === '1'"));
        assert!(check("count.value == 2"));
        assert!(check("count.value != 3"));
        assert!(check("color.value"));
        assert!(check("list.value"));
        assert!(!check("list.value == 1"));
    }

    #[test]
    fn missing_properties_are_undefined() {
        assert!(!check("missing.value"));
        assert!(check("!missing.value"));
        assert!(!check("missing.value == 0"));
        assert!(check("missing.value == null"));
        assert!(!check("missing.value === null"));
    }

    #[test]
    fn logic_comparison_and_grouping() {
        assert!(check("half.value > 0.4"));
        assert!(!check("half.value > .5"));
        assert!(check("half.value >= .5 && count.value <= 2"));
        assert!(check("off.value || clock.value"));
        assert!(!check("off.value && clock.value"));
        assert!(check("clock.value && !off.value"));
        assert!(check("!(off.value || count.value < 1)"));
        assert!(!check("(off.value || count.value > 5) && clock.value"));
        // `&&` binds tighter than `||`.
        assert!(check("clock.value || off.value && off.value"));
        // `!` binds tighter than `==`: (!count) == 0.
        assert!(check("!count.value == 0"));
        assert!(check("count.value > -1"));
        assert!(check("'b' > 'a'"));
        assert!(check(r#"mode.value == "du\al""#));
    }

    #[test]
    fn string_methods() {
        assert!(check("mode.value.endsWith('al') === true"));
        assert!(!check("mode.value.startsWith('al')"));
        assert!(check(
            "mode.value.startsWith('rainbow') === false && count.value == 2"
        ));
        assert!(check("mode.value.includes(\"ua\")"));
        assert!(check("mode.value.indexOf('a') == 2"));
        assert!(check("mode.value.length == 4"));
        assert!(check("mode.value.toUpperCase() == 'DUAL'"));
        assert!(check("(mode.value).trim().length === 4"));
    }

    #[test]
    fn unknown_syntax_shows() {
        assert!(check(""));
        assert!(check("value == ture"));
        assert!(check("off.value == ture"));
        assert!(check("foo()"));
        assert!(check("off.value &&"));
        assert!(check("(off.value"));
        assert!(check("off.length"));
        assert!(check("off.value.endsWith('x')"));
        assert!(check("missing.value.length > 5"));
        assert!(check("mode.value.repeat(2)"));
        assert!(check("'unterminated"));
        assert!(check("off.value ? 1 : 0"));
    }

    /// Every condition in the real library (`cargo test -- --ignored
    /// --nocapture real_conditions`): prints the ones not understood.
    #[test]
    #[ignore]
    fn real_conditions() {
        let lib = crate::Library::scan(&crate::Sources::discover(&[]));
        let (mut total, mut unknown) = (0, Vec::new());
        for item in &lib.items {
            let props = crate::properties(&item.path);
            let values: HashMap<String, Value> = props
                .iter()
                .map(|p| (p.name.clone(), p.value.clone()))
                .collect();
            for c in props.iter().filter_map(|p| p.condition.as_deref()) {
                total += 1;
                if evaluate(c, &values).is_none() {
                    unknown.push(c.to_owned());
                }
            }
        }
        println!("{total} conditions, not understood: {unknown:?}");
    }
}
