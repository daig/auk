//! The SDK's example plugins at the runtime level: each loads unmodified and its
//! core behavior works through the real host, workers, blocks, lenses and window runtime.

mod common;

use std::sync::Arc;
use std::time::Duration;

use auk_plugins::{HostOutput, PaneInfo, PaneTable, SpawnSpec, Status, ToastLevel};
use common::*;
use serde_json::json;

const WAIT: Duration = Duration::from_secs(5);

fn spec(cwd: &str, pane: u64) -> SpawnSpec {
    SpawnSpec {
        program: "/bin/zsh".into(),
        args: vec!["-l".into()],
        cwd: cwd.into(),
        env: vec![
            ("TERN_PANE".into(), pane.to_string()),
            ("TERN_IDENTITY".into(), "test".into()),
        ],
    }
}

#[test]
fn dirvars_block_approves_exact_bytes_and_the_spawn_filter_applies_them() {
    let root = Root::new();
    root.example("dirvars");
    let project = root.path.join("project");
    std::fs::create_dir_all(&project).expect("mkdir");
    let env_file = project.join(".tern-env");
    std::fs::write(
        &env_file,
        "APP_ENV=development\nURL=postgres://db/${APP_ENV}\n",
    )
    .expect("write");
    let mut host = root.host(None);
    assert_eq!(host.catalog().plugins[0].status, Status::Ready);

    // Not approved yet: the filter leaves the spec alone.
    let cwd = project.display().to_string();
    let filtered = host.filter_spawn(spec(&cwd, 7));
    assert_eq!(filtered, spec(&cwd, 7));

    // The palette opens the Directory Variables block.
    let mut pane = BlockPane::new(1, 80, 24);
    host.start_block(pane.launch("dirvars.approve", &[&cwd], None, None))
        .expect("block type exists");
    let mut others = Vec::new();
    pane.until(&host, WAIT, &mut others, |p| {
        p.text().contains("Approve these exact bytes")
    });
    assert_eq!(
        pane.surface(),
        Some(("s1".into(), Some("plugin.dirvars.approve".into())))
    );
    pane.until(&host, WAIT, &mut others, |p| {
        p.title() == "Directory Variables"
    });
    assert!(pane.text().contains(&env_file.display().to_string()));

    // Click "Approve these exact bytes": the click carries the displayed bytes.
    let (id, click) = clicks(&pane.region("main"))
        .into_iter()
        .find(|(_, c)| c.starts_with("approve="))
        .expect("an approve button");
    let value = click.strip_prefix("approve=").expect("prefix").to_owned();
    pane.event(
        &host,
        json!({"ev": "action", "id": id, "act": "approve", "value": value}),
    );
    let toast = pane.next_effect(&host, WAIT, &mut others);
    assert_eq!(
        toast_text(&toast),
        Some((
            ToastLevel::Info,
            "Exact contents approved".into(),
            Some("Only new shells load these variables.".into())
        ))
    );
    pane.until(&host, WAIT, &mut others, |p| {
        p.text().contains("Revoke approval")
    });

    // Approved: new shells get the variables, expanded, and TERN_DIRVARS.
    let filtered = host.filter_spawn(spec(&cwd, 8));
    let env: std::collections::BTreeMap<_, _> = filtered.env.into_iter().collect();
    assert_eq!(env.get("APP_ENV").map(String::as_str), Some("development"));
    assert_eq!(
        env.get("URL").map(String::as_str),
        Some("postgres://db/development")
    );
    assert_eq!(
        env.get("TERN_DIRVARS"),
        Some(&env_file.display().to_string())
    );
    assert_eq!(env.get("TERN_PANE").map(String::as_str), Some("8"));

    // The approval is in the plugin's kv.json, shared by all its VMs.
    let kv = std::fs::read_to_string(root.state().join("plugin-data/dirvars/kv.json")).expect("kv");
    assert!(kv.contains("approved:"), "{kv}");

    // A pane that started before the file changed hears about new content on `cwd`.
    std::fs::write(&env_file, "APP_ENV=production\n").expect("write");
    host.dispatch(auk_plugins::HostEvent::Cwd {
        pane: 8,
        path: cwd.clone(),
    });
    let quiet = drain(&host, Duration::from_millis(300));
    assert!(
        quiet.iter().all(|o| o.effect().is_none()),
        "changed bytes are no longer approved, so nothing applies: {quiet:?}"
    );
    drop(others);
    host.block_closed(1);
}

