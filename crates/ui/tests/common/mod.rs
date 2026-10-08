//! Test support: the probe-page reader, the WebKit box reader and the comparison live in
//! `auk_conformance` (`layout`, `webkit`), shared with the `layout` suite.

#![allow(dead_code)]

use auk_conformance::layout;
use auk_ui::{Document, NodeId};
use std::path::Path;

pub fn ahem_bytes() -> Vec<u8> {
    layout::ahem_bytes()
}

fn tmp() -> &'static Path {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
}

/// A document with Ahem registered, holding `html`, laid out at a viewport size.
pub fn our_layout(html: &str, width: f32, height: f32) -> (Document, NodeId) {
    let mut doc = Document::new();
    doc.fonts_mut().register_font(ahem_bytes());
    let body = layout::load_html(&mut doc, html);
    doc.set_viewport(width, height);
    doc.update();
    (doc, body)
}

/// Asserts a probe page (`crates/conformance/probes/<name>.html`) is within 1 px of WebKit on
/// every box. `PROBE_DIFF=1` prints every box off by more than 0.05 px first.
pub fn assert_probe(name: &str) {
    layout::assert_probe(tmp(), name);
}

/// Asserts a probe page is within `tolerance` of WebKit on every box.
pub fn assert_page(name: &str, tolerance: impl Fn(&auk_conformance::webkit::BoxRec) -> f32) {
    layout::assert_page(tmp(), name, &layout::probe_html(name), tolerance);
}
