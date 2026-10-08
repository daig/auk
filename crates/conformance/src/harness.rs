//! A hermetic harness for suites that drive the real `auk` binary (GOAL.md): a temporary state
//! dir and socket per test, a test `ZDOTDIR` and `HOME`, and handles for the daemon, headless
//! windows and control endpoints. Nothing touches the user's daemon, state or shell config.
//!
//! ```text
//! use auk_conformance::harness::TestEnv;
//! let env = TestEnv::new(env!("CARGO_BIN_EXE_auk"), "example");
//! let _daemon = env.start_daemon();
//! let pane = env.new_pane(&[]);
//! env.wait(pane, "prompt");
//! ```

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

/// How long a test waits for real work: a shell prompt, output, a replica catching up.
pub const WAIT: Duration = Duration::from_secs(20);

/// The test shell's `.zshrc`: a fixed prompt, and a variable proving the file was sourced.
pub const TEST_ZSHRC: &str = "PS1='%~ %# '\nexport AUK_TEST_RC=sourced\n";

/// One test's world: its own directory tree, daemon socket and shell config.
#[derive(Debug)]
pub struct TestEnv {
    pub exe: PathBuf,
    pub root: PathBuf,
    pub socket: PathBuf,
    pub state: PathBuf,
    pub home: PathBuf,
    pub zdotdir: PathBuf,
    children: Mutex<Vec<Child>>,
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

impl TestEnv {
    /// Creates the directories. `exe` is the `auk` binary (`env!("CARGO_BIN_EXE_auk")`).
    pub fn new(exe: impl Into<PathBuf>, name: &str) -> TestEnv {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        // Short: Unix socket paths are limited to 104 bytes on macOS.
        let root =
            std::env::temp_dir().join(format!("auk-{}-{}-{n}", short(name), std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let root = {
            fs::create_dir_all(&root).expect("create the test root");
            root.canonicalize().expect("canonicalize the test root")
        };
        let env = TestEnv {
            exe: exe.into(),
            socket: root.join("run/daemon.sock"),
            state: root.join("state"),
            home: root.join("home"),
            zdotdir: root.join("zdotdir"),
            root,
            children: Mutex::new(Vec::new()),
        };
        for dir in [&env.state, &env.home, &env.zdotdir, &env.root.join("run")] {
            fs::create_dir_all(dir).expect("create a test dir");
        }
        fs::write(env.zdotdir.join(".zshrc"), TEST_ZSHRC).expect("write the test .zshrc");
        env
    }

    /// The `auk` binary with the test's environment.
    pub fn command(&self) -> Command {
        let mut cmd = Command::new(&self.exe);
        cmd.env("AUK_DAEMON_SOCKET", &self.socket)
            .env("AUK_STATE_DIR", &self.state)
            .env("AUK_CONFIG_DIR", &self.state)
            .env("HOME", &self.home)
            .env("ZDOTDIR", &self.zdotdir)
            .env("SHELL", "/bin/zsh")
            .env_remove("XDG_RUNTIME_DIR")
            .env_remove("AUK_PANE")
            .env_remove("AUK_WINDOW_KEY")
            .stdin(Stdio::null());
        cmd
    }

    /// Runs a CLI verb to completion and returns its output.
    pub fn cli(&self, args: &[&str]) -> std::process::Output {
        self.command().args(args).output().expect("run auk")
    }

    /// Spawns a process the environment cleans up.
    fn spawn(&self, mut cmd: Command, log: &str) -> u32 {
        let log = fs::File::create(self.root.join(log)).expect("create a log");
        cmd.stdout(log.try_clone().expect("clone a log"))
            .stderr(log);
        let child = cmd.spawn().expect("spawn auk");
        let pid = child.id();
        self.children.lock().unwrap().push(child);
        pid
    }

    /// Starts `auk daemon` and waits until its control endpoint answers.
    pub fn start_daemon(&self) -> u32 {
        self.start_daemon_with(|_| {})
    }

    /// [`start_daemon`](Self::start_daemon), with the daemon's command adjusted first (an extra
    /// `PATH` entry for stand-in programs its plugins run, say).
    pub fn start_daemon_with(&self, configure: impl FnOnce(&mut Command)) -> u32 {
        let mut cmd = self.command();
        configure(&mut cmd);
        cmd.arg("daemon");
        let pid = self.spawn(cmd, "daemon.log");
        let control = self.control_path();
        wait_for("the daemon's control endpoint", || {
            ping(&control).then_some(())
        });
        pid
    }

    /// The daemon's control endpoint.
    pub fn control_path(&self) -> PathBuf {
        self.socket.with_file_name("daemon.ctl")
    }

    /// A connection to the daemon's control endpoint.
    pub fn control(&self) -> Control {
        Control::connect(&self.control_path())
    }

    /// Sends one command to the daemon and returns its `value`, panicking on failure.
    pub fn daemon(&self, line: &str) -> Value {
        self.control().ok(line)
    }

    /// Starts a headless window named `name` and waits until its control endpoint answers.
    pub fn start_headless(&self, name: &str, extra: &[&str]) -> Headless {
        let control = self.root.join(format!("{name}.ctl"));
        let mut cmd = self.command();
        cmd.arg("headless")
            .arg("--control")
            .arg(&control)
            .args(extra);
        let pid = self.spawn(cmd, &format!("{name}.log"));
        wait_for("the headless window's control endpoint", || {
            ping(&control).then_some(())
        });
        Headless { pid, control }
    }

    /// Opens a pane (`new` plus `args`) in the test's home dir unless `--cwd` is given.
    pub fn new_pane(&self, args: &[&str]) -> u64 {
        let mut line = String::from("new");
        if !args.contains(&"--cwd") {
            line.push_str(&format!(" --cwd {}", quote(&self.home.to_string_lossy())));
        }
        for arg in args {
            line.push(' ');
            line.push_str(&quote(arg));
        }
        self.daemon(&line)["pane"].as_u64().expect("a pane id")
    }

    /// `wait PANE --until COND` on the daemon, within [`WAIT`].
    pub fn wait(&self, pane: u64, until: &str) -> Value {
        self.daemon(&format!(
            "wait {pane} --until {until} --timeout {}",
            WAIT.as_secs()
        ))
    }

    /// Types a command line and Enter into a pane, then waits for the next prompt.
    pub fn run(&self, pane: u64, line: &str) -> Value {
        self.daemon(&format!("run {pane} {}", quote(line)));
        self.wait(pane, "prompt")
    }

    /// The daemon's view of a pane (`ls`).
    pub fn pane_info(&self, pane: u64) -> Value {
        let ls = self.daemon("ls");
        ls["panes"]
            .as_array()
            .and_then(|panes| panes.iter().find(|p| p["id"] == pane).cloned())
            .unwrap_or_else(|| panic!("pane {pane} isn't listed: {ls}"))
    }

    /// The daemon's state hash and sequence number for a pane.
    pub fn daemon_hash(&self, pane: u64) -> (String, u64) {
        hash_of(&self.daemon(&format!("state-hash {pane}")))
    }

    /// Kills a process the environment started, with SIGKILL, and reaps it.
    pub fn kill9(&self, pid: u32) {
        let mut children = self.children.lock().unwrap();
        if let Some(i) = children.iter().position(|c| c.id() == pid) {
            let mut child = children.remove(i);
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        if let Ok(mut c) = UnixStream::connect(self.control_path()) {
            let _ = c.write_all(b"shutdown\n");
            let _ = c.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = BufReader::new(&c).read_line(&mut String::new());
        }
        let mut children = self.children.lock().unwrap_or_else(|e| e.into_inner());
        let deadline = Instant::now() + Duration::from_secs(3);
        for child in children.iter_mut() {
            while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        if std::thread::panicking() {
            eprintln!("test files kept in {}", self.root.display());
        } else {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

/// A headless window's process and control endpoint.
#[derive(Debug)]
pub struct Headless {
    pub pid: u32,
    pub control: PathBuf,
}

impl Headless {
    pub fn request(&self, line: &str) -> Value {
        Control::connect(&self.control).ok(line)
    }

    /// The window's replica hash and sequence number for a pane, once it has one.
    pub fn hash(&self, pane: u64) -> Option<(String, u64)> {
        let reply = Control::connect(&self.control).send(&format!("state-hash {pane}"));
        (reply["ok"] == true).then(|| hash_of(&reply["value"]))
    }

    pub fn capture(&self, pane: u64, scrollback: bool) -> String {
        let flag = if scrollback { " --scrollback" } else { "" };
        self.request(&format!("capture {pane}{flag}"))["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    /// Stream errors the window has seen; a correct daemon causes none.
    pub fn errors(&self) -> Vec<Value> {
        self.request("panes")["errors"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
}

/// A line-protocol connection to a control endpoint.
#[derive(Debug)]
pub struct Control {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Control {
    pub fn connect(path: &Path) -> Control {
        let writer = UnixStream::connect(path)
            .unwrap_or_else(|e| panic!("connect to {}: {e}", path.display()));
        writer
            .set_read_timeout(Some(WAIT + Duration::from_secs(10)))
            .expect("set a timeout");
        Control {
            reader: BufReader::new(writer.try_clone().expect("clone a stream")),
            writer,
        }
    }

    /// Sends a line and returns the whole reply.
    pub fn send(&mut self, line: &str) -> Value {
        writeln!(self.writer, "{line}").expect("send a control line");
        let mut reply = String::new();
        self.reader
            .read_line(&mut reply)
            .expect("read a control reply");
        serde_json::from_str(&reply)
            .unwrap_or_else(|e| panic!("bad reply {reply:?} to {line:?}: {e}"))
    }

    /// Sends a line and returns its `value`, panicking unless the reply is ok.
    pub fn ok(&mut self, line: &str) -> Value {
        let reply = self.send(line);
        assert_eq!(reply["ok"], true, "`{line}` failed: {reply}");
        reply.get("value").cloned().unwrap_or(Value::Null)
    }
}

/// Whether a control endpoint answers `ping`.
pub fn ping(path: &Path) -> bool {
    let Ok(stream) = UnixStream::connect(path) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut w = &stream;
    if w.write_all(b"ping\n").is_err() {
        return false;
    }
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).is_ok() && line.contains("pong")
}

/// Polls `f` until it returns `Some`, for at most [`WAIT`]; panics naming `what` otherwise.
pub fn wait_for<T>(what: &str, f: impl FnMut() -> Option<T>) -> T {
    wait_for_or(what, f, String::new)
}

/// Like [`wait_for`], adding `context()` (for example the pane's screen) to the failure message.
pub fn wait_for_or<T>(
    what: &str,
    mut f: impl FnMut() -> Option<T>,
    context: impl FnOnce() -> String,
) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(value) = f() {
            return value;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for {what}\n{}", context());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Whether a process exists.
pub fn alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn hash_of(value: &Value) -> (String, u64) {
    (
        value["hash"].as_str().expect("a hash").to_owned(),
        value["seq"].as_u64().expect("a seq"),
    )
}

/// Quotes a word for the control line protocol (shell-like words).
pub fn quote(word: &str) -> String {
    if !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=@%+:,".contains(&b))
    {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

fn short(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_like_a_shell() {
        assert_eq!(quote("plain-word"), "plain-word");
        assert_eq!(quote("two words"), "'two words'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote(""), "''");
    }
}
