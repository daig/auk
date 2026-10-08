//! The Linux VM for the `remote` suite (GOAL.md, spec §10.2, D7): a Lima VM, `auk-linux`, that
//! builds and runs the Linux `auk`, reached with Lima's generated SSH config.
//!
//! - [`Vm::get`] checks that Lima and the VM exist (failing with what to do when they don't),
//!   starts the VM if it's stopped, and checks that the repo is mounted in it.
//! - [`Vm::linux_auk`] builds the Linux binary inside the VM from the mounted source, once per
//!   test process (cargo's lock serializes concurrent runs). Each [`RemoteEnv`] copies it into
//!   its own directory, so a test runs the binary its own run built.
//! - [`RemoteEnv`] is one test's world on both ends: a temporary root on the VM with its own
//!   daemon socket, state dir, `HOME` and test `ZDOTDIR`, and a local [`TestEnv`] for the
//!   windows that attach to it over SSH. Dropping it kills the remote daemon and removes both.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

use serde_json::Value;

use crate::harness::{Headless, TEST_ZSHRC, TestEnv, quote, wait_for};

/// The Lima instance's name.
pub const VM_NAME: &str = "auk-linux";
/// Where the VM's definition lives in the repo.
pub const VM_DEFINITION: &str = "crates/conformance/lima/auk-linux.yaml";
/// The VM-local cargo target dir for the Linux build (the repo mount is read-only).
pub const VM_TARGET_DIR: &str = "auk-remote-suite/target";

/// The Linux VM.
#[derive(Debug)]
pub struct Vm {
    /// Lima's generated SSH config for the instance.
    pub ssh_config: PathBuf,
    /// The `Host` alias in that config.
    pub host: String,
    /// The repo, at the same path inside the VM (Lima mounts it read-only).
    pub repo: PathBuf,
}

static VM: OnceLock<Vm> = OnceLock::new();
static LINUX_AUK: OnceLock<String> = OnceLock::new();

/// Holds a lock file across processes: nextest runs each test in its own process.
fn with_lock<T>(name: &str, f: impl FnOnce() -> T) -> T {
    let path = std::env::temp_dir().join(format!("auk-remote-{name}.lock"));
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    file.lock()
        .unwrap_or_else(|e| panic!("lock {}: {e}", path.display()));
    let value = f();
    let _ = file.unlock();
    value
}

fn limactl(args: &[&str]) -> Output {
    match Command::new("limactl").args(args).output() {
        Ok(out) => out,
        Err(err) => panic!(
            "the remote suite needs Lima (`limactl`), which couldn't run: {err}. \
             Install it with `brew install lima`, then create the VM with \
             `limactl create --name {VM_NAME} {VM_DEFINITION}`"
        ),
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

impl Vm {
    /// The VM, running. Panics with what to do when Lima or the VM is missing.
    pub fn get() -> &'static Vm {
        VM.get_or_init(|| with_lock("vm", Vm::start))
    }

    fn start() -> Vm {
        let status = |out: &Output| text(&out.stdout);
        let listed = limactl(&["list", "--format", "{{.Status}}\t{{.Dir}}", VM_NAME]);
        assert!(
            listed.status.success(),
            "the Lima VM `{VM_NAME}` doesn't exist ({}). Create it with \
             `limactl create --name {VM_NAME} {VM_DEFINITION}`",
            text(&listed.stderr)
        );
        let line = status(&listed);
        let (state, dir) = line
            .split_once('\t')
            .unwrap_or_else(|| panic!("unexpected `limactl list` output: {line:?}"));
        let dir = PathBuf::from(dir);
        if state != "Running" {
            eprintln!("starting the Lima VM `{VM_NAME}` (it was {state})");
            let started = limactl(&["start", "--tty=false", VM_NAME]);
            assert!(
                started.status.success(),
                "`limactl start {VM_NAME}` failed: {}",
                text(&started.stderr)
            );
        }
        let ssh_config = dir.join("ssh.config");
        assert!(
            ssh_config.is_file(),
            "Lima's SSH config for `{VM_NAME}` is missing at {}",
            ssh_config.display()
        );
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("the repo root")
            .to_owned();
        let vm = Vm {
            ssh_config,
            host: format!("lima-{VM_NAME}"),
            repo,
        };
        let probe = vm.ssh(&format!(
            "test -f {}/Cargo.toml && command -v zsh && test -x \"$HOME/.cargo/bin/cargo\"",
            quote(&vm.repo.to_string_lossy())
        ));
        assert!(
            probe.status.success(),
            "the VM `{VM_NAME}` lacks the repo mount at {}, zsh or ~/.cargo/bin/cargo \
             (see {VM_DEFINITION}): {}",
            vm.repo.display(),
            text(&probe.stderr)
        );
        vm
    }

    /// `ssh` to the VM with Lima's config.
    pub fn ssh_command(&self) -> Command {
        let mut cmd = Command::new("ssh");
        cmd.arg("-F").arg(&self.ssh_config).arg(&self.host);
        cmd
    }

    /// Runs a shell script on the VM.
    pub fn ssh(&self, script: &str) -> Output {
        self.ssh_command()
            .arg(script)
            .stdin(Stdio::null())
            .output()
            .expect("run ssh")
    }

    /// Runs a script on the VM, panicking unless it succeeds; returns its stdout.
    pub fn ssh_ok(&self, script: &str) -> String {
        let out = self.ssh(script);
        assert!(
            out.status.success(),
            "on the VM, `{script}` failed ({}): {}",
            out.status,
            text(&out.stderr)
        );
        text(&out.stdout)
    }

    /// Runs a script on the VM with `input` on its stdin.
    pub fn ssh_with_input(&self, script: &str, input: &[u8]) -> Output {
        let mut child = self
            .ssh_command()
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run ssh");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input)
            .expect("write to ssh");
        child.wait_with_output().expect("wait for ssh")
    }

    /// Builds the Linux `auk` in the VM from the mounted source and returns its path there.
    pub fn linux_auk(&self) -> &'static str {
        LINUX_AUK.get_or_init(|| {
            with_lock("build", || {
                let script = format!(
                    "cd {} && export PATH=\"$HOME/.cargo/bin:$PATH\" \
                     CARGO_TARGET_DIR=\"$HOME/{VM_TARGET_DIR}\" && \
                     cargo build --locked --release -p auk 2>&1 && \
                     echo \"$CARGO_TARGET_DIR/release/auk\"",
                    quote(&self.repo.to_string_lossy())
                );
                let out = self.ssh(&script);
                let log = text(&out.stdout);
                assert!(
                    out.status.success(),
                    "building the Linux auk in the VM failed:\n{log}\n{}",
                    text(&out.stderr)
                );
                log.lines().last().expect("the binary's path").to_owned()
            })
        })
    }
}

