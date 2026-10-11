//! The SDK's `review-queue` block against a stand-in `gh` on `PATH`, so the test never
//! touches the user's GitHub login. Its own test binary: it sets `PATH` for the process before
//! any thread starts.

mod common;

use std::time::Duration;

use auk_plugins::{Effect, PaneInfo, PaneTable};
use common::*;
use std::sync::Arc;

const WAIT: Duration = Duration::from_secs(5);

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
fn review_queue_block_lists_moves_opens_toggles_drafts_and_restores() {
    let root = Root::new();
    let bin = root.path.join("bin");
    std::fs::create_dir_all(&bin).expect("mkdir");
    let gh = bin.join("gh");
    std::fs::write(&gh, format!("#!/bin/sh\ncat <<'EOF'\n{PRS}\nEOF\n")).expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let path = format!("{}:/usr/bin:/bin", bin.display());
    // SAFETY: the only test in this binary, and no thread has started yet.
    unsafe { std::env::set_var("PATH", path) };

    root.example("review-queue");
    let panes = Arc::new(PaneTable::default());
    panes.upsert(PaneInfo {
        pane: 4,
        cwd: None,
        program: "review-queue.queue".into(),
        title: "Review Queue".into(),
        busy: false,
    });
    let mut host = root.host(Some(Arc::clone(&panes)));
    let mut pane = BlockPane::new(4, 100, 30);
    host.start_block(pane.launch("review-queue.queue", &[], None, None))
        .expect("type");
    let mut others = Vec::new();
    // Drafts are hidden at first: two of three, oldest first.
    pane.until(&host, WAIT, &mut others, |p| {
        p.title() == "Review Queue (2)"
    });
    let text = pane.text();
    assert!(
        text.contains("acme/app#3") && text.contains("acme/app#12"),
        "{text}"
    );
    assert!(!text.contains("WIP: new renderer"));
    let selected = |p: &BlockPane| {
        p.region("main")["c"][0]["p"]["selected"]
            .as_str()
            .map(str::to_owned)
    };
    assert_eq!(selected(&pane).as_deref(), Some("main.queue.acme/app#3"));

    // ↓ moves; Enter opens the pull request in the windows showing the pane.
    pane.keys(&host, b"\x1b[B");
    pane.until(&host, WAIT, &mut others, |p| {
        selected(p).as_deref() == Some("main.queue.acme/app#12")
    });
    pane.keys(&host, b"\r");
    let open = pane.next_effect(&host, WAIT, &mut others);
    assert_eq!(
        open.effect,
        Effect::Open("https://github.com/acme/app/pull/12".into())
    );
    assert_eq!(open.target, auk_plugins::EffectTarget::Pane(4));

    // A click on a row selects it (the list's own event).
    pane.event(
        &host,
        serde_json::json!({"ev": "select", "id": "main.queue", "item": "main.queue.acme/app#3"}),
    );
    pane.until(&host, WAIT, &mut others, |p| {
        selected(p).as_deref() == Some("main.queue.acme/app#3")
    });

    // `d` shows drafts; that's what `save` keeps.
    pane.keys(&host, b"d");
    pane.until(&host, WAIT, &mut others, |p| {
        p.title() == "Review Queue (3)"
    });
    pane.until(&host, WAIT, &mut others, |p| {
        p.saved.as_deref() == Some(r#"{"drafts":true}"#)
    });
    // A key the block ignores returns false: no render.
    let frames = pane.frames;
    pane.keys(&host, b"\x1bz");
    let quiet = pane.settle(&host, Duration::from_millis(300));
    assert!(quiet.is_empty());
    assert_eq!(pane.frames, frames);

    // A reload moves the block to the new worker, which re-runs init from the last save.
    let catalog = host.reload().clone();
    assert!(catalog.block_type("review-queue.queue").is_some());
    pane.until(&host, WAIT, &mut others, |p| {
        p.surface().is_some_and(|(id, _)| id == "s2") && p.title() == "Review Queue (3)"
    });
}
