//! Auk's config and state directories for plugins.
//!
//! - macOS: `~/Library/Application Support/Auk` for both;
//! - Linux: `$XDG_CONFIG_HOME/auk` (`~/.config/auk`) and `$XDG_STATE_HOME/auk` (`~/.local/state/auk`);
//! - `$AUK_CONFIG_DIR` overrides both.
//!
//! The daemon's `$AUK_STATE_DIR` (which tests set) also names `<state>` here, so both halves of
//! a plugin, and every window, share one `plugin-data` folder with the daemon.

use std::path::{Path, PathBuf};

/// The environment variable that overrides both directories.
pub const CONFIG_DIR_ENV: &str = "AUK_CONFIG_DIR";

/// The daemon's state-dir override, honored here so `plugin-data` is the daemon's.
pub const STATE_DIR_ENV: &str = "AUK_STATE_DIR";

/// The platforms whose default paths differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Linux,
}

impl Platform {
    /// The platform this build runs on.
    pub fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Linux
        }
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn home(var: &dyn Fn(&str) -> Option<PathBuf>) -> PathBuf {
    var("HOME").unwrap_or_else(|| PathBuf::from("/"))
}

/// `<config>` for `platform`, with environment variables read through `var` (unset and empty
/// are the same).
pub fn config_dir_for(platform: Platform, var: &dyn Fn(&str) -> Option<PathBuf>) -> PathBuf {
    if let Some(dir) = var(CONFIG_DIR_ENV) {
        return dir;
    }
    match platform {
        Platform::MacOs => home(var).join("Library/Application Support/Auk"),
        Platform::Linux => var("XDG_CONFIG_HOME")
            .unwrap_or_else(|| home(var).join(".config"))
            .join("auk"),
    }
}

/// `<state>` for `platform`, as [`config_dir_for`].
pub fn state_dir_for(platform: Platform, var: &dyn Fn(&str) -> Option<PathBuf>) -> PathBuf {
    if let Some(dir) = var(STATE_DIR_ENV).or_else(|| var(CONFIG_DIR_ENV)) {
        return dir;
    }
    match platform {
        Platform::MacOs => home(var).join("Library/Application Support/Auk"),
        Platform::Linux => var("XDG_STATE_HOME")
            .unwrap_or_else(|| home(var).join(".local/state"))
            .join("auk"),
    }
}

/// `<config>`: holds `plugins/` and `settings.json`.
pub fn config_dir() -> PathBuf {
    config_dir_for(Platform::current(), &env_path)
}

/// `<state>`: holds `plugin-data/`.
pub fn state_dir() -> PathBuf {
    state_dir_for(Platform::current(), &env_path)
}

/// `<config>/plugins/`.
pub fn plugins_dir(config: &Path) -> PathBuf {
    config.join("plugins")
}

/// `<state>/plugin-data/<id>/`, shared by both halves and kept when the plugin is removed.
pub fn data_dir(state: &Path, id: &str) -> PathBuf {
    state.join("plugin-data").join(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<PathBuf> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(k, v)| k == name && !v.is_empty())
                .map(|(_, v)| PathBuf::from(v))
        }
    }

    #[test]
    fn default_paths_per_platform_and_the_override() {
        let home = vars(&[
            ("HOME", "/Users/u"),
            ("XDG_CONFIG_HOME", "/x/c"),
            ("XDG_STATE_HOME", "/x/s"),
        ]);
        // macOS: Application Support for both; XDG is ignored.
        let mac = Path::new("/Users/u/Library/Application Support/Auk");
        assert_eq!(config_dir_for(Platform::MacOs, &home), mac);
        assert_eq!(state_dir_for(Platform::MacOs, &home), mac);
        // Linux: the XDG dirs.
        assert_eq!(
            config_dir_for(Platform::Linux, &home),
            Path::new("/x/c/auk")
        );
        assert_eq!(state_dir_for(Platform::Linux, &home), Path::new("/x/s/auk"));
        // Linux without XDG (or with it empty): ~/.config and ~/.local/state.
        let bare = vars(&[("HOME", "/home/u"), ("XDG_CONFIG_HOME", "")]);
        assert_eq!(
            config_dir_for(Platform::Linux, &bare),
            Path::new("/home/u/.config/auk")
        );
        assert_eq!(
            state_dir_for(Platform::Linux, &bare),
            Path::new("/home/u/.local/state/auk")
        );
        // $AUK_CONFIG_DIR overrides both, everywhere.
        let over = vars(&[
            ("HOME", "/home/u"),
            ("XDG_STATE_HOME", "/x/s"),
            ("AUK_CONFIG_DIR", "/o"),
        ]);
        for p in [Platform::MacOs, Platform::Linux] {
            assert_eq!(config_dir_for(p, &over), Path::new("/o"));
            assert_eq!(state_dir_for(p, &over), Path::new("/o"));
        }
        // The daemon's state override names <state> (plugin-data) too, not <config>.
        let state = vars(&[
            ("HOME", "/home/u"),
            ("AUK_CONFIG_DIR", "/o"),
            ("AUK_STATE_DIR", "/s"),
        ]);
        assert_eq!(state_dir_for(Platform::Linux, &state), Path::new("/s"));
        assert_eq!(config_dir_for(Platform::Linux, &state), Path::new("/o"));
        assert_eq!(
            data_dir(Path::new("/s"), "demo"),
            Path::new("/s/plugin-data/demo")
        );
    }
}
