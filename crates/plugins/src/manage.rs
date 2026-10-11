//! The backing functions of `auk plugin …`. None of them runs
//! Lua or starts a daemon; the CLI asks the daemon to reload after a change.
//!
//! ```text
//! auk plugin list|reload|dir|install SRC [--force]|remove ID|link DIR|unlink ID|types DIR [--json] [--window KEY]
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value as Json, json};

use crate::catalog::{Catalog, Status};
use crate::discovery::{self, PluginSettings};
use crate::manifest;

/// `tern.d.luau` at the SDK pin, which `types` writes.
pub const TYPES: &str = include_str!("../../../third_party/tern-sdk/plugins/tern.d.luau");

/// The verbs' usage line.
pub const USAGE: &str = "usage: auk plugin list|reload|dir|install SRC [--force]|remove ID|link DIR|unlink ID|types DIR [--json] [--window KEY]";

/// A parsed `auk plugin` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verb {
    /// `list`.
    List,
    /// `reload`.
    Reload,
    /// `dir`.
    Dir,
    /// `install SRC [--force]`.
    Install {
        /// A git URL, `github.com/OWNER/REPO[/PATH]`, or a directory.
        src: String,
        /// Replace an installed copy.
        force: bool,
    },
    /// `remove ID`.
    Remove(String),
    /// `link DIR`.
    Link(String),
    /// `unlink ID`.
    Unlink(String),
    /// `types DIR`.
    Types(String),
    /// `--help`.
    Help,
}

/// A parsed invocation with its common flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// The verb.
    pub verb: Verb,
    /// `--json`.
    pub json: bool,
    /// `--window KEY`.
    pub window: Option<String>,
}

/// Parses the words after `plugin`. A usage error carries its message (exit 2).
pub fn parse_args(args: &[String]) -> Result<Invocation, String> {
    let mut json = false;
    let mut force = false;
    let mut window = None;
    let mut words: Vec<&str> = Vec::new();
    let mut help = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--json" => json = true,
            "--force" => force = true,
            "--help" | "-h" => help = true,
            "--window" => {
                i += 1;
                window = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| "--window takes a value".to_owned())?,
                );
            }
            _ if a.starts_with("--window=") => window = Some(a["--window=".len()..].to_owned()),
            _ if a.starts_with('-') && a.len() > 1 => {
                return Err(format!("unknown flag {a}"));
            }
            _ => words.push(a),
        }
        i += 1;
    }
    if help {
        return Ok(Invocation {
            verb: Verb::Help,
            json,
            window,
        });
    }
    let other = || "plugin takes other arguments".to_owned();
    let verb = match words.as_slice() {
        ["list"] => Verb::List,
        ["reload"] => Verb::Reload,
        ["dir"] => Verb::Dir,
        ["install", src] => Verb::Install {
            src: (*src).to_owned(),
            force,
        },
        ["remove", id] => Verb::Remove((*id).to_owned()),
        ["link", dir] => Verb::Link((*dir).to_owned()),
        ["unlink", id] => Verb::Unlink((*id).to_owned()),
        ["types", dir] => Verb::Types((*dir).to_owned()),
        _ => return Err(other()),
    };
    if force && !matches!(verb, Verb::Install { .. }) {
        return Err("unknown flag --force".into());
    }
    Ok(Invocation { verb, json, window })
}

/// The catalog without a daemon: the folder read without Lua, every allowed valid package Ready.
pub fn offline_catalog(plugins_dir: &Path, settings: &PluginSettings) -> Catalog {
    let scan = discovery::scan(plugins_dir);
    Catalog::build(&scan, settings, false, |_| Status::Ready)
}

/// `list` (and `reload`) in text form: `ID VERSION NAME — N blocks, M lenses[, window]  STATUS`
/// per plugin, then `problem DIR: ERROR` lines; `no plugins` when there are none.
pub fn format_list(catalog: &Catalog) -> String {
    let mut out = String::new();
    for p in &catalog.plugins {
        let plural =
            |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let status = match &p.status {
            Status::Ready => "ready".to_owned(),
            Status::Disabled => "disabled".to_owned(),
            Status::Failed(e) => format!("failed: {}", e.lines().next().unwrap_or_default()),
        };
        out.push_str(&format!(
            "{} {} {} — {}, {}{}  {}\n",
            p.id,
            p.version,
            p.name,
            plural(p.blocks.len(), "block", "blocks"),
            plural(p.lenses.len(), "lens", "lenses"),
            if p.window { ", window" } else { "" },
            status
        ));
    }
    for problem in &catalog.problems {
        out.push_str(&format!("problem {}: {}\n", problem.dir, problem.error));
    }
    if out.is_empty() {
        out.push_str("no plugins\n");
    }
    out
}