/// The plan output as a terminal would receive it: CRLF line ends and some color.
fn plan_bytes() -> Vec<u8> {
    let text = include_str!("fixtures/terraform-plan.txt");
    let mut out = Vec::new();
    for line in text.lines() {
        if line.starts_with("Plan:") {
            out.extend_from_slice(b"\x1b[1m");
            out.extend_from_slice(line.as_bytes());
            out.extend_from_slice(b"\x1b[0m");
        } else {
            out.extend_from_slice(line.as_bytes());
        }
        out.extend_from_slice(b"\r\n");
    }
    out
}

fn all_true() -> auk_plugins::Preconditions {
    auk_plugins::Preconditions {
        shell_integration: true,
        lenses_on: true,
        main_screen: true,
        surface_open: false,
    }
}

#[test]
fn terraform_lens_claims_captures_views_and_rehydrates() {
    let root = Root::new();
    root.example("terraform");
    let mut host = root.host(None);
    let catalog = host.catalog().clone();

    // Claims, from the catalog alone.
    let line = "TF_LOG=warn terraform -chdir=infra plan -out tf.plan";
    let claim = catalog.claim(line, all_true()).expect("claimed");
    assert_eq!(
        (claim.plugin.as_str(), claim.lens.as_str()),
        ("terraform", "plan")
    );
    assert_eq!(claim.role(), "lens.plugin.terraform.plan");
    for not in [
        "terraform apply",
        "terraform plan | tee out",
        "terraform plan > out",
        "tofu",
    ] {
        assert_eq!(catalog.claim(not, all_true()), None, "{not}");
    }
    let mut off = all_true();
    off.surface_open = true;
    assert_eq!(catalog.claim("terraform plan", off), None);
    assert!(catalog.claim("tofu plan", all_true()).is_some());

    // Capture: every replica reads the same lines; the host feeds them to the lens.
    let raw = plan_bytes();
    let mut capture = auk_plugins::capture::Capture::new(100);
    let run = claim.run(line, line, Some("/work"));
    assert_eq!(run.program, "terraform");
    assert_eq!(run.args, ["-chdir=infra", "plan", "-out", "tf.plan"]);
    host.lens_open(1, 5, &claim, run);
    let mut seen = Vec::new();
    let mut sent_lines = 0;
    for chunk in raw.chunks(97) {
        let lines = capture.feed(chunk).expect("not interactive");
        sent_lines += lines.len();
        host.lens_lines(1, lines);
    }
    let rest = capture.finish();
    sent_lines += rest.len();
    assert_eq!(
        sent_lines,
        include_str!("fixtures/terraform-plan.txt").lines().count()
    );
    host.lens_lines(1, rest);
    host.lens_finish(1, 2);
    let view = wait_for(&host, WAIT, &mut seen, |o| match o {
        HostOutput::LensView {
            block: 1,
            pane: 5,
            view: Some(v),
        } if v["p"]["role"] == "terraform.plan.danger" && find(v, "text", "2 to add").is_some() => {
            Some(v.clone())
        }
        _ => None,
    });
    for badge in [
        "2 to add",
        "1 to change",
        "2 to destroy",
        "1 output change",
        "1 warning",
        "2 refreshed",
    ] {
        let mut text = String::new();
        collect_text(&view, &mut text);
        assert!(text.contains(badge), "{badge} missing from {text}");
    }
    let mut text = String::new();
    collect_text(&view, &mut text);
    for group in ["Destroy", "Replace", "Update in place", "Create", "Outputs"] {
        assert!(text.contains(group), "{group} missing");
    }
    // `find` returns the props holding the match.
    let destroy = find(&view, "role", "terraform.destroy").expect("a destroy row");
    assert_eq!(destroy["actions"]["click"], "copy-address=aws_instance.old");
    let diag = find(&view, "key", "main.tf:12::").expect("the warning card");
    assert_eq!(diag["tone"], "warning");

    // A click copies the address; the view runs again after the event.
    let event = |act: &str, value: &str| {
        let mut m = serde_json::Map::new();
        m.insert("ev".into(), json!("action"));
        m.insert("id".into(), json!("n7"));
        m.insert("act".into(), json!(act));
        m.insert("value".into(), json!(value));
        m
    };
    let record = auk_plugins::LensRecord {
        line: line.into(),
        typed: line.into(),
        cwd: Some("/work".into()),
        raw: raw.clone(),
        cols: 100,
        status: Some(2),
    };
    host.lens_event(
        1,
        5,
        event("copy-address", "aws_instance.web"),
        record.clone(),
    );
    let copy = next_effect(&host, WAIT, &mut seen);
    assert_eq!(
        copy.effect,
        auk_plugins::Effect::Copy("aws_instance.web".into())
    );
    assert_eq!(copy.target, auk_plugins::EffectTarget::Pane(5));
    let toast = next_effect(&host, WAIT, &mut seen);
    assert_eq!(
        toast_text(&toast),
        Some((
            ToastLevel::Success,
            "Copied resource address".into(),
            Some("aws_instance.web".into())
        ))
    );

    // Rehydration: a fresh host has no state for block 1; the record rebuilds it.
    drop(host);
    let mut host = root.host(None);
    let mut seen = Vec::new();
    host.lens_event(
        1,
        5,
        event("copy-address", "aws_db_instance.main"),
        record.clone(),
    );
    let copy = next_effect(&host, WAIT, &mut seen);
    assert_eq!(
        copy.effect,
        auk_plugins::Effect::Copy("aws_db_instance.main".into())
    );
    let rebuilt = wait_for(&host, WAIT, &mut seen, |o| match o {
        HostOutput::LensView { view: Some(v), .. } => Some(v.clone()),
        _ => None,
    });
    assert_eq!(rebuilt, view, "the rebuilt state draws the same view");

    // A command the current globs no longer claim is dropped.
    let mut gone = record.clone();
    gone.line = "terraform apply".into();
    host.lens_event(2, 5, event("copy-address", "x"), gone);
    assert!(drain(&host, Duration::from_millis(300)).is_empty());
}

