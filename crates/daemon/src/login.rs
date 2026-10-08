//! The fresh login environment panes start from (§5.2): HOME, USER, SHELL and the PATH the login
//! shell builds, not the daemon's environment. Tern gets it by probing `$SHELL` [O]; so do we,
//! once, when the daemon starts (the daemon keeps the environment it started with, §4.1).

use std::collections::BTreeMap;
use std::ffi::CStr;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

/// The PATH used when the login shell can't be probed.
pub const FALLBACK_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// How long the PATH probe may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The user's passwd entry.
#[derive(Clone, Debug)]
struct Passwd {
    name: String,
    home: PathBuf,
    shell: PathBuf,
}

fn passwd() -> Option<Passwd> {
    let uid = rustix::process::getuid().as_raw();
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: getpwuid_r writes into `pwd` and `buf`, which outlive the call, and sets `result`
    // to `&pwd` on success. The strings it points at live in `buf` and are copied out below.
    let rc = unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) };
    if rc != 0 || result.is_null() {
        return None;
    }
    // SAFETY: on success the fields are NUL-terminated strings inside `buf`.
    let field =
        |p: *const libc::c_char| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    Some(Passwd {
        name: field(pwd.pw_name),
        home: field(pwd.pw_dir).into(),
        shell: field(pwd.pw_shell).into(),
    })
}

pub(crate) fn passwd_home() -> Option<PathBuf> {
    passwd().map(|p| p.home)
}

/// The login environment and identity panes start with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginEnv {
    /// HOME, USER, LOGNAME, SHELL, PATH, plus LANG and TMPDIR.
    pub vars: BTreeMap<String, String>,
    pub shell: PathBuf,
    pub home: PathBuf,
    /// `user@host`, for `AUK_IDENTITY`.
    pub identity: String,
    /// The user's own ZDOTDIR, when the daemon was started with one. Shells source their zsh
    /// files from here (§5.3); tests point it at a test directory.
    pub user_zdotdir: Option<PathBuf>,
}

impl LoginEnv {
    /// Builds the environment: identity from the daemon's own HOME, USER and SHELL (falling back
    /// to the passwd entry), then the PATH a login shell builds.
    pub async fn probe() -> LoginEnv {
        let pw = passwd();
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let user = var("USER")
            .or_else(|| pw.as_ref().map(|p| p.name.clone()))
            .unwrap_or_else(|| rustix::process::getuid().as_raw().to_string());
        let home: PathBuf = var("HOME")
            .map(PathBuf::from)
            .or_else(|| pw.as_ref().map(|p| p.home.clone()))
            .unwrap_or_else(|| "/".into());
        let shell: PathBuf = var("SHELL")
            .map(PathBuf::from)
            .or_else(|| pw.as_ref().map(|p| p.shell.clone()))
            .unwrap_or_else(|| "/bin/zsh".into());
        let user_zdotdir = var("ZDOTDIR").map(PathBuf::from);

        let mut vars = BTreeMap::new();
        vars.insert("HOME".to_owned(), home.to_string_lossy().into_owned());
        vars.insert("USER".to_owned(), user.clone());
        vars.insert("LOGNAME".to_owned(), user.clone());
        vars.insert("SHELL".to_owned(), shell.to_string_lossy().into_owned());
        vars.insert(
            "LANG".to_owned(),
            var("LANG").unwrap_or_else(|| "en_US.UTF-8".to_owned()),
        );
        if let Some(tmp) = var("TMPDIR") {
            vars.insert("TMPDIR".to_owned(), tmp);
        }
        let path = probe_path(&shell, &vars, user_zdotdir.as_ref())
            .await
            .unwrap_or_else(|| FALLBACK_PATH.to_owned());
        vars.insert("PATH".to_owned(), path);

        let host = rustix::system::uname()
            .nodename()
            .to_string_lossy()
            .into_owned();
        let host = host.split('.').next().unwrap_or(&host).to_owned();
        LoginEnv {
            vars,
            shell,
            home,
            identity: format!("{user}@{host}"),
            user_zdotdir,
        }
    }
}

/// Runs the login shell non-interactively in a clean environment and reads the PATH it built.
async fn probe_path(
    shell: &PathBuf,
    vars: &BTreeMap<String, String>,
    zdotdir: Option<&PathBuf>,
) -> Option<String> {
    const MARK: &str = "\u{1}AUK_PATH\u{1}";
    let mut cmd = Command::new(shell);
    cmd.arg("-l")
        .arg("-c")
        .arg(format!("printf '{MARK}%s{MARK}' \"$PATH\""))
        .env_clear()
        .envs(vars)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(dir) = zdotdir {
        cmd.env("ZDOTDIR", dir);
    }
    let output = tokio::time::timeout(PROBE_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let path = text.split(MARK).nth(1)?;
    (!path.is_empty()).then(|| path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probes_a_login_path() {
        let env = LoginEnv::probe().await;
        let path = &env.vars["PATH"];
        assert!(path.split(':').any(|p| p == "/usr/bin"), "{path}");
        assert!(env.identity.contains('@'));
        for key in ["HOME", "USER", "LOGNAME", "SHELL", "LANG"] {
            assert!(env.vars.contains_key(key), "{key}");
        }
    }

    #[test]
    fn passwd_has_a_home() {
        assert!(passwd_home().is_some_and(|h| h.is_absolute()));
    }
}
