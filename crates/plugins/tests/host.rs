//! The daemon side: workers, the catalog, events and effects, isolation, the spawn filter,
//! timers and reload (§4.1, §9.2–§9.4, §9.7, §20; spike S5).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use auk_plugins::{
    Effect, EffectTarget, HostConfig, HostEvent, HostOutput, LogSink, PluginHost, PluginSettings,
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
        }
    }

    fn start(&self) -> PluginHost {
        PluginHost::start(self.config(PluginSettings::default()))
    }
}

fn next(host: &PluginHost, within: Duration) -> HostOutput {
    host.outputs()
        .recv_timeout(within)
        .expect("an output in time")
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