#[test]
fn lens_views_run_at_most_every_250_ms_while_running() {
    let root = Root::new();
    root.package(
        "counter",
        &[
            (
                "plugin.toml",
                "schema = 1\nid = \"counter\"\nname = \"Counter\"\nversion = \"1\"\nhost = \"host.luau\"\n[[lenses]]\nid = \"n\"\nmatch = [\"count *\"]\n",
            ),
            (
                "host.luau",
                r#"tern.lens.define("n", {
                    open = function(run) return {n = 0, args = run.args} end,
                    line = function(s, l) s.n += 1 end,
                    finish = function(s, status) s.status = status end,
                    view = function(s) return tern.ui.text(s.n .. " lines" .. (s.status and " done" or "")) end,
                })"#,
            ),
        ],
    );
    let mut host = root.host(None);
    let claim = host
        .catalog()
        .claim("count to 100", all_true())
        .expect("claim");
    host.lens_open(
        9,
        1,
        &claim,
        claim.run("count to 100", "count to 100", None),
    );
    let started = std::time::Instant::now();
    let mut times = Vec::new();
    let mut seen = Vec::new();
    while started.elapsed() < Duration::from_millis(900) {
        host.lens_lines(
            9,
            vec![auk_plugins::capture::LensLine {
                text: "x".into(),
                links: vec![],
            }],
        );
        std::thread::sleep(Duration::from_millis(10));
        while let Ok(o) = host.outputs().try_recv() {
            if let HostOutput::LensView { .. } = o {
                times.push(started.elapsed());
            }
            seen.push(o);
        }
    }
    host.lens_finish(9, 0);
    let last = wait_for(&host, WAIT, &mut seen, |o| match o {
        HostOutput::LensView { view: Some(v), .. }
            if v["p"]["spans"][0]["t"]
                .as_str()
                .is_some_and(|t| t.ends_with("done")) =>
        {
            Some(v.clone())
        }
        _ => None,
    });
    assert!(
        last["p"]["spans"][0]["t"]
            .as_str()
            .expect("t")
            .ends_with("lines done")
    );
    assert!(times.len() >= 3 && times.len() <= 5, "{times:?}");
    for pair in times.windows(2) {
        assert!(pair[1] - pair[0] >= Duration::from_millis(240), "{times:?}");
    }
}

