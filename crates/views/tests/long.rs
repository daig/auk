//! Long transcripts (amendment A10, §7.10 cost, §8.3): the measure loops visit only what layout
//! laid out since they last ran, so content virtualized out of view costs nothing per frame;
//! highlighting waits in the background until its node is in view; and a cut that fits stays.

mod common;

use auk_ui::Rect;
use common::Harness;
use serde_json::{Value, json};

/// `n` transcript entries: a collapsed card previewing 2 of 8 log lines, and Markdown with a
/// Rust code block.
fn entries(n: usize) -> Value {
    let log: String = (1..=8).map(|i| format!("step {i}\n")).collect();
    let md = "Done:\n\n```rust\nfn main() {\n    let x = 1;\n}\n```\n";
    Value::Array(
        (0..n)
            .flat_map(|i| {
                [
                    json!({"id": format!("c{i}"), "k": "card", "p": {"head": "Build", "collapsible": true, "collapsed": true, "preview": {"lines": 2}}, "c": [
                        {"id": format!("c{i}.a"), "k": "ansi", "p": {"text": log.trim_end()}}
                    ]}),
                    json!({"id": format!("m{i}"), "k": "md", "p": {"text": md}}),
                ]
            })
            .collect(),
    )
}

#[test]
fn a_long_transcript_measures_and_highlights_only_what_is_in_view() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface("s1", "inline", entries(150));
    h.frame();
    let (first_card, last_card) = (h.el("s1", "c0"), h.el("s1", "c149"));
    let (first_md, last_md) = (h.el("s1", "m0"), h.el("s1", "m149"));
    // In view (the document viewport, the top): measured and highlighted.
    assert!(h.doc.laid_out(first_card));
    assert!(h.class(first_card).contains("clamped"), "{}", h.class(first_card));
    assert!(!h.find_class(first_md, "tk-keyword").is_empty());
    // Out of view: only an estimated box, not measured, its code drawn plain until it shows.
    assert!(!h.doc.laid_out(last_card));
    assert!(!h.class(last_card).contains("clamped"), "{}", h.class(last_card));
    assert!(h.find_class(last_md, "tk-keyword").is_empty());
    assert!(h.text(last_md).contains("let x = 1;"));
    let work = h.view.last_work();
    assert!(
        work.measured < 60,
        "the measure loop visits what's in view, not all 150 cards: {work:?}"
    );

    // Nothing changed: the next frame measures nothing.
    h.frame();
    assert_eq!(h.view.last_work().measured, 0, "{:?}", h.view.last_work());

    // Scrolled to the end: the window shows the bottom of the document (as a host does, again
    // as boxes laid out in view replace their estimates).
    let main = h.doc.parent(first_card).expect("the main region");
    for _ in 0..4 {
        let b = h.doc.border_box(main).unwrap();
        h.view
            .set_view_rect(Some(Rect::new(0.0, b.y + b.h - 600.0, b.w, 600.0)));
        h.frame();
    }
    assert!(h.doc.laid_out(last_card));
    assert!(h.class(last_card).contains("clamped"), "{}", h.class(last_card));
    assert!(!h.find_class(last_md, "tk-keyword").is_empty());
    assert_eq!(h.text(last_md), h.text(first_md));
    assert!(h.view.last_work().measured < 60, "{:?}", h.view.last_work());
}

#[test]
fn a_cut_path_that_fits_stays_cut() {
    let mut h = Harness::new();
    h.hello(&[]);
    let path = "python/examples/progress.py";
    h.surface(
        "s1",
        "inline",
        json!([{"id": "r", "k": "row", "p": {"max": {"w": 14}}, "c": [
            {"id": "t", "k": "text", "p": {"text": path, "lines": 1, "truncate": "middle", "shrink": 1}},
        ]}]),
    );
    h.frame();
    let t = h.el("s1", "t");
    let cut = h.text(t);
    assert!(cut.contains('…') && cut.ends_with(".py"), "{cut:?}");
    let passes = h.view.last_work().passes;
    assert!(passes < 8, "the measure loop settles: {passes} passes");
    // Later frames leave it as it is (a box as wide as its cut text is a little narrower than
    // its cells: cutting again each pass would end at one character).
    for _ in 0..4 {
        h.frame();
    }
    assert_eq!(h.text(t), cut);
    assert!(cut.chars().count() >= 10, "{cut:?}");
}
