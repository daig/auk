//! A scriptable program in a daemon pane (`tools/pty_probe.py`): a test decides exactly what the
//! program writes and how its tty is set, and sees exactly the bytes that reach the PTY, through
//! the real daemon, replica and mode gate.
//!
//! ```text
//! let env = TestEnv::new(env!("CARGO_BIN_EXE_auk"), "b1");
//! env.start_daemon();
//! let probe = Probe::start(&env, &[]);
//! probe.write(&hello(&[]));
//! let input = probe.fence();
//! ```

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};

use crate::harness::{TestEnv, WAIT, quote};

/// What the probe reports.
#[derive(Debug)]
enum FromProbe {
    Read(Vec<u8>),
    Pong(u64),
}

/// A pane running the probe.
#[derive(Debug)]
pub struct Probe {
    /// The daemon's pane id.
    pub pane: u64,
    /// The probe's pid.
    pub pid: u32,
    writer: Mutex<UnixStream>,
    rx: Mutex<Receiver<FromProbe>>,
    /// Bytes read but not yet consumed by a fence or read.
    pending: Mutex<Vec<u8>>,
    next_ping: Mutex<u64>,
}

/// The probe script.
pub fn probe_script() -> PathBuf {
    repo_root().join("tools/pty_probe.py")
}

/// The repository root.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root")
}

/// The absolute path of a program on `PATH`, or a panic naming the missing prerequisite.
pub fn require_program(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("prerequisite missing: `{name}` is not on PATH"))
}

impl Probe {
    /// Opens a pane running the probe (`new` plus `args`, e.g. `--size 100x30`), waits for it to
    /// connect, and puts its tty in raw mode (non-canonical, echo off), as TSP programs do.
    pub fn start(env: &TestEnv, args: &[&str]) -> Probe {
        let probe = Self::start_cooked(env, args);
        probe.tty("raw");
        probe
    }

    /// Like [`start`](Self::start), leaving the tty as the pane started it (canonical, echo on).
    pub fn start_cooked(env: &TestEnv, args: &[&str]) -> Probe {
        Self::launch(env, |command| {
            let mut words: Vec<&str> = args.to_vec();
            words.push("--");
            words.extend(command.iter().map(String::as_str));
            env.new_pane(&words)
        })
    }

    /// Runs the probe as a command in a shell pane (typed at its prompt), in raw mode. When it
    /// exits, the shell prints its next prompt.
    pub fn run_in_shell(env: &TestEnv, shell: u64) -> Probe {
        let probe = Self::launch(env, |command| {
            let line: Vec<String> = command.iter().map(|w| quote(w)).collect();
            let line = line.join(" ");
            env.daemon(&format!("run {shell} {}", quote(&line)));
            shell
        });
        probe.tty("raw");
        probe
    }

