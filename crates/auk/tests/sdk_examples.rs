//! The SDK examples (GOAL.md row `sdk_examples`, spec §21 suite 2, AC5): `table`, `ask`, `progress`
//! and `chat` from the pinned tern-sdk, in Python, Rust, TypeScript and Go, each run in a daemon
//! pane with a headless window as the drawing (acking) primary, each completing a scripted
//! interaction (the scenarios of `fixtures/scenarios/`). Each also prints plain text in a terminal
//! without TSP (AC3): the same pane setup with TSP switched off.
//!
//! Comparisons are on decoded JSON: every SDK records the logical messages it sent and received
//! (`TERN_TSP_RECORD`); replaying the program's own record of what it sent into a fresh replica
//! must give exactly the documents the daemon's replica holds, and what the program received
//! decodes as the expected replies and events.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use auk_conformance::harness::{Headless, TestEnv, quote, wait_for};
use auk_conformance::probe::{repo_root, require_program};
use auk_replica::{Item, Record, Replica, Role, Settings};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lang {
    Python,
    Rust,
    TypeScript,
    Go,
}

fn sdk() -> PathBuf {
    repo_root().join("third_party/tern-sdk")
}

/// The target dir for artifacts the tests build (never left over: cargo and go rebuild what
/// changed in every run).
fn build_dir(what: &str) -> PathBuf {
    let dir = repo_root().join("target/sdk-examples").join(what);
    std::fs::create_dir_all(&dir).expect("create the build dir");
    dir
}

fn run(cmd: &mut Command, what: &str) {
    let out = cmd.output().unwrap_or_else(|e| panic!("{what}: {e}"));
    assert!(
        out.status.success(),
        "{what} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Builds the Rust SDK's examples in this run (`cargo build --examples`).
fn rust_examples() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let target = build_dir("rust");
        run(
            Command::new(require_program("cargo"))
                .args(["build", "--examples", "--manifest-path"])
                .arg(sdk().join("rust/Cargo.toml"))
                .arg("--target-dir")
                .arg(&target),
            "cargo build --examples in third_party/tern-sdk/rust",
        );
        target.join("debug/examples")
    })
}

/// Builds a Go example in this run (`go build ./examples/<name>` in the SDK's module).
fn go_example(name: &str) -> PathBuf {
    let out = build_dir("go").join(name);
    let mut cmd = Command::new(require_program("go"));
    cmd.args(["build", "-o"])
        .arg(&out)
        .arg(format!("./examples/{name}"))
        .current_dir(sdk().join("go/tern"))
        .env("GOCACHE", build_dir("go-cache"))
        .env("GOFLAGS", "-mod=mod")
        .env("GOPROXY", "off");
    run(&mut cmd, &format!("go build ./examples/{name}"));
    out
}

/// Installs the TypeScript SDK's dependencies when they're missing (`bun install`).
fn bun_ready() -> PathBuf {
    let bun = require_program("bun");
    let ts = sdk().join("typescript");
    if !ts.join("node_modules").is_dir() {
        run(
            Command::new(&bun).arg("install").current_dir(&ts),
            "bun install in third_party/tern-sdk/typescript",
        );
    }
    bun
}

/// The shell words that run `example` in `lang`.
fn command(lang: Lang, example: &str) -> Vec<String> {
    let s = |p: &Path| p.to_string_lossy().into_owned();
    match lang {
        Lang::Python => vec![
            "env".into(),
            format!("PYTHONPATH={}", s(&sdk().join("python/src"))),
            s(&require_program("python3")),
            s(&sdk().join(format!("python/examples/{example}.py"))),
        ],
        Lang::Rust => vec![s(&rust_examples().join(example))],
        // Bun reads the SDK's tsconfig (its JSX runtime) from the working directory.
        Lang::TypeScript => vec![s(&bun_ready()), format!("examples/{example}.tsx")],
        Lang::Go => vec![s(&go_example(example))],
    }
}

/// One example run in a daemon pane.
struct Run {
    env: TestEnv,
    window: Headless,
    pane: u64,
    record: PathBuf,
}

impl Run {
    fn start(lang: Lang, example: &str) -> Run {
        let cmd = command(lang, example);
        let env = TestEnv::new(env!("CARGO_BIN_EXE_auk"), &format!("{lang:?}{example}"));
        env.start_daemon();
        let window = env.start_headless("w", &[]);
        let record = env.root.join("record.jsonl");
        let line = format!(
            "env TERN_TSP_RECORD={} {}",
            quote(&record.to_string_lossy()),
            cmd.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ")
        );
        let pane = new_pane(&env, lang, &line);
        Run {
            env,
            window,
            pane,
            record,
        }
    }

