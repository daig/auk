//! The daemon side: workers, the catalog, events and effects, isolation, the spawn filter,
//! timers and reload (§4.1, §9.2–§9.4, §9.7, §20; spike S5).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod common;

use auk_plugins::{
    Effect, EffectOutput, EffectTarget, HostConfig, HostEvent, HostOutput, LogSink, PluginHost, PluginSettings,
    SpawnSpec, Status, ToastLevel,
};

struct Plugins {
    _dir: tempfile::TempDir,
    root: PathBuf,
    threads: Arc<Mutex<Vec<(String, String)>>>,
}

impl Plugins {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical");
        std::fs::create_dir_all(root.join("plugins")).expect("mkdir");
        Plugins {
            _dir: dir,
            root,
            threads: Arc::default(),
        }
    }

    /// Writes a package whose host entry is `host`.
    fn add(&self, id: &str, extra_manifest: &str, host: &str) -> PathBuf {
        let dir = self.root.join("plugins").join(id);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("plugin.toml"),
            format!("schema = 1\nid = \"{id}\"\nname = \"Plugin {id}\"\nversion = \"1\"\nhost = \"host.luau\"\n{extra_manifest}"),
        )
        .expect("write");
        std::fs::write(dir.join("host.luau"), host).expect("write");
        dir
    }

    fn config(&self, settings: PluginSettings) -> HostConfig {
        let threads = Arc::clone(&self.threads);
        // The log sink runs on the worker thread, so it can report the thread's name.
        let log: LogSink = Arc::new(move |r| {
            let thread = std::thread::current().name().unwrap_or_default().to_owned();
            threads
                .lock()
                .expect("threads")
                .push((thread, r.message.to_owned()));
        });
        HostConfig {
            plugins_dir: self.root.join("plugins"),
            state_dir: self.root.join("state"),
            settings,
            waker: None,
            log,
            panes: None,
        }
    }

    fn start(&self) -> PluginHost {
        PluginHost::start(self.config(PluginSettings::default()))
    }
}

fn next(host: &PluginHost, within: Duration) -> EffectOutput {
    match host.outputs().recv_timeout(within).expect("an output in time") {
        HostOutput::Effect(e) => e,
        other => panic!("expected an effect, got {other:?}"),
    }
}

fn toast(text: &str, sub: Option<&str>, level: ToastLevel) -> Effect {
    Effect::Toast {
        level,
        text: text.into(),
        sub: sub.map(Into::into),
    }
}

fn spec() -> SpawnSpec {
    SpawnSpec {
        program: "/bin/zsh".into(),
        args: vec!["-l".into()],
        cwd: "/".into(),
        env: vec![
            ("AUK_PANE".into(), "7".into()),
            ("AUK_IDENTITY".into(), "id".into()),
        ],
    }
}

#[test]
fn catalog_statuses_and_problems() {
    let p = Plugins::new();
    p.add(
        "ready",
        "[[blocks]]\nid = \"b\"\ntitle = \"B\"\n",
        "tern.block.define('b', {init = function() end, view = function() end})",
    );
    p.add("raises", "", "error('broken entry')");
    p.add(
        "undefined",
        "[[lenses]]\nid = \"l\"\nmatch = [\"x *\"]\n",
        "",
    );
    p.add(
        "undeclared",
        "",
        "tern.block.define('nope', {init = print, view = print})",
    );
    p.add("off", "", "error('never runs')");
    let window_only = p.root.join("plugins/win");
    std::fs::create_dir_all(&window_only).expect("mkdir");
    std::fs::write(
        window_only.join("plugin.toml"),
        "schema = 1\nid = \"win\"\nname = \"W\"\nversion = \"1\"\nwindow = \"w.luau\"\n",
    )
    .expect("write");
    std::fs::write(window_only.join("w.luau"), "").expect("write");
    std::fs::create_dir_all(p.root.join("plugins/junk")).expect("mkdir");

    let host = PluginHost::start(p.config(PluginSettings {
        enabled: true,
        disabled: vec!["off".into()],
    }));
    let catalog = host.catalog();
    let status = |id: &str| {
        catalog
            .plugins
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.status.clone())
            .expect(id)
    };
    assert_eq!(status("ready"), Status::Ready);
    assert_eq!(status("win"), Status::Ready);
    assert_eq!(status("off"), Status::Disabled);
    let Status::Failed(raised) = status("raises") else {
        panic!("raises should fail")
    };
    assert_eq!(
        raised.lines().next(),
        Some(
            format!(
                "{}:1: broken entry",
                p.root.join("plugins/raises/host.luau").display()
            )
            .as_str()
        )
    );
    assert_eq!(
        status("undefined"),
        Status::Failed(
            "lens l is declared in plugin.toml but not defined with tern.lens.define".into()
        )
    );
    let Status::Failed(undeclared) = status("undeclared") else {
        panic!("undeclared should fail")
    };
    assert!(
        undeclared.contains("tern.block.define: block \"nope\" is not declared in plugin.toml"),
        "{undeclared}"
    );
    assert_eq!(catalog.problems.len(), 1);
    assert_eq!(catalog.problems[0].error, "no plugin.toml");
    assert_eq!(catalog.jit, auk_plugins::codegen_supported());
    assert_eq!(
        catalog
            .block_types()
            .map(|(p, b)| format!("{}.{}", p.id, b.id))
            .collect::<Vec<_>>(),
        ["ready.b"]
    );
    assert!(
        p.root.join("state/plugin-data/ready").is_dir(),
        "the data folder exists before the entry runs"
    );

    let disabled_all = PluginHost::start(p.config(PluginSettings {
        enabled: false,
        disabled: vec![],
    }));
    assert!(
        disabled_all
            .catalog()
            .plugins
            .iter()
            .all(|e| e.status == Status::Disabled)
    );
}

