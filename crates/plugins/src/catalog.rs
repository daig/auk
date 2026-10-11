//! The catalog: the one piece of plugin state every replica shares.
//!
//! Its JSON form is that of `tern plugin list --json`. Replicas (windows, the
//! web tab) learn block types, lens claims and plugin styles from it without running Lua. This
//! module has no Luau dependency, so it builds wherever the replica does.

use serde::{Deserialize, Serialize};

use crate::discovery::{PluginSettings, Problem as ScanProblem, Scan};

/// A host entry's status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Loaded and its entry ran (or there is no host entry and the settings allow it).
    Ready,
    /// Turned off by `plugins` or `plugins_disabled`.
    Disabled,
    /// The entry raised, tripped its budget while loading, or left a declared block or lens
    /// undefined. Holds the full error.
    Failed(String),
}

impl Status {
    /// Whether the plugin claims lenses, offers block types and contributes styles.
    pub fn is_ready(&self) -> bool {
        matches!(self, Status::Ready)
    }
}

/// A block type as the catalog lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockType {
    /// Block id within the plugin.
    pub id: String,
    /// Title.
    pub title: String,
    /// Resolved icon.
    pub icon: String,
    /// File-name globs.
    pub files: Vec<String>,
    /// Offered in the palette.
    pub palette: bool,
}

/// A lens rule: id and its `match` globs, in manifest order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LensRule {
    /// Lens id within the plugin.
    pub id: String,
    /// The `match` globs.
    pub patterns: Vec<String>,
}

/// One plugin's catalog entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginEntry {
    /// Plugin id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Version, as written.
    pub version: String,
    /// Description.
    pub description: String,
    /// Resolved icon.
    pub icon: String,
    /// The manifest styles, joined.
    pub css: String,
    /// Declared block types, in manifest order.
    pub blocks: Vec<BlockType>,
    /// Declared lenses, in manifest order.
    pub lenses: Vec<LensRule>,
    /// Whether the package has a host entry.
    pub host: bool,
    /// Whether the package has a window entry.
    pub window: bool,
    /// The host entry's status.
    pub status: Status,
}

/// A folder entry that is not a usable plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Problem {
    /// The folder entry.
    pub dir: String,
    /// Why it loads nothing.
    pub error: String,
}

impl From<&ScanProblem> for Problem {
    fn from(p: &ScanProblem) -> Self {
        Problem {
            dir: p.dir.display().to_string(),
            error: p.error.clone(),
        }
    }
}

/// The catalog a host sends to every replica at attach and after reloads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    /// Plugins, sorted by id.
    pub plugins: Vec<PluginEntry>,
    /// Folder entries that load nothing.
    pub problems: Vec<Problem>,
    /// Whether any host half loaded with Luau native codegen.
    pub jit: bool,
}

impl Catalog {
    /// Builds a catalog from a scan. `host_status` gives the status of each allowed package that
    /// has a host entry (by id); allowed packages without one are Ready, others Disabled.
    pub fn build(
        scan: &Scan,
        settings: &PluginSettings,
        jit: bool,
        mut host_status: impl FnMut(&str) -> Status,
    ) -> Self {
        let plugins = scan
            .packages
            .iter()
            .map(|pkg| {
                let m = &pkg.manifest;
                let status = if !settings.allows(&m.id) {
                    Status::Disabled
                } else if m.host.is_some() {
                    host_status(&m.id)
                } else {
                    Status::Ready
                };
                PluginEntry {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    version: m.version.clone(),
                    description: m.description.clone(),
                    icon: m.icon.clone(),
                    css: pkg.css.clone(),
                    blocks: m
                        .blocks
                        .iter()
                        .map(|b| BlockType {
                            id: b.id.clone(),
                            title: b.title.clone(),
                            icon: b.icon.clone(),
                            files: b.files.clone(),
                            palette: b.palette,
                        })
                        .collect(),
                    lenses: m
                        .lenses
                        .iter()
                        .map(|l| LensRule {
                            id: l.id.clone(),
                            patterns: l.patterns.clone(),
                        })
                        .collect(),
                    host: m.host.is_some(),
                    window: m.window.is_some(),
                    status,
                }
            })
            .collect();
        Catalog {
            plugins,
            problems: scan.problems.iter().map(Problem::from).collect(),
            jit,
        }
    }

    /// Ready plugins, in id order.
    pub fn ready(&self) -> impl Iterator<Item = &PluginEntry> {
        self.plugins.iter().filter(|p| p.status.is_ready())
    }

    /// Lens rules in claim order: Ready plugins in id order, then manifest order. Yields
    /// `(plugin, rule)`.
    pub fn lens_claims(&self) -> impl Iterator<Item = (&PluginEntry, &LensRule)> {
        self.ready()
            .flat_map(|p| p.lenses.iter().map(move |l| (p, l)))
    }

