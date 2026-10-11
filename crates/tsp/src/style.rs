//! Program palettes (`t`) and stylesheets (`s`).

use std::fmt::Debug;

use serde_json::{Map, Value};

/// The most CSS one surface may hold, summed over its sheets.
pub const MAX_CSS: usize = 256 * 1024;

/// A program palette: token colors per appearance, normalized to `#rrggbb`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Palette {
    /// Tokens for the dark appearance, in the order sent.
    pub dark: Vec<(String, String)>,
    /// Tokens for the light appearance.
    pub light: Vec<(String, String)>,
    /// The palette's names, `{dark, light}`, as sent.
    pub name: Option<Value>,
}

impl Palette {
    /// Reads a `t` body. Values that aren't `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa` are skipped;
    /// alpha is dropped.
    pub fn parse(body: &Map<String, Value>) -> Self {
        let variant = |key: &str| -> Vec<(String, String)> {
            body.get(key)
                .and_then(Value::as_object)
                .map(|tokens| {
                    tokens
                        .iter()
                        .filter_map(|(k, v)| Some((k.clone(), normalize_color(v.as_str()?)?)))
                        .collect()
                })
                .unwrap_or_default()
        };
        Palette {
            dark: variant("dark"),
            light: variant("light"),
            name: body.get("name").filter(|n| !n.is_null()).cloned(),
        }
    }

    /// The `t` body that sets this palette on `sf`.
    pub fn to_json(&self, sf: &str) -> Value {
        let variant = |tokens: &[(String, String)]| {
            Value::Object(
                tokens
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                    .collect(),
            )
        };
        let mut obj = Map::new();
        obj.insert("sf".into(), Value::String(sf.to_owned()));
        obj.insert("dark".into(), variant(&self.dark));
        obj.insert("light".into(), variant(&self.light));
        if let Some(name) = &self.name {
            obj.insert("name".into(), name.clone());
        }
        Value::Object(obj)
    }

    /// The retention estimate, in bytes.
    pub fn cost(&self) -> u64 {
        let tokens = |t: &[(String, String)]| {
            t.iter()
                .map(|(k, v)| (k.len() + v.len()) as u64)
                .sum::<u64>()
        };
        tokens(&self.dark)
            + tokens(&self.light)
            + self.name.as_ref().map_or(0, |n| n.to_string().len() as u64)
    }
}

/// `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa` as lowercase `#rrggbb`.
pub fn normalize_color(s: &str) -> Option<String> {
    let hex = s.strip_prefix('#')?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let hex = hex.to_ascii_lowercase();
    match hex.len() {
        3 | 4 => Some(
            hex[..3]
                .chars()
                .flat_map(|c| [c, c])
                .fold(String::from("#"), |mut s, c| {
                    s.push(c);
                    s
                }),
        ),
        6 | 8 => Some(format!("#{}", &hex[..6])),
        _ => None,
    }
}

/// Whether `name` is a valid stylesheet name: 1–64 of `[A-Za-z0-9_-]`.
pub fn valid_sheet_name(name: &str) -> bool {
    (1..=64).contains(&name.len()) && name.bytes().all(crate::wire::key_byte)
}

/// Checks program CSS. The UI engine supplies the real one; it reports dropped and rejected
/// constructs (`url()`, `position: fixed`, `@import`, parse errors) as `<line>:<col>: …` messages.
/// A sheet is installed even when it has messages; they are reported once, joined with `; `.
pub trait SheetChecker: Debug + Send {
    fn check(&self, css: &str) -> Vec<String>;
}

/// Accepts every sheet without messages.
#[derive(Clone, Copy, Debug, Default)]
pub struct AcceptAll;

impl SheetChecker for AcceptAll {
    fn check(&self, _css: &str) -> Vec<String> {
        Vec::new()
    }
}

/// A surface's stylesheets, in the order first sent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sheets {
    sheets: Vec<(String, String)>,
}

impl Sheets {
    /// `(name, css)` in cascade order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.sheets.iter().map(|(n, c)| (n.as_str(), c.as_str()))
    }

    pub fn is_empty(&self) -> bool {
        self.sheets.is_empty()
    }

    /// Total CSS bytes.
    pub fn bytes(&self) -> usize {
        self.sheets.iter().map(|(_, c)| c.len()).sum()
    }

    /// Bytes of all sheets with `css` in place of the one named `name`.
    pub fn bytes_with(&self, name: &str, css: &str) -> usize {
        self.sheets
            .iter()
            .filter(|(n, _)| n != name)
            .map(|(_, c)| c.len())
            .sum::<usize>()
            + css.len()
    }

    /// Installs or replaces a sheet; a replacement keeps its place.
    pub fn set(&mut self, name: &str, css: String) {
        match self.sheets.iter_mut().find(|(n, _)| n == name) {
            Some((_, old)) => *old = css,
            None => self.sheets.push((name.to_owned(), css)),
        }
    }

    /// Removes a sheet; returns whether it was there.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.sheets.len();
        self.sheets.retain(|(n, _)| n != name);
        self.sheets.len() != before
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn colors_normalize() {
        assert_eq!(normalize_color("#ABC").as_deref(), Some("#aabbcc"));
        assert_eq!(normalize_color("#abcd").as_deref(), Some("#aabbcc"));
        assert_eq!(normalize_color("#00b4ff80").as_deref(), Some("#00b4ff"));
        assert_eq!(normalize_color("red"), None);
        assert_eq!(normalize_color("#12345"), None);
        assert_eq!(normalize_color("#ggg"), None);
    }

    #[test]
    fn palette_skips_bad_values() {
        let body = json!({"dark": {"accent": "#00B4FF", "bad": "blue", "n": 3}, "name": {"dark": "titanium"}});
        let p = Palette::parse(body.as_object().unwrap());
        assert_eq!(p.dark, vec![("accent".to_owned(), "#00b4ff".to_owned())]);
        assert!(p.light.is_empty());
        assert_eq!(
            p.to_json("s1"),
            json!({"sf": "s1", "dark": {"accent": "#00b4ff"}, "light": {}, "name": {"dark": "titanium"}})
        );
    }

    #[test]
    fn sheet_names() {
        assert!(valid_sheet_name("main"));
        assert!(valid_sheet_name(&"a".repeat(64)));
        assert!(!valid_sheet_name(&"a".repeat(65)));
        assert!(!valid_sheet_name(""));
        assert!(!valid_sheet_name("a.b"));
    }

    #[test]
    fn replacement_keeps_place() {
        let mut s = Sheets::default();
        s.set("a", "x{}".into());
        s.set("b", "y{}".into());
        s.set("a", "z{}".into());
        assert_eq!(s.iter().collect::<Vec<_>>(), [("a", "z{}"), ("b", "y{}")]);
        assert_eq!(s.bytes_with("b", "0123"), 7);
        assert!(s.remove("a"));
        assert!(!s.remove("a"));
    }
}