#[test]
fn workers_are_named_threads() {
    let p = Plugins::new();
    p.add("alpha", "", "print('hello from', tern.plugin.id)");
    p.add("beta", "", "print('hello from', tern.plugin.id)");
    let _host = p.start();
    let mut seen = p.threads.lock().expect("threads").clone();
    seen.sort();
    assert_eq!(
        seen,
        [
            ("plugin alpha".to_owned(), "hello from\talpha".to_owned()),
            ("plugin beta".to_owned(), "hello from\tbeta".to_owned())
        ]
    );
}

#[test]
fn events_reach_handlers_in_order_with_a_shared_payload_and_cx() {
    let p = Plugins::new();
    p.add(
        "watch",
        "",
        r#"
        tern.on("command_started", function(ev, cx)
            ev.seen = "first"
            cx:toast("success", ev.line, ev.cwd)
        end)
        tern.on("command_started", function(ev, cx)
            error("second fails after seeing " .. ev.seen)
        end)
        tern.on("command_started", function(ev, cx)
            cx:copy(ev.seen .. ":" .. ev.pane)
            cx:open("https://example.com")
        end)
        tern.on("command_finished", function(ev, cx)
            cx:toast("warn", string.format("%s %d %d", ev.line, ev.status, ev.took_ms))
        end)
        "#,
    );
    let host = p.start();
    host.dispatch(HostEvent::CommandStarted {
        pane: 9,
        line: "make".into(),
        cwd: Some("/src".into()),
    });
    let target = EffectTarget::Pane(9);
    let wait = Duration::from_secs(2);
    assert_eq!(
        next(&host, wait).effect,
        toast("make", Some("/src"), ToastLevel::Success)
    );
    let failed = next(&host, wait);
    assert_eq!(failed.target, target);
    let Effect::Toast {
        level: ToastLevel::Error,
        text,
        sub: Some(sub),
    } = failed.effect
    else {
        panic!("{failed:?}")
    };
    assert_eq!(text, "Plugin Plugin watch: command_started failed");
    assert!(sub.ends_with("second fails after seeing first"), "{sub}");
    assert_eq!(next(&host, wait).effect, Effect::Copy("first:9".into()));
    let open = next(&host, wait);
    assert_eq!(
        (open.plugin.as_str(), open.target, open.effect),
        ("watch", target, Effect::Open("https://example.com".into()))
    );
    host.dispatch(HostEvent::CommandFinished {
        pane: 9,
        line: "make".into(),
        status: 2,
        took_ms: 1500.0,
    });
    assert_eq!(
        next(&host, wait).effect,
        toast("make 2 1500", None, ToastLevel::Error)
    );
}

