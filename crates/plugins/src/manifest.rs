//! `plugin.toml` (schema 1): parsing and the two validation passes of §9.1.
//!
//! [`Manifest::parse`] applies the *text rules* (size, TOML, schema, key names and types, ids,
//! name, entry and style paths, blocks and lenses). [`Manifest::check_package`] applies the
//! *package rules* (entries exist, styles are readable and within the cap). Each reports only the
//! first broken rule, with Tern's message (research/plugins.md §2.2).

use std::collections::BTreeSet;
use std::path::{Component, Path};

use serde::Deserialize;

use crate::icons;

/// Largest accepted `plugin.toml`.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Largest total of the joined manifest `styles`.
pub const MAX_STYLES_BYTES: usize = 256 * 1024;

/// A validated `plugin.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Plugin id: `^[a-z][a-z0-9-]{0,31}$`.
    pub id: String,
    /// Display name, 1–64 characters.
    pub name: String,
    /// Free-form version; never compared.
    pub version: String,
    /// One-line description (empty when absent).
    pub description: String,
    /// The resolved icon name (an unknown or absent icon is `puzzle`).
    pub icon: String,
    /// Relative path of the host entry, as written.
    pub host: Option<String>,
    /// Relative path of the window entry, as written.
    pub window: Option<String>,
    /// Relative style paths, in order.
    pub styles: Vec<String>,
    /// Declared block types, in manifest order.
    pub blocks: Vec<BlockDecl>,
    /// Declared lenses, in manifest order.
    pub lenses: Vec<LensDecl>,
}

/// A `[[blocks]]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockDecl {
    /// Block id, unique within the plugin.
    pub id: String,
    /// Palette and pane title.
    pub title: String,
    /// Resolved icon: the block's own, else the plugin's.
    pub icon: String,
    /// File-name globs that offer "Open with <title>".
    pub files: Vec<String>,
    /// Whether the palette offers "New <title> block".
    pub palette: bool,
}

/// A `[[lenses]]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LensDecl {
    /// Lens id, unique within the plugin.
    pub id: String,
    /// Globs over `program args…`; at least one.
    pub patterns: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    #[allow(dead_code, reason = "checked before deserializing")]
    schema: i64,
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    description: String,
    icon: Option<String>,
    host: Option<String>,
    window: Option<String>,
    #[serde(default)]
    styles: Vec<String>,
    #[serde(default)]
    blocks: Vec<RawBlock>,
    #[serde(default)]
    lenses: Vec<RawLens>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBlock {
    id: String,
    title: String,
    icon: Option<String>,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default = "default_palette")]
    palette: bool,
}

fn default_palette() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLens {
    id: String,
    #[serde(rename = "match")]
    patterns: Vec<String>,
}