    fn launch(env: &TestEnv, spawn: impl FnOnce(&[String]) -> u64) -> Probe {
        let python = require_program("python3");
        let sock = env.root.join(format!("probe-{}.sock", next_id()));
        let listener = UnixListener::bind(&sock).expect("bind the probe socket");
        let command = vec![
            python.to_string_lossy().into_owned(),
            "-I".to_owned(),
            probe_script().to_string_lossy().into_owned(),
            sock.to_string_lossy().into_owned(),
        ];
        let pane = spawn(&command);

        listener
            .set_nonblocking(true)
            .expect("a nonblocking listener");
        let deadline = Instant::now() + WAIT;
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "the probe in pane {pane} never connected: {}",
                        env.daemon(&format!("capture {pane} --scrollback"))
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("accept the probe: {e}"),
            }
        };
        stream.set_nonblocking(false).expect("a blocking stream");
        let mut reader = BufReader::new(stream.try_clone().expect("clone the probe stream"));
        let mut line = String::new();
        reader.read_line(&mut line).expect("the probe's hello");
        let hello: Value = serde_json::from_str(&line).expect("the probe's hello is JSON");
        let pid = hello["hello"]["pid"].as_u64().expect("the probe's pid") as u32;

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let b64 = base64::engine::general_purpose::STANDARD;
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    break;
                };
                let item = if let Some(data) = msg["read"].as_str() {
                    FromProbe::Read(b64.decode(data).unwrap_or_default())
                } else if let Some(n) = msg["pong"].as_u64() {
                    FromProbe::Pong(n)
                } else {
                    continue;
                };
                if tx.send(item).is_err() {
                    break;
                }
            }
        });
        Probe {
            pane,
            pid,
            writer: Mutex::new(stream),
            rx: Mutex::new(rx),
            pending: Mutex::new(Vec::new()),
            next_ping: Mutex::new(0),
        }
    }

    fn send(&self, msg: Value) {
        let mut w = self.writer.lock().unwrap();
        writeln!(w, "{msg}").expect("write to the probe");
    }

    /// The program writes `bytes` to its stdout (the pane's output), in one write.
    pub fn write(&self, bytes: &[u8]) {
        let b64 = base64::engine::general_purpose::STANDARD;
        self.send(json!({"write": b64.encode(bytes)}));
        self.sync();
    }

    /// Sets the tty: `raw` (non-canonical, echo off), `raw-echo` or `cooked`.
    pub fn tty(&self, mode: &str) {
        self.send(json!({"tty": mode}));
        self.sync();
    }

    /// The program exits with `code`.
    pub fn exit(&self, code: i32) {
        self.send(json!({"exit": code}));
    }

    /// Waits until the probe has done everything sent before.
    pub fn sync(&self) {
        let n = {
            let mut next = self.next_ping.lock().unwrap();
            *next += 1;
            *next
        };
        self.send(json!({"ping": n}));
        let deadline = Instant::now() + WAIT;
        loop {
            match self.recv(deadline) {
                Some(FromProbe::Pong(m)) if m == n => return,
                Some(FromProbe::Pong(_)) => {}
                Some(FromProbe::Read(bytes)) => self.pending.lock().unwrap().extend(bytes),
                None => panic!("the probe in pane {} stopped answering", self.pane),
            }
        }
    }

    fn recv(&self, deadline: Instant) -> Option<FromProbe> {
        let left = deadline.saturating_duration_since(Instant::now());
        self.rx.lock().unwrap().recv_timeout(left).ok()
    }

    /// Everything the terminal wrote to the program within `wait`.
    pub fn read_for(&self, wait: Duration) -> Vec<u8> {
        let deadline = Instant::now() + wait;
        while let Some(item) = self.recv(deadline) {
            if let FromProbe::Read(bytes) = item {
                self.pending.lock().unwrap().extend(bytes);
            }
        }
        std::mem::take(&mut *self.pending.lock().unwrap())
    }

    /// Reads until `done` holds for the input so far (within [`WAIT`]); returns all of it.
    pub fn read_until(&self, what: &str, done: impl Fn(&[u8]) -> bool) -> Vec<u8> {
        let deadline = Instant::now() + WAIT;
        loop {
            {
                let pending = self.pending.lock().unwrap();
                if done(&pending) {
                    break;
                }
            }
            match self.recv(deadline) {
                Some(FromProbe::Read(bytes)) => self.pending.lock().unwrap().extend(bytes),
                Some(FromProbe::Pong(_)) => {}
                None => panic!(
                    "timed out waiting for {what}; the program read {:?}",
                    String::from_utf8_lossy(&self.pending.lock().unwrap())
                ),
            }
        }
        std::mem::take(&mut *self.pending.lock().unwrap())
    }

    /// Reads until the terminal has sent `n` TSP messages; returns them decoded, with the rest.
    pub fn read_tsp(&self, n: usize) -> Vec<Input> {
        let input = self.read_until(&format!("{n} TSP messages"), |b| {
            split_input(b).iter().filter(|i| i.is_tsp()).count() >= n
        });
        split_input(&input)
    }

    /// The stream fence: writes DA1 and reads until its answer, which the terminal writes after
    /// everything the earlier output caused. Returns what came before the answer. Needs the tty in
    /// non-canonical mode.
    pub fn fence(&self) -> Vec<Input> {
        self.write(b"\x1b[c");
        let input = self.read_until("the DA1 answer", |b| {
            split_input(b).iter().any(|i| matches!(i, Input::Da1(_)))
        });
        let mut items = split_input(&input);
        let at = items
            .iter()
            .rposition(|i| matches!(i, Input::Da1(_)))
            .expect("a DA1 answer");
        items.truncate(at);
        items
    }

    /// The TSP messages the terminal wrote before the fence, as JSON bodies.
    pub fn fence_tsp(&self) -> Vec<Value> {
        self.fence()
            .into_iter()
            .filter_map(|i| match i {
                Input::Tsp { body, .. } => Some(body),
                _ => None,
            })
            .collect()
    }
}

fn next_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

