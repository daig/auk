//! Scenario scripts and deterministic headless runs (§11.3, §21).
//!
//! - `auk shot SCENARIO… [--out DIR] [--size WxH[@S]] [--appearance light|dark]` runs each script
//!   in a fresh private daemon with a headless window, writing a PNG and a layout JSON for every
//!   `shot` into `DIR/<scenario>/`.
//! - `auk serve [--control EP]` is the same window on a private daemon, serving its control
//!   endpoint until `quit`.
//! - `auk ctl [--control EP] COMMAND…` sends one command; `--file SCRIPT` sends a script's
//!   commands one by one, each after the last one's reply.
//!
//! Runs are deterministic: the private daemon has its own state dir (default settings, never
//! saved, nothing restored), an empty plugins folder (`plugins fixtures` links the SDK's
//! examples), and the window's clock is pinned (only `wait`, `motion-at` move it).
//!
//! Script syntax: one command per line; `#` starts a comment; blank lines are skipped; arguments
//! are separated by spaces; double-quoted strings may hold spaces and the escapes `\"`, `\\`,
//! `\n`, `\t` and `\e` (ESC); single quotes are literal. `{fixtures}` stands for
//! `$AUK_FIXTURE_ROOT/fixtures` and `{scenario}` for the script's directory.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitCode, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::control::Control;
use crate::headless::{self, draw::WindowSpec};

/// The default control endpoint of `auk serve` and `auk ctl`: `serve.ctl` in the runtime dir.
pub fn default_control() -> PathBuf {
    auk_daemon::paths::runtime_dir().join("serve.ctl")
}

/// Splits a script line into words. `None` for a blank or comment line.
pub fn tokenize(line: &str) -> Result<Option<Vec<String>>, String> {
    let mut words = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let Some(&c) = chars.peek() else { break };
        if c == '#' {
            break;
        }
        let mut word = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                break;
            }
            chars.next();
            match c {
                '"' => loop {
                    match chars.next() {
                        None => return Err("an unclosed double quote".into()),
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('n') => word.push('\n'),
                            Some('t') => word.push('\t'),
                            Some('e') => word.push('\x1b'),
                            Some('r') => word.push('\r'),
                            Some(c @ ('"' | '\\' | '$' | '`')) => word.push(c),
                            Some(c) => {
                                word.push('\\');
                                word.push(c);
                            }
                            None => return Err("a dangling backslash".into()),
                        },
                        Some(c) => word.push(c),
                    }
                },
                '\'' => loop {
                    match chars.next() {
                        None => return Err("an unclosed single quote".into()),
                        Some('\'') => break,
                        Some(c) => word.push(c),
                    }
                },
                '\\' => match chars.next() {
                    Some(c) => word.push(c),
                    None => return Err("a dangling backslash".into()),
                },
                c => word.push(c),
            }
        }
        words.push(word);
    }
    Ok((!words.is_empty()).then_some(words))
}

/// Quotes a word so [`tokenize`] gives it back, control characters escaped: a command line
/// never holds a line break.
pub fn quote_word(word: &str) -> String {
    if !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=@%+:,{}".contains(c))
    {
        return word.to_owned();
    }
    let mut out = String::from("\"");
    for c in word.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '$' => out.push_str("\\$"),
            '`' => out.push_str("\\`"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\x1b' => out.push_str("\\e"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Replaces `{fixtures}` and `{scenario}` in a word.
fn expand(word: &str, script_dir: &Path) -> Result<String, String> {
    let mut out = word.replace("{scenario}", &script_dir.to_string_lossy());
    if out.contains("{fixtures}") {
        let root = headless::fixture_root()?;
        out = out.replace("{fixtures}", &root.join("fixtures").to_string_lossy());
    }
    Ok(out)
}

/// A script's commands: (line number, words).
pub fn parse_script(path: &Path) -> Result<Vec<(usize, Vec<String>)>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let dir = path
        .parent()
        .map(|d| std::path::absolute(d).unwrap_or_else(|_| d.to_owned()))
        .unwrap_or_default();
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let words = tokenize(line).map_err(|e| format!("{}:{}: {e}", path.display(), i + 1))?;
        if let Some(words) = words {
            let words = words
                .iter()
                .map(|w| expand(w, &dir))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("{}:{}: {e}", path.display(), i + 1))?;
            out.push((i + 1, words));
        }
    }
    Ok(out)
}

