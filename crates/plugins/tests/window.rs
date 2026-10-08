//! The window half's runtime (§9.8, M9): commands, binds, overrides, routes, chrome formatters,
//! CSS and window events, with the SDK's `longrun`, `workspaces` (except Edit), `jsonx` and
//! `review-queue` window entries, against a window host that records what they ask for.

mod common;

use std::time::Duration;

use auk_plugins::ToastLevel;
use auk_plugins::window::{
    How, Launch, OpenRequest, RouteDecision, StatusPane, StatusSegment, WindowEvent, WindowPane,
    WindowPlugins,
};
use common::*;

const BUILTIN: &[&str] = &["new_tab", "close_pane", "save", "reload_plugins"];

fn pane(id: u64, cwd: &str, title: &str, busy: bool) -> WindowPane {
    WindowPane {
        pane: id,
        cwd: cwd.into(),
        program: "zsh".into(),
        title: title.into(),
        busy,
        kind: "terminal".into(),
        tab: Some(1),
        focused: true,
        block: None,
    }
}

#[test]
fn longrun_window_command_bind_and_ticking_status_segment() {
    let root = Root::new();
    root.example("longrun");
    let mut win = TestWindow::default();
    let mut plugins = WindowPlugins::load(root.window_config(BUILTIN), &mut win);
    assert_eq!(plugins.plugins(), ["longrun"]);
    assert!(plugins.failures().is_empty());
    let commands = plugins.commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].action, "plugin.longrun.show");
    assert_eq!(commands[0].title, "Show slow commands");
    assert_eq!(commands[0].icon, "clock");
    assert_eq!(commands[0].group, "Long-Running Commands");
    let binds = plugins.binds();
    assert_eq!(binds.len(), 1);
    assert_eq!(binds[0].chord, "cmd+alt+shift+l");
    assert_eq!(binds[0].action, "plugin.longrun.show");
    assert!(auk_plugins::chord::Chord::parse(&binds[0].chord).is_ok());

    // No block yet and no host type: the command tries a new tab, then toasts.
    assert_eq!(plugins.run_action(&mut win, "plugin.longrun.show"), Ok(true));
    assert_eq!(
        win.calls(),
        [
            Call::NewBlock("longrun.slow".into(), vec![], How::Tab),
            Call::Toast(
                ToastLevel::Error,
                "Slow Commands is unavailable".into(),
                Some("The longrun host half isn't loaded on this host.".into())
            )
        ]
    );
    // With a Slow Commands block in the window, it focuses that one.
    win.calls.borrow_mut().clear();
    win.panes = vec![pane(1, "/src", "zsh", false), pane(7, "", "Slow Commands (3)", false)];
    win.focused = Some(1);
    plugins.run_action(&mut win, "plugin.longrun.show").expect("runs");
    assert_eq!(win.calls(), [Call::Focus(7)]);
    assert_eq!(plugins.run_action(&mut win, "plugin.nope.show"), Ok(false));

    // The status segment shows the focused pane's running command after a second.
    plugins.window_start(&mut win);
    win.panes[0].busy = true;
    plugins.deliver(&mut win, &[WindowEvent::CommandStarted { pane: 1, line: "cargo build --release".into() }]);
    let input = StatusPane { pane: 1, cwd: "/src".into(), program: "zsh".into(), title: "zsh".into(), busy: true };
    assert_eq!(plugins.status(&input), []);
    assert!(plugins.take_chrome_dirty());
    std::thread::sleep(Duration::from_millis(1100));
    // Cached by input until the plugin's 1 s timer calls tern.chrome.refresh.
    assert_eq!(plugins.status(&input), []);
    plugins.pump(&mut win);
    assert!(plugins.take_chrome_dirty());
    assert_eq!(
        plugins.status(&input),
        [StatusSegment {
            plugin: "longrun".into(),
            text: "cargo · 1s".into(),
            icon: Some("clock".into()),
            tone: Some("muted".into()),
            command: Some("plugin.longrun.show".into()),
        }]
    );
    assert!(plugins.next_wake().is_some(), "the clock keeps ticking");
    plugins.deliver(&mut win, &[WindowEvent::CommandFinished { pane: 1, line: "cargo build --release".into(), status: 0, took_ms: 1200.0 }]);
    win.panes[0].busy = false;
    let idle = StatusPane { busy: false, ..input };
    assert_eq!(plugins.status(&idle), []);
}

fn workspace_project(root: &Root) -> std::path::PathBuf {
    let project = root.path.join("webapp");
    std::fs::create_dir_all(project.join(".tern")).expect("mkdir");
    std::fs::copy(
        example_dir("workspaces").join(".tern/workspace.json"),
        project.join(".tern/workspace.json"),
    )
    .expect("copy");
    project
}

