//! The user's settings file, `<config>/settings.json` (§9.1; research/product.md §7): one JSON
//! object holding only the keys the user changed. `$AUK_CONFIG_DIR` names the config dir, as it
//! does for `plugins/`. Each machine reads its own; nothing syncs between hosts.
//!
//! The daemon reads the keys it acts on:
//!
//! | Key | Default | Read |
//! | --- | --- | --- |
//! | `plugins` | `true` | at start and at every plugin reload (§9.3): `false` loads no plugin |
//! | `plugins_disabled` | `[]` | likewise: listed ids stay installed but Disabled |
//! | `agent_command` | `"omp"` | when an agent pane opens (§5.1); existing panes keep theirs |
//!
//! A missing file means every default. A file that isn't a JSON object is reported and ignored;
//! a key of the wrong type keeps its default. Keys the daemon doesn't use are left alone.

use std::path::PathBuf;

use auk_plugins::PluginSettings;
use serde_json::Value;

/// The file's name in the config dir.
pub const FILE: &str = "settings.json";

/// The agent command when `agent_command` isn't set.
pub const DEFAULT_AGENT_COMMAND: &str = "omp";

/// What the daemon reads from the settings file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserSettings {
    pub plugins: PluginSettings,
    /// What agent panes run through the login shell.
    pub agent_command: String,
}

impl Default for UserSettings {
    fn default() -> Self {
        UserSettings {
            plugins: PluginSettings::default(),
            agent_command: DEFAULT_AGENT_COMMAND.to_owned(),
        }
    }
}

impl UserSettings {
    /// The settings a parsed file gives.
    pub fn from_json(value: &Value) -> UserSettings {
        let mut s = UserSettings::default();
        if let Some(on) = value.get("plugins").and_then(Value::as_bool) {
            s.plugins.enabled = on;
        }
        if let Some(ids) = value.get("plugins_disabled").and_then(Value::as_array) {
            s.plugins.disabled = ids
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
        }
        if let Some(cmd) = value
            .get("agent_command")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|c| !c.is_empty())
        {
            s.agent_command = cmd.to_owned();
        }
        s
    }
}

/// `<config>/settings.json`.
pub fn path() -> PathBuf {
    auk_plugins::paths::config_dir().join(FILE)
}

/// Reads the settings file now.
pub fn read() -> UserSettings {
    let path = path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return UserSettings::default(),
        Err(err) => {
            eprintln!("auk daemon: {}: {err}", path.display());
            return UserSettings::default();
        }
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(value @ Value::Object(_)) => UserSettings::from_json(&value),
        Ok(_) => {
            eprintln!("auk daemon: {}: not a JSON object", path.display());
            UserSettings::default()
        }
        Err(err) => {
            eprintln!("auk daemon: {}: {err}", path.display());
            UserSettings::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_the_keys_it_acts_on() {
        assert_eq!(UserSettings::from_json(&json!({})), UserSettings::default());
        let s = UserSettings::from_json(&json!({
            "plugins": false,
            "plugins_disabled": ["a", 3, "b"],
            "agent_command": "  claude --fast ",
            "theme": "System",
        }));
        assert!(!s.plugins.enabled);
        assert_eq!(s.plugins.disabled, ["a", "b"]);
        assert_eq!(s.agent_command, "claude --fast");
        // Wrong types and a blank command keep the defaults.
        let s = UserSettings::from_json(&json!({"plugins": "no", "agent_command": " "}));
        assert!(s.plugins.enabled);
        assert_eq!(s.agent_command, DEFAULT_AGENT_COMMAND);
    }
}