    fn state(&self) -> Value {
        self.env.daemon(&format!("tsp {}", self.pane))
    }

    /// Waits until the live surface's document holds a node matching `pred`; returns its id.
    fn wait_node(&self, what: &str, pred: impl Fn(&Value) -> bool) -> (String, String) {
        wait_for(what, || {
            let st = self.state();
            for s in st["surfaces"].as_array()? {
                if let Some(n) = find(&s["doc"], &pred) {
                    return Some((s["id"].as_str()?.to_owned(), n["id"].as_str()?.to_owned()));
                }
            }
            None
        })
    }

    fn click(&self, sf: &str, node: &str) -> Value {
        self.window.request(&format!(
            "click {} {} {}",
            self.pane,
            quote(sf),
            quote(node)
        ))
    }

    fn send(&self, how: &str, what: &str) {
        self.window
            .request(&format!("send {} {how} {what}", self.pane));
    }

    fn wait_exit(&self) -> Value {
        self.env.wait(self.pane, "exit")
    }

    fn screen(&self) -> String {
        self.env
            .daemon(&format!("capture {} --scrollback", self.pane))["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    /// Checks the run on decoded JSON: the program's own record of what it sent, replayed into
    /// a fresh replica, gives exactly the daemon's documents; the hello reply it read lists the
    /// T0 and T1 kinds; and, when it waits for them, it read acks for its frames. Returns the
    /// record.
    fn check_record(&self, acked: bool) -> Vec<Value> {
        let text = std::fs::read_to_string(&self.record)
            .unwrap_or_else(|e| panic!("the SDK's recording {}: {e}", self.record.display()));
        let lines: Vec<Value> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("a JSONL record"))
            .collect();
        let st = self.state();
        let cols = st["surfaces"]
            .as_array()
            .and_then(|_| self.env.pane_info(self.pane)["cols"].as_u64())
            .unwrap_or(80) as u16;
        let mut replica = Replica::new(cols, 24, Settings::default(), Role::Follower);
        for line in lines.iter().filter(|l| l["dir"] == "out") {
            let verb = line["verb"].as_str().unwrap();
            let params: Vec<(String, String)> = line["params"]
                .as_object()
                .map(|p| {
                    p.iter()
                        .map(|(k, v)| {
                            (
                                k.clone(),
                                v.as_str().map_or_else(|| v.to_string(), str::to_owned),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            let params: Vec<(&str, &str)> = params
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let body = match &line["body"] {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            // The SDK's own encoder, chunking at the advertised size.
            let bytes = tern_sdk::wire::encode(verb, &params, body.as_bytes(), 65_536, "r");
            replica.push(Item::Output(&bytes));
        }
        replica.push(Item::Record(&Record::Exit { status: 0 }));
        let replayed = auk_daemon::tsp::state_json(replica.tsp(), replica.terminal());
        assert_eq!(
            documents(&replayed),
            documents(&st),
            "the documents differ from what the program says it sent"
        );

        let ins: Vec<&Value> = lines.iter().filter(|l| l["dir"] == "in").collect();
        let reply = ins
            .iter()
            .find(|l| l["body"]["r"] == "hello")
            .unwrap_or_else(|| panic!("the program never read a hello reply: {ins:?}"));
        // T0 and T1 (§8.2, D24).
        assert_eq!(reply["body"]["kinds"].as_array().unwrap().len(), 38);
        let frames = lines
            .iter()
            .filter(|l| l["dir"] == "out" && l["verb"] == "f")
            .count();
        if acked && frames > 0 {
            assert!(
                ins.iter().any(|l| l["body"]["ev"] == "ack"),
                "the program read no ack: {ins:?}"
            );
        }
        lines
    }
}

/// Where an example runs: the SDK's directory, where the fixtures were recorded; TypeScript runs
/// in its package, as bun needs.
fn cwd(lang: Lang) -> PathBuf {
    match lang {
        Lang::TypeScript => sdk().join("typescript"),
        _ => sdk(),
    }
}

/// Opens a pane in the example's directory running `line`.
fn new_pane(env: &TestEnv, lang: Lang, line: &str) -> u64 {
    env.daemon(&format!(
        "new --cwd {} --size 120x40 -- {line}",
        quote(&cwd(lang).to_string_lossy())
    ))["pane"]
        .as_u64()
        .expect("a pane id")
}

/// What `table` lists in the example's directory.
fn listed(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::TypeScript => &["examples", "src", "package.json", "README.md"],
        _ => &["python", "rust", "typescript", "README.md", "LICENSE"],
    }
}

/// The surfaces' replicated documents, without replica-local keys and anchors.
fn documents(state: &Value) -> Vec<Value> {
    state["surfaces"]
        .as_array()
        .map(|all| {
            all.iter()
                .map(|s| {
                    json!({
                        "id": s["id"], "mode": s["mode"], "state": s["state"], "listen": s["listen"],
                        "title": s["title"], "role": s["role"], "palette": s["palette"],
                        "sheets": s["sheets"], "last_s": s["last_s"], "focus": s["focus"],
                        "settled": s["settled"], "suspended": s["suspended"], "doc": s["doc"],
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn find<'a>(node: &'a Value, pred: &dyn Fn(&Value) -> bool) -> Option<&'a Value> {
    if pred(node) {
        return Some(node);
    }
    node["c"].as_array()?.iter().find_map(|c| find(c, pred))
}

/// All text in a document, depth first.
fn texts(node: &Value, out: &mut String) {
    if let Some(t) = node["p"]["text"].as_str() {
        out.push_str(t);
        out.push('\n');
    }
    for c in node["c"].as_array().into_iter().flatten() {
        texts(c, out);
    }
}

fn all_text(state: &Value) -> String {
    let mut out = String::new();
    for s in state["surfaces"].as_array().into_iter().flatten() {
        texts(&s["doc"], &mut out);
    }
    out
}

fn el_tag(n: &Value) -> Option<&str> {
    (n["k"] == "el").then(|| n["p"]["tag"].as_str()).flatten()
}

// ---------------------------------------------------------------------------------------------
// The scripted interactions

/// `table`: the SDK directory as a native table, kept in the scrollback.
fn table(lang: Lang) {
    let run = Run::start(lang, "table");
    run.wait_exit();
    // `show` writes its frame and closes without waiting for an ack.
    run.check_record(false);
    let st = run.state();
    let text = all_text(&st);
    for entry in listed(lang) {
        assert!(text.contains(entry), "{entry} is listed: {text}");
    }
    assert!(
        find(&st["surfaces"][0]["doc"], &|n| el_tag(n) == Some("table")).is_some(),
        "an el table"
    );
    assert!(
        !st["surfaces"][0]["sheets"].as_array().unwrap().is_empty(),
        "with its stylesheet"
    );
    assert_eq!(
        st["surfaces"][0]["state"], "closed",
        "kept in the scrollback"
    );
    plain(lang, "table", &[], listed(lang));
}

/// `ask`: pick Large in the form and press Create (the window's clicks give the form values).
fn ask(lang: Lang) {
    let run = Run::start(lang, "ask");
    let (sf, radio) = run.wait_node("the Large radio", |n| {
        el_tag(n) == Some("input") && n["p"]["value"] == "l"
    });
    let (_, button) = run.wait_node("the Create button", |n| el_tag(n) == Some("button"));
    let change = run.click(&sf, &radio);
    assert_eq!(change["ev"], "change", "{change}");
    assert_eq!(change["values"], json!({"size": "l"}));
    let submit = run.click(&sf, &button);
    assert_eq!(submit["act"], "submit", "{submit}");
    assert_eq!(submit["values"], json!({"size": "l"}));
    run.wait_exit();
    let lines = run.check_record(true);
    let events: Vec<&Value> = lines
        .iter()
        .filter(|l| {
            l["dir"] == "in"
                && l["verb"] == "e"
                && l["body"]["ev"] != "ack"
                && l["body"]["ev"] != "resize"
        })
        .map(|l| &l["body"])
        .collect();
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0]["ev"], "change");
    assert_eq!(
        events[1],
        &json!({"ev": "action", "sf": sf, "id": button, "act": "submit", "values": {"size": "l"}})
    );
    let screen = run.screen();
    assert!(
        screen.contains("Large") || screen.contains("size: l"),
        "the program printed the answer: {screen}"
    );
    // Each SDK words the answer its own way; all of them name the size.
    let screen = plain(lang, "ask", &["text l", "keys Enter"], &[]);
    assert!(
        screen.contains("Large") || screen.contains("size: l"),
        "{screen}"
    );
}

/// `progress`: a flow surface that runs to the end.
fn progress(lang: Lang) {
    let run = Run::start(lang, "progress");
    run.wait_exit();
    let lines = run.check_record(true);
    let frames = lines
        .iter()
        .filter(|l| l["dir"] == "out" && l["verb"] == "f")
        .count();
    assert!(frames >= 5, "it streamed its steps: {frames} frames");
    let st = run.state();
    let surface = &st["surfaces"][0];
    assert_eq!(surface["mode"], "flow");
    let bar = find(&surface["doc"], &|n| n["k"] == "progress").expect("a progress bar");
    let value = bar["p"]["value"].as_f64().unwrap_or(0.0);
    assert!((value - 1.0).abs() < 1e-9, "it finished: {bar}");
    let screen = plain(lang, "progress", &[], &[]);
    let lines = screen.lines().filter(|l| !l.trim().is_empty()).count();
    assert!(lines >= 5, "the steps print as plain lines: {screen}");
}

/// `chat`: two messages typed into the native editor, then Escape.
fn chat(lang: Lang) {
    let run = Run::start(lang, "chat");
    run.wait_node("the editor", |n| n["k"] == "editor");
    for message in ["Hello from the recorder", "A second message"] {
        run.send("text", &quote(message));
        run.wait_node("the typed draft", |n| {
            n["k"] == "editor" && n["p"]["text"] == message
        });
        run.send("keys", "Enter");
        run.wait_node("the sent message", |n| {
            n["k"] == "md" && n["p"]["text"].as_str().is_some_and(|t| t.contains(message))
        });
    }
    run.send("keys", "Esc");
    run.wait_exit();
    run.check_record(true);
    let st = run.state();
    let surface = &st["surfaces"][0];
    assert_eq!(
        (surface["mode"].clone(), surface["state"].clone()),
        (json!("inline"), json!("closed"))
    );
    let text = all_text(&st);
    assert!(
        text.contains("Hello from the recorder") && text.contains("A second message"),
        "{text}"
    );
    plain(lang, "chat", &["text hi", "keys Enter", "keys C-d"], &[]);
}

/// The example in a terminal without TSP: Auk with TSP switched off. It must print plain text
/// (the SDK falls back, and no TSP string is left on screen); `input` is typed, then each
/// `expect` must appear. Returns what the pane shows.
fn plain(lang: Lang, example: &str, input: &[&str], expect: &[&str]) -> String {
    let env = TestEnv::new(
        env!("CARGO_BIN_EXE_auk"),
        &format!("plain{lang:?}{example}"),
    );
    env.start_daemon();
    env.daemon("settings --native-surfaces off");
    let cmd = command(lang, example);
    let record = env.root.join("record.jsonl");
    let line = format!(
        "env TERN_TSP_RECORD={} {}",
        quote(&record.to_string_lossy()),
        cmd.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ")
    );
    let pane = new_pane(&env, lang, &line);
    if !input.is_empty() {
        // Let the program find there's no TSP before it reads its input.
        std::thread::sleep(Duration::from_millis(1500));
        for words in input {
            // A program that doesn't read input in plain mode may have exited already.
            env.control().send(&format!("send {pane} {words}"));
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    env.wait(pane, "exit");
    let screen = env.daemon(&format!("capture {pane} --scrollback"))["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        !screen.trim().is_empty(),
        "{lang:?} {example} printed nothing"
    );
    for e in expect {
        assert!(screen.contains(e), "{lang:?} {example}: {e:?} in {screen}");
    }
    assert!(!screen.contains("tsp;"), "{screen}");
    let st = env.daemon(&format!("tsp {pane}"));
    assert!(
        st["surfaces"].as_array().unwrap().is_empty(),
        "no surfaces: {st}"
    );
    // The program decided there's no TSP: it never sent an `o` or `f`.
    if let Ok(text) = std::fs::read_to_string(&record) {
        assert!(
            !text
                .lines()
                .any(|l| l.contains(r#""verb":"o""#) || l.contains(r#""verb":"f""#)),
            "{text}"
        );
    }
    screen
}

macro_rules! examples {
    ($($name:ident => $lang:expr, $example:ident;)*) => {
        $(
            #[test]
            fn $name() {
                $example($lang);
            }
        )*
    };
}

examples! {
    ac05_python_table => Lang::Python, table;
    ac05_python_ask => Lang::Python, ask;
    ac05_python_progress => Lang::Python, progress;
    ac05_python_chat => Lang::Python, chat;
    ac05_rust_table => Lang::Rust, table;
    ac05_rust_ask => Lang::Rust, ask;
    ac05_rust_progress => Lang::Rust, progress;
    ac05_rust_chat => Lang::Rust, chat;
    ac05_typescript_table => Lang::TypeScript, table;
    ac05_typescript_ask => Lang::TypeScript, ask;
    ac05_typescript_progress => Lang::TypeScript, progress;
    ac05_typescript_chat => Lang::TypeScript, chat;
    ac05_go_table => Lang::Go, table;
    ac05_go_ask => Lang::Go, ask;
    ac05_go_progress => Lang::Go, progress;
    ac05_go_chat => Lang::Go, chat;
}
