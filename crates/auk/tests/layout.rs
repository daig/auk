//! The layout suite (GOAL.md row `layout`, spec §8, §18): the UI engine against WebKit.
//!
//! - **Probe pages** (`crates/conformance/probes`), one per layout mode the engine implements
//!   (§18.2: block, inline, flex, grid, table, positioned) plus collapsed tables, overflow, mixed
//!   modes and zoom, set in Ahem: every element's border box within 1 CSS px of WebKit's. A page in
//!   the system fonts is held to the real-font tolerance.
//! - **Fixtures**: every `el`-plus-stylesheet recording (`fixtures/tsp/docs-*`, `python-*`) is played
//!   into a pane of the real daemon with `surface-play`; a window replica draws it through
//!   `auk-tsp`, `auk-views` and `auk-ui`, and `auk-views` exports the same DOM and CSS as a page
//!   for WebKit. Every box is within 2 CSS px or 2% of its size, whichever is larger.
//! - **Kinds**: the daemon's hello advertises exactly T0+T1 (D22), and a surface holding every one
//!   of them, sent by a program in a daemon pane, renders: each node is an `.sf.sf-<kind>` element
//!   with its `data-id`, and every kind lays out to a non-empty box.
//!
//! WebKit's boxes come from `crates/conformance/webkit/boxes.swift`, built fresh with `swiftc` in
//! each test process (`auk_conformance::webkit`).
//!
//! Exceptions: none. Every box of every page and fixture is compared.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use auk_conformance::harness::{TestEnv, quote};
use auk_conformance::layout::assert_probe;
use auk_conformance::probe::{Probe, frame, hello, open_with};
use auk_conformance::surfaces::{PANE_WIDTH, SurfaceWindow, kinds_surface};
use auk_conformance::webkit::{self, real_font_tolerance};
use serde_json::Value;

fn tmp() -> &'static Path {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
}

fn env(name: &str) -> TestEnv {
    let env = TestEnv::new(env!("CARGO_BIN_EXE_auk"), name);
    env.start_daemon();
    env
}

// ---------------------------------------------------------------------------------------------
// Probe pages in Ahem: within 1 CSS px.

#[test]
fn probe_block() {
    assert_probe(tmp(), "block");
}

#[test]
fn probe_inline() {
    assert_probe(tmp(), "inline");
}

#[test]
fn probe_flex() {
    assert_probe(tmp(), "flex");
}

#[test]
fn probe_grid() {
    assert_probe(tmp(), "grid");
}

#[test]
fn probe_table() {
    assert_probe(tmp(), "table");
}

#[test]
fn probe_table_collapsed_borders() {
    assert_probe(tmp(), "collapse");
}

#[test]
fn probe_positioned() {
    assert_probe(tmp(), "positioned");
}

#[test]
fn probe_overflow() {
    assert_probe(tmp(), "overflow");
}

#[test]
fn probe_mixed_modes() {
    assert_probe(tmp(), "mixed");
}

#[test]
fn probe_zoom() {
    assert_probe(tmp(), "zoom");
}

/// The system fonts (Menlo, system-ui, Helvetica, Georgia): the real-font tolerance.
#[test]
fn probe_real_fonts() {
    let html = auk_conformance::layout::probe_html("realfont");
    auk_conformance::layout::assert_page(tmp(), "realfont", &html, real_font_tolerance);
}

/// Every page in the probe directory is one of the tests above.
#[test]
fn every_probe_page_is_tested() {
    let tested: BTreeSet<&str> = [
        "block",
        "inline",
        "flex",
        "grid",
        "table",
        "collapse",
        "positioned",
        "overflow",
        "mixed",
        "zoom",
        "realfont",
    ]
    .into();
    let pages = page_names(&auk_conformance::layout::probes_dir(), |name| {
        name.strip_suffix(".html").map(str::to_owned)
    });
    let tested: BTreeSet<String> = tested.iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(pages, tested);
}