/// A daemon for one deterministic run: its own socket and state, removed afterwards.
pub(crate) struct Private {
    root: PathBuf,
    socket: PathBuf,
    state: PathBuf,
    child: Child,
}

static RUNS: AtomicU32 = AtomicU32::new(0);

impl std::fmt::Debug for Private {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Private")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl Private {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn socket(&self) -> &Path {
        &self.socket
    }

    pub(crate) fn start(exe: &Path) -> io::Result<Private> {
        let n = RUNS.fetch_add(1, Ordering::Relaxed);
        // Short: socket paths are limited to 104 bytes on macOS.
        let root = std::env::temp_dir().join(format!("auk-sc-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root)?;
        let root = root.canonicalize()?;
        let socket = root.join("daemon.sock");
        let state = root.join("state");
        std::fs::create_dir_all(&state)?;
        let log = std::fs::File::create(root.join("daemon.log"))?;
        let child = std::process::Command::new(exe)
            .arg("daemon")
            .env(auk_daemon::paths::DAEMON_SOCKET_ENV, &socket)
            .env(auk_daemon::paths::STATE_DIR_ENV, &state)
            .env(auk_daemon::paths::CONFIG_DIR_ENV, &state)
            .env_remove("XDG_RUNTIME_DIR")
            .env_remove("AUK_PANE")
            .env_remove("AUK_WINDOW_KEY")
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()?;
        let private = Private {
            root,
            socket,
            state,
            child,
        };
        let control = auk_daemon::paths::control_socket(&private.socket);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(mut c) = Control::connect(&control)
                && c.request(&["ping"]).is_ok()
            {
                return Ok(private);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other(format!(
                    "the private daemon didn't answer; its log:\n{}",
                    private.log()
                )));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.root.join("daemon.log")).unwrap_or_default()
    }

    pub(crate) fn env(&self) -> Vec<(String, String)> {
        let s = |p: &Path| p.to_string_lossy().into_owned();
        vec![
            (auk_daemon::paths::DAEMON_SOCKET_ENV.into(), s(&self.socket)),
            (auk_daemon::paths::STATE_DIR_ENV.into(), s(&self.state)),
            (auk_daemon::paths::CONFIG_DIR_ENV.into(), s(&self.state)),
        ]
    }

    pub(crate) fn stop(&mut self) {
        let control = auk_daemon::paths::control_socket(&self.socket);
        if let Ok(mut c) = Control::connect(&control) {
            let _ = c.request(&["shutdown"]);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Drop for Private {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spec_from(size: Option<&str>, appearance: Option<&str>) -> Result<WindowSpec, String> {
    let mut spec = WindowSpec::default();
    if let Some(size) = size {
        spec.parse_size(size)?;
    }
    if let Some(a) = appearance {
        spec.dark = a == "dark";
    }
    Ok(spec)
}

fn default_out() -> PathBuf {
    PathBuf::from("target/shots/auk")
}

/// Starts a deterministic window on a private daemon.
fn window_on(
    private: &Private,
    exe: &Path,
    spec: WindowSpec,
    out: PathBuf,
    on_quit: Option<Arc<dyn Fn() + Send + Sync>>,
) -> io::Result<headless::Started> {
    headless::start(&headless::Options {
        socket: private.socket.clone(),
        control: private.root.join("window.ctl"),
        window_key: auk_daemon::server::DEFAULT_WINDOW_KEY.to_owned(),
        size: None,
        exe: exe.to_owned(),
        state_dir: private.state.clone(),
        remote: None,
        scenario: Some(headless::Scenario {
            spec,
            out,
            plugin_env: private.env(),
            on_quit,
        }),
    })
}

/// `auk shot` options.
#[derive(Debug)]
pub struct ShotOptions {
    pub exe: PathBuf,
    pub scenarios: Vec<PathBuf>,
    pub out: Option<PathBuf>,
    pub size: Option<String>,
    pub appearance: Option<String>,
    pub json: bool,
}

/// Runs scenario scripts (`auk shot`). Exits 1 at the first failing command.
pub fn shot(opts: &ShotOptions) -> ExitCode {
    let spec = match spec_from(opts.size.as_deref(), opts.appearance.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("auk shot: {e}");
            return ExitCode::from(2);
        }
    };
    let out = opts.out.clone().unwrap_or_else(default_out);
    let mut scripts = Vec::new();
    for s in &opts.scenarios {
        if s.is_dir() {
            let mut found: Vec<PathBuf> = std::fs::read_dir(s)
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e == "auk"))
                .collect();
            found.sort();
            scripts.extend(found);
        } else {
            scripts.push(s.clone());
        }
    }
    let mut stdout = io::stdout().lock();
    for script in scripts {
        let name = script
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "scenario".into());
        match run_script(&opts.exe, &script, spec, out.join(&name)) {
            Ok(replies) => {
                if opts.json {
                    let _ = writeln!(
                        stdout,
                        "{}",
                        serde_json::json!({"scenario": name, "shots": replies})
                    );
                } else {
                    for r in replies {
                        let _ = writeln!(stdout, "{}", r["png"].as_str().unwrap_or_default());
                    }
                }
            }
            Err(e) => {
                eprintln!("auk shot: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

/// Runs one script in a fresh private daemon; returns the `shot` replies.
fn run_script(
    exe: &Path,
    script: &Path,
    spec: WindowSpec,
    out: PathBuf,
) -> Result<Vec<Value>, String> {
    let commands = parse_script(script)?;
    let private = Private::start(exe).map_err(|e| e.to_string())?;
    let window = window_on(&private, exe, spec, out, None).map_err(|e| e.to_string())?;
    let mut shots = Vec::new();
    for (line, words) in commands {
        if words[0] == "quit" {
            break;
        }
        let reply = window.command(&words);
        if reply["ok"] != true {
            let msg = reply["error"].as_str().unwrap_or("failed").to_owned();
            window.detach();
            return Err(format!(
                "{}:{line}: `{}`: {msg}\nthe daemon's log:\n{}",
                script.display(),
                words.join(" "),
                private.log()
            ));
        }
        if words[0] == "shot" {
            shots.push(reply["value"].clone());
        }
    }
    window.detach();
    drop(private);
    Ok(shots)
}

/// `auk serve` options.
#[derive(Debug)]
pub struct ServeOptions {
    pub exe: PathBuf,
    pub control: PathBuf,
    pub out: Option<PathBuf>,
    pub size: Option<String>,
    pub appearance: Option<String>,
}

/// A deterministic headless window on a private daemon until `quit` (`auk serve`).
pub fn serve(opts: &ServeOptions) -> ExitCode {
    let spec = match spec_from(opts.size.as_deref(), opts.appearance.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("auk serve: {e}");
            return ExitCode::from(2);
        }
    };
    let private = match Private::start(&opts.exe) {
        Ok(p) => Arc::new(Mutex::new(Some(p))),
        Err(e) => {
            eprintln!("auk serve: {e}");
            return ExitCode::FAILURE;
        }
    };
    let control = opts.control.clone();
    let on_quit: Arc<dyn Fn() + Send + Sync> = {
        let private = private.clone();
        let control = control.clone();
        Arc::new(move || {
            let _ = std::fs::remove_file(&control);
            if let Some(mut p) = private.lock().unwrap_or_else(|e| e.into_inner()).take() {
                p.stop();
            }
        })
    };
    let window = {
        let guard = private.lock().unwrap_or_else(|e| e.into_inner());
        let p = guard.as_ref().expect("started above");
        window_on(
            p,
            &opts.exe,
            spec,
            opts.out.clone().unwrap_or_else(default_out),
            Some(on_quit.clone()),
        )
    };
    let window = match window {
        Ok(w) => w,
        Err(e) => {
            eprintln!("auk serve: {e}");
            on_quit();
            return ExitCode::FAILURE;
        }
    };
    if let Some(dir) = control.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = window.serve(&control) {
        eprintln!("auk serve: {}: {e}", control.display());
        on_quit();
        return ExitCode::FAILURE;
    }
    println!("ready {}", control.display());
    let result = window.join();
    on_quit();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("auk serve: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `auk ctl`: sends one command, or a script's commands one by one. Prints each reply; exits 1
/// at the first failed one.
pub fn ctl(control: &Path, file: Option<&Path>, command: &[String]) -> ExitCode {
    let mut c = match Control::connect(control) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("auk ctl: {}: {e}", control.display());
            return ExitCode::FAILURE;
        }
    };
    let commands: Vec<(usize, Vec<String>)> = match file {
        Some(path) => match parse_script(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("auk ctl: {e}");
                return ExitCode::from(2);
            }
        },
        None if command.is_empty() => {
            eprintln!("auk ctl: expected a command or --file");
            return ExitCode::from(2);
        }
        // The words joined into one command line, which the window parses as a script line:
        // quote what it should keep together (`auk ctl expect '"On branch"'`).
        None => vec![(1, Vec::new())],
    };
    let single = command.join(" ");
    let mut out = io::stdout().lock();
    for (number, words) in commands {
        let text = if file.is_some() {
            words
                .iter()
                .map(|w| quote_word(w))
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            single.clone()
        };
        let first = text
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned();
        let reply = match c.send_line(&text) {
            Ok(r) => r,
            Err(e) if first == "quit" && e.kind() == io::ErrorKind::UnexpectedEof => {
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("auk ctl: line {number}: {e}");
                return ExitCode::FAILURE;
            }
        };
        let _ = writeln!(out, "{reply}");
        if reply["ok"] != true {
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_script_lines() {
        let t = |s: &str| tokenize(s).unwrap();
        assert_eq!(t("   # a comment"), None);
        assert_eq!(t(""), None);
        assert_eq!(
            t(r#"run "git status"  # trailing"#),
            Some(vec!["run".to_owned(), "git status".to_owned()])
        );
        assert_eq!(
            t(r#"feed "a\nb\t\"c\" \\ \e[1m""#),
            Some(vec!["feed".to_owned(), "a\nb\t\"c\" \\ \x1b[1m".to_owned()])
        );
        assert_eq!(
            t("expect 'it''s'"),
            Some(vec!["expect".to_owned(), "its".to_owned()])
        );
        assert_eq!(
            t(r#"a b"c d"e"#),
            Some(vec!["a".to_owned(), "bc de".to_owned()])
        );
        assert!(tokenize(r#"run "open"#).is_err());
    }

    #[test]
    fn quoted_words_come_back() {
        for w in [
            "plain",
            "two words",
            "a\nb\tc\x1b[1m",
            "it's \"x\" $HOME `y` \\",
            "",
        ] {
            let line = format!("cmd {}", quote_word(w));
            assert!(!line.contains('\n'));
            assert_eq!(
                tokenize(&line).unwrap().unwrap(),
                vec!["cmd".to_owned(), w.to_owned()]
            );
        }
    }

    #[test]
    fn expands_placeholders() {
        assert_eq!(
            expand("{scenario}/x.jsonl", Path::new("/s")).unwrap(),
            "/s/x.jsonl"
        );
    }
}
