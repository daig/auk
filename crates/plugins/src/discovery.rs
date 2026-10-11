//! Package discovery: reading the plugins folder.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::manifest::{self, Manifest};

/// At most this many valid packages load; those past it (by id) are problems.
pub const MAX_PLUGINS: usize = 64;

/// A valid package found in the plugins folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// The validated manifest.
    pub manifest: Manifest,
    /// The package directory: the folder entry, or the target of a linked package.
    pub dir: PathBuf,
    /// The manifest styles, joined.
    pub css: String,
}

/// A folder entry that loads nothing, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// The folder entry.
    pub dir: PathBuf,
    /// The first broken rule.
    pub error: String,
}

/// The result of reading a plugins folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    /// Valid packages, sorted by id, at most [`MAX_PLUGINS`].
    pub packages: Vec<Package>,
    /// Entries that load nothing, in folder order (then `too many plugins`, by id).
    pub problems: Vec<Problem>,
}

/// The `plugins` and `plugins_disabled` settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSettings {
    /// `plugins`: false loads no plugin at all.
    pub enabled: bool,
    /// `plugins_disabled`: ids that stay installed but unloaded.
    pub disabled: Vec<String>,
}

impl Default for PluginSettings {
    fn default() -> Self {
        PluginSettings {
            enabled: true,
            disabled: Vec::new(),
        }
    }
}

impl PluginSettings {
    /// Whether the settings allow plugin `id` to load.
    pub fn allows(&self, id: &str) -> bool {
        self.enabled && !self.disabled.iter().any(|d| d == id)
    }
}

/// Resolves a folder entry to its package directory: a directory (or a symlink to one) is a
/// package; a `<name>.path` file is a link; anything else is ignored (`Ok(None)`).
fn package_dir(entry: &Path) -> Result<Option<PathBuf>, String> {
    if entry.is_dir() {
        return Ok(Some(entry.to_path_buf()));
    }
    if entry.extension().is_none_or(|ext| ext != "path") || !entry.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(entry).map_err(|e| format!("cannot read link: {e}"))?;
    let target = text.trim();
    let path = Path::new(target);
    if !path.is_absolute() {
        return Err(format!("link target {target:?} is not an absolute path"));
    }
    if !path.is_dir() {
        return Err(format!("link target {} is not a directory", path.display()));
    }
    Ok(Some(path.to_path_buf()))
}

/// Reads `plugins_dir` in name order. A missing folder is an empty scan.
pub fn scan(plugins_dir: &Path) -> Scan {
    let mut names: Vec<_> = match std::fs::read_dir(plugins_dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect(),
        Err(_) => return Scan::default(),
    };
    names.sort();

    let mut scan = Scan::default();
    let mut by_id: BTreeMap<String, Package> = BTreeMap::new();
    for name in names {
        if name.as_encoded_bytes().first() == Some(&b'.') {
            continue;
        }
        let entry = plugins_dir.join(&name);
        let dir = match package_dir(&entry) {
            Ok(Some(dir)) => dir,
            Ok(None) => continue,
            Err(error) => {
                scan.problems.push(Problem { dir: entry, error });
                continue;
            }
        };
        match manifest::load_package(&dir) {
            Ok((manifest, css)) => {
                if by_id.contains_key(&manifest.id) {
                    scan.problems.push(Problem {
                        dir: entry,
                        error: format!("duplicate id {}", manifest.id),
                    });
                } else {
                    by_id.insert(manifest.id.clone(), Package { manifest, dir, css });
                }
            }
            Err(error) => scan.problems.push(Problem { dir: entry, error }),
        }
    }
    let mut packages: Vec<Package> = by_id.into_values().collect();
    for extra in packages.split_off(packages.len().min(MAX_PLUGINS)) {
        scan.problems.push(Problem {
            dir: extra.dir,
            error: "too many plugins".into(),
        });
    }
    scan.packages = packages;
    scan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_pkg(dir: &Path, id: &str) {
        std::fs::create_dir_all(dir).expect("mkdir");
        std::fs::write(
            dir.join("plugin.toml"),
            format!("schema = 1\nid = \"{id}\"\nname = \"{id}\"\nversion = \"1\"\nhost = \"host.luau\"\n"),
        )
        .expect("write");
        std::fs::write(dir.join("host.luau"), "").expect("write");
    }

    #[test]
    fn folder_rules() {
        let root = tempfile::tempdir().expect("tempdir");
        let plugins = root.path().join("plugins");
        write_pkg(&plugins.join("b-pkg"), "beta");
        write_pkg(&plugins.join("a-pkg"), "alpha");
        write_pkg(&plugins.join("c-dup"), "alpha");
        write_pkg(&plugins.join(".staging"), "staged");
        std::fs::create_dir_all(plugins.join("empty")).expect("mkdir");
        std::fs::write(plugins.join("notes.txt"), "ignored").expect("write");
        let linked = root.path().join("elsewhere");
        write_pkg(&linked, "linked");
        std::fs::write(
            plugins.join("linked.path"),
            format!("{}\n", linked.display()),
        )
        .expect("write");
        std::fs::write(plugins.join("rel.path"), "relative/dir").expect("write");
        std::fs::write(plugins.join("gone.path"), "/does/not/exist").expect("write");
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.path().join("elsewhere"), plugins.join("z-symlink"))
            .expect("symlink");

        let scan = scan(&plugins);
        let ids: Vec<_> = scan
            .packages
            .iter()
            .map(|p| p.manifest.id.as_str())
            .collect();
        assert_eq!(ids, ["alpha", "beta", "linked"]);
        assert_eq!(scan.packages[2].dir, linked);
        let problems: Vec<_> = scan
            .problems
            .iter()
            .map(|p| {
                (
                    p.dir
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default(),
                    p.error.as_str(),
                )
            })
            .collect();
        assert_eq!(
            problems,
            [
                ("c-dup", "duplicate id alpha"),
                ("empty", "no plugin.toml"),
                (
                    "gone.path",
                    "link target /does/not/exist is not a directory"
                ),
                (
                    "rel.path",
                    "link target \"relative/dir\" is not an absolute path"
                ),
                ("z-symlink", "duplicate id linked"),
            ]
        );
    }

    #[test]
    fn at_most_64_by_id() {
        let root = tempfile::tempdir().expect("tempdir");
        for i in 0..66 {
            write_pkg(
                &root.path().join(format!("{:02}", 65 - i)),
                &format!("p{i:02}"),
            );
        }
        let scan = scan(root.path());
        assert_eq!(scan.packages.len(), 64);
        assert_eq!(
            scan.packages.last().map(|p| p.manifest.id.as_str()),
            Some("p63")
        );
        let too_many: Vec<_> = scan
            .problems
            .iter()
            .filter(|p| p.error == "too many plugins")
            .collect();
        assert_eq!(too_many.len(), 2);
    }

    #[test]
    fn missing_folder_is_empty() {
        assert_eq!(scan(Path::new("/does/not/exist")), Scan::default());
    }

    #[test]
    fn settings() {
        let s = PluginSettings {
            enabled: true,
            disabled: vec!["x".into()],
        };
        assert!(s.allows("y"));
        assert!(!s.allows("x"));
        assert!(
            !PluginSettings {
                enabled: false,
                disabled: vec![]
            }
            .allows("y")
        );
    }
}
