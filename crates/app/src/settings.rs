//! The window's settings (§7.8, §7.9, §7.12, §7.13, §9.8): read from `<config>/settings.json`
//! (the file plugins' `plugins` and `plugins_disabled` live in, research/plugins.md §2.4) when a
//! window opens and on "Reload Settings".
//!
//! | Key | Default | Effect |
//! | --- | --- | --- |
//! | `program_colors` | `true` | "Use program colors": apply program palettes (§7.12) |
//! | `native_editing` | `true` | native editing in program fields (§7.9) |
//! | `links_plain_click` | `false` | a plain click opens links (otherwise ⌘-click, §7.8) |
//! | `reduce_motion` | `"system"` | `true`, `false`, or follow the system setting (§7.13) |
//! | `keymap` | none | a preset layer: `"tmux"` (§9.8 keymap layers) |
//! | `keymap_prefix` | `"ctrl+b"` | the preset's prefix |
//! | `keybinds` | `{}` | the user layer: chord → action, a list of actions, or `"unbind"` |

use std::path::{Path, PathBuf};

use auk_plugins::chord::Chord;
use serde_json::Value;

/// `reduce_motion`: follow the system, or Auk's own choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Motion {
    #[default]
    System,
    Reduce,
    Full,
}

/// One user keybind: the chord and its actions (empty: unbound).
#[derive(Clone, Debug, PartialEq)]
pub struct UserBind {
    pub chord: Chord,
    pub text: String,
    pub actions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub program_colors: bool,
    pub native_editing: bool,
    pub links_plain_click: bool,
    pub reduce_motion: Motion,
    pub keymap: Option<String>,
    pub keymap_prefix: String,
    pub keybinds: Vec<UserBind>,
    /// Problems reading the file (a key of the wrong type, a chord that doesn't parse).
    pub problems: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            program_colors: true,
            native_editing: true,
            links_plain_click: false,
            reduce_motion: Motion::System,
            keymap: None,
            keymap_prefix: "ctrl+b".to_owned(),
            keybinds: Vec::new(),
            problems: Vec::new(),
        }
    }
}

/// `<config>/settings.json`.
pub fn path() -> PathBuf {
    auk_plugins::paths::config_dir().join("settings.json")
}

impl Settings {
    /// Reads the settings file; a missing file gives the defaults.
    pub fn load() -> Settings {
        Settings::load_from(&path())
    }

