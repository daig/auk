//! The local end of an SSH link (§10.2): `ssh <host> auk attach --stdio` with the system `ssh`.
//!
//! Authentication, host keys and encryption are SSH's own: the agent, `~/.ssh/config` and
//! `known_hosts` (§12.5). The link adds a few options of its own:
//!
//! - `-T`: no remote terminal; the channel carries binary frames.
//! - `-o ControlMaster=no -o ControlPath=none`: a connection of its own, never a multiplexed
//!   session, so a reconnect really reconnects and `-v` reports how it authenticated.
//! - `-o ServerAliveInterval=5 -o ServerAliveCountMax=3`: a dead link ends `ssh`, which starts a
//!   reconnect.
//! - `-v`: the debug log names the key the server accepted and the method, for the identity
//!   chain. The log is read and dropped; its other lines explain a failure.

use std::collections::VecDeque;
use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::bridge::{BRIDGE_VERSION, BridgeInfo, PREAMBLE, PREAMBLE_ERROR};
use crate::identity::{Hop, SSH_KEY};

/// The remote command when none is given: `auk` on the remote `PATH`.
pub const DEFAULT_REMOTE_COMMAND: &str = "auk attach --stdio";

/// How long SSH may take to connect and run the bridge.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// How many lines a login script may print before the preamble.
const MAX_NOISE_LINES: usize = 200;

/// Where and how to reach a remote host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshTarget {
    /// The `ssh` destination: a host, `user@host`, or a `Host` alias from the config.
    pub destination: String,
    /// The `ssh` program.
    pub program: PathBuf,
    /// An SSH config file (`ssh -F`), instead of `~/.ssh/config`.
    pub config: Option<PathBuf>,
    /// More `ssh` arguments, before the destination.
    pub options: Vec<String>,
    /// The command the remote shell runs; it must run `auk attach --stdio`.
    pub remote_command: String,
}

impl SshTarget {
    pub fn new(destination: impl Into<String>) -> SshTarget {
        SshTarget {
            destination: destination.into(),
            program: PathBuf::from("ssh"),
            config: None,
            options: Vec::new(),
            remote_command: DEFAULT_REMOTE_COMMAND.to_owned(),
        }
    }

    /// The `ssh` command line.
    pub fn command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        if let Some(config) = &self.config {
            cmd.arg("-F").arg(config);
        }
        cmd.args(["-T", "-v"])
            .args(["-o", "ControlMaster=no", "-o", "ControlPath=none"])
            .args(["-o", "ServerAliveInterval=5", "-o", "ServerAliveCountMax=3"])
            .arg("-o")
            .arg(format!("ConnectTimeout={}", CONNECT_TIMEOUT.as_secs()))
            .args(&self.options)
            .arg("--")
            .arg(&self.destination)
            .arg(&self.remote_command);
        cmd
    }

    /// Runs `ssh` and reads the bridge's preamble.
    pub fn open(&self) -> Result<SshSession, OpenError> {
        let mut child = self
            .command()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| OpenError::Retry(format!("run {}: {err}", self.program.display())))?;
        let log = Arc::new(SshLog::default());
        {
            let stderr = child.stderr.take().expect("stderr is piped");
            let log = log.clone();
            thread::Builder::new()
                .name("auk ssh log".into())
                .spawn(move || log.read(stderr))
                .map_err(|err| OpenError::Retry(err.to_string()))?;
        }
        let stdin = child.stdin.take().expect("stdin is piped");
        let mut stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        match read_preamble(&mut stdout) {
            Ok(bridge) => {
                let auth = log.wait_auth(Duration::from_secs(2));
                Ok(SshSession {
                    child,
                    stdin,
                    stdout,
                    bridge,
                    auth,
                    log,
                })
            }
            Err(err) => {
                // Let ssh finish so its exit status and last words explain the failure.
                let status = wait_timeout(&mut child, Duration::from_secs(2));
                let _ = child.kill();
                let _ = child.wait();
                let detail = log.tail();
                let reason = match (&err, status) {
                    (PreambleError::Bridge(reason), _) => format!("the remote bridge: {reason}"),
                    (_, Some(255)) => format!("ssh couldn't connect: {detail}"),
                    (_, Some(127)) => format!(
                        "the remote command `{}` wasn't found: {detail}",
                        self.remote_command
                    ),
                    (err, status) => format!(
                        "{err} (ssh exit status {}): {detail}",
                        status.map_or("unknown".into(), |s| s.to_string())
                    ),
                };
                Err(match status {
                    // The connection failed: worth retrying while reconnecting.
                    Some(255) => OpenError::Retry(reason),
                    _ => OpenError::Fatal(reason),
                })
            }
        }
    }
}

/// Why a link couldn't open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// The connection failed; a later attempt may succeed.
    Retry(String),
    /// The host answered but can't serve this link (no `auk`, a bridge error).
    Fatal(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Retry(r) | OpenError::Fatal(r) => f.write_str(r),
        }
    }
}