fn page_names(dir: &Path, keep: impl Fn(&str) -> Option<String>) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| {
            e.expect("a directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter_map(|name| keep(&name))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Fixtures in the real fonts, through the daemon and a drawing window.

/// The `el`-plus-stylesheet fixtures this suite compares.
const EL_FIXTURES: [&str; 4] = ["docs-ask", "docs-ls", "python-ask", "python-table"];

/// Plays `name` into a shell pane of a fresh daemon, draws it in a window, and compares the
/// window's boxes with WebKit's layout of the page `auk-views` exports.
fn check_fixture(name: &str) {
    let env = env(name);
    let pane = env.new_pane(&[]);
    env.wait(pane, "prompt");
    let mut window = SurfaceWindow::attach(&env, "main");
    let path = auk_conformance::fixtures_dir().join(format!("{name}.jsonl"));
    let played = env.daemon(&format!(
        "surface-play --pane {pane} {}",
        quote(&path.to_string_lossy())
    ));
    assert!(
        played["frames"].as_u64().unwrap_or(0) > 0,
        "{name}: {played}"
    );
    window.converge(&env, pane);
    let p = window.projection(pane);

    // The window projected every surface the daemon holds.
    let surfaces = window.replica(pane).tsp().surfaces();
    assert!(!surfaces.is_empty(), "{name}: no surfaces");
    for s in surfaces {
        assert!(
            p.view.scope(s.key()).is_some(),
            "{name}: surface {} not drawn",
            s.id()
        );
    }
    // The fixture's sheets and `el` nodes reached the page.
    let html = p.html();
    assert!(
        html.contains("data-scope"),
        "{name}: no program sheet in the page"
    );

    let ours = p.boxes();
    let wk = webkit::boxes(tmp(), name, &html, PANE_WIDTH, &[]);
    let c = webkit::compare(name, &wk, &ours, real_font_tolerance, |_| false);
    eprintln!(
        "{name}: {} boxes, worst difference {:.2} px",
        c.boxes, c.worst
    );
    assert!(c.boxes > 5, "{name}: {} boxes", c.boxes);
    assert_eq!(
        c.boxes, c.ours,
        "{name}: both engines see the same elements"
    );
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
fn fixture_docs_ask_matches_webkit() {
    check_fixture("docs-ask");
}

#[test]
fn fixture_docs_ls_matches_webkit() {
    check_fixture("docs-ls");
}

#[test]
fn fixture_python_ask_matches_webkit() {
    check_fixture("python-ask");
}

#[test]
fn fixture_python_table_matches_webkit() {
    check_fixture("python-table");
}

/// The fixtures above are exactly the docs and Python recordings that send `el` nodes and a
/// stylesheet, so a new recording can't be missed.
#[test]
fn every_el_and_sheet_fixture_is_compared() {
    let dir = auk_conformance::fixtures_dir();
    let found = page_names(&dir, |file| {
        let name = file.strip_suffix(".jsonl")?;
        if !(name.starts_with("docs-") || name.starts_with("python-")) {
            return None;
        }
        let records = auk_conformance::load(&dir.join(file)).expect("a recording");
        let out = |verb: &str| {
            records
                .iter()
                .filter(|r| r.dir == auk_conformance::Dir::Out && r.verb == verb)
                .collect::<Vec<_>>()
        };
        let sheet = !out("s").is_empty();
        let el = out("f").iter().any(|r| has_el(&r.body));
        (sheet && el).then(|| name.to_owned())
    });
    let want: BTreeSet<String> = EL_FIXTURES.iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(found, want);
}

fn has_el(v: &Value) -> bool {
    match v {
        Value::Object(m) => {
            m.get("k").and_then(Value::as_str) == Some("el") || m.values().any(has_el)
        }
        Value::Array(a) => a.iter().any(has_el),
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------------
// Kinds: advertised exactly T0+T1, and each renders.

/// §8.2's tiers, spelled out from the spec.
const T0: &str =
    "col row card section rule spacer text badge kv icon spinner elapsed progress ansi rows el";
const T1: &str = "md code diff editor input status seg overlay toast shimmer rate meter list item tabs table tree kbd tool agent checklist image";

/// Kinds that render as spans (§8.3: inline kinds).
const INLINE: [&str; 7] = [
    "badge", "kbd", "icon", "spinner", "shimmer", "elapsed", "rate",
];

#[test]
fn every_t0_and_t1_kind_renders() {
    let env = env("kinds");
    let probe = Probe::start(&env, &[]);
    let mut window = SurfaceWindow::attach(&env, "main");

    // The daemon advertises exactly T0+T1.
    probe.write(&hello(&["edit", "undo", "send"]));
    let bodies = probe.fence_tsp();
    let reply = bodies
        .iter()
        .find(|b| b["r"] == "hello")
        .unwrap_or_else(|| panic!("no hello reply in {bodies:?}"));
    let advertised: Vec<&str> = reply["kinds"]
        .as_array()
        .expect("kinds")
        .iter()
        .map(|k| k.as_str().expect("a kind name"))
        .collect();
    let want: BTreeSet<&str> = T0.split(' ').chain(T1.split(' ')).collect();
    let have: BTreeSet<&str> = advertised.iter().copied().collect();
    assert_eq!(have, want, "the hello lists exactly the T0 and T1 kinds");
    assert_eq!(advertised.len(), want.len(), "no duplicates");

    // A surface with a node of every kind.
    let (main, frame2) = kinds_surface();
    let mut bytes = open_with("s1", "inline", main);
    bytes.extend(frame("s1", 2, frame2));
    probe.write(&bytes);
    let errors: Vec<Value> = probe
        .fence_tsp()
        .into_iter()
        .filter(|b| b["ev"] == "error")
        .collect();
    // The one error is the surface's deliberate unknown `el` tag (§7.7).
    assert_eq!(
        errors,
        [serde_json::json!({"ev": "error", "sf": "s1", "id": "el2", "msg": "unknown el tag marquee"})],
        "the program got errors"
    );
    window.converge(&env, probe.pane);

    let replica = window.replica(probe.pane);
    let surface = replica
        .tsp()
        .surfaces()
        .iter()
        .find(|s| s.id() == "s1")
        .expect("surface s1");
    let doc = surface.document();
    let p = window.projection(probe.pane);
    // kind → (nodes, nodes with a non-empty box)
    let mut seen: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut problems = Vec::new();
    let mut stack: Vec<_> = ["main", "layer"]
        .iter()
        .filter_map(|r| doc.region(r))
        .flat_map(|r| r.children().collect::<Vec<_>>())
        .collect();
    while let Some(n) = stack.pop() {
        stack.extend(n.children());
        let kind = n.kind().to_owned();
        let Some(el) = window.element(probe.pane, "s1", n.id()) else {
            problems.push(format!("{} ({kind}): no element", n.id()));
            continue;
        };
        let class = p.class(el);
        if !class.starts_with(&format!("sf sf-{kind}")) {
            problems.push(format!("{} ({kind}): class {class:?}", n.id()));
        }
        if p.doc.attribute(el, "data-id") != Some(n.id()) {
            problems.push(format!(
                "{} ({kind}): data-id {:?}",
                n.id(),
                p.doc.attribute(el, "data-id")
            ));
        }
        let tag = p
            .doc
            .node(el)
            .element()
            .map(|e| e.tag.as_str().to_owned())
            .unwrap_or_default();
        if INLINE.contains(&kind.as_str()) && tag != "span" {
            problems.push(format!("{} ({kind}): inline kind as <{tag}>", n.id()));
        }
        let r = p.doc.border_box(el).unwrap_or_default();
        let entry = seen.entry(kind).or_default();
        entry.0 += 1;
        if r.w > 0.0 && r.h > 0.0 {
            entry.1 += 1;
        }
    }
    let kinds: BTreeSet<&str> = seen.keys().map(String::as_str).collect();
    assert_eq!(
        kinds, want,
        "the surface holds a node of every advertised kind"
    );
    // A toast node is hidden by design; the pane's toast layer shows it (book:7636).
    let shown = descendants(&p.doc, p.body)
        .into_iter()
        .filter(|&el| p.class(el).split(' ').any(|c| c == "toast"))
        .any(|el| p.doc.border_box(el).is_some_and(|r| r.w > 0.0 && r.h > 0.0));
    if shown {
        seen.get_mut("toast").expect("a toast node").1 += 1;
    }
    for (kind, (nodes, sized)) in &seen {
        if *sized == 0 {
            problems.push(format!(
                "{kind}: none of its {nodes} nodes has a non-empty box"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "kinds that don't render:\n{}",
        problems.join("\n")
    );
}

/// Every element under `el`, in document order.
fn descendants(
    doc: &auk_conformance::layout::Document,
    el: auk_conformance::layout::NodeId,
) -> Vec<auk_conformance::layout::NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![el];
    while let Some(n) = stack.pop() {
        out.push(n);
        stack.extend(doc.element_children(n).into_iter().rev());
    }
    out
}