#[test]
fn workspaces_open_override_status_and_css() {
    let root = Root::new();
    root.example("workspaces");
    let project = workspace_project(&root);
    let inside = project.join("web").display().to_string();
    let mut win = TestWindow {
        panes: vec![pane(1, &inside, "zsh", false)],
        focused: Some(1),
        ..TestWindow::default()
    };
    let mut plugins = WindowPlugins::load(root.window_config(BUILTIN), &mut win);
    assert_eq!(
        plugins.sheets(),
        [(
            "plugin:local:workspaces:styles".to_owned(),
            std::fs::read_to_string(example_dir("workspaces").join("workspaces.css")).expect("css")
        )]
    );
    let actions: Vec<_> = plugins.commands().into_iter().map(|c| c.action).collect();
    assert_eq!(actions, ["plugin.workspaces.open", "plugin.workspaces.edit"]);

    // Before anything is open, a new tab is the built-in one.
    assert!(!plugins.run_override(&mut win, "new_tab"));

    plugins.run_action(&mut win, "plugin.workspaces.open").expect("runs");
    let calls = win.calls();
    let tabs: Vec<_> = calls
        .iter()
        .filter_map(|c| match c {
            Call::Tab(spec) => Some(spec.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(tabs.len(), 3);
    let root_dir = project.display().to_string();
    assert_eq!(tabs[0].name.as_deref(), Some("code"));
    assert_eq!(tabs[0].split, Some(auk_plugins::window::Dir::Right));
    let editor = tabs[0].first.as_ref().expect("first side");
    assert_eq!(editor.launch.as_ref().and_then(|l| l.command.as_deref()), Some("nvim ."));
    assert_eq!(editor.cwd.as_deref(), Some(root_dir.as_str()));
    let right = tabs[0].second.as_ref().expect("second side");
    assert_eq!(right.split, Some(auk_plugins::window::Dir::Down));
    assert_eq!(
        right.first.as_ref().and_then(|s| s.cwd.clone()),
        Some(format!("{root_dir}/web"))
    );
    let shell = &tabs[1];
    assert_eq!(
        shell.second.as_ref().and_then(|s| s.launch.as_ref()).and_then(|l| l.block.clone()),
        Some("longrun.slow".into())
    );
    assert_eq!(tabs[2].cwd, Some(format!("{root_dir}/infra")));
    assert_eq!(
        win.toasts().last(),
        Some(&(ToastLevel::Success, "Opened webapp".into(), Some(format!("3 tabs · {root_dir}"))))
    );

    // Opening again says so.
    plugins.run_action(&mut win, "plugin.workspaces.open").expect("runs");
    assert_eq!(win.toasts().last().map(|t| t.1.clone()), Some("webapp is already open".into()));

    // New tabs inside the workspace start at its root; the override replaces the built-in.
    win.calls.borrow_mut().clear();
    assert!(plugins.run_override(&mut win, "new_tab"));
    assert_eq!(win.calls(), [Call::NewTab(Launch { cwd: Some(root_dir.clone()), ..Launch::default() })]);
    assert!(!plugins.run_override(&mut win, "close_pane"), "only new_tab is overridden");

    // The status segment names the workspace for panes inside it.
    let status = |cwd: &str| StatusPane { pane: 1, cwd: cwd.into(), program: "zsh".into(), title: "zsh".into(), busy: false };
    assert_eq!(
        plugins.status(&status(&inside)),
        [StatusSegment {
            plugin: "workspaces".into(),
            text: "webapp".into(),
            icon: Some("folder".into()),
            tone: None,
            command: Some("plugin.workspaces.edit".into()),
        }]
    );
    assert_eq!(plugins.status(&status("/elsewhere")), []);

    // Closing its tabs forgets it (tab ids 101..103 came from the window host).
    plugins.deliver(&mut win, &[101, 102, 103].map(|tab| WindowEvent::TabClosed { tab }));
    assert!(plugins.take_chrome_dirty());
    assert_eq!(plugins.status(&status(&inside)), []);
    assert!(!plugins.run_override(&mut win, "new_tab"));
}

#[test]
fn jsonx_route_claims_json_opens_from_files_and_palette_until_toggled() {
    let root = Root::new();
    root.example("jsonx");
    let mut win = TestWindow::default();
    let mut plugins = WindowPlugins::load(root.window_config(BUILTIN), &mut win);
    let req = |origin: &str, how: How, path: &str| OpenRequest {
        path: path.into(),
        line: None,
        col: None,
        host: "local".into(),
        how,
        origin: origin.into(),
    };
    assert_eq!(
        plugins.route_open(&mut win, &req("palette", How::Default, "/p/data.JSON")),
        Some(RouteDecision::Block { kind: "jsonx.explorer".into(), args: vec!["/p/data.JSON".into()] })
    );
    assert!(plugins.route_open(&mut win, &req("files", How::Default, "/p/a.json")).is_some());
    assert_eq!(plugins.route_open(&mut win, &req("cli", How::Default, "/p/a.json")), None);
    assert_eq!(plugins.route_open(&mut win, &req("palette", How::Beside, "/p/a.json")), None);
    assert_eq!(plugins.route_open(&mut win, &req("palette", How::Default, "/p/a.txt")), None);

    plugins.run_action(&mut win, "plugin.jsonx.toggle_route").expect("runs");
    assert_eq!(
        win.toasts(),
        [(ToastLevel::Info, ".json files open in the editor".into(), Some("Open with › JSON Explorer still works".into()))]
    );
    assert_eq!(plugins.route_open(&mut win, &req("palette", How::Default, "/p/a.json")), None);
    // The switch is in kv.json, which every VM of the plugin shares: a second window agrees.
    let mut other = TestWindow::default();
    let mut second = WindowPlugins::load(root.window_config(BUILTIN), &mut other);
    assert_eq!(second.route_open(&mut other, &req("palette", How::Default, "/p/a.json")), None);
    second.run_action(&mut other, "plugin.jsonx.toggle_route").expect("runs");
    assert!(plugins.route_open(&mut win, &req("palette", How::Default, "/p/a.json")).is_some());
}

#[test]
fn review_queue_window_command_and_bind() {
    let root = Root::new();
    root.example("review-queue");
    let mut win = TestWindow { new_block_answer: Some(55), ..TestWindow::default() };
    let mut plugins = WindowPlugins::load(root.window_config(BUILTIN), &mut win);
    assert_eq!(plugins.binds()[0].chord, "alt+shift+cmd+r");
    assert_eq!(plugins.binds()[0].action, "plugin.review-queue.open");
    plugins.run_action(&mut win, "plugin.review-queue.open").expect("runs");
    assert_eq!(win.calls(), [Call::NewBlock("review-queue.queue".into(), vec![], How::Beside)]);
}

#[test]
fn registrations_errors_budgets_and_the_cx_lifetime() {
    let root = Root::new();
    root.package(
        "probe",
        &[
            ("plugin.toml", "schema = 1\nid = \"probe\"\nname = \"Probe\"\nversion = \"1\"\nwindow = \"window.luau\"\nstyles = [\"a.css\"]\n"),
            ("a.css", ".x{color:red}"),
            (
                "window.luau",
                r#"
                kept = nil
                tern.command({id = "keep", title = "Keep", run = function(cx) kept = cx end})
                tern.command({id = "use", title = "Use", run = function(cx) kept:copy("late") end})
                tern.command({id = "slow", title = "Slow", available = function() while true do end end, run = function() end})
                tern.command({id = "toast", title = "Toast", keys = {"ctrl+a>t"}, run = function(cx) cx:toast("warn", "x") end})
                tern.bind("ctrl+shift+9", function(cx) cx:copy("bound") end)
                tern.bind("ctrl+shift+8", "new_tab")
                tern.css("extra", ".y{}")
                tern.chrome.tab_title(function(tab) return tab.title .. "!" end)
                tern.chrome.window_title(function(win) return 7 end)
                tern.chrome.status(function(p) local t = os.clock() while os.clock() - t < 0.01 do end return {{text = "x"}} end)
                tern.route.link(function(req) if req.url == "x:bad" then return {handled = false} end return nil end)
                tern.override("new_tab", function() error("nope") end)
                ok_override = pcall(tern.override, "frobnicate", function() end)
                ok_cmd = pcall(tern.command, {id = "bind", title = "x", run = print})
                ok_css = pcall(tern.css, "styles", "")
                ok_bind = pcall(tern.bind, "ctrl+x", 5)
                assert(not ok_override and not ok_cmd and not ok_css and not ok_bind)
                "#,
            ),
        ],
    );
    let mut win = TestWindow::default();
    let mut plugins = WindowPlugins::load(root.window_config(BUILTIN), &mut win);
    assert!(plugins.failures().is_empty(), "{:?}", plugins.failures());
    assert_eq!(
        plugins.sheets(),
        [
            ("plugin:local:probe:styles".to_owned(), ".x{color:red}".to_owned()),
            ("plugin:local:probe:extra".to_owned(), ".y{}".to_owned())
        ]
    );
    let binds: Vec<_> = plugins.binds().into_iter().map(|b| (b.chord, b.action)).collect();
    assert_eq!(
        binds,
        [
            ("ctrl+a>t".to_owned(), "plugin.probe.toast".to_owned()),
            ("ctrl+shift+9".to_owned(), "plugin.probe.bind.0".to_owned()),
            ("ctrl+shift+8".to_owned(), "new_tab".to_owned()),
        ]
    );
    plugins.run_action(&mut win, "plugin.probe.bind.0").expect("runs");
    assert_eq!(win.calls(), [Call::Copy("bound".into())]);

    // A kept cx raises once its call returned.
    plugins.run_action(&mut win, "plugin.probe.keep").expect("runs");
    plugins.run_action(&mut win, "plugin.probe.use").expect("runs");
    let (_, text, sub) = win.toasts().pop().expect("failure toast");
    assert_eq!(text, "Plugin Probe: command use failed");
    assert!(sub.is_some());

    // The window cx checks toast levels.
    win.calls.borrow_mut().clear();
    plugins.run_action(&mut win, "plugin.probe.toast").expect("runs");
    assert_eq!(
        win.toasts()[0].2.as_deref(),
        Some("unknown toast level `warn` (success, info or error)")
    );

    // `available` gets 4 ms; an overrun hides the row and stays disabled until reload.
    win.calls.borrow_mut().clear();
    assert!(!plugins.available(&mut win, "plugin.probe.slow"));
    assert_eq!(win.toasts()[0].1, "Plugin Probe: available plugin.probe.slow failed");
    assert_eq!(win.toasts()[0].2.as_deref(), Some("tern: available plugin.probe.slow exceeded 4 ms"));
    assert!(!plugins.available(&mut win, "plugin.probe.slow"));
    assert!(plugins.available(&mut win, "plugin.probe.keep"));

    // A raising override counts as false, so the built-in still runs.
    win.calls.borrow_mut().clear();
    assert!(!plugins.run_override(&mut win, "new_tab"));
    assert_eq!(win.toasts()[0].1, "Plugin Probe: override new_tab failed");

    // Formatters: cached by input, wrong types and overruns reported after the frame.
    let tab = auk_plugins::window::TabInfo { id: 1, title: "zsh".into(), ..Default::default() };
    assert_eq!(plugins.tab_title(&tab).as_deref(), Some("zsh!"));
    assert_eq!(plugins.window_title(&serde_json::json!({"space": "main"})), None);
    assert_eq!(
        plugins.take_toasts(),
        [("Plugin Probe: window_title failed".to_owned(), "a title formatter returns a string or nil".to_owned())]
    );
    let sp = StatusPane::default();
    assert_eq!(plugins.status(&sp), []);
    assert_eq!(
        plugins.take_toasts(),
        [("Plugin Probe: status failed".to_owned(), "tern: status exceeded 4 ms".to_owned())]
    );

    // Route decisions are checked.
    let link = auk_plugins::window::LinkRequest { url: "x:bad".into(), pane: 1, mods: Default::default() };
    win.calls.borrow_mut().clear();
    assert_eq!(plugins.route_link(&mut win, &link), None);
    assert_eq!(
        win.toasts()[0].2.as_deref(),
        Some("a route decision sets block, url, path or handled = true")
    );

    // Reload rebuilds the VMs: disabled hooks come back.
    plugins.reload(&mut win);
    win.calls.borrow_mut().clear();
    assert!(!plugins.available(&mut win, "plugin.probe.slow"));
    assert_eq!(win.toasts().len(), 1, "it ran (and tripped) again after the reload");
}

#[test]
fn a_failed_window_entry_toasts_once_and_keeps_its_styles() {
    let root = Root::new();
    root.package(
        "broken",
        &[
            ("plugin.toml", "schema = 1\nid = \"broken\"\nname = \"Broken\"\nversion = \"1\"\nwindow = \"w.luau\"\nstyles = [\"s.css\"]\n"),
            ("w.luau", "error('no good')"),
            ("s.css", ".b{}"),
        ],
    );
    let mut win = TestWindow::default();
    let mut plugins = WindowPlugins::load(root.window_config(BUILTIN), &mut win);
    assert_eq!(win.toasts().len(), 1);
    assert_eq!(win.toasts()[0].1, "Plugin Broken failed to load");
    assert!(win.toasts()[0].2.as_deref().is_some_and(|s| s.ends_with("no good")));
    assert_eq!(plugins.sheets(), [("plugin:local:broken:styles".to_owned(), ".b{}".to_owned())]);
    plugins.reload(&mut win);
    assert_eq!(win.toasts().len(), 1, "an identical failure isn't toasted again");
}
