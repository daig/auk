//! The `el`-plus-stylesheet fixtures (the docs' and the Python SDK's recorded frames) laid out by
//! our engine and by WebKit from the same serialized page: every element box within 2 CSS px or
//! 2% of its size, whichever is larger. The WebKit tool and the comparison are
//! `auk_conformance::webkit`; the `layout` suite runs the same fixtures through the daemon.

mod common;

use auk_conformance::layout::our_boxes;
use auk_conformance::surfaces::PANE_WIDTH;
use auk_conformance::webkit::{self, PAGE_HEIGHT, real_font_tolerance};
use common::{HOST_CSS, replay};
use std::path::Path;

fn check(name: &str) {
    let mut h = replay(&format!("{name}.jsonl"), |_, _| {});
    h.doc.set_viewport(PANE_WIDTH, PAGE_HEIGHT);
    h.view.layout(&mut h.doc);
    let html = h.view.to_html(&h.doc, h.body, HOST_CSS);
    let wk = webkit::boxes(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        name,
        &html,
        PANE_WIDTH,
        &[],
    );
    let ours = our_boxes(&h.doc, h.body, &|el| h.class(el));
    let c = webkit::compare(name, &wk, &ours, real_font_tolerance, |_| false);
    eprintln!(
        "{name}: {} boxes, worst difference {:.2} px",
        c.boxes, c.worst
    );
    assert_eq!(c.boxes, c.ours, "{name}: the same element count");
    assert!(c.boxes > 5, "{name}: {} boxes", c.boxes);
    let sized = wk.iter().filter(|w| w.w > 0.0 && w.h > 0.0).count();
    assert!(sized * 2 > wk.len(), "{name}: most boxes have a size");
    assert!(
        c.bad.is_empty(),
        "{} of {} boxes differ from WebKit:\n{}",
        c.bad.len(),
        c.boxes,
        c.bad.join("\n")
    );
}

#[test]
fn docs_ask_matches_webkit() {
    check("docs-ask");
}

#[test]
fn docs_ls_matches_webkit() {
    check("docs-ls");
}

#[test]
fn python_ask_matches_webkit() {
    check("python-ask");
}

#[test]
fn python_table_matches_webkit() {
    check("python-table");
}
