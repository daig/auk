//! Frame cost of the views (budget: 8.3 ms per frame): replaying the whole omp session
//! (project + measure loops + layout + paint per frame), and a streaming `md` append into a long
//! transcript. Numbers print with `--nocapture`; the budget is asserted in release builds.

mod common;

use common::Harness;
use serde_json::json;
use std::time::{Duration, Instant};

const BUDGET_MS: f64 = 8.3;

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

fn paint(h: &Harness, scene: &mut vello::Scene) {
    scene.reset();
    auk_ui::paint(&h.doc, scene);
}

#[test]
fn omp_session_frame_times() {
    let t0 = Instant::now();
    let h = common::replay_with("omp-session.jsonl", true, |_, _| {});
    let total = ms(t0.elapsed());
    // Each frame: the engine applies it, the view syncs, ticks, runs the measure loops and lays
    // out, and the document paints into a scene.
    let mut frames = h.frame_times.clone();
    let mut slow: Vec<(usize, f64)> = frames
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, t)| *t > BUDGET_MS / 2.0)
        .collect();
    slow.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    for (i, t) in &slow[..slow.len().min(4)] {
        let (a, b, c) = h.frame_parts[*i];
        eprintln!("slow frame {i}: {t:.2} ms = sync {a:.2} + layout {b:.2} + paint {c:.2}");
    }
    let n = frames.len();
    let p50 = pct(&mut frames, 0.5);
    let p95 = pct(&mut frames, 0.95);
    let max = pct(&mut frames, 1.0);
    eprintln!(
        "omp-session: {n} frames, whole replay {total:.0} ms; tick+layout+paint per frame p50 {p50:.2} ms, p95 {p95:.2} ms, max {max:.2} ms"
    );
    // The full projection of the final transcript from scratch: a fresh view syncing the whole
    // document at once (a window attaching late).
    let mut doc = auk_ui::Document::new();
    auk_views::install_sheets(&mut doc, true);
    doc.set_stylesheet("host", auk_ui::SheetKind::BuiltIn, None, common::HOST_CSS);
    let root = doc.root();
    let body = doc.create_element("body");
    doc.append_child(root, body);
    doc.set_viewport(1200.0, 800.0);
    let mut view = auk_views::PaneView::new(&mut doc, auk_views::ViewSettings::default());
    let el = view.element();
    doc.append_child(body, el);
    let changes = auk_tsp::Changes {
        surfaces: h
            .pane
            .surfaces()
            .iter()
            .map(|s| auk_tsp::SurfaceChanges {
                key: s.key(),
                opened: true,
                meta: true,
                palette: true,
                sheets: true,
                doc: Default::default(),
                reveal: None,
                scrolls: Vec::new(),
            })
            .collect(),
        ..Default::default()
    };
    let t = Instant::now();
    view.sync(&mut doc, &h.pane, &changes, h.now);
    let project = ms(t.elapsed());
    let t = Instant::now();
    view.layout(&mut doc);
    let layout = ms(t.elapsed());
    let mut scene = vello::Scene::new();
    let t = Instant::now();
    auk_ui::paint(&doc, &mut scene);
    let paint_ms = ms(t.elapsed());
    eprintln!(
        "omp-session full projection from scratch: project {project:.2} ms, layout {layout:.2} ms, paint {paint_ms:.2} ms"
    );
    if !cfg!(debug_assertions) {
        assert!(
            p95 <= BUDGET_MS,
            "omp-session p95 frame {p95:.2} ms over {BUDGET_MS} ms"
        );
    }
}

#[test]
fn streaming_md_append_frame_cost() {
    let mut h = Harness::new();
    h.hello(&[]);
    // A long transcript: 300 entries of markdown, code and tool cards.
    let mut main = Vec::new();
    for i in 0..300 {
        main.push(json!({"id": format!("m{i}"), "k": "md", "p": {"text": format!(
            "## Step {i}\n\nThe quick brown fox jumps over the lazy dog while the build runs; here is `code` and **bold** text that wraps.\n\n- one\n- two\n\n```rust\nfn f{i}() -> u32 {{ {i} }}\n```\n"
        )}}));
        if i % 3 == 0 {
            main.push(json!({"id": format!("t{i}"), "k": "tool", "p": {"name": "bash", "title": "Run", "target": "cargo test", "targetKind": "command", "status": "done", "took": 1200}, "c": [
                {"id": format!("t{i}.o"), "k": "ansi", "p": {"text": "running 12 tests\ntest a ... ok\ntest b ... ok"}}
            ]}));
        }
    }
    main.push(json!({"id": "live", "k": "md", "p": {"text": "Streaming", "stream": true}}));
    h.surface("s1", "inline", serde_json::Value::Array(main));
    h.doc.set_viewport(1200.0, 800.0);
    h.frame();
    // The pane follows the tail: the window shows the last 800 px of the transcript.
    let follow = |h: &mut Harness| {
        let pane = h.doc.border_box(h.view.element()).unwrap();
        h.view.set_view_rect(Some(auk_ui::Rect::new(
            0.0,
            (pane.h - 800.0).max(0.0),
            1200.0,
            800.0,
        )));
    };
    follow(&mut h);
    h.view.layout(&mut h.doc);
    let mut scene = vello::Scene::new();
    paint(&h, &mut scene);
    let words = [
        "the",
        "answer",
        "is",
        "**forty-two**",
        "and",
        "`code`",
        "follows",
        "\n\n- item",
        "\n\n```rs\nlet x = 1;\n```\n",
        "done.",
    ];
    let mut times = Vec::new();
    let mut blocks_redrawn = 0;
    for i in 0..200 {
        let tok = format!(" {}", words[i % words.len()]);
        let t = Instant::now();
        h.frame_ops("s1", 2 + i as u64, json!([["text", "live", "append", tok]]));
        let a = ms(t.elapsed());
        h.now += 16.0;
        h.view.tick(&mut h.doc, &h.pane, h.now);
        let b = ms(t.elapsed());
        follow(&mut h);
        h.view.layout(&mut h.doc);
        let c = ms(t.elapsed());
        paint(&h, &mut scene);
        times.push(ms(t.elapsed()));
        if i == 100 {
            eprintln!(
                "sync {a:.2} tick {:.2} layout {:.2} paint {:.2} stats {:?}",
                b - a,
                c - b,
                ms(t.elapsed()) - c,
                h.doc.last_update_stats()
            );
        }
        blocks_redrawn = blocks_redrawn.max(h.doc.last_update_stats().inline_rebuilt);
    }
    let p50 = pct(&mut times, 0.5);
    let p95 = pct(&mut times, 0.95);
    let max = pct(&mut times, 1.0);
    eprintln!(
        "streaming md append (300-entry transcript): p50 {p50:.2} ms, p95 {p95:.2} ms, max {max:.2} ms; most inline contexts rebuilt in a frame: {blocks_redrawn}"
    );
    if !cfg!(debug_assertions) {
        assert!(
            p95 <= BUDGET_MS,
            "streaming append p95 {p95:.2} ms over {BUDGET_MS} ms"
        );
    }
}
