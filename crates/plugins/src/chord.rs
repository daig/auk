//! Chords (§9.8 "Binds"), the grammar `tern.bind`, command `keys` and the user's `keybinds`
//! share: `+`-joined modifiers and one key (`ctrl+shift+2`, `cmd+plus`, `cmd+digit_1`), `>`
//! between the steps of a sequence (`ctrl+a>c`), and a lone modifier as a tap in a sequence of
//! taps (`cmd>cmd`).
//!
//! The app builds its keymap from these; the plugin layer only adds keys, so a plugin chord that
//! [`Chord::overlaps`] a default or preset chord is left out.

/// A modifier set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct Mods {
    /// Control.
    pub ctrl: bool,
    /// Alt (Option).
    pub alt: bool,
    /// Shift.
    pub shift: bool,
    /// Cmd on macOS, Super elsewhere.
    pub cmd: bool,
}

/// The key of a step.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    /// A character as the layout types it (lowercased ASCII letters).
    Char(char),
    /// A named key (`enter`, `f5`, `page_up`, …).
    Named(String),
    /// A physical key (`digit_1`, `key_a`, `bracket_left`, …).
    Physical(String),
}

/// One step of a chord: modifiers and a key, or modifiers alone (a tap).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Step {
    /// The modifiers held (for a tap, the modifier tapped).
    pub mods: Mods,
    /// The key; `None` for a modifier tap.
    pub key: Option<Key>,
}

/// A parsed chord.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Chord {
    /// Its steps; the ones before the last are a prefix.
    pub steps: Vec<Step>,
}

const NAMED: &[&str] = &[
    "enter",
    "return",
    "tab",
    "backspace",
    "escape",
    "esc",
    "space",
    "up",
    "down",
    "left",
    "right",
    "home",
    "end",
    "page_up",
    "page_down",
    "pageup",
    "pagedown",
    "insert",
    "delete",
    "plus",
    "minus",
    "equal",
    "comma",
    "period",
    "slash",
    "backslash",
    "semicolon",
    "quote",
    "backquote",
    "grave",
    "bracket_left",
    "bracket_right",
    "menu",
    "caps_lock",
];

fn named_char(name: &str) -> Option<char> {
    Some(match name {
        "plus" => '+',
        "minus" => '-',
        "equal" => '=',
        "comma" => ',',
        "period" => '.',
        "slash" => '/',
        "backslash" => '\\',
        "semicolon" => ';',
        "quote" => '\'',
        "backquote" | "grave" => '`',
        _ => return None,
    })
}

fn parse_key(word: &str) -> Result<Key, String> {
    let lower = word.to_ascii_lowercase();
    let mut chars = word.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Ok(Key::Char(c.to_ascii_lowercase()));
    }
    if let Some(c) = named_char(&lower) {
        return Ok(Key::Char(c));
    }
    if let Some(n) = lower.strip_prefix('f')
        && let Ok(n) = n.parse::<u8>()
        && (1..=20).contains(&n)
    {
        return Ok(Key::Named(lower));
    }
    if NAMED.contains(&lower.as_str()) {
        let canonical = match lower.as_str() {
            "return" => "enter",
            "esc" => "escape",
            "pageup" => "page_up",
            "pagedown" => "page_down",
            other => other,
        };
        return Ok(Key::Named(canonical.to_owned()));
    }
    let physical = lower
        .strip_prefix("digit_")
        .is_some_and(|d| d.len() == 1 && d.as_bytes()[0].is_ascii_digit())
        || lower
            .strip_prefix("key_")
            .is_some_and(|k| k.len() == 1 && k.as_bytes()[0].is_ascii_lowercase())
        || matches!(
            lower.as_str(),
            "bracket_left" | "bracket_right" | "numpad_enter"
        )
        || lower
            .strip_prefix("numpad_")
            .is_some_and(|d| d.len() == 1 && d.as_bytes()[0].is_ascii_digit());
    if physical {
        return Ok(Key::Physical(lower));
    }
    Err(format!("unknown key `{word}`"))
}

fn set_mod(mods: &mut Mods, word: &str) -> bool {
    match word.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => mods.ctrl = true,
        "alt" | "opt" | "option" => mods.alt = true,
        "shift" => mods.shift = true,
        "cmd" | "super" | "command" | "meta" => mods.cmd = true,
        _ => return false,
    }
    true
}