/// Whether a catalog makes `reload` exit 1 (a Failed plugin or a problem).
pub fn has_trouble(catalog: &Catalog) -> bool {
    !catalog.problems.is_empty()
        || catalog
            .plugins
            .iter()
            .any(|p| matches!(p.status, Status::Failed(_)))
}

/// What `install` and `link` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// Plugin id.
    pub id: String,
    /// Version as written.
    pub version: String,
    /// Where it is now.
    pub dir: PathBuf,
}

impl Installed {
    /// `--json` form.
    pub fn to_json(&self) -> Json {
        json!({"id": self.id, "version": self.version, "dir": self.dir.display().to_string()})
    }
}

/// A folder entry holding plugin `id`: `(entry path, linked?)`.
fn find_entry(plugins_dir: &Path, id: &str) -> Option<(PathBuf, bool)> {
    let entries = std::fs::read_dir(plugins_dir).ok()?;
    let mut names: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    names.sort();
    for entry in names {
        let name = entry.file_name()?.to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if entry.extension().is_some_and(|e| e == "path") && entry.is_file() {
            let Ok(text) = std::fs::read_to_string(&entry) else {
                continue;
            };
            if let Ok((m, _)) = manifest::load_package(Path::new(text.trim()))
                && m.id == id
            {
                return Some((entry, true));
            }
            if entry.file_stem().is_some_and(|s| s == id) {
                return Some((entry, true));
            }
        } else if entry.is_dir()
            && let Ok((m, _)) = manifest::load_package(&entry)
            && m.id == id
        {
            return Some((entry, false));
        }
    }
    None
}