    pub fn load_from(path: &Path) -> Settings {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Settings::default();
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(v) => Settings::from_json(&v),
            Err(e) => Settings {
                problems: vec![format!("{}: {e}", path.display())],
                ..Settings::default()
            },
        }
    }

    pub fn from_json(v: &Value) -> Settings {
        let mut s = Settings::default();
        let flag = |key: &str, slot: &mut bool, problems: &mut Vec<String>| match v.get(key) {
            None | Some(Value::Null) => {}
            Some(Value::Bool(b)) => *slot = *b,
            Some(_) => problems.push(format!("{key}: expected true or false")),
        };
        let mut problems = Vec::new();
        flag("program_colors", &mut s.program_colors, &mut problems);
        flag("native_editing", &mut s.native_editing, &mut problems);
        flag("links_plain_click", &mut s.links_plain_click, &mut problems);
        s.reduce_motion = match v.get("reduce_motion") {
            None | Some(Value::Null) => Motion::System,
            Some(Value::Bool(true)) => Motion::Reduce,
            Some(Value::Bool(false)) => Motion::Full,
            Some(Value::String(x)) if x == "system" => Motion::System,
            Some(_) => {
                problems.push("reduce_motion: expected true, false or \"system\"".into());
                Motion::System
            }
        };
        match v.get("keymap") {
            None | Some(Value::Null) => {}
            Some(Value::String(k)) if k == "default" || k.is_empty() => {}
            Some(Value::String(k)) if k == "tmux" => s.keymap = Some(k.clone()),
            Some(other) => problems.push(format!("keymap: unknown preset {other}")),
        }
        if let Some(p) = v.get("keymap_prefix").and_then(Value::as_str) {
            match Chord::parse(p) {
                Ok(c) if c.steps.len() == 1 => s.keymap_prefix = p.to_owned(),
                _ => problems.push(format!("keymap_prefix: `{p}` isn't a single chord")),
            }
        }
        if let Some(binds) = v.get("keybinds").and_then(Value::as_object) {
            for (chord, action) in binds {
                let parsed = match Chord::parse(chord) {
                    Ok(c) => c,
                    Err(e) => {
                        problems.push(format!("keybinds: `{chord}`: {e}"));
                        continue;
                    }
                };
                let actions: Vec<String> = match action {
                    Value::String(a) if a == "unbind" => Vec::new(),
                    Value::String(a) => vec![a.clone()],
                    Value::Array(list) => list
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|a| *a != "unbind")
                        .map(str::to_owned)
                        .collect(),
                    _ => {
                        problems.push(format!("keybinds: `{chord}`: expected an action"));
                        continue;
                    }
                };
                s.keybinds.push(UserBind {
                    chord: parsed,
                    text: chord.clone(),
                    actions,
                });
            }
        }
        s.problems = problems;
        s
    }

    /// The chords the user layer binds or unbinds.
    pub fn user_chords(&self) -> Vec<Chord> {
        self.keybinds.iter().map(|b| b.chord.clone()).collect()
    }

    /// Whether motion is reduced, given the system setting.
    pub fn reduces_motion(&self, system: bool) -> bool {
        match self.reduce_motion {
            Motion::System => system,
            Motion::Reduce => true,
            Motion::Full => false,
        }
    }

    /// The preset layer (§9.8): `(chord, action)`, under the prefix.
    pub fn preset(&self) -> Vec<(Chord, String)> {
        let Some(name) = &self.keymap else {
            return Vec::new();
        };
        if name != "tmux" {
            return Vec::new();
        }
        let p = &self.keymap_prefix;
        let mut out: Vec<(String, String)> = [
            ("c", "new_tab"),
            ("%", "split_right"),
            ("\"", "split_down"),
            ("n", "next_tab"),
            ("p", "previous_tab"),
            ("x", "close_pane"),
            ("z", "toggle_zoom"),
            ("left", "focus_left"),
            ("right", "focus_right"),
            ("up", "focus_up"),
            ("down", "focus_down"),
            ("$", "palette"),
        ]
        .iter()
        .map(|(k, a)| (format!("{p}>{k}"), (*a).to_owned()))
        .collect();
        for n in 1..=9 {
            out.push((format!("{p}>{n}"), format!("goto_tab:{n}")));
        }
        out.into_iter()
            .filter_map(|(c, a)| Some((Chord::parse(&c).ok()?, a)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_settings_and_keybinds() {
        let s = Settings::from_json(&json!({
            "program_colors": false,
            "links_plain_click": true,
            "reduce_motion": true,
            "keymap": "tmux",
            "keymap_prefix": "ctrl+a",
            "keybinds": {"cmd+t": "unbind", "ctrl+a>c": ["split_down", "new_tab"], "bad+": "x"},
        }));
        assert!(!s.program_colors && s.native_editing && s.links_plain_click);
        assert!(s.reduces_motion(false));
        assert_eq!(s.keybinds.len(), 2, "{:?}", s.problems);
        assert_eq!(s.problems.len(), 1);
        assert!(
            s.keybinds
                .iter()
                .any(|b| b.text == "cmd+t" && b.actions.is_empty())
        );
        let preset = s.preset();
        assert!(
            preset
                .iter()
                .any(|(c, a)| a == "new_tab" && *c == Chord::parse("ctrl+a>c").unwrap())
        );
        assert_eq!(Settings::from_json(&json!({})), Settings::default());
        assert!(Settings::default().reduces_motion(true));
    }
}