#[test]
fn s5_a_runaway_hook_stalls_only_its_worker_and_is_disabled() {
    let p = Plugins::new();
    p.add(
        "slow",
        "",
        r#"
        tern.on("command_started", function(ev, cx) while true do end end)
        tern.on("cwd", function(ev, cx) cx:toast("info", "slow cwd " .. ev.path) end)
        "#,
    );
    p.add(
        "fast",
        "",
        r#"tern.on("command_started", function(ev, cx) cx:toast("info", "fast " .. ev.line) end)"#,
    );
    let host = p.start();

    let sent = Instant::now();
    host.dispatch(HostEvent::CommandStarted {
        pane: 1,
        line: "one".into(),
        cwd: None,
    });
    let fast = next(&host, Duration::from_secs(1));
    let fast_latency = sent.elapsed();
    assert_eq!(
        (fast.plugin.as_str(), fast.effect),
        ("fast", toast("fast one", None, ToastLevel::Info))
    );
    assert!(
        fast_latency < Duration::from_millis(100),
        "the other worker answered in {fast_latency:?}"
    );

    // The runaway handler trips at 2 s, toasts once, and is disabled.
    let trip = next(&host, Duration::from_secs(4));
    let trip_after = sent.elapsed();
    assert_eq!(trip.plugin, "slow");
    assert_eq!(
        trip.effect,
        toast(
            "Plugin Plugin slow: command_started failed",
            Some("tern: command_started exceeded 2000 ms"),
            ToastLevel::Error
        )
    );
    assert!(
        trip_after >= Duration::from_secs(2) && trip_after < Duration::from_millis(2200),
        "{trip_after:?}"
    );

    // Later command_started calls are skipped without a toast; the plugin's other hooks run.
    host.dispatch(HostEvent::CommandStarted {
        pane: 1,
        line: "two".into(),
        cwd: None,
    });
    host.dispatch(HostEvent::Cwd {
        pane: 1,
        path: "/tmp".into(),
    });
    let mut outputs = [
        next(&host, Duration::from_secs(1)),
        next(&host, Duration::from_secs(1)),
    ];
    outputs.sort_by(|a, b| a.plugin.cmp(&b.plugin));
    assert_eq!(outputs[0].effect, toast("fast two", None, ToastLevel::Info));
    assert_eq!(
        outputs[1].effect,
        toast("slow cwd /tmp", None, ToastLevel::Info)
    );
    assert!(
        host.outputs().try_recv().is_err(),
        "no second trip toast and no output from the disabled hook"
    );
    println!("fast worker latency {fast_latency:?}; trip toast after {trip_after:?}");
}

#[test]
fn s5_entry_budget_fails_only_that_plugin() {
    let p = Plugins::new();
    p.add("stuck", "", "while true do end");
    p.add("fine", "", "");
    let started = Instant::now();
    let host = p.start();
    let took = started.elapsed();
    let status = |id: &str| {
        host.catalog()
            .plugins
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.status.clone())
            .expect(id)
    };
    assert_eq!(
        status("stuck"),
        Status::Failed("tern: load exceeded 2000 ms".into())
    );
    assert_eq!(status("fine"), Status::Ready);
    assert!(
        took >= Duration::from_secs(2) && took < Duration::from_secs(3),
        "{took:?}"
    );
}

