//! The `plugins` suite (GOAL.md; spec §9, §20, M7, M8, AC11–AC14): the SDK's example plugins and
//! purpose-built ones, loaded by the real daemon from a temporary config dir, with headless
//! windows as clients.
//!
//! The window-half examples (`longrun`, `workspaces`, `jsonx`, M9) need the app's window host
//! and are tested with the app; so is the window side of AC14 and the 4 ms and 50 ms budgets,
//! which only window halves have.

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