/// One test's remote daemon on the VM, and its local side.
#[derive(Debug)]
pub struct RemoteEnv {
    pub vm: &'static Vm,
    /// The local side: windows attach from here (no local daemon runs).
    pub local: TestEnv,
    /// The test's root on the VM.
    pub root: String,
    /// The Linux `auk` this test runs, copied into `root`.
    pub auk: String,
}

impl RemoteEnv {
    /// Builds the Linux binary (once per process) and sets up a root on the VM.
    pub fn new(exe: impl Into<PathBuf>, name: &str) -> RemoteEnv {
        let vm = Vm::get();
        let built = vm.linux_auk();
        let root = vm.ssh_ok("mktemp -d /tmp/auk-remote-XXXXXXXX");
        let auk = format!("{root}/auk");
        vm.ssh_ok(&format!(
            "mkdir -p {root}/home {root}/zdotdir {root}/run {root}/state && cp {} {auk}",
            quote(built)
        ));
        let written = vm.ssh_with_input(
            &format!("cat > {root}/zdotdir/.zshrc"),
            TEST_ZSHRC.as_bytes(),
        );
        assert!(written.status.success(), "write the remote .zshrc");
        RemoteEnv {
            vm,
            local: TestEnv::new(exe, name),
            root,
            auk,
        }
    }

    /// The remote `auk` with the test's environment, as a shell command prefix.
    pub fn auk_prefix(&self) -> String {
        let r = &self.root;
        format!(
            "env -u XDG_RUNTIME_DIR -u AUK_PANE HOME={r}/home AUK_DAEMON_SOCKET={r}/run/daemon.sock \
             AUK_STATE_DIR={r}/state AUK_CONFIG_DIR={r}/state ZDOTDIR={r}/zdotdir SHELL=/bin/zsh \
             {}",
            self.auk
        )
    }

    /// While this file exists on the VM, a new link's bridge waits before it starts: a test
    /// holds a reconnect open with it.
    pub fn gate(&self) -> String {
        format!("{}/gate", self.root)
    }

    /// What a window's `ssh` runs on the VM: `auk attach --stdio` with the test's environment,
    /// once the gate is open.
    pub fn remote_command(&self) -> String {
        let script = format!(
            "while [ -e {} ]; do sleep 0.05; done; exec {} attach --stdio",
            self.gate(),
            self.auk_prefix()
        );
        format!("sh -c {}", quote(&script))
    }

    /// Runs a remote CLI verb; returns its output.
    pub fn auk(&self, args: &[&str]) -> Output {
        let words: Vec<String> = args.iter().map(|a| quote(a)).collect();
        self.vm
            .ssh(&format!("{} {}", self.auk_prefix(), words.join(" ")))
    }

    /// Runs a remote CLI verb, panicking unless it succeeds; returns its stdout.
    pub fn auk_ok(&self, args: &[&str]) -> String {
        let out = self.auk(args);
        assert!(
            out.status.success(),
            "remote `auk {}` failed ({}): {}",
            args.join(" "),
            out.status,
            text(&out.stderr)
        );
        text(&out.stdout)
    }