#[test]
fn spawn_filter_chains_in_id_order_and_fails_open() {
    let p = Plugins::new();
    p.add(
        "a-env",
        "",
        r#"
        tern.on("spawn", function(spec)
            spec.env.A_VAR = "1"
            spec.args = { "-l", "-i" }
            return spec
        end)
        tern.on("spawn", function(spec)
            spec.env.AUK_PANE = nil
            return spec
        end)
        "#,
    );
    p.add(
        "b-nil",
        "",
        r#"tern.on("spawn", function(spec) return nil end)"#,
    );
    p.add(
        "c-bad",
        "",
        r#"tern.on("spawn", function(spec) return 42 end)"#,
    );
    p.add(
        "d-late",
        "",
        r#"tern.on("spawn", function(spec) local t = os.clock() while os.clock() - t < 0.2 do end spec.program = "/late" return spec end)"#,
    );
    p.add("e-cwd", "", r#"tern.on("spawn", function(spec) spec.cwd = "~" spec.program = spec.program .. "!" return spec end)"#);
    p.add("f-none", "", "");
    let host = p.start();

    let started = Instant::now();
    let out = host.filter_spawn(spec());
    let took = started.elapsed();
    assert_eq!(
        out.program, "/bin/zsh!",
        "d-late's answer was discarded; e-cwd saw the spec from before it"
    );
    assert_eq!(out.args, ["-l", "-i"]);
    assert_eq!(
        out.env,
        [
            ("AUK_IDENTITY".to_owned(), "id".to_owned()),
            ("A_VAR".to_owned(), "1".to_owned())
        ]
    );
    if let Some(home) = std::env::var_os("HOME") {
        assert_eq!(Path::new(&out.cwd), Path::new(&home));
    }
    assert!(
        took >= Duration::from_millis(50) && took < Duration::from_millis(150),
        "waited at most 50 ms for d-late: {took:?}"
    );

    let failure = next(&host, Duration::from_secs(1));
    assert_eq!(
        failure.target,
        EffectTarget::Host,
        "spawn failures toast every window of the host"
    );
    assert_eq!(
        failure.effect,
        toast(
            "Plugin Plugin c-bad: spawn failed",
            Some("spawn must return a spec table or nil, got integer"),
            ToastLevel::Error
        )
    );
}

#[test]
fn timers_fire_on_the_worker_and_can_be_cancelled() {
    let p = Plugins::new();
    p.add(
        "ticks",
        "",
        r#"
        local start = tern.now()
        local cancelled = tern.timer(10, function() print("cancelled timer ran") end)
        cancelled:cancel()
        tern.timer(30, function() print("second") end)
        tern.timer(-5, function() print("first") end)
        tern.timer(40, function() error("timer broke") end)
        "#,
    );
    let host = p.start();
    let failure = next(&host, Duration::from_secs(2));
    assert_eq!(failure.target, EffectTarget::Host);
    let Effect::Toast {
        text,
        sub: Some(sub),
        ..
    } = failure.effect
    else {
        panic!("{failure:?}")
    };
    assert_eq!(text, "Plugin Plugin ticks: timer failed");
    assert!(sub.ends_with("timer broke"), "{sub}");
    let logs: Vec<_> = p
        .threads
        .lock()
        .expect("threads")
        .iter()
        .map(|(_, m)| m.clone())
        .collect();
    assert_eq!(logs, ["first", "second"]);
}

#[test]
fn reload_rebuilds_vms_and_reenables_hooks() {
    let p = Plugins::new();
    let dir = p.add(
        "r",
        "",
        r#"tern.on("title", function(ev, cx) while true do end end)"#,
    );
    let mut host = p.start();
    host.dispatch(HostEvent::Title {
        pane: 2,
        title: "t".into(),
    });
    let trip = next(&host, Duration::from_secs(4));
    assert!(
        matches!(&trip.effect, Effect::Toast { sub: Some(s), .. } if s == "tern: title exceeded 2000 ms")
    );

    std::fs::write(
        dir.join("host.luau"),
        r#"tern.on("title", function(ev, cx) cx:toast("info", "v2 " .. ev.title) end)"#,
    )
    .expect("write");
    let started = Instant::now();
    let catalog = host.reload().clone();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(catalog.plugins[0].status, Status::Ready);
    host.dispatch(HostEvent::Title {
        pane: 2,
        title: "again".into(),
    });
    assert_eq!(
        next(&host, Duration::from_secs(1)).effect,
        toast("v2 again", None, ToastLevel::Info)
    );
}

const BLOCK_MANIFEST: &str = "[[blocks]]\nid = \"b\"\ntitle = \"Ticker\"\n";

#[test]
fn ac11_a_block_takes_timer_ticks_with_no_window_open() {
    let root = common::Root::new();
    root.package(
        "tick",
        &[
            ("plugin.toml", &format!("schema = 1\nid = \"tick\"\nname = \"Tick\"\nversion = \"1\"\nhost = \"host.luau\"\n{BLOCK_MANIFEST}")),
            (
                "host.luau",
                r#"
                local function tick(state)
                    state.timer = tern.timer(40, function()
                        state.n += 1
                        state.cx:render()
                        tick(state)
                    end)
                end
                tern.block.define("b", {
                    init = function(cx, args, saved)
                        local state = {cx = cx, n = if saved then saved.n else 0}
                        tick(state)
                        return state
                    end,
                    view = function(state) return {main = tern.ui.col({tern.ui.text("tick " .. state.n)})} end,
                    title = function(state) return "Ticker " .. state.n end,
                    save = function(state) return {n = state.n} end,
                })
                "#,
            ),
        ],
    );
    let mut host = root.host(None);
    let mut pane = common::BlockPane::new(1, 40, 10);
    host.start_block(pane.launch("tick.b", &[], None, None)).expect("type");
    // Nothing acks frames (no window draws them), yet the ticks keep reaching the replica's
    // document, which is what a window attaching later reads.
    let mut others = Vec::new();
    pane.until(&host, Duration::from_secs(5), &mut others, |p| p.text().contains("tick 10"));
    assert!(pane.frames >= 10);
    pane.until(&host, Duration::from_secs(5), &mut others, |p| p.title() == "Ticker 10" || p.title().starts_with("Ticker 1"));
    let late = common::BlockPane::new(1, 40, 10);
    drop(late);
    let saved: serde_json::Value = serde_json::from_str(host.blocks()[&1].saved.as_deref().expect("saved")).expect("json");
    assert!(saved["n"].as_f64().expect("n") >= 10.0);
    assert!(others.is_empty(), "{others:?}");
}

#[test]
fn ac13_a_host_hook_past_2s_is_disabled_until_reload_and_others_keep_going() {
    let root = common::Root::new();
    let slow = root.package(
        "slow",
        &[
            ("plugin.toml", "schema = 1\nid = \"slow\"\nname = \"Slow\"\nversion = \"1\"\nhost = \"host.luau\"\n"),
            ("host.luau", r#"tern.on("cwd", function(ev) while true do end end)
                tern.on("title", function(ev, cx) cx:toast("info", "slow title " .. ev.title) end)"#),
        ],
    );
    root.package(
        "fast",
        &[
            ("plugin.toml", "schema = 1\nid = \"fast\"\nname = \"Fast\"\nversion = \"1\"\nhost = \"host.luau\"\n"),
            ("host.luau", r#"tern.on("cwd", function(ev, cx) cx:toast("info", "fast " .. ev.path) end)"#),
        ],
    );
    let mut host = root.host(None);
    let mut seen = Vec::new();
    let started = Instant::now();
    host.dispatch(HostEvent::Cwd { pane: 1, path: "/a".into() });
    let fast = common::next_effect(&host, Duration::from_secs(1), &mut seen);
    assert_eq!(fast.plugin, "fast");
    assert!(started.elapsed() < Duration::from_millis(500), "the fast plugin didn't wait");
    let trip = common::next_effect(&host, Duration::from_secs(4), &mut seen);
    assert_eq!(trip.plugin, "slow");
    assert_eq!(
        common::toast_text(&trip),
        Some((ToastLevel::Error, "Plugin Slow: cwd failed".into(), Some("tern: cwd exceeded 2000 ms".into())))
    );
    let took = started.elapsed();
    assert!(took >= Duration::from_secs(2) && took < Duration::from_secs(3), "{took:?}");

    // Disabled: the next cwd skips it (no toast, no stall); its other hooks still run.
    let again = Instant::now();
    host.dispatch(HostEvent::Cwd { pane: 1, path: "/b".into() });
    host.dispatch(HostEvent::Title { pane: 1, title: "t".into() });
    let mut got = [
        common::next_effect(&host, Duration::from_secs(1), &mut seen),
        common::next_effect(&host, Duration::from_secs(1), &mut seen),
    ];
    got.sort_by(|a, b| a.plugin.cmp(&b.plugin));
    assert_eq!(common::toast_text(&got[0]).map(|t| t.1), Some("fast /b".into()));
    assert_eq!(common::toast_text(&got[1]).map(|t| t.1), Some("slow title t".into()));
    assert!(again.elapsed() < Duration::from_millis(500));
    assert!(common::drain(&host, Duration::from_millis(200)).is_empty());

    // A reload re-enables it.
    std::fs::write(slow.join("host.luau"), r#"tern.on("cwd", function(ev, cx) cx:toast("info", "slow " .. ev.path) end)"#).expect("write");
    host.reload();
    host.dispatch(HostEvent::Cwd { pane: 1, path: "/c".into() });
    let mut texts: Vec<_> = (0..2)
        .map(|_| common::toast_text(&common::next_effect(&host, Duration::from_secs(1), &mut seen)).map(|t| t.1))
        .collect();
    texts.sort();
    assert_eq!(texts, [Some("fast /c".into()), Some("slow /c".into())]);
}

#[test]
fn ac14_window_code_cant_write_block_frames_and_host_code_cant_name_a_window() {
    use auk_plugins::window::WindowPlugins;
    let root = common::Root::new();
    root.package(
        "both",
        &[
            ("plugin.toml", &format!("schema = 1\nid = \"both\"\nname = \"Both\"\nversion = \"1\"\nhost = \"host.luau\"\nwindow = \"window.luau\"\n{BLOCK_MANIFEST}")),
            (
                "host.luau",
                r#"
                -- The host half has no window namespace: nothing here can name a window.
                local missing = {}
                for _, k in {"command", "bind", "override", "route", "chrome", "css"} do
                    if tern[k] ~= nil then table.insert(missing, k) end
                end
                assert(#missing == 0, table.concat(missing, ","))
                tern.on("title", function(ev, cx)
                    -- Its effect cx reaches the pane's windows only.
                    local ok = pcall(function() return cx.window end)
                    cx:toast("info", "effect " .. tostring(ok and cx.window))
                end)
                tern.block.define("b", {
                    init = function(cx) return {cx = cx} end,
                    view = function() return {main = tern.ui.col({tern.ui.text("host frame")})} end,
                })
                "#,
            ),
            (
                "window.luau",
                r#"
                -- The window half has no block, lens or pane namespace.
                assert(tern.block == nil and tern.lens == nil and tern.pane == nil)
                tern.command({id = "try", title = "Try", run = function(cx)
                    local frame = pcall(function() cx:frame({}) end)
                    local render = pcall(function() cx:render() end)
                    local blob = pcall(function() cx:blob("x", "image/png") end)
                    -- The only way to a pane is a TSP *event* to its program.
                    cx.session:event(1, {ev = "action", id = "main.0", act = "poke"})
                    cx:copy(string.format("%s %s %s", tostring(frame), tostring(render), tostring(blob)))
                end})
                "#,
            ),
        ],
    );
    let mut host = root.host(None);
    assert_eq!(host.catalog().plugins[0].status, Status::Ready);
    let mut pane = common::BlockPane::new(1, 40, 10);
    host.start_block(pane.launch("both.b", &[], None, None)).expect("type");
    let mut others = Vec::new();
    pane.until(&host, Duration::from_secs(5), &mut others, |p| p.text().contains("host frame"));
    let frames = pane.frames;

    let mut win = common::TestWindow::default();
    let mut plugins = WindowPlugins::load(root.window_config(&[]), &mut win);
    assert!(plugins.failures().is_empty(), "{:?}", plugins.failures());
    plugins.run_action(&mut win, "plugin.both.try").expect("runs");
    let calls = win.calls();
    assert!(matches!(&calls[0], common::Call::Event(1, ev) if ev["ev"] == "action"));
    assert_eq!(calls[1], common::Call::Copy("false false false".into()));
    assert!(pane.settle(&host, Duration::from_millis(200)).is_empty());
    assert_eq!(pane.frames, frames, "nothing from the window reached the block's frames");

    host.dispatch(HostEvent::Title { pane: 1, title: "x".into() });
    let effect = common::next_effect(&host, Duration::from_secs(1), &mut others);
    assert_eq!(effect.target, EffectTarget::Pane(1));
    assert_eq!(common::toast_text(&effect).map(|t| t.1), Some("effect nil".into()));
}

#[test]
fn blocks_init_failure_exit_raw_frames_blobs_chunking_and_reload_without_the_type() {
    let root = common::Root::new();
    let dir = root.package(
        "blk",
        &[
            ("plugin.toml", "schema = 1\nid = \"blk\"\nname = \"Blk\"\nversion = \"1\"\nhost = \"host.luau\"\n[[blocks]]\nid = \"ok\"\ntitle = \"Ok\"\n[[blocks]]\nid = \"bad\"\ntitle = \"Bad\"\n"),
            (
                "host.luau",
                r#"
                tern.block.define("bad", {init = function() error("cannot start") end, view = function() end})
                tern.block.define("ok", {
                    init = function(cx, args) return {cx = cx, big = false, args = args} end,
                    view = function(s)
                        local t = if s.big then string.rep("x", 70000) else "small " .. table.concat(s.args, ",")
                        return {main = tern.ui.col({tern.ui.text(t)}), dock = tern.ui.row({tern.ui.badge(s.cx.cols .. "x" .. s.cx.rows, "muted")})}
                    end,
                    key = function(s, key, cx)
                        if key.name == "b" then s.big = true
                        elseif key.name == "f" then cx:frame({{"set", "main.0", {role = "raw"}}})
                        elseif key.name == "i" then cx:copy(cx:blob("png-bytes", "image/png"))
                        elseif key.name == "q" then cx:exit(3)
                        elseif key.name == "paste" then cx:copy("pasted " .. key.text)
                        else return false end
                        return nil
                    end,
                })
                "#,
            ),
        ],
    );
    let mut host = root.host(None);
    let mut bad = common::BlockPane::new(9, 40, 10);
    host.start_block(bad.launch("blk.bad", &[], None, None)).expect("type");
    let mut others = Vec::new();
    bad.until(&host, Duration::from_secs(5), &mut others, |p| p.exited.is_some());
    let (status, message) = bad.exited.clone().expect("exited");
    assert_eq!(status, 1);
    assert!(message.expect("message").contains("cannot start"));
    assert!(others.iter().any(|o| o.effect().is_some_and(|e| matches!(&e.effect, Effect::Toast { text, .. } if text == "Plugin Blk: bad.init failed"))));

    let mut pane = common::BlockPane::new(1, 40, 10);
    host.start_block(pane.launch("blk.ok", &["a", "b"], None, None)).expect("type");
    pane.until(&host, Duration::from_secs(5), &mut others, |p| p.text().contains("small a,b") && p.text().contains("40x10"));
    // Resize: cx.cols/rows follow and the block re-renders.
    host.block_resize(1, 60, 20);
    pane.until(&host, Duration::from_secs(5), &mut others, |p| p.text().contains("60x20"));
    // A frame over 64 KiB is chunked, and the replica joins it.
    pane.keys(&host, b"b");
    pane.until(&host, Duration::from_secs(5), &mut others, |p| p.text().contains(&"x".repeat(70000)));
    // Raw frames bypass the diff.
    pane.keys(&host, b"f");
    pane.until(&host, Duration::from_secs(5), &mut others, |p| p.region("main")["c"][0]["p"]["role"] == "raw");
    // A paste arrives as one key.
    pane.keys(&host, b"\x1b[200~two\nlines\x1b[201~");
    let copy = pane.next_effect(&host, Duration::from_secs(5), &mut others);
    assert_eq!(copy.effect, Effect::Copy("pasted two\nlines".into()));
    // cx:blob returns the SHA-256 id and sends the bytes.
    pane.keys(&host, b"i");
    let copy = pane.next_effect(&host, Duration::from_secs(5), &mut others);
    let Effect::Copy(id) = copy.effect else { panic!() };
    assert_eq!(id.len(), 64);
    assert!(pane.tsp.blobs().has(&id));
    // A reload whose manifest dropped the type ends the block.
    std::fs::write(dir.join("plugin.toml"), "schema = 1\nid = \"blk\"\nname = \"Blk\"\nversion = \"1\"\nhost = \"host.luau\"\n[[blocks]]\nid = \"bad\"\ntitle = \"Bad\"\n").expect("write");
    std::fs::write(dir.join("host.luau"), r#"tern.block.define("bad", {init = function() end, view = function() end})"#).expect("write");
    host.reload();
    pane.until(&host, Duration::from_secs(5), &mut others, |p| p.exited.is_some());
    assert_eq!(pane.exited, Some((1, Some("no block type blk.ok on this host".into()))));
    assert!(host.blocks().is_empty());
}

#[test]
fn block_exit_and_pane_api() {
    let root = common::Root::new();
    root.package(
        "pw",
        &[
            ("plugin.toml", "schema = 1\nid = \"pw\"\nname = \"Pw\"\nversion = \"1\"\nhost = \"host.luau\"\n[[blocks]]\nid = \"b\"\ntitle = \"B\"\n"),
            (
                "host.luau",
                r#"
                tern.on("command_finished", function(ev, cx)
                    local n = 0
                    for _, p in tern.pane.list() do n += 1 end
                    tern.pane.write(ev.pane, "echo " .. n .. "\r")
                    tern.pane.write(999, "ignored by the daemon")
                end)
                tern.block.define("b", {
                    init = function(cx) return {} end,
                    view = function() return {main = tern.ui.col({})} end,
                    key = function(s, key, cx) cx:exit(4) cx:render() end,
                })
                "#,
            ),
        ],
    );
    let panes = std::sync::Arc::new(auk_plugins::PaneTable::default());
    panes.set(vec![
        auk_plugins::PaneInfo { pane: 1, cwd: Some("/".into()), program: "zsh".into(), title: "zsh".into(), busy: false },
        auk_plugins::PaneInfo { pane: 2, cwd: None, program: "pw.b".into(), title: "B".into(), busy: false },
    ]);
    let mut host = root.host(Some(std::sync::Arc::clone(&panes)));
    host.dispatch(HostEvent::CommandFinished { pane: 1, line: "ls".into(), status: 0, took_ms: 3.0 });
    let mut seen = Vec::new();
    let writes = common::wait_for(&host, Duration::from_secs(2), &mut seen, |o| match o {
        HostOutput::PaneWrite { plugin, pane, text } => Some((plugin.clone(), *pane, text.clone())),
        _ => None,
    });
    assert_eq!(writes, ("pw".into(), 1, "echo 2\r".into()));
    let mut pane = common::BlockPane::new(2, 20, 5);
    host.start_block(pane.launch("pw.b", &[], None, None)).expect("type");
    pane.until(&host, Duration::from_secs(5), &mut seen, |p| p.surface().is_some());
    pane.keys(&host, b"x");
    pane.until(&host, Duration::from_secs(5), &mut seen, |p| p.exited.is_some());
    assert_eq!(pane.exited, Some((4, None)));
    assert!(pane.surface().is_none(), "the surface closed");
}

#[test]
fn a_render_loop_stops_after_8_rounds() {
    let root = common::Root::new();
    root.package(
        "loop",
        &[
            ("plugin.toml", "schema = 1\nid = \"loop\"\nname = \"Loop\"\nversion = \"1\"\nhost = \"host.luau\"\n[[blocks]]\nid = \"b\"\ntitle = \"B\"\n"),
            (
                "host.luau",
                r#"tern.block.define("b", {
                    init = function(cx) return {n = 0} end,
                    view = function(s, cx) s.n += 1 cx:render() return {main = tern.ui.col({tern.ui.text("n=" .. s.n)})} end,
                })"#,
            ),
        ],
    );
    let mut host = root.host(None);
    let mut pane = common::BlockPane::new(1, 20, 5);
    host.start_block(pane.launch("loop.b", &[], None, None)).expect("type");
    let others = pane.settle(&host, Duration::from_millis(500));
    assert!(others.is_empty(), "{others:?}");
    // The first render, then 8 rounds of re-rendering; the 9th request is dropped.
    assert_eq!(pane.text(), "n=9\n");
}

#[test]
fn running_lens_captures_finish_on_the_old_worker_after_a_reload() {
    let root = common::Root::new();
    let manifest = "schema = 1\nid = \"l\"\nname = \"L\"\nversion = \"1\"\nhost = \"host.luau\"\n[[lenses]]\nid = \"v\"\nmatch = [\"run *\"]\n";
    let host_src = |tag: &str| {
        format!(
            r#"tern.lens.define("v", {{
                open = function(run) return {{n = 0}} end,
                line = function(s, l) s.n += 1 end,
                finish = function(s) s.done = true end,
                view = function(s) return tern.ui.text("{tag} " .. s.n .. (s.done and " done" or "")) end,
            }})"#
        )
    };
    let dir = root.package("l", &[("plugin.toml", manifest), ("host.luau", &host_src("old"))]);
    let mut host = root.host(None);
    let pre = auk_plugins::Preconditions { shell_integration: true, lenses_on: true, main_screen: true, surface_open: false };
    let claim = host.catalog().claim("run a", pre).expect("claim");
    host.lens_open(1, 1, &claim, claim.run("run a", "run a", None));
    std::fs::write(dir.join("host.luau"), host_src("new")).expect("write");
    host.reload();
    host.lens_lines(1, vec![auk_plugins::capture::LensLine { text: "x".into(), links: vec![] }]);
    host.lens_finish(1, 0);
    let mut seen = Vec::new();
    let text = |o: &HostOutput| match o {
        HostOutput::LensView { view: Some(v), .. } => v["p"]["spans"][0]["t"].as_str().map(str::to_owned),
        _ => None,
    };
    let done = common::wait_for(&host, Duration::from_secs(5), &mut seen, |o| text(o).filter(|t| t.ends_with("done")));
    assert_eq!(done, "old 1 done", "the capture finished under the old code");
    // New commands go to the new worker.
    host.lens_open(2, 1, &claim, claim.run("run b", "run b", None));
    host.lens_finish(2, 0);
    let done = common::wait_for(&host, Duration::from_secs(5), &mut seen, |o| text(o).filter(|t| t.ends_with("done")));
    assert_eq!(done, "new 0 done");
}
