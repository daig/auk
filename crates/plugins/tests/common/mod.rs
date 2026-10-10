//! Shared harness: a temp plugins folder (with the SDK's example plugins copied in), a host, a
//! block pane that applies the worker's bytes with the real terminal and TSP engine (as the
//! daemon's replica does), and a window host that records what plugin code asks of it.

#![allow(dead_code)]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use auk_plugins::window::{
    Dir, How, Launch, Reveal, TabInfo, TabSpec, WindowConfig, WindowHost, WindowPane,
};
use auk_plugins::{
    BlockLaunch, BlockOutput, EffectOutput, HostConfig, HostOutput, LogSink, PaneTable, PluginHost,
    PluginSettings, ToastLevel,
};
use auk_term::{Config, Event, Terminal};
use auk_tsp::{Config as TspConfig, ToProgram, TspPane};
use serde_json::{Value, json};

/// The SDK's example plugins at the pin.
pub fn example_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../third_party/tern-sdk/plugins/examples")
        .join(name)
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for entry in std::fs::read_dir(from).expect("read") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy");
        }
    }
}

/// A temp config/state root with a plugins folder.
pub struct Root {
    _dir: tempfile::TempDir,
    pub path: PathBuf,
    pub logs: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Root {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().canonicalize().expect("canonical");
        std::fs::create_dir_all(path.join("plugins")).expect("mkdir");
        Root {
            _dir: dir,
            path,
            logs: Arc::default(),
        }
    }

    pub fn plugins(&self) -> PathBuf {
        self.path.join("plugins")
    }

    pub fn state(&self) -> PathBuf {
        self.path.join("state")
    }

    /// Copies an SDK example into the plugins folder.
    pub fn example(&self, name: &str) -> PathBuf {
        let to = self.plugins().join(name);
        copy_dir(&example_dir(name), &to);
        to
    }

    /// Writes a package from `(file, text)` pairs.
    pub fn package(&self, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = self.plugins().join(name);
        for (file, text) in files {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, text).expect("write");
        }
        dir
    }

    pub fn log_sink(&self) -> LogSink {
        let logs = Arc::clone(&self.logs);
        Arc::new(move |r| logs.lock().expect("logs").push(r.message.to_owned()))
    }

    pub fn host(&self, panes: Option<Arc<PaneTable>>) -> PluginHost {
        PluginHost::start(HostConfig {
            plugins_dir: self.plugins(),
            state_dir: self.state(),
            settings: PluginSettings::default(),
            waker: None,
            log: self.log_sink(),
            panes: panes.map(|p| p as Arc<dyn auk_plugins::PaneDirectory>),
        })
    }

    pub fn window_config(&self, builtin: &[&str]) -> WindowConfig {
        WindowConfig {
            plugins_dir: self.plugins(),
            state_dir: self.state(),
            settings: PluginSettings::default(),
            builtin_commands: Arc::new(builtin.iter().map(|s| (*s).to_owned()).collect()),
            log: self.log_sink(),
            waker: None,
        }
    }
}

/// Collects host outputs until `pred` returns `Some`, or panics after `within`.
pub fn wait_for<T>(
    host: &PluginHost,
    within: Duration,
    seen: &mut Vec<HostOutput>,
    mut pred: impl FnMut(&HostOutput) -> Option<T>,
) -> T {
    let deadline = Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match host.outputs().recv_timeout(left) {
            Ok(out) => {
                let hit = pred(&out);
                seen.push(out);
                if let Some(hit) = hit {
                    return hit;
                }
            }
            Err(_) => panic!("nothing matched within {within:?}; saw {seen:#?}"),
        }
    }
}

/// The next effect, skipping other outputs (kept in `seen`).
pub fn next_effect(
    host: &PluginHost,
    within: Duration,
    seen: &mut Vec<HostOutput>,
) -> EffectOutput {
    wait_for(host, within, seen, |o| o.effect().cloned())
}

/// Drains outputs that arrive within `quiet` of each other.
pub fn drain(host: &PluginHost, quiet: Duration) -> Vec<HostOutput> {
    let mut out = Vec::new();
    while let Ok(o) = host.outputs().recv_timeout(quiet) {
        out.push(o);
    }
    out
}