#[test]
fn longrun_toasts_slow_commands_keeps_history_and_its_block_ticks() {
    let root = Root::new();
    root.example("longrun");
    let panes = Arc::new(PaneTable::default());
    let host = root.host(Some(Arc::clone(&panes)));
    let mut seen = Vec::new();

    host.dispatch(auk_plugins::HostEvent::CommandStarted {
        pane: 3,
        line: "make all".into(),
        cwd: Some("/src".into()),
    });
    host.dispatch(auk_plugins::HostEvent::CommandFinished {
        pane: 3,
        line: "make all".into(),
        status: 0,
        took_ms: 16_000.0,
    });
    let toast = next_effect(&host, WAIT, &mut seen);
    assert_eq!(toast.target, auk_plugins::EffectTarget::Pane(3));
    assert_eq!(
        toast_text(&toast),
        Some((
            ToastLevel::Success,
            "make all finished in 16s".into(),
            Some("/src".into())
        ))
    );
    host.dispatch(auk_plugins::HostEvent::CommandStarted {
        pane: 3,
        line: "cargo test".into(),
        cwd: None,
    });
    host.dispatch(auk_plugins::HostEvent::CommandFinished {
        pane: 3,
        line: "cargo test".into(),
        status: 101,
        took_ms: 61_000.0,
    });
    let toast = next_effect(&host, WAIT, &mut seen);
    assert_eq!(
        toast_text(&toast),
        Some((
            ToastLevel::Error,
            "cargo test failed after 1m 1s".into(),
            Some("exit 101".into())
        ))
    );
    // Quick and interactive commands are not slow.
    host.dispatch(auk_plugins::HostEvent::CommandFinished {
        pane: 3,
        line: "ls".into(),
        status: 0,
        took_ms: 20.0,
    });
    host.dispatch(auk_plugins::HostEvent::CommandFinished {
        pane: 3,
        line: "vim notes".into(),
        status: 0,
        took_ms: 90_000.0,
    });
    assert!(
        drain(&host, Duration::from_millis(300))
            .iter()
            .all(|o| o.effect().is_none())
    );
    let kv = std::fs::read_to_string(root.state().join("plugin-data/longrun/kv.json")).expect("kv");
    let kv: serde_json::Value = serde_json::from_str(&kv).expect("json");
    assert_eq!(kv["history"].as_array().map(Vec::len), Some(2));
    assert_eq!(kv["history"][0]["line"], "cargo test");

    // The Slow Commands block lists them; the daemon lists its pane.
    let mut pane = BlockPane::new(10, 100, 30);
    panes.upsert(PaneInfo {
        pane: 10,
        cwd: None,
        program: "longrun.slow".into(),
        title: "Slow Commands".into(),
        busy: false,
    });
    let mut host = host;
    host.start_block(pane.launch("longrun.slow", &[], None, None))
        .expect("type");
    let mut others = Vec::new();
    pane.until(&host, WAIT, &mut others, |p| {
        p.text().contains("cargo test") && p.title() == "Slow Commands (2)"
    });

    // A command running elsewhere makes the block tick once a second, with no window open.
    host.dispatch(auk_plugins::HostEvent::CommandStarted {
        pane: 4,
        line: "sleep 100".into(),
        cwd: Some("/tmp".into()),
    });
    let frames = pane.frames;
    pane.until(&host, Duration::from_secs(4), &mut others, |p| {
        p.text().contains("Running now") && p.text().contains("sleep 100")
    });
    pane.until(&host, Duration::from_secs(4), &mut others, |p| {
        p.frames >= frames + 2
    });

    // Keys: `c` clears the history, `+` raises the threshold.
    pane.keys(&host, b"+");
    pane.until(&host, WAIT, &mut others, |p| {
        p.text().contains("threshold 20s")
    });
    pane.keys(&host, b"c");
    pane.until(&host, WAIT, &mut others, |p| p.title() == "Slow Commands");
    let kv = std::fs::read_to_string(root.state().join("plugin-data/longrun/kv.json")).expect("kv");
    let kv: serde_json::Value = serde_json::from_str(&kv).expect("json");
    assert_eq!(kv["threshold_ms"], 20000);
    assert!(kv.get("history").is_none());
}