fn valid_id_arg(id: &str) -> Result<(), String> {
    if id.is_empty() || id.starts_with('.') || id.contains('/') || id.contains('\\') {
        return Err(format!("no plugin id {id}"));
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let kind = entry.file_type()?;
        let target = to.join(&name);
        if kind.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if kind.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(entry.path())?, &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Where `src` points: a local directory, or a clone (into `scratch`) and the path inside it.
fn fetch_source(src: &str, scratch: &Path) -> Result<PathBuf, String> {
    let (url, sub) = if let Some(rest) = src.strip_prefix("github.com/").or_else(|| {
        src.strip_prefix("https://github.com/")
            .filter(|r| !r.ends_with(".git"))
    }) {
        let parts: Vec<&str> = rest.splitn(3, '/').collect();
        if parts.len() < 2 {
            return Err(format!("no directory {src}: not OWNER/REPO"));
        }
        (
            format!("https://github.com/{}/{}", parts[0], parts[1]),
            parts.get(2).map(|p| p.trim_end_matches('/').to_owned()),
        )
    } else if src.contains("://") || src.starts_with("git@") || src.ends_with(".git") {
        (src.to_owned(), None)
    } else {
        let dir = PathBuf::from(src);
        let meta = std::fs::metadata(&dir).map_err(|e| format!("no directory {src}: {e}"))?;
        if !meta.is_dir() {
            return Err(format!("{src} is not a directory"));
        }
        return Ok(dir);
    };
    let clone = scratch.join("clone");
    let output = Command::new("git")
        .args(["clone", "--depth", "1", "--quiet", &url])
        .arg(&clone)
        .output()
        .map_err(|_| "installing from git needs git".to_owned())?;
    if !output.status.success() {
        return Err(format!(
            "git clone {url} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    match sub {
        None => Ok(clone),
        Some(path) => {
            let dir = clone.join(&path);
            match std::fs::metadata(&dir) {
                Err(_) => Err(format!("no directory {path} in {url}")),
                Ok(m) if !m.is_dir() => Err(format!("{path} is not a directory in {url}")),
                Ok(_) => Ok(dir),
            }
        }
    }
}

/// `install SRC [--force]`: copies the package, staged in a dot-named directory, then renamed
/// into `<plugins>/<id>`.
pub fn install(plugins_dir: &Path, src: &str, force: bool) -> Result<Installed, String> {
    std::fs::create_dir_all(plugins_dir)
        .map_err(|e| format!("cannot create {}: {e}", plugins_dir.display()))?;
    let scratch = plugins_dir.join(format!(".install-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).map_err(|e| format!("cannot stage: {e}"))?;
    let result = install_staged(plugins_dir, src, force, &scratch);
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

fn install_staged(
    plugins_dir: &Path,
    src: &str,
    force: bool,
    scratch: &Path,
) -> Result<Installed, String> {
    let source = fetch_source(src, scratch)?;
    let (m, _) = manifest::load_package(&source)
        .map_err(|why| format!("{} is not a plugin: {why}", source.display()))?;
    if let Some((entry, linked)) = find_entry(plugins_dir, &m.id) {
        if linked {
            return Err(format!(
                "{} is linked ({}); use unlink first",
                m.id,
                entry.display()
            ));
        }
        if !force {
            return Err(format!(
                "{} is already installed ({}); --force replaces it",
                m.id,
                entry.display()
            ));
        }
    }
    let staged = scratch.join("package");
    copy_dir(&source, &staged).map_err(|e| format!("cannot copy {}: {e}", source.display()))?;
    let target = plugins_dir.join(&m.id);
    if force {
        if let Some((entry, false)) = find_entry(plugins_dir, &m.id) {
            std::fs::remove_dir_all(&entry)
                .map_err(|e| format!("cannot replace {}: {e}", entry.display()))?;
        }
        if target.exists() {
            std::fs::remove_dir_all(&target)
                .map_err(|e| format!("cannot replace {}: {e}", target.display()))?;
        }
    } else if target.exists() {
        return Err(format!(
            "{} is already installed ({}); --force replaces it",
            m.id,
            target.display()
        ));
    }
    std::fs::rename(&staged, &target)
        .map_err(|e| format!("cannot install into {}: {e}", target.display()))?;
    Ok(Installed {
        id: m.id,
        version: m.version,
        dir: target,
    })
}

/// `remove ID`: deletes an installed (not linked) package. Its data folder is kept.
pub fn remove(plugins_dir: &Path, id: &str) -> Result<PathBuf, String> {
    valid_id_arg(id)?;
    match find_entry(plugins_dir, id) {
        None => Err(format!("no plugin {id} in {}", plugins_dir.display())),
        Some((_, true)) => Err(format!("{id} is linked; use unlink")),
        Some((dir, false)) => {
            std::fs::remove_dir_all(&dir)
                .map_err(|e| format!("cannot remove {}: {e}", dir.display()))?;
            Ok(dir)
        }
    }
}

/// `link DIR`: writes `<plugins>/<id>.path` holding the absolute directory.
pub fn link(plugins_dir: &Path, dir: &str) -> Result<Installed, String> {
    let abs = std::fs::canonicalize(dir).map_err(|e| format!("no directory {dir}: {e}"))?;
    if !abs.is_dir() {
        return Err(format!("{dir} is not a directory"));
    }
    let text = abs
        .to_str()
        .ok_or_else(|| format!("{} is not UTF-8", abs.display()))?
        .to_owned();
    let (m, _) = manifest::load_package(&abs)
        .map_err(|why| format!("{} is not a plugin: {why}", abs.display()))?;
    if let Some((entry, _)) = find_entry(plugins_dir, &m.id) {
        return Err(format!(
            "{} is already installed ({})",
            m.id,
            entry.display()
        ));
    }
    std::fs::create_dir_all(plugins_dir)
        .map_err(|e| format!("cannot create {}: {e}", plugins_dir.display()))?;
    let file = plugins_dir.join(format!("{}.path", m.id));
    std::fs::write(&file, format!("{text}\n"))
        .map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    Ok(Installed {
        id: m.id,
        version: m.version,
        dir: abs,
    })
}

/// `unlink ID`: removes the link file.
pub fn unlink(plugins_dir: &Path, id: &str) -> Result<PathBuf, String> {
    valid_id_arg(id)?;
    match find_entry(plugins_dir, id) {
        None => Err(format!("no plugin {id} in {}", plugins_dir.display())),
        Some((_, false)) => Err(format!("{id} is installed, not linked; use remove")),
        Some((file, true)) => {
            std::fs::remove_file(&file)
                .map_err(|e| format!("cannot remove {}: {e}", file.display()))?;
            Ok(file)
        }
    }
}

/// `types DIR`: writes `tern.d.luau` into DIR (created; an existing file is overwritten).
pub fn types(dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let file = dir.join("tern.d.luau");
    std::fs::write(&file, TYPES).map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(dir: &Path, id: &str) {
        std::fs::create_dir_all(dir).expect("mkdir");
        std::fs::write(
            dir.join("plugin.toml"),
            format!("schema = 1\nid = \"{id}\"\nname = \"{id}\"\nversion = \"2.0\"\nhost = \"host.luau\"\n[[blocks]]\nid = \"b\"\ntitle = \"B\"\n"),
        )
        .expect("write");
        std::fs::write(dir.join("host.luau"), "").expect("write");
    }

    #[test]
    fn args() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            parse_args(&s(&["list", "--json"])).expect("ok").verb,
            Verb::List
        );
        assert!(parse_args(&s(&["--json", "list"])).expect("ok").json);
        assert_eq!(
            parse_args(&s(&["install", "x", "--force"]))
                .expect("ok")
                .verb,
            Verb::Install {
                src: "x".into(),
                force: true
            }
        );
        assert_eq!(
            parse_args(&s(&["list", "--window=k"]))
                .expect("ok")
                .window
                .as_deref(),
            Some("k")
        );
        assert_eq!(
            parse_args(&s(&["list", "--nope"])),
            Err("unknown flag --nope".into())
        );
        assert_eq!(
            parse_args(&s(&["frob"])),
            Err("plugin takes other arguments".into())
        );
        assert_eq!(
            parse_args(&s(&["list", "--window"])),
            Err("--window takes a value".into())
        );
        assert_eq!(parse_args(&s(&["-h"])).expect("ok").verb, Verb::Help);
    }

    #[test]
    fn install_remove_link_unlink_types() {
        let root = tempfile::tempdir().expect("tempdir");
        let plugins = root.path().join("plugins");
        let src = root.path().join("src-pkg");
        package(&src, "demo");
        let got = install(&plugins, src.to_str().expect("utf8"), false).expect("install");
        assert_eq!((got.id.as_str(), got.version.as_str()), ("demo", "2.0"));
        assert!(plugins.join("demo/plugin.toml").is_file());
        let again = install(&plugins, src.to_str().expect("utf8"), false).expect_err("dup");
        assert!(again.starts_with("demo is already installed ("), "{again}");
        assert!(again.ends_with("); --force replaces it"));
        install(&plugins, src.to_str().expect("utf8"), true).expect("force");
        let names: Vec<_> = std::fs::read_dir(&plugins)
            .expect("read")
            .map(|e| e.expect("e").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["demo"], "staging is cleaned up");
        assert_eq!(
            install(&plugins, "/does/not/exist", false).map(|_| ()),
            Err("no directory /does/not/exist: No such file or directory (os error 2)".into())
        );
        let not_plugin = root.path().join("empty");
        std::fs::create_dir_all(&not_plugin).expect("mkdir");
        assert!(
            install(&plugins, not_plugin.to_str().expect("utf8"), false)
                .expect_err("not a plugin")
                .ends_with("is not a plugin: no plugin.toml")
        );

        let catalog = offline_catalog(&plugins, &PluginSettings::default());
        assert_eq!(
            format_list(&catalog),
            "demo 2.0 demo — 1 block, 0 lenses  ready\n"
        );

        assert_eq!(
            remove(&plugins, "nope").map(|_| ()),
            Err(format!("no plugin nope in {}", plugins.display()))
        );
        assert_eq!(
            remove(&plugins, "../x").map(|_| ()),
            Err("no plugin id ../x".into())
        );
        remove(&plugins, "demo").expect("remove");
        assert_eq!(
            format_list(&offline_catalog(&plugins, &PluginSettings::default())),
            "no plugins\n"
        );

        let linked = link(&plugins, src.to_str().expect("utf8")).expect("link");
        assert_eq!(linked.id, "demo");
        assert!(plugins.join("demo.path").is_file());
        assert_eq!(
            remove(&plugins, "demo").map(|_| ()),
            Err("demo is linked; use unlink".into())
        );
        assert!(
            install(&plugins, src.to_str().expect("utf8"), true)
                .expect_err("linked")
                .contains("is linked")
        );
        unlink(&plugins, "demo").expect("unlink");
        assert!(!plugins.join("demo.path").exists());

        let file = types(&root.path().join("types")).expect("types");
        assert!(
            std::fs::read_to_string(file)
                .expect("read")
                .contains("declare tern")
        );
    }
}