pub fn toast_text(e: &EffectOutput) -> Option<(ToastLevel, String, Option<String>)> {
    match &e.effect {
        auk_plugins::Effect::Toast { level, text, sub } => {
            Some((*level, text.clone(), sub.clone()))
        }
        _ => None,
    }
}

/// A block pane as the daemon's replica holds it: a terminal and a TSP engine, fed the worker's
/// bytes, writing the engine's answers back to the block's input.
pub struct BlockPane {
    pub pane: u64,
    pub term: Terminal,
    pub tsp: TspPane,
    pub saved: Option<String>,
    pub exited: Option<(i32, Option<String>)>,
    pub frames: usize,
}

impl BlockPane {
    pub fn new(pane: u64, cols: u16, rows: u16) -> Self {
        BlockPane {
            pane,
            term: Terminal::new(cols, rows, Config::default()),
            tsp: TspPane::new(TspConfig::default()),
            saved: None,
            exited: None,
            frames: 0,
        }
    }

    pub fn launch(
        &self,
        kind: &str,
        args: &[&str],
        saved: Option<String>,
        cwd: Option<&str>,
    ) -> BlockLaunch {
        BlockLaunch {
            pane: self.pane,
            kind: kind.into(),
            args: args.iter().map(|s| (*s).to_owned()).collect(),
            saved,
            cwd: cwd.map(str::to_owned),
            cols: self.term.cols(),
            rows: self.term.rows(),
        }
    }

    /// Applies one output if it is this pane's; returns whether it was.
    pub fn apply(&mut self, host: &PluginHost, out: &HostOutput) -> bool {
        let HostOutput::Block(b) = out else {
            return false;
        };
        match b {
            BlockOutput::Bytes { pane, bytes } if *pane == self.pane => {
                self.frames += bytes.windows(7).filter(|w| *w == b"\x1b_tsp;f").count();
                let mut rest: &[u8] = bytes;
                while !rest.is_empty() {
                    let (n, event) = self.term.advance(rest);
                    rest = &rest[n..];
                    if let Some(Event::Tsp(payload)) = event {
                        self.tsp.on_apc(&mut self.term, &payload);
                    } else if let Some(Event::Title(_)) = event {
                        self.tsp.on_title();
                    }
                }
                for e in self.term.take_events() {
                    if let Event::Tsp(payload) = e {
                        self.tsp.on_apc(&mut self.term, &payload);
                    }
                }
                let answers: Vec<u8> = self
                    .tsp
                    .take_output()
                    .iter()
                    .flat_map(|o| o.encode())
                    .collect();
                if !answers.is_empty() {
                    host.block_input(self.pane, &answers);
                }
                true
            }
            BlockOutput::Saved { pane, saved } if *pane == self.pane => {
                self.saved = Some(saved.clone());
                true
            }
            BlockOutput::Exited {
                pane,
                status,
                message,
            } if *pane == self.pane => {
                self.exited = Some((*status, message.clone()));
                true
            }
            _ => false,
        }
    }

    /// Applies outputs for `quiet` (other outputs are returned).
    pub fn settle(&mut self, host: &PluginHost, quiet: Duration) -> Vec<HostOutput> {
        let mut others = Vec::new();
        while let Ok(out) = host.outputs().recv_timeout(quiet) {
            if !self.apply(host, &out) {
                others.push(out);
            }
        }
        others
    }

    /// Applies outputs until `pred` holds on the pane, or panics after `within`.
    pub fn until(
        &mut self,
        host: &PluginHost,
        within: Duration,
        others: &mut Vec<HostOutput>,
        mut pred: impl FnMut(&BlockPane) -> bool,
    ) {
        let deadline = Instant::now() + within;
        while !pred(self) {
            let left = deadline.saturating_duration_since(Instant::now());
            match host.outputs().recv_timeout(left) {
                Ok(out) => {
                    if !self.apply(host, &out) {
                        others.push(out);
                    }
                }
                Err(_) => panic!(
                    "pane {} never got there; main = {}, title = {:?}, others = {others:#?}",
                    self.pane,
                    self.region("main"),
                    self.title()
                ),
            }
        }
    }

