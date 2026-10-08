//! Probe pages laid out by our engine and by WebKit (through `webkit-boxes`), set in Ahem, must
//! agree on every element's border box within 1 CSS px (GOAL.md, `layout`).

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
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/probes/realfont.html");
    let html = std::fs::read_to_string(path).unwrap();
    let bad = common::compare("realfont", &html, 800.0, |b| (b.w.max(b.h) * 0.02).max(2.0));
    assert!(
        bad.is_empty(),
        "{} boxes differ from WebKit beyond tolerance:\n{}",
        bad.len(),
        bad.join("\n")
    );
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

/// Diagnostics: `PROBE=<name> cargo test -p auk-ui --test layout probe_diff -- --ignored
/// --nocapture` prints every box that differs from WebKit by more than 0.05 px.
#[test]
#[ignore]
fn probe_diff() {
    let Ok(name) = std::env::var("PROBE") else {
        return;
    };
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/probes/{name}.html"));
    let html = std::fs::read_to_string(path).unwrap();
    for line in common::compare(&name, &html, 800.0, |_| 0.05) {
        eprintln!("{line}");
    }
}