/// Whether `id` matches `^[a-z][a-z0-9-]{0,31}$` (plugin, block and lens ids).
pub fn is_valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 32
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Whether `path` is relative and stays inside the package: only normal components and `.`.
fn is_inside_relative(path: &str) -> bool {
    let path = Path::new(path);
    let mut normal = false;
    for component in path.components() {
        match component {
            Component::Normal(_) => normal = true,
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    normal
}

fn check_entry(key: &str, path: &str) -> Result<(), String> {
    if !is_inside_relative(path) {
        return Err(format!(
            "{key} {path:?} must be a relative path inside the plugin"
        ));
    }
    if !(path.ends_with(".luau") || path.ends_with(".lua")) {
        return Err(format!("{key} {path:?} must be a .luau or .lua file"));
    }
    Ok(())
}

impl Manifest {
    /// Parses `text` and applies the text rules. The error is the first broken rule.
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > MAX_MANIFEST_BYTES {
            return Err("plugin.toml is larger than 64 KiB".into());
        }
        let table: toml::Table = toml::from_str(text).map_err(|e| e.message().to_owned())?;
        match table.get("schema") {
            None => return Err("missing schema".into()),
            Some(toml::Value::Integer(1)) => {}
            Some(toml::Value::Integer(n)) => return Err(format!("unsupported schema {n}")),
            Some(_) => return Err("schema must be an integer".into()),
        }
        let raw: RawManifest = toml::from_str(text).map_err(|e| e.message().to_owned())?;

        if !is_valid_id(&raw.id) {
            return Err(format!(
                "invalid id {:?} (lowercase letters, digits and -, 1-32 long)",
                raw.id
            ));
        }
        let name_len = raw.name.chars().count();
        if !(1..=64).contains(&name_len) {
            return Err("name must be 1-64 characters".into());
        }
        if raw.host.is_none() && raw.window.is_none() {
            return Err("needs a host or window entry".into());
        }
        if let Some(host) = &raw.host {
            check_entry("host", host)?;
        }
        if let Some(window) = &raw.window {
            check_entry("window", window)?;
        }
        for style in &raw.styles {
            if !is_inside_relative(style) {
                return Err(format!(
                    "style {style:?} must be a relative path inside the plugin"
                ));
            }
        }

        let icon = icons::resolve(raw.icon.as_deref()).to_owned();

        if !raw.blocks.is_empty() && raw.host.is_none() {
            return Err("blocks need a host entry".into());
        }
        let mut seen = BTreeSet::new();
        let mut blocks = Vec::with_capacity(raw.blocks.len());
        for block in raw.blocks {
            if !is_valid_id(&block.id) {
                return Err(format!("invalid block id {:?}", block.id));
            }
            if !seen.insert(block.id.clone()) {
                return Err(format!("duplicate block id {:?}", block.id));
            }
            let icon = match block.icon.as_deref() {
                Some(name) => icons::resolve(Some(name)).to_owned(),
                None => icon.clone(),
            };
            blocks.push(BlockDecl {
                id: block.id,
                title: block.title,
                icon,
                files: block.files,
                palette: block.palette,
            });
        }

        if !raw.lenses.is_empty() && raw.host.is_none() {
            return Err("lenses need a host entry".into());
        }
        let mut seen = BTreeSet::new();
        let mut lenses = Vec::with_capacity(raw.lenses.len());
        for lens in raw.lenses {
            if !is_valid_id(&lens.id) {
                return Err(format!("invalid lens id {:?}", lens.id));
            }
            if !seen.insert(lens.id.clone()) {
                return Err(format!("duplicate lens id {:?}", lens.id));
            }
            if lens.patterns.is_empty() {
                return Err(format!(
                    "lens {:?} needs at least one match pattern",
                    lens.id
                ));
            }
            lenses.push(LensDecl {
                id: lens.id,
                patterns: lens.patterns,
            });
        }

        Ok(Manifest {
            id: raw.id,
            name: raw.name,
            version: raw.version,
            description: raw.description,
            icon,
            host: raw.host,
            window: raw.window,
            styles: raw.styles,
            blocks,
            lenses,
        })
    }

    /// Applies the package rules against the package directory `dir`: every entry exists, and
    /// the styles are readable and at most 256 KiB once joined. Returns the joined styles.
    pub fn check_package(&self, dir: &Path) -> Result<String, String> {
        for entry in [&self.host, &self.window].into_iter().flatten() {
            if !dir.join(entry).is_file() {
                return Err(format!("missing entry {entry}"));
            }
        }
        let mut css = String::new();
        for (i, style) in self.styles.iter().enumerate() {
            let text = std::fs::read_to_string(dir.join(style))
                .map_err(|e| format!("cannot read style {style}: {e}"))?;
            if i > 0 {
                css.push('\n');
            }
            css.push_str(&text);
            if css.len() > MAX_STYLES_BYTES {
                return Err("styles are larger than 256 KiB".into());
            }
        }
        Ok(css)
    }

    /// The declared block, if any.
    pub fn block(&self, id: &str) -> Option<&BlockDecl> {
        self.blocks.iter().find(|b| b.id == id)
    }

    /// The declared lens, if any.
    pub fn lens(&self, id: &str) -> Option<&LensDecl> {
        self.lenses.iter().find(|l| l.id == id)
    }
}

/// Reads and validates the package at `dir` (text rules, then package rules), as `install` and
/// `link` do. Returns the manifest and the joined styles.
pub fn load_package(dir: &Path) -> Result<(Manifest, String), String> {
    let path = dir.join("plugin.toml");
    let meta = match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) => return Err("no plugin.toml".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err("no plugin.toml".into()),
        Err(e) => return Err(format!("cannot read plugin.toml: {e}")),
    };
    if meta.len() > MAX_MANIFEST_BYTES as u64 {
        return Err("plugin.toml is larger than 64 KiB".into());
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("cannot read plugin.toml: {e}"))?;
    let manifest = Manifest::parse(&text)?;
    let css = manifest.check_package(dir)?;
    Ok((manifest, css))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str =
        "schema = 1\nid = \"k8s\"\nname = \"Kubernetes\"\nversion = \"1\"\nhost = \"host.luau\"\n";

    fn err(text: &str) -> String {
        Manifest::parse(text).expect_err("manifest should be rejected")
    }

    #[test]
    fn parses_every_sdk_example_manifest() {
        let examples = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../third_party/tern-sdk/plugins/examples");
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(&examples).expect("examples dir") {
            let dir = entry.expect("entry").path();
            let (manifest, _css) =
                load_package(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
            ids.push(manifest.id);
        }
        ids.sort();
        assert_eq!(
            ids,
            [
                "canvas-demo",
                "dirvars",
                "jsonx",
                "longrun",
                "review-queue",
                "terraform",
                "workspaces"
            ]
        );
    }

    #[test]
    fn terraform_example_fields() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../third_party/tern-sdk/plugins/examples/terraform");
        let (m, css) = load_package(&dir).expect("terraform");
        assert_eq!(m.icon, "stack");
        assert_eq!(m.lenses.len(), 1);
        assert_eq!(m.lenses[0].id, "plan");
        assert_eq!(m.lenses[0].patterns.len(), 8);
        assert_eq!(m.lenses[0].patterns[0], "terraform plan");
        assert!(!css.is_empty());
    }

    #[test]
    fn schema_is_checked_first() {
        assert_eq!(err("id = \"K8s\"\nbogus = 1\n"), "missing schema");
        assert_eq!(err("schema = 2\nbogus = 1\n"), "unsupported schema 2");
        assert_eq!(err("schema = \"1\"\n"), "schema must be an integer");
    }

    #[test]
    fn toml_errors_carry_parser_text_without_location() {
        let e = err("schema = 1\nid = k8s\n");
        assert!(!e.contains("line"), "{e}");
        assert!(e.contains("string values must be quoted"), "{e}");
    }

    #[test]
    fn unknown_keys_are_errors_everywhere() {
        assert_eq!(
            err(&format!("{BASE}color = \"red\"\n")),
            "unknown field `color`, expected one of `schema`, `id`, `name`, `version`, `description`, `icon`, `host`, `window`, `styles`, `blocks`, `lenses`"
        );
        assert_eq!(
            err(&format!(
                "{BASE}[[blocks]]\nid = \"pods\"\ntitle = \"Pods\"\nshape = 1\n"
            )),
            "unknown field `shape`, expected one of `id`, `title`, `icon`, `files`, `palette`"
        );
        assert_eq!(
            err(&format!(
                "{BASE}[[lenses]]\nid = \"get\"\nmatch = [\"x\"]\nx = 1\n"
            )),
            "unknown field `x`, expected `id` or `match`"
        );
    }

    #[test]
    fn type_errors() {
        assert_eq!(
            err("schema = 1\nid = 5\nname = \"n\"\nversion = \"1\"\nhost = \"h.luau\"\n"),
            "invalid type: integer `5`, expected a string"
        );
        assert_eq!(
            err(&format!(
                "{BASE}[[blocks]]\nid = \"a\"\ntitle = \"A\"\npalette = \"no\"\n"
            )),
            "invalid type: string \"no\", expected a boolean"
        );
        assert_eq!(
            err("schema = 1\nid = \"k\"\nname = \"n\"\nhost = \"h.luau\"\n"),
            "missing field `version`"
        );
    }

    #[test]
    fn id_and_name_rules() {
        assert_eq!(
            err(&BASE.replace("\"k8s\"", "\"K8s\"")),
            "invalid id \"K8s\" (lowercase letters, digits and -, 1-32 long)"
        );
        assert_eq!(
            err(&BASE.replace("\"k8s\"", "\"\"")),
            "invalid id \"\" (lowercase letters, digits and -, 1-32 long)"
        );
        let long = format!("\"a{}\"", "b".repeat(32));
        assert!(err(&BASE.replace("\"k8s\"", &long)).starts_with("invalid id"));
        let ok32 = format!("\"a{}\"", "-".repeat(31));
        assert!(Manifest::parse(&BASE.replace("\"k8s\"", &ok32)).is_ok());
        assert_eq!(
            err(&BASE.replace("\"Kubernetes\"", "\"\"")),
            "name must be 1-64 characters"
        );
        let name64 = format!("\"{}\"", "é".repeat(64));
        assert!(Manifest::parse(&BASE.replace("\"Kubernetes\"", &name64)).is_ok());
        let name65 = format!("\"{}\"", "é".repeat(65));
        assert_eq!(
            err(&BASE.replace("\"Kubernetes\"", &name65)),
            "name must be 1-64 characters"
        );
    }

    #[test]
    fn entry_rules() {
        let no_entry = "schema = 1\nid = \"k\"\nname = \"n\"\nversion = \"1\"\n";
        assert_eq!(err(no_entry), "needs a host or window entry");
        assert_eq!(
            err(&format!("{no_entry}host = \"../h.luau\"\n")),
            "host \"../h.luau\" must be a relative path inside the plugin"
        );
        assert_eq!(
            err(&format!("{no_entry}host = \"/abs/h.luau\"\n")),
            "host \"/abs/h.luau\" must be a relative path inside the plugin"
        );
        assert_eq!(
            err(&format!("{no_entry}window = \"w.js\"\n")),
            "window \"w.js\" must be a .luau or .lua file"
        );
        assert!(Manifest::parse(&format!("{no_entry}window = \"./src/w.lua\"\n")).is_ok());
        assert_eq!(
            err(&format!("{BASE}styles = [\"../x.css\"]\n")),
            "style \"../x.css\" must be a relative path inside the plugin"
        );
    }

    #[test]
    fn block_and_lens_rules() {
        let window_only =
            "schema = 1\nid = \"k\"\nname = \"n\"\nversion = \"1\"\nwindow = \"w.luau\"\n";
        assert_eq!(
            err(&format!(
                "{window_only}[[blocks]]\nid = \"a\"\ntitle = \"A\"\n"
            )),
            "blocks need a host entry"
        );
        assert_eq!(
            err(&format!(
                "{window_only}[[lenses]]\nid = \"a\"\nmatch = [\"x\"]\n"
            )),
            "lenses need a host entry"
        );
        assert_eq!(
            err(&format!("{BASE}[[blocks]]\nid = \"Pods\"\ntitle = \"P\"\n")),
            "invalid block id \"Pods\""
        );
        assert_eq!(
            err(&format!(
                "{BASE}[[blocks]]\nid = \"pods\"\ntitle = \"P\"\n[[blocks]]\nid = \"pods\"\ntitle = \"Q\"\n"
            )),
            "duplicate block id \"pods\""
        );
        assert_eq!(
            err(&format!("{BASE}[[blocks]]\nid = \"pods\"\n")),
            "missing field `title`"
        );
        assert_eq!(
            err(&format!(
                "{BASE}[[lenses]]\nid = \"Get\"\nmatch = [\"x\"]\n"
            )),
            "invalid lens id \"Get\""
        );
        assert_eq!(
            err(&format!(
                "{BASE}[[lenses]]\nid = \"get\"\nmatch = [\"x\"]\n[[lenses]]\nid = \"get\"\nmatch = [\"y\"]\n"
            )),
            "duplicate lens id \"get\""
        );
        assert_eq!(
            err(&format!("{BASE}[[lenses]]\nid = \"get\"\nmatch = []\n")),
            "lens \"get\" needs at least one match pattern"
        );
        assert_eq!(
            err(&format!("{BASE}[[lenses]]\nid = \"get\"\n")),
            "missing field `match`"
        );
    }

    #[test]
    fn icons_and_defaults() {
        let m = Manifest::parse(&format!(
            "{BASE}icon = \"icon.folder\"\n[[blocks]]\nid = \"a\"\ntitle = \"A\"\n[[blocks]]\nid = \"b\"\ntitle = \"B\"\nicon = \"nope\"\npalette = false\nfiles = [\"*.json\"]\n"
        ))
        .expect("valid");
        assert_eq!(m.icon, "folder");
        assert_eq!(m.description, "");
        assert_eq!(m.blocks[0].icon, "folder");
        assert!(m.blocks[0].palette);
        assert_eq!(m.blocks[1].icon, "puzzle");
        assert!(!m.blocks[1].palette);
        assert_eq!(m.blocks[1].files, vec!["*.json".to_owned()]);
        assert_eq!(Manifest::parse(BASE).expect("valid").icon, "puzzle");
        assert_eq!(
            Manifest::parse(&format!("{BASE}icon = \"git-branch\"\n"))
                .expect("valid")
                .icon,
            "branch"
        );
    }

    #[test]
    fn size_cap() {
        let big = format!(
            "{BASE}description = \"{}\"\n",
            "x".repeat(MAX_MANIFEST_BYTES)
        );
        assert_eq!(err(&big), "plugin.toml is larger than 64 KiB");
    }

    #[test]
    fn package_rules() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m =
            Manifest::parse(&format!("{BASE}styles = [\"a.css\", \"b.css\"]\n")).expect("valid");
        assert_eq!(
            m.check_package(dir.path()),
            Err("missing entry host.luau".into())
        );
        std::fs::write(dir.path().join("host.luau"), "").expect("write");
        let e = m.check_package(dir.path()).expect_err("missing style");
        assert!(e.starts_with("cannot read style a.css: "), "{e}");
        std::fs::write(dir.path().join("a.css"), "a{}").expect("write");
        std::fs::write(dir.path().join("b.css"), "b{}").expect("write");
        assert_eq!(m.check_package(dir.path()), Ok("a{}\nb{}".into()));
        std::fs::write(dir.path().join("b.css"), "x".repeat(MAX_STYLES_BYTES)).expect("write");
        assert_eq!(
            m.check_package(dir.path()),
            Err("styles are larger than 256 KiB".into())
        );
    }
}