    /// Sends one line to the remote daemon's control endpoint (with the VM's `python3`) and
    /// returns the reply's `value`, panicking unless the reply is ok.
    pub fn daemon(&self, line: &str) -> Value {
        const SEND: &str = "import socket, sys\n\
            s = socket.socket(socket.AF_UNIX)\n\
            s.connect(sys.argv[1])\n\
            s.sendall((sys.argv[2] + '\\n').encode())\n\
            print(s.makefile().readline())\n";
        let out = self.vm.ssh_ok(&format!(
            "python3 -c {} {}/run/daemon.ctl {}",
            quote(SEND),
            self.root,
            quote(line)
        ));
        let reply: Value = serde_json::from_str(&out)
            .unwrap_or_else(|e| panic!("remote `{line}`: not JSON ({e}): {out}"));
        assert_eq!(reply["ok"], true, "remote `{line}` failed: {reply}");
        reply.get("value").cloned().unwrap_or(Value::Null)
    }

    /// Writes a file on the VM, creating its directory.
    pub fn write_file(&self, path: &str, contents: &str) {
        let out = self.vm.ssh_with_input(
            &format!("mkdir -p \"$(dirname {0})\" && cat > {0}", quote(path)),
            contents.as_bytes(),
        );
        assert!(
            out.status.success(),
            "write {path} on the VM: {}",
            text(&out.stderr)
        );
    }

    /// Opens a remote pane in the test's remote home (starting the remote daemon) and waits for
    /// its prompt.
    pub fn new_pane(&self, extra: &[&str]) -> u64 {
        let home = format!("{}/home", self.root);
        let mut args = vec!["new"];
        args.extend_from_slice(extra);
        args.extend(["--cwd", &home]);
        let pane: u64 = self
            .auk_ok(&args)
            .parse()
            .expect("`auk new` prints a pane id");
        self.wait(pane, "prompt");
        pane
    }

    /// `auk wait PANE --until COND` on the remote daemon.
    pub fn wait(&self, pane: u64, until: &str) {
        self.auk_ok(&[
            "wait",
            &pane.to_string(),
            "--until",
            until,
            "--timeout",
            "20",
        ]);
    }

    /// Types a line and Enter into a remote pane from the remote CLI, then waits for the prompt.
    pub fn run(&self, pane: u64, line: &str) {
        self.auk_ok(&["run", &pane.to_string(), line]);
        self.wait(pane, "prompt");
    }

    /// The remote daemon's text of a pane.
    pub fn capture(&self, pane: u64) -> String {
        self.auk_ok(&["capture", &pane.to_string(), "--scrollback"])
    }

    /// The remote daemon's `ls --json`.
    pub fn ls(&self) -> Value {
        serde_json::from_str(&self.auk_ok(&["ls", "--json"])).expect("ls --json")
    }

    /// The remote daemon's entry for a pane.
    pub fn pane_info(&self, pane: u64) -> Value {
        let ls = self.ls();
        ls["panes"]
            .as_array()
            .and_then(|panes| panes.iter().find(|p| p["id"] == pane).cloned())
            .unwrap_or_else(|| panic!("pane {pane} isn't listed on the remote daemon: {ls}"))
    }

    /// Starts a local headless window attached to the remote daemon over SSH.
    pub fn window(&self, name: &str) -> Headless {
        let config = self.vm.ssh_config.to_string_lossy().into_owned();
        let command = self.remote_command();
        self.local.start_headless(
            name,
            &[
                "--host",
                &self.vm.host,
                "--ssh-config",
                &config,
                "--remote-command",
                &command,
            ],
        )
    }

    /// Waits until a window's capture of a pane contains `needle`, and returns it.
    pub fn window_shows(&self, window: &Headless, pane: u64, needle: &str) -> String {
        wait_for(
            &format!("the window to show {needle:?} in pane {pane}"),
            || {
                let reply = crate::harness::Control::connect(&window.control)
                    .send(&format!("capture {pane} --scrollback"));
                let text = reply["value"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                text.contains(needle).then_some(text)
            },
        )
    }

    /// Whether a process on the VM matches `pattern` (`pgrep -f`).
    pub fn remote_running(&self, pattern: &str) -> bool {
        // The bracket keeps the ssh shell's own command line from matching.
        let (head, tail) = pattern.split_at(1);
        self.vm
            .ssh(&format!("pgrep -f {}", quote(&format!("[{head}]{tail}"))))
            .status
            .success()
    }
}

impl Drop for RemoteEnv {
    fn drop(&mut self) {
        let (head, tail) = self.root.split_at(1);
        let pattern = quote(&format!("[{head}]{tail}/"));
        let _ = self.vm.ssh(&format!(
            "pkill -KILL -f {pattern}; sleep 0.2; rm -rf {}",
            self.root
        ));
    }
}
