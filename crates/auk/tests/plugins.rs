//! The `plugins` suite (GOAL.md; spec §9, §20, M7, M8, AC11–AC14): the SDK's example plugins and
//! purpose-built ones, loaded by the real daemon from a temporary config dir, with headless
//! windows as clients.
//!
//! The window-half examples (`longrun`, `workspaces` except Edit, `jsonx`; M9) and the window
//! side of AC14 run in the real app (`m9_…`, `ac14_window_…`), driven through its automation
//! endpoint; they share the `screen` test group with the app suite.

use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

use auk_conformance::harness::{Headless, TestEnv, ping, quote, wait_for};
use serde_json::{Value, json};

const EXE: &str = env!("CARGO_BIN_EXE_auk");
const EXAMPLES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/tern-sdk/plugins/examples");
const PLAN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../plugins/tests/fixtures/terraform-plan.txt");

// ---------------------------------------------------------------------------------------------
// Helpers

fn plugins_dir(env: &TestEnv) -> PathBuf {
    // The harness points AUK_CONFIG_DIR at the state dir.
    env.state.join("plugins")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for entry in std::fs::read_dir(from).expect("read a plugin dir") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy");
        }
    }
}

/// Copies an SDK example into the test's plugins folder.
fn example(env: &TestEnv, name: &str) {
    let from = Path::new(EXAMPLES).join(name);
    assert!(from.is_dir(), "the SDK example {name} is missing at {}", from.display());
    copy_dir(&from, &plugins_dir(env).join(name));
}

/// Writes a package into the plugins folder.
fn package(env: &TestEnv, id: &str, toml: &str, files: &[(&str, &str)]) {
    let dir = plugins_dir(env).join(id);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("plugin.toml"), toml).expect("write plugin.toml");
    for (name, text) in files {
        std::fs::write(dir.join(name), text).expect("write");
    }
}

fn executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, script).expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// A daemon started with an extra `PATH` (for plugins' `tern.process.run`), killed on drop.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_daemon_with_path(env: &TestEnv, path: &str) -> Daemon {
    let log = std::fs::File::create(env.root.join("daemon.log")).expect("log");
    let child = env
        .command()
        .arg("daemon")
        .env("PATH", path)
        .stdout(log.try_clone().expect("clone"))
        .stderr(log)
        .spawn()
        .expect("spawn the daemon");
    let control = env.control_path();
    wait_for("the daemon's control endpoint", || ping(&control).then_some(()));
    Daemon(child)
}

