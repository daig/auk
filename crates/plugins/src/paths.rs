//! Auk's config and state directories for plugins (§9.1, D5).
//!
//! - macOS: `~/Library/Application Support/Auk` for both;
//! - Linux: `$XDG_CONFIG_HOME/auk` (`~/.config/auk`) and `$XDG_STATE_HOME/auk` (`~/.local/state/auk`);
//! - `$AUK_CONFIG_DIR` overrides both.

use std::path::{Path, PathBuf};

/// The environment variable that overrides both directories.
pub const CONFIG_DIR_ENV: &str = "AUK_CONFIG_DIR";

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn home() -> PathBuf {
    env_path("HOME").unwrap_or_else(|| PathBuf::from("/"))
}

/// `<config>`: holds `plugins/` and `settings.json`.
pub fn config_dir() -> PathBuf {
    if let Some(dir) = env_path(CONFIG_DIR_ENV) {
        return dir;
    }
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/Auk")
    } else {
        env_path("XDG_CONFIG_HOME")
            .unwrap_or_else(|| home().join(".config"))
            .join("auk")
    }
}

/// `<state>`: holds `plugin-data/`.
pub fn state_dir() -> PathBuf {
    if let Some(dir) = env_path(CONFIG_DIR_ENV) {
        return dir;
    }
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/Auk")
    } else {
        env_path("XDG_STATE_HOME")
            .unwrap_or_else(|| home().join(".local/state"))
            .join("auk")
    }
}

/// `<config>/plugins/`.
pub fn plugins_dir(config: &Path) -> PathBuf {
    config.join("plugins")
}

/// `<state>/plugin-data/<id>/`, shared by both halves and kept when the plugin is removed.
pub fn data_dir(state: &Path, id: &str) -> PathBuf {
    state.join("plugin-data").join(id)
}