#[test]
fn jsonx_explorer_block_reads_navigates_copies_and_restores() {
    let root = Root::new();
    root.example("jsonx");
    let file = root.path.join("data.json");
    std::fs::write(
        &file,
        r#"{"name": "auk", "tags": ["a", "b"], "nested": {"x": 1, "y": null}}"#,
    )
    .expect("write");
    let path = file.display().to_string();
    let mut host = root.host(None);
    let mut pane = BlockPane::new(2, 100, 30);
    host.start_block(pane.launch("jsonx.explorer", &[&path], None, None))
        .expect("type");
    let mut others = Vec::new();
    // `wc -c` runs through tern.process.run, then the file is parsed.
    pane.until(&host, WAIT, &mut others, |p| {
        p.text().contains("tags") && p.title() == "data.json"
    });
    let selected = |p: &BlockPane| {
        p.region("main")["c"][0]["p"]["selected"]
            .as_str()
            .map(str::to_owned)
    };
    assert_eq!(selected(&pane).as_deref(), Some("main.tree.."));

    // j, j, l (expand), j, y: the cursor lands on `.tags[0]`, whose jq path is copied.
    pane.keys(&host, b"j");
    pane.until(&host, WAIT, &mut others, |p| {
        selected(p).as_deref() == Some("main.tree..name")
    });
    pane.keys(&host, b"j");
    pane.keys(&host, b"l");
    pane.until(&host, WAIT, &mut others, |p| p.text().contains("\"a\""));
    pane.keys(&host, b"j");
    pane.until(&host, WAIT, &mut others, |p| {
        selected(p).as_deref() == Some("main.tree..tags[0]")
    });
    pane.keys(&host, b"y");
    let copy = pane.next_effect(&host, WAIT, &mut others);
    assert_eq!(copy.effect, auk_plugins::Effect::Copy(".tags[0]".into()));
    let toast = pane.next_effect(&host, WAIT, &mut others);
    assert_eq!(
        toast_text(&toast).map(|t| t.1),
        Some("Copied jq path".into())
    );
    pane.until(&host, WAIT, &mut others, |p| {
        p.saved.as_deref().is_some_and(|s| s.contains(".tags[0]"))
    });
    let saved: serde_json::Value =
        serde_json::from_str(pane.saved.as_deref().expect("saved")).expect("json");
    assert_eq!(
        saved,
        json!({"cursor": ".tags[0]", "expanded": [".", ".tags"]})
    );
    assert_eq!(host.blocks()[&2].saved.as_deref(), pane.saved.as_deref());

    // A restart brings the block back from `save`, with the same args.
    host.block_closed(2);
    let mut again = BlockPane::new(2, 100, 30);
    host.start_block(again.launch("jsonx.explorer", &[&path], pane.saved.clone(), None))
        .expect("type");
    again.until(&host, WAIT, &mut others, |p| {
        selected(p).as_deref() == Some("main.tree..tags[0]")
    });
    assert!(again.text().contains("\"b\""));

    // A missing type can't start.
    assert_eq!(
        host.start_block(again.launch("jsonx.nope", &[], None, None)),
        Err("no block type jsonx.nope on this host".into())
    );
}