/// Every string value in a JSON tree, joined with newlines.
fn strings(v: &Value) -> String {
    fn walk(v: &Value, out: &mut String) {
        match v {
            Value::String(s) => {
                out.push_str(s);
                out.push('\n');
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            Value::Object(m) => m.values().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = String::new();
    walk(v, &mut out);
    out
}

/// The live surface of a `tsp PANE` answer: `(id, doc)`.
fn live(tsp: &Value) -> Option<(String, Value)> {
    tsp["surfaces"]
        .as_array()?
        .iter()
        .rev()
        .find(|s| s["state"] == "live")
        .map(|s| (s["id"].as_str().unwrap_or_default().to_owned(), s["doc"].clone()))
}

fn daemon_doc(env: &TestEnv, pane: u64) -> String {
    live(&env.daemon(&format!("tsp {pane}"))).map(|(_, d)| strings(&d)).unwrap_or_default()
}

fn window_doc(h: &Headless, pane: u64) -> Option<(String, Value)> {
    let reply = auk_conformance::harness::Control::connect(&h.control).send(&format!("tsp {pane}"));
    (reply["ok"] == true).then(|| live(&reply["value"])).flatten()
}

/// Nodes with a click action: `(node id, action)`.
fn clicks(v: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    fn walk(v: &Value, out: &mut Vec<(String, String)>) {
        match v {
            Value::Object(m) => {
                if let (Some(id), Some(click)) = (
                    m.get("id").and_then(Value::as_str),
                    m.get("p").and_then(|p| p["actions"]["click"].as_str()),
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

fn effects(h: &Headless) -> Vec<Value> {
    h.request("effects").as_array().cloned().unwrap_or_default()
}

fn wait_effect(h: &Headless, what: &str, f: impl Fn(&Value) -> bool) -> Value {
    wait_for(what, || effects(h).into_iter().find(|e| f(e)))
}

fn title(env: &TestEnv, pane: u64) -> String {
    env.pane_info(pane)["title"].as_str().unwrap_or_default().to_owned()
}

fn kv(env: &TestEnv, plugin: &str) -> Value {
    std::fs::read_to_string(env.state.join(format!("plugin-data/{plugin}/kv.json")))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null)
}

/// Waits until a window's replica of `pane` has caught up with the daemon's, and both hash the
/// same.
fn same_hash(env: &TestEnv, h: &Headless, pane: u64) -> String {
    wait_for("the window to match the daemon's replica", || {
        let (hash, seq) = env.daemon_hash(pane);
        (h.hash(pane) == Some((hash.clone(), seq))).then_some(hash)
    })
}

fn auk(env: &TestEnv, args: &[&str]) -> (i32, String, String) {
    let out = env.cli(args);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// ---------------------------------------------------------------------------------------------
// M7: the palette-opened example blocks

#[test]
fn m7_dirvars_block_approves_and_the_spawn_filter_sets_new_panes_env() {
    let env = TestEnv::new(EXE, "dirvars");
    example(&env, "dirvars");
    let project = env.home.join("project");
    std::fs::create_dir_all(&project).expect("mkdir");
    let env_file = project.join(".tern-env");
    std::fs::write(&env_file, "APP_ENV=development\nURL=postgres://db/${APP_ENV}\n").expect("write");
    let _daemon = env.start_daemon();
    let window = env.start_headless("w", &["--size", "100x30"]);

    // Not approved: a new pane in the project has none of it.
    let before = env.new_pane(&["--cwd", &project.to_string_lossy()]);
    env.wait(before, "prompt");
    env.run(before, "echo \"APP=<$APP_ENV>\"");
    assert!(env.daemon(&format!("capture {before}"))["text"].as_str().unwrap().contains("APP=<>"));

    // Opened as from the palette: a block pane whose worker draws it.
    let pane = env.daemon(&format!("block dirvars.approve -- {}", quote(&project.to_string_lossy())))["pane"]
        .as_u64()
        .expect("a pane");
    let (sf, doc) = wait_for("the block's view in the window", || {
        window_doc(&window, pane).filter(|(_, d)| strings(d).contains("Approve these exact bytes"))
    });
    let tsp = env.daemon(&format!("tsp {pane}"));
    assert_eq!(tsp["surfaces"][0]["role"], "plugin.dirvars.approve");
    wait_for("the block's title", || (title(&env, pane) == "Directory Variables").then_some(()));
    assert!(strings(&doc).contains(&env_file.display().to_string()));
    let list = env.daemon("pane-list");
    assert!(
        list.as_array().unwrap().iter().any(|p| p["pane"] == pane && p["program"] == "dirvars.approve"),
        "{list}"
    );

    // The window clicks "Approve these exact bytes"; the block toasts in that window.
    let (node, _) = clicks(&doc)
        .into_iter()
        .find(|(_, c)| c.starts_with("approve="))
        .expect("an approve button");
    window.request(&format!("click {pane} {sf} {}", quote(&node)));
    let toast = wait_effect(&window, "the approval toast", |e| e["text"] == "Exact contents approved");
    assert_eq!(toast["effect"], "toast");
    assert_eq!(toast["sub"], "Only new shells load these variables.");
    wait_for("the revoke button", || {
        window_doc(&window, pane).filter(|(_, d)| strings(d).contains("Revoke approval"))
    });

    // The spawn filter now gives new panes the variables, under the identity set they had.
    let after = env.new_pane(&["--cwd", &project.to_string_lossy()]);
    env.wait(after, "prompt");
    env.run(after, "echo \"APP=<$APP_ENV> URL=<$URL> DV=<$TERN_DIRVARS> P=<$AUK_PANE>\"");
    let text = env.daemon(&format!("capture {after}"))["text"].as_str().unwrap().to_owned();
    assert!(text.contains("APP=<development>"), "{text}");
    assert!(text.contains("URL=<postgres://db/development>"), "{text}");
    assert!(text.contains(&format!("DV=<{}>", env_file.display())), "{text}");
    assert!(text.contains(&format!("P=<{after}>")), "{text}");

    // Closing the block ends its pane; the host forgets it.
    env.daemon(&format!("kill {pane}"));
    wait_for("the block to end", || env.daemon("blocks").get(pane.to_string()).is_none().then_some(()));
}

const PRS: &str = r#"[
  {"number": 12, "title": "Fix the parser", "url": "https://github.com/acme/app/pull/12",
   "repository": {"nameWithOwner": "acme/app"}, "author": {"login": "ada"},
   "createdAt": "2026-09-30T14:03:11Z", "isDraft": false},
  {"number": 7, "title": "WIP: new renderer", "url": "https://github.com/acme/gfx/pull/7",
   "repository": {"nameWithOwner": "acme/gfx"}, "author": {"login": "bob"},
   "createdAt": "2026-09-29T09:00:00Z", "isDraft": true},
  {"number": 3, "title": "Docs", "url": "https://github.com/acme/app/pull/3",
   "repository": {"nameWithOwner": "acme/app"}, "author": {"login": "cy"},
   "createdAt": "2026-09-28T09:00:00Z", "isDraft": false}
]"#;

#[test]
fn m7_review_queue_block_lists_opens_toggles_saves_and_survives_reload() {
    let env = TestEnv::new(EXE, "reviewq");
    example(&env, "review-queue");
    // A stand-in `gh` on the daemon's PATH: the test never touches the user's GitHub login.
    let bin = env.root.join("bin");
    executable(&bin.join("gh"), &format!("#!/bin/sh\ncat <<'EOF'\n{PRS}\nEOF\n"));
    let _daemon = start_daemon_with_path(&env, &format!("{}:/usr/bin:/bin", bin.display()));
    let window = env.start_headless("w", &["--size", "100x30"]);

    let pane = env.daemon("block review-queue.queue")["pane"].as_u64().expect("a pane");
    wait_for("the queue without drafts", || (title(&env, pane) == "Review Queue (2)").then_some(()));
    let (_, doc) = wait_for("the queue in the window", || {
        window_doc(&window, pane).filter(|(_, d)| strings(d).contains("acme/app#12"))
    });
    let text = strings(&doc);
    assert!(text.contains("acme/app#3") && !text.contains("WIP: new renderer"), "{text}");

    // Keys typed in the window reach the block: ↓ then Enter opens the pull request there.
    window.request(&format!("focus {pane}"));
    window.request(&format!("send {pane} keys Down"));
    window.request(&format!("send {pane} keys Enter"));
    let open = wait_effect(&window, "the open effect", |e| e["effect"] == "open");
    assert_eq!(open["target"], "https://github.com/acme/app/pull/12");

    // `d` shows drafts, and that's what `save` keeps.
    window.request(&format!("send {pane} text d"));
    wait_for("drafts shown", || (title(&env, pane) == "Review Queue (3)").then_some(()));
    wait_for("the saved state", || {
        (env.daemon("blocks")[pane.to_string()]["saved"] == r#"{"drafts":true}"#).then_some(())
    });
    let kind = &env.pane_info(pane)["kind"];
    assert_eq!(kind["kind"], "block");
    assert_eq!(kind["block"], "review-queue.queue");
    assert_eq!(kind["saved"], r#"{"drafts":true}"#, "the pane record keeps the save for restore");

    // A reload moves the block to the new worker, which starts from the last save.
    let (code, out, _) = auk(&env, &["plugin", "reload"]);
    assert_eq!(code, 0, "{out}");
    wait_for("the block restarted on surface s2", || {
        live(&env.daemon(&format!("tsp {pane}")))
            .filter(|(id, d)| id == "s2" && strings(d).contains("WIP: new renderer"))
    });
    assert_eq!(title(&env, pane), "Review Queue (3)");
}

// ---------------------------------------------------------------------------------------------
// M8: the terraform lens

fn terraform(env: &TestEnv) -> PathBuf {
    let path = env.root.join("bin/terraform");
    assert!(Path::new(PLAN).is_file(), "the plan fixture is missing at {PLAN}");
    executable(&path, &format!("#!/bin/sh\ncat '{PLAN}'\nexit 2\n"));
    path
}

#[test]
fn m8_terraform_lens_captures_views_late_windows_and_events() {
    let env = TestEnv::new(EXE, "terraform");
    example(&env, "terraform");
    let tf = terraform(&env);
    let _daemon = env.start_daemon();
    let window = env.start_headless("w", &["--size", "120x30"]);
    let pane = env.new_pane(&["--size", "120x30"]);
    env.wait(pane, "prompt");

    env.run(pane, &format!("{} plan", tf.display()));
    let lens = env.daemon(&format!("lens {pane}"));
    let block = &lens["blocks"][0];
    assert_eq!((block["plugin"].as_str(), block["lens"].as_str()), (Some("terraform"), Some("plan")));
    assert_eq!(block["program"], "terraform");
    assert_eq!(block["done"], true);
    assert_eq!(block["status"], 2);
    assert!(block["took_ms"].as_u64().is_some(), "the daemon's duration: {block}");
    assert_eq!(block["cols"], 120);
    let raw = block["raw"].as_str().unwrap();
    assert!(raw.contains("Plan: 2 to add, 1 to change, 2 to destroy"), "{raw}");
    assert!(block["cwd"].as_str().is_some_and(|c| c.starts_with("file://")));
    // The output went to the command block, not the grid.
    let grid = env.daemon(&format!("capture {pane} --scrollback"))["text"].as_str().unwrap().to_owned();
    assert!(!grid.contains("Plan: 2 to add"), "{grid}");

    // The view arrives out of band, in the daemon's replica and the window's.
    let view = wait_for("the lens view in the window", || {
        let v = window.request(&format!("lens {pane}"))["blocks"][0]["view"].clone();
        strings(&v).contains("2 to add").then_some(v)
    });
    assert_eq!(view["p"]["role"], "terraform.plan.danger");
    wait_for("the view in the daemon's replica", || {
        (env.daemon(&format!("lens {pane}"))["blocks"][0]["view"] == view).then_some(())
    });
    same_hash(&env, &window, pane);

    // A window that attaches later gets the block and its current view with the snapshot.
    let late = env.start_headless("late", &[]);
    let got = wait_for("the late window's view", || {
        let v = late.request(&format!("lens {pane}"));
        (!v["blocks"][0]["view"].is_null()).then_some(v)
    });
    assert_eq!(got["blocks"][0]["view"], view);
    assert_eq!(got["blocks"][0]["raw"], block["raw"]);

    // An event from the view: the lens copies the address in the pane's windows.
    // Lens view node ids are unstable (§9.6); the action carries the whole choice.
    assert!(strings(&view).contains("copy-address=aws_instance.old"), "a destroy row");
    let ev = json!({"ev": "action", "id": "n1", "act": "copy-address", "value": "aws_instance.old"});
    window.request(&format!("event {pane} lens:0 {}", quote(&ev.to_string())));
    let copy = wait_effect(&window, "the copy effect", |e| e["effect"] == "copy");
    assert_eq!(copy["text"], "aws_instance.old");
    wait_effect(&late, "the copy in the other window too", |e| e["effect"] == "copy");

    // A command no lens claims stays in the grid.
    env.run(pane, "echo plain-output");
    assert_eq!(env.daemon(&format!("lens {pane}"))["blocks"].as_array().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------------------------
// Acceptance checks

/// AC11: a block takes timer ticks with no window open, and a later window sees the update.
#[test]
fn ac11_a_block_ticks_with_no_window_open_and_a_later_window_sees_it() {
    let env = TestEnv::new(EXE, "ac11");
    package(
        &env,
        "ticker",
        "schema = 1\nid = \"ticker\"\nname = \"Ticker\"\nversion = \"1\"\nhost = \"host.luau\"\n[[blocks]]\nid = \"tick\"\ntitle = \"Tick\"\n",
        &[(
            "host.luau",
            r#"tern.block.define("tick", {
                init = function(cx, args, saved)
                    local s = { n = 0 }
                    local function step()
                        s.n += 1
                        cx:render()
                        tern.timer(100, step)
                    end
                    tern.timer(100, step)
                    return s
                end,
                view = function(s) return { main = tern.ui.text("tick " .. s.n) } end,
                save = function(s) return { n = s.n } end,
            })"#,
        )],
    );
    let _daemon = env.start_daemon();
    let pane = env.daemon("block ticker.tick")["pane"].as_u64().expect("a pane");
    assert_eq!(env.daemon("clients"), json!([]), "no window is open");
    let n = |text: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix("tick ").and_then(|n| n.parse::<u64>().ok()))
    };
    let ticked = wait_for("ticks with no window", || n(&daemon_doc(&env, pane)).filter(|n| *n >= 5));
    let saved = env.daemon("blocks")[pane.to_string()]["saved"].clone();
    assert!(saved.as_str().is_some_and(|s| s.starts_with("{\"n\":")), "{saved}");

    let window = env.start_headless("w", &[]);
    let seen = wait_for("the later window's view", || {
        window_doc(&window, pane).and_then(|(_, d)| n(&strings(&d)))
    });
    assert!(seen >= ticked, "{seen} < {ticked}");
    wait_for("the window to follow later ticks", || {
        window_doc(&window, pane).and_then(|(_, d)| n(&strings(&d))).filter(|m| *m > seen)
    });
    same_hash(&env, &window, pane);
}

/// AC12: a lens decision is identical on every replica and doesn't depend on any window half.
#[test]
fn ac12_lens_decisions_are_identical_on_every_replica() {
    let env = TestEnv::new(EXE, "ac12");
    example(&env, "terraform");
    // An earlier plugin id that also claims `terraform plan` wins while it is installed. Its
    // window half raises at load; window halves have no say in claims.
    package(
        &env,
        "alpha",
        "schema = 1\nid = \"alpha\"\nname = \"Alpha\"\nversion = \"1\"\nhost = \"host.luau\"\nwindow = \"window.luau\"\n[[lenses]]\nid = \"tf\"\nmatch = [\"terraform *\"]\n",
        &[
            (
                "host.luau",
                r#"tern.lens.define("tf", {
                    open = function(run) return { n = 0 } end,
                    line = function(s) s.n += 1 end,
                    finish = function() end,
                    view = function(s) return tern.ui.text("alpha " .. s.n) end,
                })"#,
            ),
            ("window.luau", "error('window halves never decide claims')"),
        ],
    );
    let tf = terraform(&env);
    let _daemon = env.start_daemon();
    let a = env.start_headless("a", &[]);
    let pane = env.new_pane(&["--size", "100x30"]);
    env.wait(pane, "prompt");
    let b = env.start_headless("b", &[]);

    env.run(pane, &format!("{} plan", tf.display()));
    env.run(pane, &format!("{} plan | cat", tf.display()));
    // Remove alpha: the new catalog is sequenced into the stream; later claims go to terraform.
    let (code, _, err) = auk(&env, &["plugin", "remove", "alpha"]);
    assert_eq!(code, 0, "{err}");
    wait_for("the catalog record", || {
        let c = env.daemon(&format!("lens {pane}"))["catalog"].clone();
        (c["plugins"].as_array().map(Vec::len) == Some(1)).then_some(())
    });
    env.run(pane, &format!("{} plan -out x", tf.display()));

    let hash = same_hash(&env, &a, pane);
    assert_eq!(same_hash(&env, &b, pane), hash);
    let decisions = |v: &Value| -> Vec<(String, String, String)> {
        v["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| {
                (
                    b["plugin"].as_str().unwrap().to_owned(),
                    b["line"].as_str().unwrap().to_owned(),
                    b["raw"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    let daemon = decisions(&env.daemon(&format!("lens {pane}")));
    assert_eq!(daemon.iter().map(|d| d.0.as_str()).collect::<Vec<_>>(), ["alpha", "terraform"], "{daemon:?}");
    assert!(daemon[1].1.ends_with("plan -out x"));
    assert_eq!(decisions(&a.request(&format!("lens {pane}"))), daemon);
    assert_eq!(decisions(&b.request(&format!("lens {pane}"))), daemon);
    // The pipeline stayed in the grid, everywhere.
    assert!(a.capture(pane, true).contains("Plan: 2 to add"));
}

/// AC13: a host hook past 2 s is disabled until reload; other plugins and the daemon keep going.
#[test]
fn ac13_a_hook_past_2s_is_disabled_until_reload_and_the_rest_keeps_going() {
    let env = TestEnv::new(EXE, "ac13");
    let count = r#"tern.kv.set("n", (tern.kv.get("n") or 0) + 1)"#;
    package(
        &env,
        "slow",
        "schema = 1\nid = \"slow\"\nname = \"Slow\"\nversion = \"1\"\nhost = \"host.luau\"\n",
        &[("host.luau", &format!("tern.on(\"cwd\", function(ev) {count}; while true do end end)"))],
    );
    package(
        &env,
        "fast",
        "schema = 1\nid = \"fast\"\nname = \"Fast\"\nversion = \"1\"\nhost = \"host.luau\"\n",
        &[("host.luau", &format!("tern.on(\"cwd\", function(ev) {count} end)"))],
    );
    let _daemon = env.start_daemon();
    let window = env.start_headless("w", &[]);
    let pane = env.new_pane(&[]);
    env.wait(pane, "prompt");
    let n = |p: &str| kv(&env, p)["n"].as_u64().unwrap_or(0);
    let base = (n("slow"), n("fast"));

    let dir = env.root.join("d1");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let sent = Instant::now();
    env.run(pane, &format!("cd {}", quote(&dir.to_string_lossy())));
    wait_for("the fast plugin's hook", || (n("fast") > base.1).then_some(()));
    assert!(sent.elapsed() < Duration::from_secs(2), "the other worker isn't held up");
    // The daemon and the pane answer while the slow hook spins.
    assert_eq!(env.daemon("ping"), "pong");
    env.run(pane, "echo still-here");
    assert!(sent.elapsed() < Duration::from_secs(2), "{:?}", sent.elapsed());

    let toast = wait_effect(&window, "the trip toast", |e| {
        e["sub"].as_str().is_some_and(|s| s.contains("exceeded 2000 ms"))
    });
    assert!(toast["text"].as_str().unwrap().to_lowercase().contains("slow"), "{toast}");
    assert_eq!(toast["level"], "error");
    assert_eq!(n("slow"), base.0 + 1);

    // Disabled: the next change reaches only the fast plugin.
    let dir2 = env.root.join("d2");
    std::fs::create_dir_all(&dir2).expect("mkdir");
    env.run(pane, &format!("cd {}", quote(&dir2.to_string_lossy())));
    wait_for("the fast hook again", || (n("fast") > base.1 + 1).then_some(()));
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(n("slow"), base.0 + 1, "the disabled hook didn't run");
    let trips = effects(&window).iter().filter(|e| e["sub"].as_str().is_some_and(|s| s.contains("exceeded"))).count();
    assert_eq!(trips, 1, "one toast per trip");

    // Until reload.
    let (code, _, err) = auk(&env, &["plugin", "reload"]);
    assert_eq!(code, 0, "{err}");
    env.run(pane, &format!("cd {}", quote(&dir.to_string_lossy())));
    wait_for("the slow hook re-enabled", || (n("slow") > base.0 + 1).then_some(()));
}

/// AC14, host side: host code can't name a window. The window side (window code can't write
/// block frames) is the app's window host.
#[test]
fn ac14_host_code_cannot_name_a_window() {
    let env = TestEnv::new(EXE, "ac14");
    let probe = r#"
        local function probe(t, names)
            local out = {}
            for _, name in names do
                local ok, v = pcall(function() return (t :: any)[name] end)
                out[name] = if ok then type(v) else "error"
            end
            return out
        end
        local WINDOW = { "window", "windows", "session", "layout", "run", "new_block", "command", "action" }
        tern.kv.set("tern", probe(tern, { "window", "windows", "command", "bind", "override", "route", "chrome", "css", "canvas" }))
        tern.kv.set("context", tern.context)
        tern.on("cwd", function(ev, cx) tern.kv.set("effect_cx", probe(cx, WINDOW)) end)
        tern.block.define("b", {
            init = function(cx) tern.kv.set("block_cx", probe(cx, WINDOW)); return {} end,
            view = function() return { main = tern.ui.text("ok") } end,
        })
    "#;
    package(
        &env,
        "probe",
        "schema = 1\nid = \"probe\"\nname = \"Probe\"\nversion = \"1\"\nhost = \"host.luau\"\n[[blocks]]\nid = \"b\"\ntitle = \"B\"\n",
        &[("host.luau", probe)],
    );
    let _daemon = env.start_daemon();
    env.daemon("block probe.b");
    let pane = env.new_pane(&[]);
    env.wait(pane, "prompt");
    env.run(pane, "cd /");
    let got = wait_for("the probes", || {
        let v = kv(&env, "probe");
        (!v["effect_cx"].is_null() && !v["block_cx"].is_null()).then_some(v)
    });
    assert_eq!(got["context"], "host");
    for table in ["tern", "effect_cx", "block_cx"] {
        for (name, ty) in got[table].as_object().expect("a probe") {
            assert!(
                ty == "nil" || ty == "error",
                "host code reaches {table}.{name} ({ty}): {got}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// S5 through the daemon

#[test]
fn s5_runtime_budgets_memory_cap_and_codegen_through_the_daemon() {
    let env = TestEnv::new(EXE, "s5");
    // An entry that never returns trips its 2 s budget: Failed, and the daemon starts anyway.
    package(
        &env,
        "spin",
        "schema = 1\nid = \"spin\"\nname = \"Spin\"\nversion = \"1\"\nhost = \"host.luau\"\n",
        &[("host.luau", "while true do end")],
    );
    // 256 MiB per VM: running out is an ordinary error pcall catches, and the VM goes on.
    package(
        &env,
        "hog",
        "schema = 1\nid = \"hog\"\nname = \"Hog\"\nversion = \"1\"\nhost = \"host.luau\"\n[[blocks]]\nid = \"b\"\ntitle = \"B\"\n",
        &[(
            "host.luau",
            r#"
            local ok, err = pcall(function()
                local t = {}
                for i = 1, 100000 do t[i] = string.rep("x", 1024 * 1024 - 64) .. i end
                return #t
            end)
            tern.kv.set("hog", { ok = ok, err = tostring(err), jit = tern.runtime.jit })
            tern.block.define("b", {
                init = function() return {} end,
                view = function() return { main = tern.ui.text("still alive") } end,
            })
            "#,
        )],
    );
    let started = Instant::now();
    let _daemon = env.start_daemon();
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());

    let (code, out, err) = auk(&env, &["plugin", "list", "--json"]);
    assert_eq!(code, 0, "{err}");
    let catalog: Value = serde_json::from_str(&out).expect("json");
    assert_eq!(catalog["jit"], true, "Luau native codegen is on: {catalog}");
    let status = |id: &str| {
        catalog["plugins"].as_array().unwrap().iter().find(|p| p["id"] == id).expect("listed")["status"].clone()
    };
    assert!(
        status("spin")["failed"].as_str().is_some_and(|e| e.contains("exceeded 2000 ms")),
        "{catalog}"
    );
    assert_eq!(status("hog"), "ready");
    let hog = kv(&env, "hog")["hog"].clone();
    assert_eq!(hog["ok"], false, "{hog}");
    assert!(hog["err"].as_str().unwrap().contains("memory"), "{hog}");
    assert_eq!(hog["jit"], true);
    let pane = env.daemon("block hog.b")["pane"].as_u64().expect("a pane");
    wait_for("the block after the cap", || daemon_doc(&env, pane).contains("still alive").then_some(()));
}

/// Runs a command to completion; returns its status, stdout and stderr.
fn output(cmd: &mut std::process::Command) -> (bool, String, String) {
    let out = cmd.output().unwrap_or_else(|e| panic!("can't run {cmd:?}: {e}"));
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `auk doctor`'s report from the app bundle at `app`.
fn doctor(app: &Path) -> Value {
    let exe = app.join("Contents/MacOS/auk");
    let (ok, out, err) = output(std::process::Command::new(&exe).arg("doctor"));
    assert!(ok, "{} doctor failed: {err}", exe.display());
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("doctor printed no JSON ({e}): {out}"))
}

/// §19, §20: the app built by `scripts/bundle.sh`, signed ad hoc with the hardened runtime and
/// `allow-jit`, runs Luau's native code. As a control, the same app re-signed without the
/// entitlement falls back to the interpreter, which shows the probe tells the two apart.
#[test]
fn s5_jit_runs_in_the_signed_app_bundle() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("root");
    // Its own target dir: building `-p auk` alone can rebuild the target/release/auk that the
    // suites running beside this one use.
    let target = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().expect("target dir").join("bundle-suite");
    let (ok, out, err) = output(
        std::process::Command::new("/bin/bash")
            .arg(root.join("scripts/bundle.sh"))
            .env("CARGO_TARGET_DIR", &target),
    );
    assert!(ok, "scripts/bundle.sh failed:\n{out}\n{err}");
    let app = target.join("Auk.app");

    // The signature: ad hoc, hardened runtime, allow-jit.
    let (ok, _, err) = output(std::process::Command::new("codesign").args(["--verify", "--strict"]).arg(&app));
    assert!(ok, "codesign --verify: {err}");
    let (ok, out, err) = output(
        std::process::Command::new("codesign").args(["-d", "--entitlements", "-", "--xml"]).arg(&app),
    );
    assert!(ok, "codesign -d --entitlements: {err}");
    assert!(
        out.contains("<key>com.apple.security.cs.allow-jit</key><true/>"),
        "no allow-jit entitlement: {out}"
    );
    let (ok, _, details) = output(std::process::Command::new("codesign").arg("-dv").arg(&app));
    assert!(ok, "codesign -dv: {details}");
    let flags = details.lines().find(|l| l.starts_with("CodeDirectory ")).unwrap_or_default();
    assert!(flags.contains("(adhoc,runtime)"), "not ad hoc with the hardened runtime: {details}");
    assert!(details.contains("Signature=adhoc"), "{details}");

    // The app: native code runs, and the kernel sees the hardened runtime.
    let report = doctor(&app);
    println!("{report:#}");
    assert_eq!(report["bundle"], app.to_string_lossy().as_ref(), "{report:#}");
    assert_eq!(report["code_signature"]["hardened_runtime"], true, "{report:#}");
    assert_eq!(report["web_assets"]["present"], true, "{report:#}");
    let jit = &report["jit"];
    assert_eq!(jit["error"], Value::Null, "{report:#}");
    assert!(jit["native_functions"].as_u64() >= Some(1), "{report:#}");
    assert_eq!(jit["native_ran"], true, "{report:#}");
    // The same bound as auk-plugins' s5_native_codegen_is_on.
    assert!(jit["speedup"].as_f64() > Some(1.5), "{report:#}");

    // The control: hardened runtime without allow-jit. Luau can't map its code and quietly
    // interprets instead.
    let dir = scratch_dir("s5-no-jit");
    let control = dir.join("Auk.app");
    let (ok, _, err) = output(std::process::Command::new("cp").arg("-R").arg(&app).arg(&control));
    assert!(ok, "cp: {err}");
    let (ok, _, err) = output(
        std::process::Command::new("codesign")
            .args(["--force", "--sign", "-", "--options", "runtime", "--timestamp=none"])
            .arg(&control),
    );
    assert!(ok, "re-signing without entitlements: {err}");
    let report = doctor(&control);
    println!("{report:#}");
    assert_eq!(report["code_signature"]["hardened_runtime"], true, "{report:#}");
    assert_eq!(report["jit"]["native_functions"], 0, "{report:#}");
    assert_eq!(report["jit"]["native_ran"], false, "{report:#}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A fresh directory under the test's target tmpdir.
fn scratch_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

// ---------------------------------------------------------------------------------------------
// Lifecycle

#[test]
fn lifecycle_plugin_verbs_and_reload_on_change() {
    let env = TestEnv::new(EXE, "lifecycle");
    let dir = plugins_dir(&env);

    // No daemon: the verbs work on the folder, and never start one (§4.1).
    let (code, out, _) = auk(&env, &["plugin", "list"]);
    assert_eq!((code, out.as_str()), (0, "no plugins\n"));
    let (code, out, _) = auk(&env, &["plugin", "dir"]);
    assert_eq!((code, out.trim()), (0, dir.to_string_lossy().as_ref()));
    let (code, _, err) = auk(&env, &["plugin", "reload"]);
    assert_eq!((code, err.trim()), (1, "auk plugin: no daemon running"));
    let (code, _, err) = auk(&env, &["plugin", "frob"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.starts_with("auk plugin: plugin takes other arguments\nusage: auk plugin"), "{err}");
    let types = env.root.join("types");
    let (code, out, _) = auk(&env, &["plugin", "types", &types.to_string_lossy()]);
    assert_eq!(code, 0);
    assert_eq!(out.trim(), format!("wrote {}", types.join("tern.d.luau").display()));

    let src = env.root.join("src");
    copy_dir(&Path::new(EXAMPLES).join("terraform"), &src.join("terraform"));
    copy_dir(&Path::new(EXAMPLES).join("dirvars"), &src.join("dirvars"));
    let (code, out, err) = auk(&env, &["plugin", "install", &src.join("terraform").to_string_lossy()]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.trim(), format!("installed terraform 1.0.0 → {}", dir.join("terraform").display()));
    assert_eq!(err.trim(), "no daemon running; changes apply at next start");
    let (_, out, _) = auk(&env, &["plugin", "list"]);
    assert!(out.starts_with("terraform 1.0.0 Terraform — 0 blocks, 1 lens  ready"), "{out}");
    assert!(!env.control_path().exists() && !env.socket.exists(), "`auk plugin` started a daemon");

    // With a daemon: it loads what's installed, and every verb reloads it.
    let _daemon = env.start_daemon();
    let window = env.start_headless("w", &[]);
    let catalog = || window.request("catalog");
    let ids = |c: &Value| -> Vec<String> {
        c["plugins"].as_array().map(|p| p.iter().map(|p| p["id"].as_str().unwrap().to_owned()).collect()).unwrap_or_default()
    };
    assert_eq!(ids(&catalog()), ["terraform"], "the catalog arrives in the welcome");
    let (code, out, err) = auk(&env, &["plugin", "link", &src.join("dirvars").to_string_lossy()]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("linked dirvars "), "{out}");
    assert_eq!(err, "", "the daemon reloaded");
    assert_eq!(ids(&catalog()), ["dirvars", "terraform"], "a catalog message after the reload");
    let (code, out, _) = auk(&env, &["plugin", "list", "--json"]);
    assert_eq!(code, 0);
    let listed: Value = serde_json::from_str(&out).expect("json");
    assert_eq!(listed["plugins"][0]["status"], "ready");

    // A change in the folder reloads every plugin, with no verb.
    let toml = dir.join("terraform/plugin.toml");
    let text = std::fs::read_to_string(&toml).expect("read").replace("version = \"1.0.0\"", "version = \"1.0.1\"");
    std::fs::write(&toml, text).expect("write");
    wait_for("the watcher's reload", || {
        let c = catalog();
        c["plugins"].as_array()?.iter().find(|p| p["id"] == "terraform" && p["version"] == "1.0.1").map(drop)
    });

    // A broken package: reload reports it and exits 1.
    std::fs::create_dir_all(dir.join("broken")).expect("mkdir");
    std::fs::write(dir.join("broken/plugin.toml"), "schema = 1\nid = \"broken\"\n").expect("write");
    let (code, out, _) = auk(&env, &["plugin", "reload"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.lines().any(|l| l.starts_with("problem ")), "{out}");
    std::fs::remove_dir_all(dir.join("broken")).expect("rm");

    let (code, out, _) = auk(&env, &["plugin", "unlink", "dirvars"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("unlinked dirvars ("), "{out}");
    let (code, out, _) = auk(&env, &["plugin", "remove", "terraform"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("removed terraform ("), "{out}");
    assert_eq!(ids(&catalog()), Vec::<String>::new());
    let (code, out, _) = auk(&env, &["plugin", "reload"]);
    assert_eq!((code, out.as_str()), (0, "no plugins\n"));
}

// ---------------------------------------------------------------------------------------------
// M9: window halves in the real app

#[cfg(target_os = "macos")]
mod window_half {
    use super::*;
    use auk_conformance::app::{App, InputSource, US_LAYOUT};

    /// A test env with these examples installed, its daemon, and an app window on it, typing
    /// with the US layout (the strokes name ANSI keys).
    pub fn app_with(
        name: &str,
        examples: &[&str],
        vars: &[(&str, &str)],
    ) -> (TestEnv, App, InputSource) {
        let env = TestEnv::new(EXE, name);
        for e in examples {
            example(&env, e);
        }
        env.start_daemon();
        let app = App::start_with(&env, "app", &[], vars);
        let layout = InputSource::select(&app, US_LAYOUT);
        (env, app, layout)
    }

    /// The focused pane, once its shell is at a prompt.
    pub fn shell(env: &TestEnv, app: &App) -> u64 {
        let pane = app.state()["focused"].as_u64().expect("a focused pane");
        env.wait(pane, "prompt");
        pane
    }

    /// Types a line into the focused pane through the window, and waits for the next prompt.
    pub fn type_line(env: &TestEnv, app: &App, pane: u64, line: &str) {
        app.request(&format!("text {}", quote(line)));
        app.request("type return");
        env.wait(pane, "prompt");
    }

    pub fn loaded(app: &App, plugins: &[&str]) -> Value {
        wait_for("the window halves to load", || {
            let v = app.request("plugins");
            let ids: Vec<&str> = v["loaded"].as_array()?.iter().filter_map(Value::as_str).collect();
            plugins.iter().all(|p| ids.contains(p)).then_some(v)
        })
    }

    pub fn toasts(app: &App) -> Vec<Value> {
        app.request("toasts").as_array().cloned().unwrap_or_default()
    }

    pub fn wait_toast(app: &App, text: &str) -> Value {
        wait_for(&format!("the toast {text:?}"), || {
            toasts(app).into_iter().find(|t| t["text"] == text)
        })
    }

    /// Opens the palette, types `query` and runs the selected row with Return.
    pub fn palette_run(app: &App, query: &str, expect_action: &str) {
        app.request("menu View 'Command Palette…'");
        wait_for("the palette", || (app.request("palette")["open"] == true).then_some(()));
        app.request(&format!("text {}", quote(query)));
        let rows = wait_for("the palette's rows", || {
            let p = app.request("palette");
            (p["query"] == query).then(|| p["rows"].clone())
        });
        assert_eq!(rows[0]["action"], expect_action, "{rows}");
        app.request("type return");
        wait_for("the palette to close", || (app.request("palette")["open"] == false).then_some(()));
    }

    /// The panes of the window key's layout, by tab name (`""` for unnamed tabs).
    pub fn tab_panes(app: &App) -> Vec<(String, Vec<u64>)> {
        let layout = app.layout();
        let mut out = Vec::new();
        for s in layout["sessions"].as_array().into_iter().flatten() {
            for t in s["tabs"].as_array().into_iter().flatten() {
                let mut panes = Vec::new();
                fn walk(n: &Value, out: &mut Vec<u64>) {
                    if let Some(p) = n["pane"].as_u64() {
                        out.push(p);
                    }
                    for k in ["first", "second"] {
                        if n[k].is_object() {
                            walk(&n[k], out);
                        }
                    }
                }
                walk(&t["root"], &mut panes);
                out.push((t["title"].as_str().unwrap_or_default().to_owned(), panes));
            }
        }
        out
    }

    /// Clicks the middle of a chrome element by its action.
    pub fn click_chrome(app: &App, action: &str) {
        let rect = wait_for(&format!("the chrome's {action}"), || {
            app.request("chrome")["actions"]
                .as_array()?
                .iter()
                .find(|a| a["action"] == action)
                .map(|a| a["rect"].clone())
        });
        let x = rect[0].as_f64().unwrap() + rect[2].as_f64().unwrap() / 2.0;
        let y = rect[1].as_f64().unwrap() + rect[3].as_f64().unwrap() / 2.0;
        app.request(&format!("mouse down {x} {y}"));
        app.request(&format!("mouse up {x} {y}"));
    }

    /// The daemon's pane kinds: pane → block kind.
    pub fn blocks(env: &TestEnv) -> Vec<(u64, String)> {
        env.daemon("ls")["panes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["exit"].is_null())
            .filter_map(|p| Some((p["id"].as_u64()?, p["kind"]["block"].as_str()?.to_owned())))
            .collect()
    }
}

#[cfg(target_os = "macos")]
#[test]
fn m9_longrun_command_bind_status_segment_and_block_through_the_app() {
    use window_half::*;
    let (env, app, _us) = app_with("m9-longrun", &["longrun"], &[]);
    let pane = shell(&env, &app);
    let plugins = loaded(&app, &["longrun"]);
    assert!(plugins["failures"].as_array().unwrap().is_empty(), "{plugins}");
    assert_eq!(plugins["commands"][0]["action"], "plugin.longrun.show", "{plugins}");
    // The bind joins the keymap: no default takes cmd+alt+shift+l.
    assert_eq!(plugins["keymap"][0]["action"], "plugin.longrun.show", "{plugins}");

    // The palette lists the command, and running it opens the Slow Commands block in a tab.
    palette_run(&app, "slow commands", "plugin.longrun.show");
    let block = wait_for("the Slow Commands block", || {
        blocks(&env).into_iter().find(|(_, k)| k == "longrun.slow").map(|(p, _)| p)
    });
    wait_for("the block's tab, focused", || {
        let tabs = tab_panes(&app);
        (tabs.len() == 2 && app.state()["focused"] == block).then_some(())
    });
    wait_for("the block's title", || title(&env, block).starts_with("Slow Commands").then_some(()));

    // Back in the shell, the bind finds the open block instead of opening another.
    app.request(&format!("focus {pane}"));
    wait_for("the window to know the block's title", || {
        app.request("panes")["panes"]
            .as_array()?
            .iter()
            .any(|p| p["id"] == block && p["title"].as_str().is_some_and(|t| t.starts_with("Slow Commands")))
            .then_some(())
    });
    app.request("type cmd+opt+shift+l");
    wait_for("the bind to focus the block", || (app.state()["focused"] == block).then_some(()));
    assert_eq!(blocks(&env).len(), 1, "no second block");
    assert_eq!(tab_panes(&app).len(), 2);

    // A command running in the focused pane shows in the status line once it passes a second,
    // ticking, and goes when it finishes.
    app.request(&format!("focus {pane}"));
    app.request(&format!("text {}", quote("sleep 3")));
    app.request("type return");
    let seg = wait_for("the running command's status segment", || {
        app.request("chrome")["status"]
            .as_array()?
            .iter()
            .find(|s| s["plugin"] == "longrun" && s["text"].as_str().is_some_and(|t| t.starts_with("sleep · ")))
            .cloned()
    });
    assert_eq!(seg["command"], "plugin.longrun.show", "{seg}");
    assert_eq!(seg["tone"], "muted", "{seg}");
    wait_for("the segment to tick", || {
        app.request("chrome")["status"]
            .as_array()?
            .iter()
            .any(|s| s["text"] == "sleep · 2s")
            .then_some(())
    });
    // A click on the segment runs its command.
    click_chrome(&app, "run:plugin.longrun.show");
    wait_for("the click to focus the block", || (app.state()["focused"] == block).then_some(()));
    env.wait(pane, "prompt");
    app.request(&format!("focus {pane}"));
    wait_for("the segment to go", || {
        app.request("chrome")["status"]
            .as_array()?
            .iter()
            .all(|s| s["plugin"] != "longrun")
            .then_some(())
    });
}

#[cfg(target_os = "macos")]
#[test]
fn m9_workspaces_open_override_status_and_css_through_the_app() {
    use window_half::*;
    let (env, app, _us) = app_with("m9-workspaces", &["workspaces", "longrun"], &[]);
    // A project whose tabs run programs that stay up (the sample's nvim and npm aren't here).
    let root = env.root.join("webapp");
    for d in ["web", "api", "infra", ".tern"] {
        std::fs::create_dir_all(root.join(d)).expect("mkdir");
    }
    std::fs::write(
        root.join(".tern/workspace.json"),
        json!({
            "name": "webapp",
            "tabs": [
                {"name": "code", "split": "right", "panes": [
                    {"command": "cat"},
                    {"split": "down", "panes": [
                        {"cwd": "web", "command": "cat"},
                        {"cwd": "api", "command": "cat"}
                    ]}
                ]},
                {"name": "shell", "panes": [{}, {"block": "longrun.slow"}]},
                {"name": "infra", "cwd": "infra"}
            ]
        })
        .to_string(),
    )
    .expect("write workspace.json");
    let root = std::fs::canonicalize(&root).expect("canonical root");
    let root_s = root.to_string_lossy().into_owned();
    let pane = shell(&env, &app);
    let plugins = loaded(&app, &["workspaces", "longrun"]);
    // Manifest styles install into the chrome document.
    assert!(
        plugins["sheets"].as_array().unwrap().iter().any(|s| s == "plugin:local:workspaces:styles"),
        "{plugins}"
    );
    type_line(&env, &app, pane, &format!("cd {}", quote(&format!("{root_s}/web"))));

    palette_run(&app, "open workspace", "plugin.workspaces.open");
    let toast = wait_toast(&app, "Opened webapp");
    assert_eq!(toast["level"], "success");
    assert_eq!(toast["sub"], format!("3 tabs · {root_s}"));
    let tabs = wait_for("the workspace's tabs", || {
        let t = tab_panes(&app);
        let names: Vec<&str> = t.iter().map(|(n, _)| n.as_str()).collect();
        (names == ["", "code", "shell", "infra"] && t[1].1.len() == 3 && t[2].1.len() == 2).then_some(t)
    });
    let info = |p: u64| env.pane_info(p);
    let code = &tabs[1].1;
    assert_eq!(info(code[0])["kind"]["line"], "cat");
    assert_eq!(info(code[1])["cwd"], format!("{root_s}/web"));
    assert_eq!(info(code[2])["cwd"], format!("{root_s}/api"));
    let shell_tab = &tabs[2].1;
    assert_eq!(info(shell_tab[1])["kind"]["block"], "longrun.slow");
    assert_eq!(info(tabs[3].1[0])["cwd"], format!("{root_s}/infra"));

    // The status line names the workspace for a pane inside it, in the plugin's own style.
    app.request(&format!("focus {pane}"));
    let seg = wait_for("the workspace's status segment", || {
        app.request("chrome")["status"]
            .as_array()?
            .iter()
            .find(|s| s["plugin"] == "workspaces")
            .cloned()
    });
    assert_eq!(seg["text"], "webapp");
    assert_eq!(seg["command"], "plugin.workspaces.edit");
    // The segment is drawn with the manifest sheet's rule (`font-weight: 600`).
    let style = wait_for("the drawn segment", || {
        let r = app.send("chrome-style workspaces");
        (r["ok"] == true).then(|| r["value"].clone())
    });
    assert!(style["font_weight"].to_string().contains("600"), "{style}");

    // New Tab from inside the workspace starts at its root: the override replaces the built-in.
    menu_new_tab(&app);
    let new = wait_for("the override's tab", || {
        let t = tab_panes(&app);
        (t.len() == 5).then(|| t[4].1[0])
    });
    wait_for("the new tab at the root", || (info(new)["cwd"] == root_s.as_str()).then_some(()));

    // The bind opens it again, which says so.
    app.request(&format!("focus {pane}"));
    app.request("type cmd+opt+shift+o");
    wait_toast(&app, "webapp is already open");

    // Closing every workspace tab forgets it: the segment goes and New Tab is the built-in again.
    for (_, panes) in &tabs[1..] {
        for p in panes {
            env.daemon(&format!("close {p}"));
        }
    }
    wait_for("the workspace tabs to close", || (tab_panes(&app).len() == 2).then_some(()));
    app.request(&format!("focus {pane}"));
    wait_for("the segment to go", || {
        app.request("chrome")["status"]
            .as_array()?
            .iter()
            .all(|s| s["plugin"] != "workspaces")
            .then_some(())
    });
}

#[cfg(target_os = "macos")]
fn menu_new_tab(app: &auk_conformance::app::App) {
    app.request("menu Shell 'New Tab'");
}

#[cfg(target_os = "macos")]
#[test]
fn m9_jsonx_route_open_through_auk_open_and_the_palette() {
    use window_half::*;
    let opened = |env: &TestEnv| env.root.join("opened.txt");
    let (env, app, _us) = {
        let env = TestEnv::new(EXE, "m9-jsonx");
        example(&env, "jsonx");
        let opener = env.root.join("opener.sh");
        executable(
            &opener,
            &format!("#!/bin/sh\nprintf '%s\\n' \"$1\" >> {}\n", quote(&opened(&env).to_string_lossy())),
        );
        env.start_daemon();
        let app = auk_conformance::app::App::start_with(
            &env,
            "app",
            &[],
            &[("AUK_OPENER", &opener.to_string_lossy())],
        );
        let us = auk_conformance::app::InputSource::select(&app, auk_conformance::app::US_LAYOUT);
        (env, app, us)
    };
    let pane = shell(&env, &app);
    loaded(&app, &["jsonx"]);
    let data = env.root.join("data.json");
    std::fs::write(&data, r#"{"name": "auk", "tags": ["terminal", "plugins"]}"#).expect("write");
    let data = std::fs::canonicalize(&data).expect("canonical");
    let data_s = data.to_string_lossy().into_owned();
    let opened_lines = |env: &TestEnv| {
        std::fs::read_to_string(opened(env))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };

    // `auk open` reaches the window, whose route.open sees origin `cli`: jsonx leaves those to
    // the built-in open (the system opener, here a stand-in).
    let out = env.cli(&["--json", "open", &data_s]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(v["routed"], "window", "{v}");
    wait_for("the window's built-in open", || (opened_lines(&env) == [data_s.clone()]).then_some(()));
    let opens = app.request("opens");
    assert_eq!(opens["opened"][0]["origin"], "cli", "{opens}");
    assert!(opens["routed"].as_array().unwrap().is_empty(), "{opens}");
    assert!(blocks(&env).is_empty());

    // A path printed in the terminal and ⌘-clicked is origin `link`: jsonx leaves it too.
    type_line(&env, &app, pane, &format!("echo {}", quote(&data_s)));
    let (row, col) = wait_for("the printed path", || {
        let text = app.request(&format!("capture {pane}"))["text"].as_str()?.to_owned();
        text.lines().enumerate().find_map(|(i, l)| (l.trim_end() == data_s).then_some((i, 3usize)))
    });
    let layout = app.request(&format!("pane-layout {pane}"));
    let rect = app.state()["visible"][0]["rect"].clone();
    let info = app.request("panes")["panes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == pane)
        .cloned()
        .unwrap();
    let cw = (rect[2].as_f64().unwrap() - 12.0) / info["cols"].as_f64().unwrap();
    let ch = (rect[3].as_f64().unwrap() - 12.0) / info["rows"].as_f64().unwrap();
    let first = layout["first_line"].as_u64().unwrap() as f64;
    let x = layout["origin"][0].as_f64().unwrap() + (col as f64 + 0.5) * cw;
    let y = layout["origin"][1].as_f64().unwrap() + (row as f64 - first + 0.5) * ch.floor();
    app.request(&format!("mouse down {x} {y} --cmd"));
    app.request(&format!("mouse up {x} {y} --cmd"));
    wait_for("the clicked path's built-in open", || (opened_lines(&env).len() == 2).then_some(()));
    let opens = app.request("opens");
    assert_eq!(opens["opened"][1]["origin"], "link", "{opens}");
    assert_eq!(opens["opened"][1]["target"], data_s.as_str(), "{opens}");
    assert!(blocks(&env).is_empty());

    // "Open file…" in the palette is origin `palette`: jsonx claims .json and the JSON Explorer
    // block opens beside the pane, on the file.
    palette_run(&app, &data_s, &format!("open:{data_s}"));
    let block = wait_for("the JSON Explorer block", || {
        blocks(&env).into_iter().find(|(_, k)| k == "jsonx.explorer").map(|(p, _)| p)
    });
    assert_eq!(env.daemon("blocks")[block.to_string()]["args"], json!([data_s]));
    let opens = app.request("opens");
    assert_eq!(opens["routed"][0]["block"], "jsonx.explorer", "{opens}");
    assert_eq!(opens["routed"][0]["origin"], "palette", "{opens}");
    wait_for("the block beside the shell, focused", || {
        let tabs = tab_panes(&app);
        (tabs[0].1 == [pane, block] && app.state()["focused"] == block).then_some(())
    });
    wait_for("the explorer's tree", || daemon_doc(&env, block).contains("tags").then_some(()));
    wait_for("the block's title", || (title(&env, block) == "data.json").then_some(()));

    // The toggle (in kv.json, shared by every VM) hands .json opens back to the editor.
    app.request(&format!("focus {pane}"));
    palette_run(&app, "toggle json", "plugin.jsonx.toggle_route");
    wait_toast(&app, ".json files open in the editor");
    palette_run(&app, &data_s, &format!("open:{data_s}"));
    wait_for("the built-in open from the palette", || (opened_lines(&env).len() == 3).then_some(()));
    assert_eq!(app.request("opens")["opened"][2]["origin"], "palette");
    assert_eq!(blocks(&env).len(), 1, "no second block");
}

#[cfg(target_os = "macos")]
#[test]
fn ac14_window_code_cannot_write_block_frames() {
    use window_half::*;
    let env = TestEnv::new(EXE, "ac14-window");
    let host = r#"
        tern.block.define("b", {
            init = function(cx) return {} end,
            view = function() return { main = tern.ui.text("block body") } end,
            title = function() return "Probe Block" end,
        })
    "#;
    let window = r#"
        local function probe(t, names)
            local out = {}
            for _, name in names do
                local ok, v = pcall(function() return (t :: any)[name] end)
                out[name] = if ok then type(v) else "error"
            end
            return out
        end
        local WRITE = { "frame", "render", "save", "exit", "blob" }
        tern.command({
            id = "probe",
            title = "Probe block writing",
            run = function(cx)
                tern.kv.set("context", tern.context)
                tern.kv.set("tern", probe(tern, { "block", "lens", "frame", "render" }))
                tern.kv.set("cx", probe(cx, WRITE))
                local ok_pane, pane_t = pcall(function() return (tern :: any).pane end)
                tern.kv.set("pane", if ok_pane and type(pane_t) == "table" then probe(pane_t, { "write" }) else {})
                -- Raw TSP bytes typed into the block pane are keys for its handler, never frames.
                local block = cx.session:resolve("Probe Block")
                tern.kv.set("block", block)
                local ok = pcall(function()
                    cx:run(block, "\27_tsp;f;sf=injected;s=1\27\\")
                end)
                tern.kv.set("ran", ok)
            end,
        })
    "#;
    package(
        &env,
        "probe",
        "schema = 1\nid = \"probe\"\nname = \"Probe\"\nversion = \"1\"\nhost = \"host.luau\"\nwindow = \"window.luau\"\n[[blocks]]\nid = \"b\"\ntitle = \"B\"\n",
        &[("host.luau", host), ("window.luau", window)],
    );
    env.start_daemon();
    let app = auk_conformance::app::App::start(&env, "app", &[]);
    let _us = auk_conformance::app::InputSource::select(&app, auk_conformance::app::US_LAYOUT);
    let pane = shell(&env, &app);
    loaded(&app, &["probe"]);
    let block = env.daemon(&format!("block probe.b --split {pane} right"))["pane"]
        .as_u64()
        .expect("a block pane");
    wait_for("the block's view", || daemon_doc(&env, block).contains("block body").then_some(()));
    wait_for("the window to know the block's title", || {
        app.request("panes")["panes"]
            .as_array()?
            .iter()
            .any(|p| p["id"] == block && p["title"] == "Probe Block")
            .then_some(())
    });
    let before = env.daemon(&format!("tsp {block}"))["surfaces"].clone();
    palette_run(&app, "probe block", "plugin.probe.probe");
    let got = wait_for("the window probes", || {
        let v = kv(&env, "probe");
        (!v["ran"].is_null()).then_some(v)
    });
    assert_eq!(got["context"], "window");
    assert_eq!(got["block"], block, "{got}");
    for table in ["tern", "cx", "pane"] {
        for (name, ty) in got[table].as_object().expect("a probe") {
            assert!(
                ty == "nil" || ty == "error",
                "window code reaches {table}.{name} ({ty}): {got}"
            );
        }
    }
    // The bytes went to the block as keys: its surfaces are the same, with no `injected` one.
    std::thread::sleep(Duration::from_millis(500));
    let after = env.daemon(&format!("tsp {block}"))["surfaces"].clone();
    assert_eq!(before, after);
    assert!(!after.to_string().contains("injected"));
}

#[cfg(target_os = "macos")]
#[test]
fn m9_window_budgets_trip_in_the_app_and_reload_on_change_restores_the_hook() {
    use window_half::*;
    let env = TestEnv::new(EXE, "m9-budgets");
    let manifest = |id: &str, name: &str| {
        format!("schema = 1\nid = \"{id}\"\nname = \"{name}\"\nversion = \"1\"\nwindow = \"window.luau\"\n")
    };
    let spinning = r#"
        tern.command({ id = "spin", title = "Spin forever", run = function(cx) while true do end end })
        tern.command({ id = "ok", title = "Still fine", run = function(cx) cx:toast("info", "still fine") end })
        tern.chrome.status(function(p)
            local t = tern.now()
            while tern.now() - t < 100 do end
            return { { text = "never shown" } }
        end)
    "#;
    package(&env, "slow", &manifest("slow", "Slow"), &[("window.luau", spinning)]);
    package(
        &env,
        "steady",
        &manifest("steady", "Steady"),
        &[("window.luau", r#"tern.chrome.status(function(p) return { { text = "steady segment" } } end)"#)],
    );
    env.start_daemon();
    let app = auk_conformance::app::App::start(&env, "app", &[]);
    shell(&env, &app);
    loaded(&app, &["slow", "steady"]);

    // The status formatter passes 4 ms: it trips, toasts once, and the other plugin's segment
    // still shows.
    let trip = wait_for("the formatter's trip", || {
        toasts(&app).into_iter().find(|t| t["sub"].as_str().is_some_and(|s| s.contains("exceeded 4 ms")))
    });
    assert!(trip["text"].as_str().unwrap().starts_with("Plugin Slow"), "{trip}");
    wait_for("the steady segment", || {
        let status = app.request("chrome")["status"].clone();
        let texts: Vec<&str> = status.as_array()?.iter().filter_map(|s| s["text"].as_str()).collect();
        (texts == ["steady segment"]).then_some(())
    });

    // A command that never returns is cut at 50 ms; the window carries on.
    let started = Instant::now();
    palette_run(&app, "spin forever", "plugin.slow.spin");
    let trip = wait_for("the command's trip", || {
        toasts(&app).into_iter().find(|t| t["sub"].as_str().is_some_and(|s| s.contains("exceeded 50 ms")))
    });
    assert!(trip["text"].as_str().unwrap().starts_with("Plugin Slow"), "{trip}");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    palette_run(&app, "still fine", "plugin.slow.ok");
    wait_toast(&app, "still fine");
    // Tripped hooks stay off until reload: spinning again does nothing, and toasts nothing.
    let before = toasts(&app).len();
    palette_run(&app, "spin forever", "plugin.slow.spin");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(toasts(&app).len(), before);
    assert_eq!(
        toasts(&app).iter().filter(|t| t["sub"].as_str().is_some_and(|s| s.contains("exceeded 4 ms"))).count(),
        1,
        "the formatter toasted once"
    );

    // Editing the plugin reloads the window halves: the hook is back.
    std::fs::write(
        plugins_dir(&env).join("slow/window.luau"),
        r#"tern.command({ id = "spin", title = "Spin forever", run = function(cx) cx:toast("info", "spun") end })"#,
    )
    .expect("rewrite the plugin");
    wait_for("the reload", || {
        let commands = app.request("plugins")["commands"].clone();
        (commands.as_array()?.iter().filter(|c| c["action"].as_str().is_some_and(|a| a.starts_with("plugin.slow."))).count() == 1)
            .then_some(())
    });
    palette_run(&app, "spin forever", "plugin.slow.spin");
    wait_toast(&app, "spun");
}

/// The middle of a rect `[x, y, w, h]`.
#[cfg(target_os = "macos")]
fn middle(rect: &Value) -> (f64, f64) {
    let n = |i: usize| rect[i].as_f64().unwrap_or_default();
    (n(0) + n(2) / 2.0, n(1) + n(3) / 2.0)
}

/// Clicks a point in the app: the pointer moves there first (hover shows a block's toolbar).
#[cfg(target_os = "macos")]
fn click_at(app: &auk_conformance::app::App, (x, y): (f64, f64)) {
    app.request(&format!("mouse move {x} {y}"));
    app.request(&format!("mouse down {x} {y}"));
    app.request(&format!("mouse up {x} {y}"));
}

/// §9.6, §8.2 (`block` drawn internally), §8.4 step 3, §10.4 Desktop "lens views": the app draws
/// a lensed command as a block in the flow, raw until the view arrives, then the view with the
/// plugin's manifest CSS; Native/Raw is a local toggle; Copy copies the raw output as plain
/// text; a click in the view is a lens event whose `copy` effect reaches the app's clipboard.
#[cfg(target_os = "macos")]
#[test]
fn m9_lens_blocks_draw_in_the_app_with_native_raw_copy_and_view_events() {
    use window_half::*;
    let (env, app, _us) = app_with("m9-lens", &["terraform"], &[]);
    let tf = terraform(&env);
    let pane = shell(&env, &app);
    type_line(&env, &app, pane, &format!("{} plan", tf.display()));

    // The block draws in the window's flow with the lens's view.
    let block = wait_for("the command block, drawn with its view", || {
        let b = app.request(&format!("lens {pane}"))["blocks"][0].clone();
        (b["shown"] == "native" && b["rect"].is_array() && b["text"].as_str()?.contains("2 to add"))
            .then_some(b)
    });
    assert_eq!((block["plugin"].as_str(), block["status"].as_i64()), (Some("terraform"), Some(2)));
    let parts = block["parts"].as_array().unwrap().clone();
    let part = |class: &str, name: &str| {
        parts
            .iter()
            .find(|p| p["class"].as_str().unwrap().split(' ').any(|c| c == class) && p["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("no {class} {name} in {parts:?}"))
    };
    // The plugin's manifest sheet styles the view (cascade step 3): the danger rule's padding.
    assert!(
        app.request(&format!("surface-sheets {pane}")).as_array().unwrap().iter().any(|s| s == "plugin:local:terraform:styles"),
        "the manifest styles install into the pane's document"
    );
    let style = app.request(&format!("lens-style {pane} terraform.plan.danger"));
    assert!(style["padding"].as_str().unwrap().contains("10"), "{style}");

    // Raw shows the captured output; Native comes back.
    click_at(&app, middle(&part("sf-block-opt", "raw")["rect"]));
    wait_for("the raw output", || {
        let b = app.request(&format!("lens {pane}"))["blocks"][0].clone();
        (b["shown"] == "raw").then_some(())
    });
    // The raw output is taller than the pane, which follows its tail: scroll back to the
    // block's toolbar.
    let raw = block_top_in_view(&app, pane);
    assert!(raw["text"].as_str().unwrap().contains("Plan: 2 to add, 1 to change, 2 to destroy"), "{raw}");

    // Copy copies the raw text as plain text.
    app.request("clipboard-set nothing");
    let raw_parts = raw["parts"].as_array().unwrap().clone();
    let copy = raw_parts.iter().find(|p| p["class"].as_str().unwrap().contains("sf-block-copy")).unwrap();
    click_at(&app, middle(&copy["rect"]));
    let copied = wait_for("the raw output on the clipboard", || {
        let c = app.request("clipboard");
        c.as_str().filter(|t| t.contains("Plan: 2 to add")).map(str::to_owned)
    });
    assert!(!copied.contains('\x1b'), "plain text: {copied:?}");

    let native = raw_parts.iter().find(|p| p["name"] == "native").unwrap();
    click_at(&app, middle(&native["rect"]));
    let block = wait_for("the view again", || {
        let b = app.request(&format!("lens {pane}"))["blocks"][0].clone();
        (b["shown"] == "native" && b["parts"].as_array()?.len() >= 3).then_some(b)
    });

    // A click on a destroy row is a lens event: the lens copies the address, an effect the app
    // carries out.
    let row = block["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["class"].as_str().unwrap().contains("sf-act") && p["text"].as_str().unwrap().contains("aws_instance.old"))
        .cloned()
        .expect("the destroy row");
    click_at(&app, middle(&row["rect"]));
    wait_for("the address on the clipboard", || {
        (app.request("clipboard") == "aws_instance.old").then_some(())
    });
}

/// §9.4, §9.8 "CSS": the window's plugin sheets come from the host's catalog, so a plugin whose
/// host entry failed contributes no styles, while a Ready one does.
#[cfg(target_os = "macos")]
#[test]
fn m9_catalog_drives_plugin_css_and_a_failed_host_contributes_none() {
    use window_half::*;
    let env = TestEnv::new(EXE, "m9-catalog-css");
    package(
        &env,
        "good",
        "schema = 1\nid = \"good\"\nname = \"Good\"\nversion = \"1\"\nhost = \"host.luau\"\nstyles = [\"good.css\"]\n",
        &[("good.css", ".good { color: red; }"), ("host.luau", "-- nothing to do")],
    );
    package(
        &env,
        "broken",
        "schema = 1\nid = \"broken\"\nname = \"Broken\"\nversion = \"1\"\nhost = \"host.luau\"\nstyles = [\"broken.css\"]\n",
        &[("broken.css", ".broken { color: red; }"), ("host.luau", "error('no')")],
    );
    env.start_daemon();
    let app = auk_conformance::app::App::start(&env, "app", &[]);
    let pane = shell(&env, &app);
    let catalog = wait_for("the catalog's styles", || {
        let c = app.request("catalog");
        (c["styles"].as_array()?.iter().any(|s| s == "good")).then_some(c)
    });
    assert_eq!(catalog["host"], "local");
    assert!(!catalog["styles"].as_array().unwrap().iter().any(|s| s == "broken"), "{catalog}");
    let sheets: Vec<String> =
        catalog["sheets"].as_array().unwrap().iter().filter_map(|s| s.as_str().map(str::to_owned)).collect();
    assert!(sheets.contains(&"plugin:local:good:styles".to_owned()), "{sheets:?}");
    assert!(!sheets.iter().any(|s| s.contains("broken")), "a Failed host contributes nothing: {sheets:?}");
    let chrome = app.request("plugins")["sheets"].clone();
    assert!(!chrome.to_string().contains("broken"), "{chrome}");
    wait_for("the sheets in the pane's document", || {
        let s = app.request(&format!("surface-sheets {pane}"));
        s.as_array()?.iter().any(|n| n == "plugin:local:good:styles").then_some(())
    });
}

/// §9.5 "Opening": the palette offers "New <title> block" for Ready plugins' palette block
/// types, and an open whose type is gone shows "Could not start the block" with Retry, which
/// opens it once the type is back.
#[cfg(target_os = "macos")]
#[test]
fn m9_palette_new_block_rows_and_the_could_not_start_card_with_retry() {
    use window_half::*;
    let (env, app, _us) = app_with("m9-new-block", &["dirvars"], &[]);
    shell(&env, &app);
    wait_for("the block type in the window's catalog", || {
        let c = app.request("catalog");
        c["block_types"].as_array()?.iter().any(|b| b["kind"] == "dirvars.approve").then_some(())
    });
    palette_run(&app, "new directory variables block", "block:dirvars.approve");
    let first = wait_for("the block pane", || {
        blocks(&env).into_iter().find(|(_, k)| k == "dirvars.approve").map(|(p, _)| p)
    });
    assert!(app.request("catalog")["card"].is_null());

    // The type goes while the palette is open: the open fails and the card shows.
    app.request("menu View 'Command Palette…'");
    wait_for("the palette", || (app.request("palette")["open"] == true).then_some(()));
    app.request(&format!("text {}", quote("new directory variables block")));
    wait_for("the row", || {
        let p = app.request("palette");
        (p["rows"][0]["action"] == "block:dirvars.approve").then_some(())
    });
    let away = env.root.join("dirvars-away");
    std::fs::rename(plugins_dir(&env).join("dirvars"), &away).expect("move the plugin away");
    let (code, out, err) = auk(&env, &["plugin", "reload"]);
    assert_eq!(code, 0, "{out}{err}");
    wait_for("the type to leave the window's catalog", || {
        let c = app.request("catalog");
        (!c["block_types"].as_array()?.iter().any(|b| b["kind"] == "dirvars.approve")).then_some(())
    });
    app.request("type return");
    let card = wait_for("the card", || {
        let c = app.request("catalog")["card"].clone();
        (!c.is_null()).then_some(c)
    });
    assert_eq!(card["kind"], "dirvars.approve");
    assert!(card["error"].as_str().unwrap().contains("dirvars.approve"), "{card}");
    assert!(blocks(&env).iter().all(|(p, _)| *p == first), "nothing opened");

    // The type is back: Retry opens it and the card goes.
    std::fs::rename(&away, plugins_dir(&env).join("dirvars")).expect("move the plugin back");
    let (code, out, err) = auk(&env, &["plugin", "reload"]);
    assert_eq!(code, 0, "{out}{err}");
    wait_for("the type back in the window's catalog", || {
        let c = app.request("catalog");
        c["block_types"].as_array()?.iter().any(|b| b["kind"] == "dirvars.approve").then_some(())
    });
    click_chrome(&app, "run:card:retry");
    wait_for("the retried block pane", || {
        blocks(&env).iter().any(|(p, k)| *p != first && k == "dirvars.approve").then_some(())
    });
    wait_for("the card to go", || app.request("catalog")["card"].is_null().then_some(()));
}

/// The pane's first command block once its top shows: scrolls the pane back while it is above.
#[cfg(target_os = "macos")]
fn block_top_in_view(app: &auk_conformance::app::App, pane: u64) -> Value {
    let rect = app.state()["visible"]
        .as_array()
        .and_then(|v| v.iter().find(|p| p["pane"] == pane).map(|p| p["rect"].clone()))
        .expect("the pane's rect");
    let (x, y) = middle(&rect);
    wait_for("the block's top in view", || {
        let b = app.request(&format!("lens {pane}"))["blocks"][0].clone();
        let top = b["rect"][1].as_f64()?;
        if top < rect[1].as_f64()? + 4.0 {
            app.request(&format!("scroll 5 {x} {y}"));
            return None;
        }
        (b["parts"].as_array()?.len() >= 3).then_some(b)
    })
}

// ---------------------------------------------------------------------------------------------
// Host hooks through the daemon, settings, and the CLI (§9.1, §9.5, §9.7, §9.11)

/// A plugin whose `title` and `pane_exited` hooks toast what they hear.
fn hooks_plugin(env: &TestEnv) {
    package(
        env,
        "hooks",
        "schema = 1\nid = \"hooks\"\nname = \"Hooks\"\nversion = \"1\"\nhost = \"host.luau\"\n",
        &[(
            "host.luau",
            r#"tern.on("title", function(ev, cx) cx:toast("info", "title " .. ev.title, "pane " .. ev.pane) end)
               tern.on("pane_exited", function(ev, cx) cx:toast("info", "exited " .. ev.pane .. " with " .. ev.status) end)"#,
        )],
    );
}

#[test]
fn title_and_pane_exited_hooks_run_in_the_daemon_and_effects_reach_only_the_panes_windows() {
    let env = TestEnv::new(EXE, "hooks");
    hooks_plugin(&env);
    let _daemon = env.start_daemon();
    let main = env.start_headless("main", &[]);
    let other = env.start_headless("other", &["--window", "other"]);
    let pane = env.new_pane(&[]);
    env.wait(pane, "prompt");

    // `title` fires from the daemon's own stream (no window half involved); its toast goes to the
    // windows showing the pane's window key, and no other.
    env.run(pane, "printf '\\e]2;%s\\a' hooked-$((40+2))");
    let toast = wait_effect(&main, "the title hook's toast", |e| e["text"] == "title hooked-42");
    assert_eq!(toast["sub"], format!("pane {pane}"));
    // `pane_exited` fires when the program ends, with its status.
    let quitter = env.new_pane(&["--", "exit 5"]);
    wait_effect(&main, "the pane_exited hook's toast", |e| {
        e["text"] == format!("exited {quitter} with 5")
    });

    // A window key nobody shows: its effects are dropped, not queued for a later window.
    let lonely = env.new_pane(&["--window", "lonely"]);
    env.wait(lonely, "prompt");
    env.run(lonely, "printf '\\e]2;%s\\a' lonely-$((1+1))");
    // Effects go out in hook order: once a later toast arrived, the earlier one would have too.
    env.run(pane, "printf '\\e]2;%s\\a' later-$((2+2))");
    wait_effect(&main, "the later toast", |e| e["text"] == "title later-4");
    let late = env.start_headless("late", &["--window", "lonely"]);
    std::thread::sleep(Duration::from_millis(500));
    for w in [&main, &other, &late] {
        assert!(
            !effects(w).iter().any(|e| e["text"] == "title lonely-2"),
            "{:?}",
            effects(w)
        );
    }
    assert!(
        !effects(&other).iter().any(|e| e["text"].as_str().is_some_and(|t| t.starts_with("title "))),
        "another window key's window got a toast: {:?}",
        effects(&other)
    );
    // The window that attached later does get what happens next.
    env.run(lonely, "printf '\\e]2;%s\\a' seen-$((3+3))");
    wait_effect(&late, "the lonely window's toast", |e| e["text"] == "title seen-6");
}

#[test]
fn the_settings_file_turns_plugins_off_or_disables_listed_ones() {
    let env = TestEnv::new(EXE, "settings");
    hooks_plugin(&env);
    example(&env, "terraform");
    let settings = env.state.join("settings.json");
    std::fs::write(&settings, json!({"plugins_disabled": ["hooks"]}).to_string()).expect("write");
    let status = |list: &Value, id: &str| {
        list["plugins"]
            .as_array()
            .and_then(|p| p.iter().find(|p| p["id"] == id))
            .map(|p| p["status"].clone())
            .unwrap_or(Value::Null)
    };
    // Read without a daemon too (`auk plugin list` never starts one).
    let (code, out, err) = auk(&env, &["plugin", "list", "--json"]);
    assert_eq!(code, 0, "{err}");
    let list: Value = serde_json::from_str(&out).expect("json");
    assert_eq!((status(&list, "hooks"), status(&list, "terraform")), (json!("disabled"), json!("ready")));

    let _daemon = env.start_daemon();
    let list = env.daemon("plugin list");
    assert_eq!(status(&list, "hooks"), "disabled", "{list}");
    assert_eq!(status(&list, "terraform"), "ready", "{list}");
    // A disabled plugin runs no Lua: its hooks never fire.
    let window = env.start_headless("w", &[]);
    let pane = env.new_pane(&[]);
    env.wait(pane, "prompt");
    env.run(pane, "printf '\\e]2;%s\\a' quiet");
    std::thread::sleep(Duration::from_millis(500));
    assert!(effects(&window).is_empty(), "{:?}", effects(&window));

    // The daemon re-reads the file at every reload: `plugins: false` loads none.
    std::fs::write(&settings, json!({"plugins": false}).to_string()).expect("write");
    let (code, out, err) = auk(&env, &["plugin", "reload", "--json"]);
    assert_eq!(code, 0, "{out}{err}");
    let list: Value = serde_json::from_str(&out).expect("json");
    assert_eq!((status(&list, "hooks"), status(&list, "terraform")), (json!("disabled"), json!("disabled")));
    // The window gets the new catalog.
    wait_for("the window's catalog", || {
        (status(&window.request("catalog"), "terraform") == "disabled").then_some(())
    });
    // Back to the defaults: everything loads, and the hooks run.
    std::fs::remove_file(&settings).expect("rm");
    let (code, _, err) = auk(&env, &["plugin", "reload"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(status(&env.daemon("plugin list"), "hooks"), "ready");
    env.run(pane, "printf '\\e]2;%s\\a' loud");
    wait_effect(&window, "the enabled hook's toast", |e| e["text"] == "title loud");
}

#[test]
fn a_block_that_cannot_start_shows_a_card_with_retry() {
    let env = TestEnv::new(EXE, "blockcard");
    let _daemon = env.start_daemon();
    let window = env.start_headless("w", &[]);
    // No plugin defines the type yet: the pane still opens, with the card.
    let opened = env.daemon("block later.panel");
    let pane = opened["pane"].as_u64().expect("a pane");
    assert_eq!(opened["failed"], "no block type later.panel on this host", "{opened}");
    let (sf, doc) = wait_for("the card in the window", || window_doc(&window, pane));
    let text = strings(&doc);
    assert!(text.contains("Could not start the block"), "{text}");
    assert!(text.contains("no block type later.panel on this host"), "{text}");
    let tsp = env.daemon(&format!("tsp {pane}"));
    assert_eq!(tsp["surfaces"][0]["role"], "block.failed", "{tsp}");
    let (retry, action) = clicks(&doc).into_iter().next().expect("a Retry button");
    assert_eq!(action, "retry");
    assert!(env.pane_info(pane)["exit"].is_null(), "the pane stays open");

    // Retry while the type is still missing: the card says so again.
    window.request(&format!("click {pane} {sf} {}", quote(&retry)));
    let (sf2, _) = wait_for("the second card", || window_doc(&window, pane).filter(|(s, _)| *s != sf));
    // Install the plugin; the catalog every window gets names the type and its title, which is
    // what the palette's "New <title> block" rows read (§9.4, §9.5).
    package(
        &env,
        "later",
        "schema = 1\nid = \"later\"\nname = \"Later\"\nversion = \"1\"\nhost = \"host.luau\"\n[[blocks]]\nid = \"panel\"\ntitle = \"Panel\"\n",
        &[(
            "host.luau",
            r#"tern.block.define("panel", {
                init = function(cx) return {} end,
                view = function() return { main = tern.ui.text("the panel runs") } end,
            })"#,
        )],
    );
    let (code, _, err) = auk(&env, &["plugin", "reload"]);
    assert_eq!(code, 0, "{err}");
    let catalog = wait_for("the type in the window's catalog", || {
        let c = window.request("catalog");
        (c["plugins"][0]["blocks"][0]["id"] == "panel").then_some(c)
    });
    let block = &catalog["plugins"][0]["blocks"][0];
    assert_eq!((&block["title"], &block["palette"]), (&json!("Panel"), &json!(true)), "{catalog}");
    // Retry now starts the block in the same pane.
    let (_, doc) = window_doc(&window, pane).expect("the card");
    let (retry, _) = clicks(&doc).into_iter().next().expect("Retry");
    window.request(&format!("click {pane} {sf2} {}", quote(&retry)));
    wait_for("the block's own view", || {
        window_doc(&window, pane).filter(|(_, d)| strings(d).contains("the panel runs"))
    });
    let tsp = env.daemon(&format!("tsp {pane}"));
    let live: Vec<&Value> = tsp["surfaces"].as_array().unwrap().iter().filter(|s| s["state"] == "live").collect();
    assert_eq!(live.len(), 1, "{tsp}");
    assert_eq!(live[0]["role"], "plugin.later.panel");
    assert_eq!(env.daemon("blocks")[pane.to_string()]["kind"], "later.panel");
}

#[test]
fn plugin_reload_exits_124_when_the_daemon_does_not_answer_in_60_s() {
    let env = TestEnv::new(EXE, "reload124");
    // A daemon that takes the connection and never answers.
    let listener = std::os::unix::net::UnixListener::bind(env.control_path()).expect("bind");
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            held.push(stream);
        }
    });
    let started = Instant::now();
    let (code, _, err) = auk(&env, &["plugin", "reload"]);
    let took = started.elapsed();
    assert_eq!(code, 124, "{err}");
    assert!(err.contains("the session daemon did not answer"), "{err}");
    assert!(took >= Duration::from_secs(59) && took < Duration::from_secs(90), "{took:?}");
}

/// Makes a git repository holding a plugin package at `sub` (or the root), committed.
fn git_repo(dir: &Path, sub: &str) {
    let pkg = dir.join(sub);
    std::fs::create_dir_all(&pkg).expect("mkdir");
    std::fs::write(
        pkg.join("plugin.toml"),
        "schema = 1\nid = \"gitplug\"\nname = \"Git Plug\"\nversion = \"7\"\nhost = \"host.luau\"\n",
    )
    .expect("write");
    std::fs::write(pkg.join("host.luau"), "tern.log('from git')").expect("write");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@e", "-c", "init.defaultBranch=main"])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git (a prerequisite of `auk plugin install` from git)");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "--quiet"]);
    git(&["add", "-A"]);
    git(&["commit", "--quiet", "-m", "plugin"]);
}

#[test]
fn plugin_install_from_a_git_url_and_from_github_owner_repo() {
    let env = TestEnv::new(EXE, "gitinstall");
    // A local repository stands in for the remote one.
    let repos = env.root.join("repos");
    git_repo(&repos.join("plain"), "");
    let url = format!("file://{}", repos.join("plain").display());
    let (code, out, err) = auk(&env, &["plugin", "install", &url]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.starts_with("installed gitplug 7 → "), "{out}");
    let dir = plugins_dir(&env).join("gitplug");
    assert!(dir.join("plugin.toml").is_file());
    assert!(!dir.join(".git").exists(), "the clone's .git isn't installed");
    // Staged in a dot-named directory, then renamed: nothing else is left in the folder.
    let names: Vec<String> = std::fs::read_dir(plugins_dir(&env))
        .expect("ls")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["gitplug"]);
    let (code, _, err) = auk(&env, &["plugin", "install", &url]);
    assert_eq!(code, 1);
    assert!(err.contains("already installed"), "{err}");

    // `github.com/OWNER/REPO/PATH`, with git sending github.com to the local repositories.
    git_repo(&repos.join("acme/tools"), "plugins/gitplug");
    let gitconfig = env.root.join("gitconfig");
    std::fs::write(
        &gitconfig,
        format!("[url \"file://{}/\"]\n\tinsteadOf = https://github.com/\n", repos.display()),
    )
    .expect("write");
    let out = env
        .command()
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["plugin", "install", "github.com/acme/tools/plugins/gitplug", "--force"])
        .output()
        .expect("auk");
    assert!(out.status.success(), "{out:?}");
    let list: Value = serde_json::from_slice(&env.cli(&["plugin", "list", "--json"]).stdout).expect("json");
    assert_eq!(list["plugins"][0]["id"], "gitplug", "{list}");
    assert_eq!(list["plugins"][0]["status"], "ready", "{list}");
}