/// A running `ssh` whose remote bridge has answered.
#[derive(Debug)]
pub struct SshSession {
    pub child: Child,
    /// Toward the remote daemon.
    pub stdin: ChildStdin,
    /// From the remote daemon, after the preamble.
    pub stdout: BufReader<ChildStdout>,
    pub bridge: BridgeInfo,
    /// How `ssh` authenticated, from its debug log.
    pub auth: Option<Auth>,
    log: Arc<SshLog>,
}

impl SshSession {
    /// The hop this session adds to the host's identity chain.
    pub fn hop(&self) -> Hop {
        let origin = crate::bridge::hostname();
        match &self.auth {
            Some(auth) => Hop {
                principal: auth.principal(),
                method: auth.method_name(),
                origin,
                acting: self.bridge.acting(),
                key: auth.key.as_ref().map(|k| k.fingerprint.clone()),
            },
            None => Hop {
                principal: format!("{}@{}", crate::bridge::current_user(), origin),
                method: "ssh".to_owned(),
                origin,
                acting: self.bridge.acting(),
                key: None,
            },
        }
    }

    /// The last lines `ssh` wrote that weren't debug output.
    pub fn log_tail(&self) -> String {
        self.log.tail()
    }
}

/// How `ssh` authenticated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Auth {
    /// The SSH method: `publickey`, `password`, `keyboard-interactive`, …
    pub method: String,
    /// The key the server accepted, for `publickey`.
    pub key: Option<AcceptedKey>,
}

/// A key the server accepted, from `Server accepts key: <source> <type> <fingerprint> …`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedKey {
    /// A key file's path, or an agent key's comment.
    pub source: String,
    /// `ED25519 SHA256:…`.
    pub fingerprint: String,
}

impl Auth {
    /// The identity-chain method: `ssh-key` for a public key, else `ssh-<method>`.
    pub fn method_name(&self) -> String {
        if self.method == "publickey" {
            SSH_KEY.to_owned()
        } else {
            format!("ssh-{}", self.method)
        }
    }

    /// Who authenticated: the key's comment (from its `.pub` file, or the agent), else its
    /// fingerprint; without a key, the local user.
    pub fn principal(&self) -> String {
        let Some(key) = &self.key else {
            return format!(
                "{}@{}",
                crate::bridge::current_user(),
                crate::bridge::hostname()
            );
        };
        let path = Path::new(&key.source);
        if path.is_absolute() {
            let mut public = path.as_os_str().to_owned();
            public.push(".pub");
            let comment = fs::read_to_string(&public).ok().and_then(|line| {
                let mut words = line.split_whitespace().skip(2);
                let first = words.next()?;
                Some(
                    std::iter::once(first)
                        .chain(words)
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            });
            match comment {
                Some(c) if !c.is_empty() => c,
                _ => key.fingerprint.clone(),
            }
        } else {
            key.source.clone()
        }
    }
}

/// `ssh`'s stderr: the authentication facts, and the last lines that explain a failure.
#[derive(Debug, Default)]
struct SshLog {
    state: Mutex<LogState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct LogState {
    accepted: Option<AcceptedKey>,
    auth: Option<Auth>,
    tail: VecDeque<String>,
    done: bool,
}

impl SshLog {
    fn read(&self, stderr: impl Read) {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(key) = parse_accepted(&line) {
                st.accepted = Some(key);
            } else if let Some(method) = parse_authenticated(&line) {
                let key = (method == "publickey")
                    .then(|| st.accepted.clone())
                    .flatten();
                st.auth = Some(Auth { method, key });
                self.changed.notify_all();
            } else if !line.starts_with("debug") {
                st.tail.push_back(line);
                if st.tail.len() > 8 {
                    st.tail.pop_front();
                }
            }
        }
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.done = true;
        self.changed.notify_all();
    }