    /// Block types Ready plugins offer, as `(plugin, block)`; the kind is `<plugin>.<block>`.
    pub fn block_types(&self) -> impl Iterator<Item = (&PluginEntry, &BlockType)> {
        self.ready()
            .flat_map(|p| p.blocks.iter().map(move |b| (p, b)))
    }

    /// Looks up a block kind `<plugin>.<block>` among Ready plugins.
    pub fn block_type(&self, kind: &str) -> Option<(&PluginEntry, &BlockType)> {
        let (plugin, block) = kind.split_once('.')?;
        self.block_types()
            .find(|(p, b)| p.id == plugin && b.id == block)
    }

    /// Ready plugins' styles, as `(plugin id, css)` in id order, skipping empty ones.
    pub fn styles(&self) -> impl Iterator<Item = (&str, &str)> {
        self.ready()
            .filter(|p| !p.css.is_empty())
            .map(|p| (p.id.as_str(), p.css.as_str()))
    }

    /// The JSON form (`tern plugin list --json`).
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{Package, Problem as ScanProblem};
    use crate::manifest::Manifest;
    use std::path::PathBuf;

    fn package(text: &str, css: &str) -> Package {
        Package {
            manifest: Manifest::parse(text).expect("valid"),
            dir: PathBuf::from("/p"),
            css: css.into(),
        }
    }

    fn sample() -> (Scan, PluginSettings) {
        let scan = Scan {
            packages: vec![
                package(
                    "schema = 1\nid = \"a\"\nname = \"A\"\nversion = \"1\"\nhost = \"h.luau\"\nicon = \"git\"\n[[blocks]]\nid = \"x\"\ntitle = \"X\"\n[[lenses]]\nid = \"l\"\nmatch = [\"a *\"]\n",
                    "a{}",
                ),
                package(
                    "schema = 1\nid = \"b\"\nname = \"B\"\nversion = \"2\"\nwindow = \"w.luau\"\n",
                    "",
                ),
                package(
                    "schema = 1\nid = \"c\"\nname = \"C\"\nversion = \"3\"\nhost = \"h.luau\"\n[[lenses]]\nid = \"m\"\nmatch = [\"c\"]\n",
                    "c{}",
                ),
                package(
                    "schema = 1\nid = \"d\"\nname = \"D\"\nversion = \"4\"\nhost = \"h.luau\"\n",
                    "",
                ),
            ],
            problems: vec![ScanProblem {
                dir: PathBuf::from("/plugins/bad"),
                error: "no plugin.toml".into(),
            }],
        };
        (
            scan,
            PluginSettings {
                enabled: true,
                disabled: vec!["d".into()],
            },
        )
    }

    #[test]
    fn statuses_and_queries() {
        let (scan, settings) = sample();
        let catalog = Catalog::build(&scan, &settings, true, |id| match id {
            "a" => Status::Ready,
            _ => Status::Failed("boom\ntrace".into()),
        });
        let statuses: Vec<_> = catalog
            .plugins
            .iter()
            .map(|p| (p.id.as_str(), p.status.clone()))
            .collect();
        assert_eq!(
            statuses,
            [
                ("a", Status::Ready),
                ("b", Status::Ready),
                ("c", Status::Failed("boom\ntrace".into())),
                ("d", Status::Disabled)
            ]
        );
        let claims: Vec<_> = catalog
            .lens_claims()
            .map(|(p, l)| format!("{}.{}", p.id, l.id))
            .collect();
        assert_eq!(claims, ["a.l"]);
        assert!(catalog.block_type("a.x").is_some());
        assert!(catalog.block_type("c.x").is_none());
        assert_eq!(catalog.styles().collect::<Vec<_>>(), [("a", "a{}")]);
    }

    #[test]
    fn json_form() {
        let (scan, settings) = sample();
        let catalog = Catalog::build(&scan, &settings, false, |id| {
            if id == "a" {
                Status::Ready
            } else {
                Status::Failed("boom".into())
            }
        });
        let json = catalog.to_json();
        assert_eq!(
            json["plugins"][0],
            serde_json::json!({
                "id": "a", "name": "A", "version": "1", "description": "", "icon": "branch", "css": "a{}",
                "blocks": [{"id": "x", "title": "X", "icon": "branch", "files": [], "palette": true}],
                "lenses": [{"id": "l", "patterns": ["a *"]}],
                "host": true, "window": false, "status": "ready"
            })
        );
        assert_eq!(
            json["plugins"][2]["status"],
            serde_json::json!({"failed": "boom"})
        );
        assert_eq!(json["plugins"][3]["status"], "disabled");
        assert_eq!(
            json["problems"],
            serde_json::json!([{"dir": "/plugins/bad", "error": "no plugin.toml"}])
        );
        assert_eq!(json["jit"], false);
        let back: Catalog = serde_json::from_value(json).expect("round trip");
        assert_eq!(back, catalog);
    }
}