    /// The next effect, applying this pane's block output meanwhile (other outputs go to
    /// `others`).
    pub fn next_effect(
        &mut self,
        host: &PluginHost,
        within: Duration,
        others: &mut Vec<HostOutput>,
    ) -> EffectOutput {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match host.outputs().recv_timeout(left) {
                Ok(HostOutput::Effect(e)) => return e,
                Ok(out) => {
                    if !self.apply(host, &out) {
                        others.push(out);
                    }
                }
                Err(_) => panic!("no effect within {within:?}; others = {others:#?}"),
            }
        }
    }

    /// The live surface's region tree as JSON (`null` when absent).
    pub fn region(&self, name: &str) -> Value {
        self.tsp
            .surfaces()
            .iter()
            .rev()
            .find(|s| s.is_live())
            .and_then(|s| s.document().region(name).map(|r| r.to_json()))
            .unwrap_or(Value::Null)
    }

    /// The live surface's id and role.
    pub fn surface(&self) -> Option<(String, Option<String>)> {
        self.tsp
            .surfaces()
            .iter()
            .rev()
            .find(|s| s.is_live())
            .map(|s| (s.id().to_owned(), s.role().map(str::to_owned)))
    }

    pub fn title(&self) -> String {
        self.term.title().to_owned()
    }

    /// Every text in the main region, joined.
    pub fn text(&self) -> String {
        let mut out = String::new();
        collect_text(&self.region("main"), &mut out);
        collect_text(&self.region("dock"), &mut out);
        out
    }

    /// Sends a view event as the window would.
    pub fn event(&self, host: &PluginHost, ev: Value) {
        let (sf, _) = self.surface().expect("a live surface");
        let mut ev = ev;
        ev["sf"] = json!(sf);
        let bytes = auk_tsp::wire::encode_to_program(ToProgram::Event, &ev);
        host.block_input(self.pane, &bytes);
    }

    /// Types keys (raw terminal input bytes).
    pub fn keys(&self, host: &PluginHost, bytes: &[u8]) {
        host.block_input(self.pane, bytes);
    }
}

pub fn collect_text(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            for (k, v) in m {
                if (k == "t" || k == "text" || k == "head") && v.is_string() {
                    out.push_str(v.as_str().unwrap_or_default());
                    out.push('\n');
                } else {
                    collect_text(v, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|v| collect_text(v, out)),
        _ => {}
    }
}

/// Finds the first object anywhere in `v` with `key == value`.
pub fn find<'v>(v: &'v Value, key: &str, value: &str) -> Option<&'v Value> {
    match v {
        Value::Object(m) => {
            if m.get(key).and_then(Value::as_str) == Some(value) {
                return Some(v);
            }
            m.values().find_map(|x| find(x, key, value))
        }
        Value::Array(a) => a.iter().find_map(|x| find(x, key, value)),
        _ => None,
    }
}

/// Every `actions.click` value in `v`.
pub fn clicks(v: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    fn walk(v: &Value, out: &mut Vec<(String, String)>) {
        match v {
            Value::Object(m) => {
                if let (Some(id), Some(click)) = (
                    m.get("id").and_then(Value::as_str),
                    m.get("p")
                        .and_then(|p| p.get("actions"))
                        .and_then(|a| a.get("click"))
                        .and_then(Value::as_str),
                ) {
                    out.push((id.to_owned(), click.to_owned()));
                }
                m.values().for_each(|x| walk(x, out));
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    walk(v, &mut out);
    out
}

/// A call a window plugin made of its window.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    Run(u64, String),
    Open(String, Option<How>),
    NewBlock(String, Vec<String>, How),
    Command(String),
    Action(String),
    Toast(ToastLevel, String, Option<String>),
    Copy(String),
    NewTab(Launch),
    Split(u64, Dir, Launch),
    Focus(u64),
    Close(u64),
    Tab(TabSpec),
    Event(u64, Value),
}

/// A window that records calls and answers session queries from fixed data.
#[derive(Default)]
pub struct TestWindow {
    pub calls: Rc<RefCell<Vec<Call>>>,
    pub panes: Vec<WindowPane>,
    pub tabs: Vec<TabInfo>,
    pub focused: Option<u64>,
    pub next_id: u64,
    pub new_block_answer: Option<u64>,
}

impl TestWindow {
    pub fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }

    pub fn toasts(&self) -> Vec<(ToastLevel, String, Option<String>)> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::Toast(l, t, s) => Some((l, t, s)),
                _ => None,
            })
            .collect()
    }

    fn record(&self, c: Call) {
        self.calls.borrow_mut().push(c);
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        100 + self.next_id
    }
}

