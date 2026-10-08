//! The real macOS app under test (`auk` with no subcommand): started on a test daemon with its
//! automation endpoint (`--control`), driven one command line at a time. Each app uses its own
//! named pasteboard (`$AUK_PASTEBOARD`), so tests never touch the user's clipboard.
//!
//! ```text
//! let env = TestEnv::new(env!("CARGO_BIN_EXE_auk"), "app");
//! env.start_daemon();
//! let app = App::start(&env, "a", &[]);
//! app.request("windows");
//! ```

use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::harness::{Control, TestEnv, WAIT, ping, wait_for};

/// The US keyboard layout: the synthesized and posted strokes name ANSI keys, so tests type
/// with it selected (and put the user's back after).
pub const US_LAYOUT: &str = "com.apple.keylayout.US";

static PASTEBOARDS: AtomicU32 = AtomicU32::new(0);

/// A running app.
#[derive(Debug)]
pub struct App {
    pub pid: u32,
    pub control: PathBuf,
    child: Option<Child>,
}

impl App {
    /// Starts the app named `name` on the test's daemon with `extra` flags, and waits until its
    /// endpoint answers and its first window is up.
    pub fn start(env: &TestEnv, name: &str, extra: &[&str]) -> App {
        App::start_with(env, name, extra, &[])
    }

    /// [`App::start`] with extra environment variables (a stand-in `AUK_OPENER`, say).
    pub fn start_with(env: &TestEnv, name: &str, extra: &[&str], vars: &[(&str, &str)]) -> App {
        let control = env.root.join(format!("{name}.ctl"));
        let log = std::fs::File::create(env.root.join(format!("{name}.log"))).expect("a log");
        let board = format!(
            "auk-test-{}-{}",
            std::process::id(),
            PASTEBOARDS.fetch_add(1, Ordering::Relaxed)
        );
        let child = env
            .command()
            .arg("--control")
            .arg(&control)
            .args(extra)
            .env("AUK_PASTEBOARD", board)
            .envs(vars.iter().copied())
            .stdout(log.try_clone().expect("clone a log"))
            .stderr(log)
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn the app");
        let app = App {
            pid: child.id(),
            control,
            child: Some(child),
        };
        wait_for("the app's control endpoint", || {
            ping(&app.control).then_some(())
        });
        wait_for("the app's first window", || {
            let reply = app.send("state");
            (reply["ok"] == true && reply["value"]["focused"].is_u64()).then_some(())
        });
        app
    }

    /// Sends a line and returns the whole reply.
    pub fn send(&self, line: &str) -> Value {
        Control::connect(&self.control).send(line)
    }

    /// Sends a line and returns its `value`, panicking unless the reply is ok.
    pub fn request(&self, line: &str) -> Value {
        Control::connect(&self.control).ok(line)
    }

    /// Sends a line and returns its `value`, or `None` when the app answers with an error (for
    /// polling something that may not exist yet).
    pub fn try_request(&self, line: &str) -> Option<Value> {
        let reply = Control::connect(&self.control).send(line);
        (reply["ok"] == true).then(|| reply.get("value").cloned().unwrap_or(Value::Null))
    }

    /// The window's state: shown session, tab, focus, visible panes.
    pub fn state(&self) -> Value {
        self.request("state")
    }

    /// The window key's sessions, as this app's window replicated them.
    pub fn layout(&self) -> Value {
        self.request("layout")
    }

    /// Waits until the app's replica of `pane` has applied at least `seq`.
    pub fn caught_up(&self, pane: u64, seq: u64) {
        wait_for("the app's replica to catch up with the daemon", || {
            let reply = self.send(&format!("state-hash {pane}"));
            (reply["value"]["seq"].as_u64()? >= seq).then_some(())
        });
    }

    /// Waits until this app's window is `pane`'s primary.
    pub fn wait_primary(&self, pane: u64) {
        wait_for("the app to become the pane's primary", || {
            let panes = self.request("panes");
            panes["panes"]
                .as_array()?
                .iter()
                .any(|p| p["id"] == pane && p["primary"] == true)
                .then_some(())
        });
    }

    /// Quits through the app menu and waits for the process to exit.
    pub fn quit(mut self) {
        // The app may exit before it answers.
        if let Ok(mut c) = std::os::unix::net::UnixStream::connect(&self.control) {
            let _ = std::io::Write::write_all(&mut c, b"quit\n");
            let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
            let _ = std::io::Read::read(&mut c, &mut [0u8; 256]);
        }
        let mut child = self.child.take().expect("a child");
        let deadline = Instant::now() + WAIT;
        loop {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            assert!(Instant::now() < deadline, "the app didn't quit");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Selects an input source through an app for as long as it lives, then puts back the one the
/// user had.
#[derive(Debug)]
pub struct InputSource {
    control: PathBuf,
    previous: String,
}

impl InputSource {
    /// Selects `id` through `app`; panics naming the input source if it isn't installed.
    pub fn select(app: &App, id: &str) -> InputSource {
        let reply = app.send(&format!("input-source {id}"));
        assert_eq!(
            reply["ok"], true,
            "prerequisite missing: the input source {id} ({reply})"
        );
        let previous = reply["value"]["previous"]
            .as_str()
            .expect("the previous input source")
            .to_owned();
        InputSource {
            control: app.control.clone(),
            previous,
        }
    }
}

impl Drop for InputSource {
    fn drop(&mut self) {
        if ping(&self.control) {
            let _ =
                Control::connect(&self.control).send(&format!("input-source {}", self.previous));
        }
    }
}