    fn wait_auth(&self, timeout: Duration) -> Option<Auth> {
        let deadline = Instant::now() + timeout;
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while st.auth.is_none() && !st.done {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            st = self
                .changed
                .wait_timeout(st, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        st.auth.clone()
    }

    fn tail(&self) -> String {
        // Give the reader a moment to take ssh's last words.
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while !st.done && Instant::now() < deadline {
            st = self
                .changed
                .wait_timeout(st, Duration::from_millis(50))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        let lines: Vec<&str> = st.tail.iter().map(String::as_str).collect();
        if lines.is_empty() {
            "no message".to_owned()
        } else {
            lines.join("; ")
        }
    }
}

/// `debug1: Server accepts key: /path/id_ed25519 ED25519 SHA256:… explicit`.
fn parse_accepted(line: &str) -> Option<AcceptedKey> {
    let rest = line.split_once("Server accepts key: ")?.1;
    let words: Vec<&str> = rest.split_whitespace().collect();
    // The source may contain spaces (an agent key's comment); the type and the fingerprint are
    // the two words before the trailing flags, the fingerprint being the one with a hash prefix.
    let at = words
        .iter()
        .rposition(|w| w.starts_with("SHA256:") || w.starts_with("MD5:"))?;
    if at < 2 {
        return None;
    }
    Some(AcceptedKey {
        source: words[..at - 1].join(" "),
        fingerprint: format!("{} {}", words[at - 1], words[at]),
    })
}

/// `Authenticated to host ([addr]:port) using "publickey".`
fn parse_authenticated(line: &str) -> Option<String> {
    let rest = line.strip_prefix("Authenticated to ")?;
    let method = rest.split_once("using \"")?.1.split('"').next()?;
    Some(method.to_owned())
}

#[derive(Debug)]
enum PreambleError {
    Io(io::Error),
    Closed,
    Bridge(String),
    Noise(Vec<String>),
    Version(String),
}

impl std::fmt::Display for PreambleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PreambleError::Io(err) => write!(f, "reading from ssh: {err}"),
            PreambleError::Closed => write!(f, "ssh closed before the remote bridge answered"),
            PreambleError::Bridge(reason) => write!(f, "{reason}"),
            PreambleError::Noise(lines) => write!(
                f,
                "the remote command didn't start an Auk bridge; it printed: {}",
                lines.join(" | ")
            ),
            PreambleError::Version(line) => write!(f, "an unsupported bridge preamble: {line}"),
        }
    }
}

/// Reads up to the preamble, skipping what a login script printed before it.
fn read_preamble(r: &mut impl BufRead) -> Result<BridgeInfo, PreambleError> {
    let mut noise = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = Read::take(&mut *r, 64 << 10)
            .read_until(b'\n', &mut line)
            .map_err(PreambleError::Io)?;
        if n == 0 {
            return Err(if noise.is_empty() {
                PreambleError::Closed
            } else {
                PreambleError::Noise(noise)
            });
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end();
        if let Some(reason) = text.strip_prefix(PREAMBLE_ERROR) {
            return Err(PreambleError::Bridge(reason.trim().to_owned()));
        }
        if let Some(rest) = text
            .strip_prefix(PREAMBLE)
            .and_then(|r| r.strip_prefix(' '))
        {
            let (version, json) = rest.split_once(' ').unwrap_or((rest, ""));
            if version != BRIDGE_VERSION.to_string() {
                return Err(PreambleError::Version(text.to_owned()));
            }
            return serde_json::from_str(json).map_err(|_| PreambleError::Version(text.to_owned()));
        }
        noise.push(text.to_owned());
        if noise.len() > MAX_NOISE_LINES {
            return Err(PreambleError::Noise(noise.split_off(noise.len() - 3)));
        }
    }
}

/// The child's exit code once it exits within `timeout`.
fn wait_timeout(child: &mut Child, timeout: Duration) -> Option<i32> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_debug_log() {
        let key = parse_accepted(
            "debug1: Server accepts key: /Users/me/.ssh/id_ed25519 ED25519 \
             SHA256:RMT2xF+nC39QOIVW4l0T1C5XnXHD2EfsWfYdUS57qfU explicit",
        )
        .expect("accepted key");
        assert_eq!(key.source, "/Users/me/.ssh/id_ed25519");
        assert_eq!(
            key.fingerprint,
            "ED25519 SHA256:RMT2xF+nC39QOIVW4l0T1C5XnXHD2EfsWfYdUS57qfU"
        );
        let agent =
            parse_accepted("debug1: Server accepts key: me@laptop key ED25519 SHA256:abc agent")
                .expect("agent key");
        assert_eq!(agent.source, "me@laptop key");
        assert_eq!(
            parse_authenticated(
                "Authenticated to 127.0.0.1 ([127.0.0.1]:64522) using \"publickey\"."
            ),
            Some("publickey".into())
        );
        let auth = Auth {
            method: "publickey".into(),
            key: Some(agent),
        };
        assert_eq!(auth.method_name(), SSH_KEY);
        assert_eq!(auth.principal(), "me@laptop key");
    }

    #[test]
    fn skips_noise_before_the_preamble() {
        let info = BridgeInfo::here(false);
        let mut stream = io::Cursor::new(format!("welcome!\n{}\x01\x02", info.preamble()));
        assert_eq!(read_preamble(&mut stream).expect("preamble"), info);
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).expect("rest");
        assert_eq!(rest, b"\x01\x02");

        let mut failed = io::Cursor::new(format!("{PREAMBLE_ERROR} no daemon\n"));
        assert!(matches!(
            read_preamble(&mut failed),
            Err(PreambleError::Bridge(r)) if r == "no daemon"
        ));
        assert!(matches!(
            read_preamble(&mut io::Cursor::new("zsh: command not found: auk\n")),
            Err(PreambleError::Noise(_))
        ));
    }

    #[test]
    fn the_command_line_keeps_the_users_ssh_setup() {
        let mut target = SshTarget::new("build-box");
        target.config = Some("/tmp/cfg".into());
        let cmd = target.command();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&args[..2], ["-F", "/tmp/cfg"]);
        assert_eq!(
            &args[args.len() - 3..],
            ["--", "build-box", DEFAULT_REMOTE_COMMAND]
        );
        assert!(args.contains(&"ControlPath=none".to_owned()));
    }
}