impl WindowHost for TestWindow {
    fn run(&mut self, pane: u64, text: &str) -> Result<(), String> {
        self.record(Call::Run(pane, text.into()));
        Ok(())
    }
    fn open(&mut self, target: &str, how: Option<How>, _reveal: Reveal) -> Result<(), String> {
        self.record(Call::Open(target.into(), how));
        Ok(())
    }
    fn new_block(&mut self, kind: &str, args: &[String], how: How, _reveal: Reveal) -> Option<u64> {
        self.record(Call::NewBlock(kind.into(), args.to_vec(), how));
        self.new_block_answer
    }
    fn command(&mut self, id: &str) -> Result<(), String> {
        self.record(Call::Command(id.into()));
        Ok(())
    }
    fn action(&mut self, name: &str) -> Result<(), String> {
        self.record(Call::Action(name.into()));
        Ok(())
    }
    fn toast(&mut self, level: ToastLevel, text: &str, sub: Option<&str>) {
        self.record(Call::Toast(level, text.into(), sub.map(Into::into)));
    }
    fn copy(&mut self, text: &str) {
        self.record(Call::Copy(text.into()));
    }
    fn panes(&self) -> Vec<WindowPane> {
        self.panes.clone()
    }
    fn tabs(&self) -> Vec<TabInfo> {
        self.tabs.clone()
    }
    fn focused(&self) -> Option<u64> {
        self.focused
    }
    fn resolve(&self, spec: &str) -> Option<u64> {
        if spec == "@focused" {
            return self.focused;
        }
        spec.parse().ok()
    }
    fn sessions(&self) -> Value {
        json!([{"id": 1, "name": "main", "current": true, "locked": false, "tabs": [1]}])
    }
    fn layout(&self, _tab: u64) -> Option<Value> {
        None
    }
    fn read(&self, _pane: u64, _opts: &Value) -> Option<Value> {
        None
    }
    fn surface(&self, _pane: u64, _opts: &Value) -> Option<Value> {
        None
    }
    fn event(&mut self, pane: u64, ev: &Value) -> Result<(), String> {
        self.record(Call::Event(pane, ev.clone()));
        Ok(())
    }
    fn settle(&mut self, _pane: u64, _ms: f64, done: auk_plugins::Completer) {
        done.resolve(Ok(json!("settled output")));
    }
    fn new_tab(&mut self, launch: &Launch, _reveal: Reveal) -> Option<u64> {
        self.record(Call::NewTab(launch.clone()));
        Some(self.id())
    }
    fn split(&mut self, target: u64, dir: Dir, launch: &Launch, _reveal: Reveal) -> Option<u64> {
        self.record(Call::Split(target, dir, launch.clone()));
        Some(self.id())
    }
    fn focus(&mut self, pane: u64) {
        self.record(Call::Focus(pane));
    }
    fn close(&mut self, pane: u64) {
        self.record(Call::Close(pane));
    }
    fn move_pane(&mut self, _pane: u64, _target: u64, _dir: Option<Dir>, _reveal: Reveal) -> bool {
        true
    }
    fn resize(&mut self, _pane: u64, _dir: Dir, _cells: f64) -> bool {
        true
    }
    fn tab(&mut self, spec: &TabSpec, _reveal: Reveal) -> (Option<u64>, Option<String>) {
        self.record(Call::Tab(spec.clone()));
        (Some(self.id()), None)
    }
}
