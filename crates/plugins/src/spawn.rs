//! The spawn filter's data: the spec a filter sees and how its answer is applied (§9.7).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A process pane about to start. `env` holds only the pane-identity set Auk adds, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSpec {
    /// The resolved program.
    pub program: String,
    /// Its arguments, including the login flag the daemon adds.
    pub args: Vec<String>,
    /// The start directory.
    pub cwd: String,
    /// The pane-identity variables, in order.
    pub env: Vec<(String, String)>,
}

/// A filter's answer, converted from the returned table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnPatch {
    /// Replaces the program when present.
    pub program: Option<String>,
    /// Replaces the arguments when present.
    pub args: Option<Vec<String>>,
    /// Replaces the directory when it names one (a leading `~` is expanded).
    pub cwd: Option<String>,
    /// The complete variable set; absent removes them all.
    pub env: Option<BTreeMap<String, String>>,
}

fn expand_tilde(path: &str) -> PathBuf {
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    if path == "~" {
        if let Some(home) = home() {
            return home;
        }
    } else if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = home()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

impl SpawnSpec {
    /// Applies a filter's answer:
    /// - `program` and `args` are replaced when present;
    /// - `cwd` is replaced when it names an existing directory;
    /// - `env` is the complete set: missing names are removed, existing names keep their
    ///   position, and new names follow, sorted. An absent `env` removes them all.
    pub fn apply(&mut self, patch: SpawnPatch) {
        if let Some(program) = patch.program {
            self.program = program;
        }
        if let Some(args) = patch.args {
            self.args = args;
        }
        if let Some(cwd) = patch.cwd {
            let dir = expand_tilde(&cwd);
            if Path::new(&dir).is_dir() {
                self.cwd = dir.to_string_lossy().into_owned();
            }
        }
        let mut wanted = patch.env.unwrap_or_default();
        let mut env: Vec<(String, String)> = Vec::with_capacity(wanted.len());
        for (name, _) in &self.env {
            if let Some(value) = wanted.remove(name) {
                env.push((name.clone(), value));
            }
        }
        env.extend(wanted);
        self.env = env;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> SpawnSpec {
        SpawnSpec {
            program: "/bin/zsh".into(),
            args: vec!["-l".into()],
            cwd: "/".into(),
            env: vec![
                ("AUK_PANE".into(), "1".into()),
                ("AUK_IDENTITY".into(), "x".into()),
            ],
        }
    }

    #[test]
    fn replaces_present_fields_only() {
        let mut s = spec();
        let env: BTreeMap<_, _> = s.env.iter().cloned().collect();
        s.apply(SpawnPatch {
            program: Some("/bin/bash".into()),
            env: Some(env),
            ..Default::default()
        });
        assert_eq!(s.program, "/bin/bash");
        assert_eq!(s.args, ["-l"]);
        assert_eq!(s.env, spec().env, "existing names keep their position");
    }

    #[test]
    fn env_is_the_complete_set() {
        let mut s = spec();
        let env = BTreeMap::from([
            ("ZED".into(), "z".into()),
            ("AUK_IDENTITY".into(), "y".into()),
            ("ABC".into(), "a".into()),
        ]);
        s.apply(SpawnPatch {
            env: Some(env),
            ..Default::default()
        });
        let names: Vec<_> = s.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        assert_eq!(names, ["AUK_IDENTITY=y", "ABC=a", "ZED=z"]);
        s.apply(SpawnPatch::default());
        assert!(s.env.is_empty(), "absent env removes them all");
    }

    #[test]
    fn cwd_must_name_a_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut s = spec();
        s.apply(SpawnPatch {
            cwd: Some("/does/not/exist".into()),
            ..Default::default()
        });
        assert_eq!(s.cwd, "/");
        s.apply(SpawnPatch {
            cwd: Some(dir.path().display().to_string()),
            ..Default::default()
        });
        assert_eq!(s.cwd, dir.path().display().to_string());
        if let Some(home) = std::env::var_os("HOME") {
            s.apply(SpawnPatch {
                cwd: Some("~".into()),
                ..Default::default()
            });
            assert_eq!(s.cwd, PathBuf::from(home).to_string_lossy());
        }
    }
}
