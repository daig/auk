//! Icon names (§7.12) and keycap glyphs.

pub use auk_text::is_private_use;

use crate::icon_table::{ALIASES, ICONS};

/// The glyph for an icon name or alias. Anything up to the last `.` is dropped (`status.error`).
/// Unknown names give `None` (an empty box).
pub fn glyph(name: &str) -> Option<char> {
    let name = name.rsplit('.').next().unwrap_or(name);
    let find = |n: &str| {
        ICONS
            .binary_search_by(|(k, _, _)| (*k).cmp(n))
            .ok()
            .map(|i| ICONS[i].1)
    };
    find(name).or_else(|| {
        ALIASES
            .binary_search_by(|(k, _)| (*k).cmp(name))
            .ok()
            .and_then(|i| find(ALIASES[i].1))
    })
}

/// Every icon name, for tests and pickers.
pub fn names() -> impl Iterator<Item = &'static str> {
    ICONS.iter().map(|(n, _, _)| *n)
}

/// A key name as a keycap glyph, and its spoken name when it has one (the `kbd` table).
pub fn keycap(key: &str) -> (String, Option<&'static str>) {
    let lower = key.to_ascii_lowercase();
    let named = match lower.as_str() {
        "ctrl" | "control" => Some(("⌃", Some("Control"))),
        "alt" | "option" | "opt" => Some(("⌥", Some("Option"))),
        "shift" => Some(("⇧", Some("Shift"))),
        "cmd" | "command" | "meta" | "super" | "win" => Some(("⌘", Some("Command"))),
        "enter" | "return" => Some(("⏎", Some("Return"))),
        "esc" | "escape" => Some(("esc", Some("Escape"))),
        "tab" => Some(("⇥", None)),
        "backspace" => Some(("⌫", None)),
        "delete" | "del" => Some(("⌦", None)),
        "space" => Some(("␣", None)),
        "up" => Some(("↑", None)),
        "down" => Some(("↓", None)),
        "left" => Some(("←", None)),
        "right" => Some(("→", None)),
        "pageup" => Some(("⇞", None)),
        "pagedown" => Some(("⇟", None)),
        "home" => Some(("↖", None)),
        "end" => Some(("↘", None)),
        _ => None,
    };
    if let Some((g, spoken)) = named {
        return (g.to_owned(), spoken);
    }
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return (c.to_uppercase().collect(), None);
    }
    if lower.len() > 1 && lower.starts_with('f') && lower[1..].bytes().all(|b| b.is_ascii_digit()) {
        return (lower.to_ascii_uppercase(), None);
    }
    (key.to_owned(), None)
}

/// A `+`-joined key id (`ctrl+r`) as keycaps (`⌃R`); `None` when `text` isn't a key id.
pub fn key_id(text: &str) -> Option<String> {
    if text.is_empty() || text.contains(char::is_whitespace) {
        return None;
    }
    let parts: Vec<&str> = if text == "+" {
        vec!["+"]
    } else {
        text.split('+').collect()
    };
    if parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    Some(parts.iter().map(|p| keycap(p).0).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_aliases() {
        assert_eq!(names().count(), ICONS.len());
        assert!(ICONS.windows(2).all(|w| w[0].0 < w[1].0), "sorted");
        assert!(ALIASES.windows(2).all(|w| w[0].0 < w[1].0), "sorted");
        assert!(glyph("folder").is_some());
        assert_eq!(glyph("icon.folder"), glyph("folder"));
        assert_eq!(glyph("success"), glyph("check"));
        assert_eq!(glyph("status.error"), glyph("x"));
        assert_eq!(glyph("nope"), None);
        assert!(is_private_use(glyph("branch").unwrap()));
    }

    #[test]
    fn keycaps() {
        assert_eq!(key_id("ctrl+r").as_deref(), Some("⌃R"));
        assert_eq!(key_id("shift+enter").as_deref(), Some("⇧⏎"));
        assert_eq!(key_id("f5").as_deref(), Some("F5"));
        assert_eq!(key_id("Esc to cancel"), None);
        assert_eq!(keycap("ctrl"), ("⌃".to_owned(), Some("Control")));
        assert_eq!(keycap("o").0, "O");
        assert_eq!(keycap("fn").0, "fn");
    }
}
