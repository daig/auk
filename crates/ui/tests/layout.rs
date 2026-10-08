//! Probe pages laid out by our engine and by WebKit (through `webkit-boxes`), set in Ahem, must
//! agree on every element's border box within 1 CSS px (GOAL.md, `layout`). The pages and the
//! comparison are shared with the `layout` suite (`auk_conformance::layout`); `PROBE_DIFF=1` prints
//! every box off by more than 0.05 px.

mod common;

#[test]
fn probe_block() {
    common::assert_probe("block");
}

#[test]
fn probe_inline() {
    common::assert_probe("inline");
}

#[test]
fn probe_flex() {
    common::assert_probe("flex");
}

#[test]
fn probe_grid() {
    common::assert_probe("grid");
}

#[test]
fn probe_table() {
    common::assert_probe("table");
}

#[test]
fn probe_positioned() {
    common::assert_probe("positioned");
}

#[test]
fn probe_overflow() {
    common::assert_probe("overflow");
}

/// Real fonts: within 2 CSS px or 2% of the box's size, whichever is larger (GOAL.md).
#[test]
fn probe_real_fonts() {
    common::assert_page("realfont", auk_conformance::webkit::real_font_tolerance);
}

#[test]
fn probe_mixed() {
    common::assert_probe("mixed");
}

#[test]
fn probe_table_collapse() {
    common::assert_probe("collapse");
}

#[test]
fn probe_zoom() {
    common::assert_probe("zoom");
}