/// One thing the terminal wrote to the program.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// `ESC _ tsp;<verb>;<json> ESC \`.
    Tsp {
        verb: String,
        body: Value,
        raw: Vec<u8>,
    },
    /// A DA1 answer, `ESC [ ? … c`.
    Da1(Vec<u8>),
    /// Anything else.
    Other(Vec<u8>),
}

impl Input {
    pub fn is_tsp(&self) -> bool {
        matches!(self, Input::Tsp { .. })
    }

    /// The JSON body of a TSP message.
    pub fn body(&self) -> Option<&Value> {
        match self {
            Input::Tsp { body, .. } => Some(body),
            _ => None,
        }
    }
}

/// Splits what the terminal wrote into TSP messages, DA1 answers and other bytes. A TSP message
/// must end with ST (F1); one that doesn't stays in the other bytes.
pub fn split_input(bytes: &[u8]) -> Vec<Input> {
    let mut out = Vec::new();
    let mut other = Vec::new();
    let mut i = 0;
    let flush = |other: &mut Vec<u8>, out: &mut Vec<Input>| {
        if !other.is_empty() {
            out.push(Input::Other(std::mem::take(other)));
        }
    };
    while i < bytes.len() {
        let rest = &bytes[i..];
        if rest.starts_with(b"\x1b_tsp;")
            && let Some(end) = find(rest, b"\x1b\\")
        {
            let payload = &rest[6..end];
            let (verb, body) = match payload.iter().position(|&b| b == b';') {
                Some(semi) => (&payload[..semi], &payload[semi + 1..]),
                None => (payload, &b""[..]),
            };
            flush(&mut other, &mut out);
            out.push(Input::Tsp {
                verb: String::from_utf8_lossy(verb).into_owned(),
                body: serde_json::from_slice(body).unwrap_or(Value::Null),
                raw: rest[..end + 2].to_vec(),
            });
            i += end + 2;
            continue;
        }
        if rest.starts_with(b"\x1b[?") {
            let n = rest[3..]
                .iter()
                .position(|b| !(b.is_ascii_digit() || *b == b';'));
            if let Some(n) = n
                && rest[3 + n] == b'c'
            {
                flush(&mut other, &mut out);
                out.push(Input::Da1(rest[..4 + n].to_vec()));
                i += 4 + n;
                continue;
            }
        }
        other.push(bytes[i]);
        i += 1;
    }
    flush(&mut other, &mut out);
    out
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The JSON bodies of the TSP messages in `items`.
pub fn tsp_bodies(items: &[Input]) -> Vec<Value> {
    items.iter().filter_map(|i| i.body().cloned()).collect()
}

/// The events named `ev` among `bodies`.
pub fn events<'a>(bodies: &'a [Value], ev: &str) -> Vec<&'a Value> {
    bodies
        .iter()
        .filter(|b| b.get("ev").and_then(Value::as_str) == Some(ev))
        .collect()
}

/// `ESC _ tsp;<verb>;<params…><body> ESC \` as a program writes it.
pub fn tsp(verb: &str, params: &[(&str, &str)], body: &str) -> Vec<u8> {
    let mut out = format!("\x1b_tsp;{verb};").into_bytes();
    for (k, v) in params {
        out.extend_from_slice(format!("{k}={v};").as_bytes());
    }
    out.extend_from_slice(body.as_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
}

/// A TSP message with a JSON body.
pub fn tsp_json(verb: &str, body: &Value) -> Vec<u8> {
    tsp(verb, &[], &body.to_string())
}

/// The hello query, with program features.
pub fn hello(features: &[&str]) -> Vec<u8> {
    tsp_json(
        "q",
        &json!({"q": "hello", "v": [1], "app": "probe", "ver": "1.0", "features": features}),
    )
}

/// An `o` for surface `id` in `mode`.
pub fn open(id: &str, mode: &str) -> Vec<u8> {
    tsp_json("o", &json!({"id": id, "mode": mode}))
}

/// A frame.
pub fn frame(sf: &str, s: u64, ops: Value) -> Vec<u8> {
    tsp_json("f", &json!({"sf": sf, "s": s, "ops": ops}))
}

/// Opens a surface whose `main` holds `children`, in frame 1.
pub fn open_with(id: &str, mode: &str, children: Value) -> Vec<u8> {
    let mut out = open(id, mode);
    out.extend(frame(
        id,
        1,
        json!([["add", "main", id, null, {"id": "main", "k": "col", "c": children}]]),
    ));
    out
}
