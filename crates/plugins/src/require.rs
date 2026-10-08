//! `require` confined to the plugin package (§9.2, research/plugins.md §2.6).
//!
//! Luau require-by-string, relative to the calling file: `./x` and `../x` resolve to `x.luau`,
//! `x.lua`, `x/init.luau` or `x/init.lua`. More than one match is an error, leaving the package
//! (lexically or through a symlink) is "not found", and `.luaurc` and `@alias` paths are not
//! read. `require("tern")` and `require("auk")` are answered before this resolver runs (see
//! `vm.rs`). Modules are cached per resolved file by mlua's loader cache, so each runs once per VM.

use std::io;
use std::path::{Component, Path, PathBuf};

use mlua::luau::{NavigateError, Require};
use mlua::{Function, Lua};

/// The chunk-name prefix of files loaded by path.
const CHUNK_PREFIX: char = '@';

/// Chunk name of a plugin file: `@` + its absolute path.
pub(crate) fn chunk_name(path: &Path) -> String {
    format!("{CHUNK_PREFIX}{}", path.display())
}

/// Navigates within one package directory.
#[derive(Debug)]
pub(crate) struct PackageRequirer {
    /// The canonical package directory.
    root: PathBuf,
    /// The current logical module path (not necessarily a file).
    path: PathBuf,
    /// The file the current path resolves to, when it is a module.
    resolved: Option<PathBuf>,
}

impl PackageRequirer {
    pub(crate) fn new(root: &Path) -> io::Result<Self> {
        let root = root.canonicalize()?;
        Ok(PackageRequirer {
            path: root.clone(),
            root,
            resolved: None,
        })
    }

    /// Lexically normalizes an absolute path (`.` dropped, `..` popped).
    fn normalize(path: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for component in path.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other),
            }
        }
        out
    }

    fn inside(&self, path: &Path) -> bool {
        path.starts_with(&self.root)
    }

    /// Resolves a logical module path to a file inside the package. `Ok(None)` is a directory
    /// without an init file, which is a valid intermediate step.
    fn resolve(&self, path: &Path) -> Result<Option<PathBuf>, NavigateError> {
        if !self.inside(path) {
            return Err(NavigateError::NotFound);
        }
        let mut found: Option<PathBuf> = None;
        let mut consider = |candidate: PathBuf| -> Result<(), NavigateError> {
            if candidate.is_file() && found.replace(candidate).is_some() {
                return Err(NavigateError::Ambiguous);
            }
            Ok(())
        };
        // The package root is only ever a directory; `<root>.luau` would sit outside it.
        if path != self.root
            && let Some(name) = path.file_name()
        {
            for ext in ["luau", "lua"] {
                let mut file = name.to_os_string();
                file.push(".");
                file.push(ext);
                consider(path.with_file_name(file))?;
            }
        }
        if path.is_dir() {
            for init in ["init.luau", "init.lua"] {
                consider(path.join(init))?;
            }
            if found.is_none() {
                return Ok(None);
            }
        }
        let file = found.ok_or(NavigateError::NotFound)?;
        // A symlink may point outside the package; that is "not found" too.
        match file.canonicalize() {
            Ok(real) if real.starts_with(&self.root) => Ok(Some(file)),
            _ => Err(NavigateError::NotFound),
        }
    }

    fn go(&mut self, path: PathBuf) -> Result<(), NavigateError> {
        let resolved = self.resolve(&path)?;
        self.path = path;
        self.resolved = resolved;
        Ok(())
    }
}

impl Require for PackageRequirer {
    fn is_require_allowed(&self, chunk_name: &str) -> bool {
        chunk_name
            .strip_prefix(CHUNK_PREFIX)
            .is_some_and(|p| self.inside(Path::new(p)))
    }

    fn reset(&mut self, chunk_name: &str) -> Result<(), NavigateError> {
        let Some(file) = chunk_name.strip_prefix(CHUNK_PREFIX) else {
            return Err(NavigateError::NotFound);
        };
        let file = Self::normalize(Path::new(file));
        if !self.inside(&file) || !file.is_file() {
            return Err(NavigateError::NotFound);
        }
        // The requirer is a file: it is the current module, and `./` navigates from its parent.
        self.resolved = Some(file.clone());
        self.path = file.with_extension("");
        Ok(())
    }

    fn jump_to_alias(&mut self, _path: &str) -> Result<(), NavigateError> {
        Err(NavigateError::NotFound)
    }

    fn to_parent(&mut self) -> Result<(), NavigateError> {
        let mut parent = self.path.clone();
        if !parent.pop() || !self.inside(&parent) {
            return Err(NavigateError::NotFound);
        }
        self.go(parent)
    }

    fn to_child(&mut self, name: &str) -> Result<(), NavigateError> {
        if name.contains('/') || name == ".." || name == "." {
            return Err(NavigateError::NotFound);
        }
        self.go(self.path.join(name))
    }

    fn has_module(&self) -> bool {
        self.resolved.as_deref().is_some_and(Path::is_file)
    }

    fn cache_key(&self) -> String {
        self.resolved
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    }

    fn has_config(&self) -> bool {
        false
    }

    fn config(&self) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            ".luaurc is not read",
        ))
    }

    fn loader(&self, lua: &Lua) -> mlua::Result<Function> {
        let file = self
            .resolved
            .as_deref()
            .ok_or_else(|| mlua::Error::runtime("no module"))?;
        let source = std::fs::read(file)
            .map_err(|e| mlua::Error::runtime(format!("cannot read {}: {e}", file.display())))?;
        lua.load(source).set_name(chunk_name(file)).into_function()
    }
}