fn parse_step(text: &str) -> Result<Step, String> {
    if text.is_empty() {
        return Err("an empty chord".into());
    }
    // `ctrl++` binds the `+` key.
    let (body, plus) = match text.strip_suffix("++") {
        Some(body) => (body, true),
        None if text == "+" => ("", true),
        None => (text, false),
    };
    let words: Vec<&str> = if body.is_empty() {
        Vec::new()
    } else {
        body.split('+').collect()
    };
    let mut mods = Mods::default();
    let mut key = plus.then_some(Key::Char('+'));
    let last = words.len().saturating_sub(1);
    for (i, word) in words.iter().enumerate() {
        if word.is_empty() {
            return Err(format!("an empty part in `{text}`"));
        }
        if set_mod(&mut mods, word) {
            continue;
        }
        if i == last && key.is_none() {
            key = Some(parse_key(word)?);
        } else {
            return Err(format!("unknown modifier `{word}`"));
        }
    }
    Ok(Step { mods, key })
}

impl Chord {
    /// Parses a chord.
    pub fn parse(text: &str) -> Result<Chord, String> {
        let text = text.trim();
        let parts: Vec<&str> = if text.contains('>') && !text.ends_with('>') {
            text.split('>').collect()
        } else {
            vec![text]
        };
        let steps = parts
            .into_iter()
            .map(parse_step)
            .collect::<Result<Vec<_>, _>>()?;
        let taps = steps.iter().filter(|s| s.key.is_none()).count();
        if taps > 0 && (taps != steps.len() || steps.len() < 2) {
            return Err("a modifier alone is a tap, only in a sequence of taps".into());
        }
        Ok(Chord { steps })
    }

    /// Whether binding both would clash: the same keys, or one starts the other.
    pub fn overlaps(&self, other: &Chord) -> bool {
        let n = self.steps.len().min(other.steps.len());
        self.steps[..n] == other.steps[..n]
    }
}

/// The plugin and command of an action `plugin.<plugin>.<rest>` (both parts non-empty).
pub fn plugin_action(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("plugin.")?;
    let (plugin, command) = rest.split_once('.')?;
    (!plugin.is_empty() && !command.is_empty()).then_some((plugin, command))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar() {
        let c = Chord::parse("ctrl+shift+2").expect("parses");
        assert_eq!(c.steps.len(), 1);
        assert!(c.steps[0].mods.ctrl && c.steps[0].mods.shift);
        assert_eq!(c.steps[0].key, Some(Key::Char('2')));
        assert_eq!(
            Chord::parse("cmd+plus").expect("ok").steps[0].key,
            Some(Key::Char('+'))
        );
        assert_eq!(
            Chord::parse("ctrl++").expect("ok").steps[0].key,
            Some(Key::Char('+'))
        );
        assert_eq!(
            Chord::parse("cmd+digit_1").expect("ok").steps[0].key,
            Some(Key::Physical("digit_1".into()))
        );
        let seq = Chord::parse("ctrl+a>c").expect("ok");
        assert_eq!(seq.steps.len(), 2);
        assert!(
            Chord::parse("cmd>cmd")
                .expect("ok")
                .steps
                .iter()
                .all(|s| s.key.is_none())
        );
        assert!(Chord::parse("alt+shift+cmd+r").is_ok());
        assert!(Chord::parse("f12").is_ok());
        assert_eq!(
            Chord::parse("hyper+x"),
            Err("unknown modifier `hyper`".into())
        );
        assert!(Chord::parse("cmd").is_err());
        assert!(Chord::parse("ctrl+a>shift").is_err());
    }

    #[test]
    fn overlaps() {
        let prefix = Chord::parse("ctrl+b").expect("ok");
        assert!(prefix.overlaps(&Chord::parse("ctrl+b>x").expect("ok")));
        assert!(!prefix.overlaps(&Chord::parse("ctrl+a>x").expect("ok")));
        assert!(prefix.overlaps(&Chord::parse("ctrl+B").expect("ok")));
    }

    #[test]
    fn actions() {
        assert_eq!(
            plugin_action("plugin.longrun.show"),
            Some(("longrun", "show"))
        );
        assert_eq!(plugin_action("plugin.x.bind.0"), Some(("x", "bind.0")));
        assert_eq!(plugin_action("plugin..x"), None);
        assert_eq!(plugin_action("new_tab"), None);
    }
}
