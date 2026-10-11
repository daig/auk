//! Frame-time measurement on a 10,000-entry transcript-like document with virtualization. The
//! frame budget is 8.3 ms; the incremental cases are asserted in release builds.

use auk_ui::{Document, NodeId, SheetKind, paint};
use std::time::{Duration, Instant};

const CSS: &str = "
html { font: 13px/1.4 Menlo, monospace; color: #222; }
body { margin: 0; }
.main { height: 100vh; overflow: auto; row-gap: 16px; padding: 24px 16px 0; }
.entry { border-radius: 12px; background: #f6f6f6; padding: 8px 12px; border: 1px solid #ddd; }
.head { display: flex; gap: 8px; align-items: baseline; color: #666; }
.head .time { margin-left: auto; }
.entry p { margin: 6px 0; }
.tool { display: flex; gap: 6px; padding: 4px 8px; background: #eee; border-radius: 6px; }
.tool code { font-weight: bold; }
.st-ok::before { content: '✓ '; color: green; }
";

fn entry(doc: &mut Document, i: usize) -> NodeId {
    let e = doc.create_element("div");
    doc.set_attribute(e, "class", "entry");
    let head = doc.create_element("div");
    doc.set_attribute(head, "class", "head");
    for (cls, text) in [
        (
            "role st-ok",
            if i.is_multiple_of(2) {
                "assistant"
            } else {
                "user"
            },
        ),
        ("time", "12:30:05"),
    ] {
        let s = doc.create_element("span");
        doc.set_attribute(s, "class", cls);
        let t = doc.create_text(text);
        doc.append_child(s, t);
        doc.append_child(head, s);
    }
    doc.append_child(e, head);
    let p = doc.create_element("p");
    let t = doc.create_text(&format!(
        "Entry {i}: the quick brown fox jumps over the lazy dog while the build runs; \
         here is a longer paragraph of transcript text so that lines wrap at the measure \
         and the layout has some real work to do for each visible entry."
    ));
    doc.append_child(p, t);
    doc.append_child(e, p);
    if i.is_multiple_of(3) {
        let tool = doc.create_element("div");
        doc.set_attribute(tool, "class", "tool");
        let c = doc.create_element("code");
        let ct = doc.create_text("bash");
        doc.append_child(c, ct);
        doc.append_child(tool, c);
        let s = doc.create_element("span");
        let st = doc.create_text("ls -la ~/code/tern-clone/crates");
        doc.append_child(s, st);
        doc.append_child(tool, s);
        doc.append_child(e, tool);
    }
    e
}

fn time<F: FnMut()>(n: u32, mut f: F) -> Duration {
    let start = Instant::now();
    for _ in 0..n {
        f();
    }
    start.elapsed() / n
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[test]
fn transcript_10k_frame_times() {
    let t0 = Instant::now();
    let mut doc = Document::new();
    let fonts_ready = t0.elapsed();
    doc.set_stylesheet("transcript", SheetKind::BuiltIn, None, CSS);
    doc.set_viewport(1000.0, 800.0);
    let root = doc.root();
    let body = doc.create_element("body");
    doc.append_child(root, body);
    let main = doc.create_element("div");
    doc.set_attribute(main, "class", "main");
    doc.append_child(body, main);
    let t = Instant::now();
    let mut entries = Vec::new();
    for i in 0..10_000 {
        let e = entry(&mut doc, i);
        doc.append_child(main, e);
        entries.push(e);
    }
    doc.set_virtual(main, Some(80.0));
    let build = t.elapsed();

    let t = Instant::now();
    doc.update();
    let first = t.elapsed();
    let first_stats = doc.last_update_stats();

    let mut scene = vello::Scene::new();
    let paint_time = time(20, || {
        scene.reset();
        paint(&doc, &mut scene);
    });

    // Streaming: append a token to the last visible entry's paragraph.
    let p = doc.element_children(entries[2])[1];
    let text = doc.children(p)[0];
    let mut s = String::from("streaming");
    let stream = time(50, || {
        s.push_str(" tok");
        doc.set_text(text, &s);
        doc.update();
        scene.reset();
        paint(&doc, &mut scene);
    });
    let stream_stats = doc.last_update_stats();

    // A new entry at the end (offscreen).
    let mut n = 10_000;
    let append = time(50, || {
        let e = entry(&mut doc, n);
        n += 1;
        doc.append_child(main, e);
        doc.update();
        scene.reset();
        paint(&doc, &mut scene);
    });

    // Scrolling through the middle.
    let mut y = 200_000.0;
    let scroll = time(50, || {
        y += 400.0;
        doc.set_scroll_offset(main, 0.0, y);
        doc.update();
        scene.reset();
        paint(&doc, &mut scene);
    });
    let scroll_stats = doc.last_update_stats();

    // Hover restyle of one entry.
    let hover = time(50, || {
        doc.set_state(entries[1], auk_ui::ElementState::HOVER, true);
        doc.update();
        doc.set_state(entries[1], auk_ui::ElementState::HOVER, false);
        doc.update();
        scene.reset();
        paint(&doc, &mut scene);
    }) / 2;

    eprintln!(
        "10k-entry transcript ({} build):",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    eprintln!("  font discovery + document:  {:8.2} ms", ms(fonts_ready));
    eprintln!("  DOM build (≈60k nodes):     {:8.2} ms", ms(build));
    eprintln!(
        "  first update (style all):   {:8.2} ms  {first_stats:?}",
        ms(first)
    );
    eprintln!("  paint (scene build):        {:8.2} ms", ms(paint_time));
    eprintln!(
        "  stream text + frame:        {:8.2} ms  {stream_stats:?}",
        ms(stream)
    );
    eprintln!("  append entry + frame:       {:8.2} ms", ms(append));
    eprintln!(
        "  scroll 400px + frame:       {:8.2} ms  {scroll_stats:?}",
        ms(scroll)
    );
    eprintln!("  hover restyle + frame:      {:8.2} ms", ms(hover));

    if !cfg!(debug_assertions) {
        for (name, d) in [
            ("stream", stream),
            ("append", append),
            ("scroll", scroll),
            ("hover", hover),
            ("paint", paint_time),
        ] {
            assert!(
                ms(d) < 8.3,
                "{name} frame took {:.2} ms, over the 8.3 ms budget",
                ms(d)
            );
        }
    }
}
