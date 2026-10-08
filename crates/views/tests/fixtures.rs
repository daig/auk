//! Recorded sessions (the SDK examples, the docs' frames and omp) replayed through the TSP engine
//! into the views and the UI engine: structure, layout sanity, and no errors.

mod common;

use auk_ui::NodeId;
use common::{Harness, replay};
use serde_json::Value;

/// Every element box is finite and inside a sane range.
fn layout_sane(h: &Harness) {
    let mut stack = vec![h.body];
    let mut n = 0;
    while let Some(e) = stack.pop() {
        if h.doc.node(e).is_element()
            && let Some(r) = h.doc.border_box(e)
        {
            assert!(
                r.x.is_finite() && r.y.is_finite() && r.w.is_finite() && r.h.is_finite(),
                "non-finite box {r:?}"
            );
            assert!(r.w >= 0.0 && r.h >= 0.0, "negative box {r:?}");
            n += 1;
        }
        stack.extend(h.doc.children(e).iter().copied());
    }
    assert!(n > 0);
}

fn no_errors(h: &Harness, name: &str) {
    let bad: Vec<&Value> = h
        .sent
        .iter()
        .filter(|e| e["ev"] == "error" || e["ev"] == "gone")
        .collect();
    assert!(bad.is_empty(), "{name}: {bad:?}");
}

fn region(h: &Harness, sf: &str) -> NodeId {
    let key = h
        .pane
        .surfaces()
        .iter()
        .rev()
        .find(|s| s.id() == sf)
        .unwrap()
        .key();
    h.view
        .region(key, auk_views::Region::Main)
        .expect("main region")
}

#[test]
fn python_ask_form_draws_with_its_sheet() {
    let mut h = replay("python-ask.jsonl", |_, _| {});
    no_errors(&h, "python-ask");
    h.view.layout(&mut h.doc);
    layout_sane(&h);
    let main = region(&h, "s1");
    assert_eq!(
        h.class(main).split(' ').take(2).collect::<Vec<_>>(),
        ["sf-region", "sf-main"]
    );
    assert!(h.attr(main, "data-scope").unwrap().starts_with("sf"));
    assert_eq!(h.attr(main, "data-app").as_deref(), Some("ask"));
    let form = h.el("s1", "main.ask");
    assert_eq!(
        h.doc.node(form).element().unwrap().tag.as_str(),
        "div",
        "form draws as div"
    );
    assert!(h.class(form).contains("sf sf-el ask"));
    // The program sheet applies inside the scope: .ask is a flex column.
    let st = h.doc.computed_style(form).unwrap();
    assert_eq!(format!("{:?}", st.display), "Flex");
    let radio = h.el("s1", "main.ask.sizes.s.r");
    assert_eq!(h.doc.node(radio).element().unwrap().tag.as_str(), "span");
    assert!(h.class(radio).contains("sf-radio"));
    assert_eq!(h.attr(radio, "role").as_deref(), Some("radio"));
    assert_eq!(h.attr(radio, "aria-checked").as_deref(), Some("false"));
    let label = h.el("s1", "main.ask.sizes.s");
    assert_eq!(h.doc.node(label).element().unwrap().tag.as_str(), "label");
    let r = h.doc.border_box(radio).unwrap();
    assert!(
        (r.w - 15.0).abs() < 0.5 && (r.h - 15.0).abs() < 0.5,
        "15px control: {r:?}"
    );
    // The form is at most 460px wide (max-width from the sheet).
    assert!(h.doc.border_box(form).unwrap().w <= 460.5);
}

#[test]
fn python_table_draws_el_table() {
    let mut h = replay("python-table.jsonl", |_, _| {});
    no_errors(&h, "python-table");
    h.view.layout(&mut h.doc);
    layout_sane(&h);
    let t = h.el("s1", "main.0");
    assert_eq!(h.doc.node(t).element().unwrap().tag.as_str(), "table");
    let rows = h.doc.element_children(t);
    assert!(rows.len() > 3);
    let size = h.el("s1", "main.0.head.1");
    assert_eq!(h.doc.node(size).element().unwrap().tag.as_str(), "th");
    assert_eq!(h.text(size), "Size");
    // Right-aligned by the program sheet.
    assert_eq!(
        format!("{:?}", h.doc.computed_style(size).unwrap().text_align),
        "Right"
    );
}

#[test]
fn python_progress_card_spinner_elapsed_ansi_progress() {
    let mut seen_running = false;
    let mut h = replay("python-progress.jsonl", |h, rec| {
        if rec.verb == "f" && rec.body["s"] == 1 {
            let card = h.el("s1", "main.build");
            assert!(h.class(card).contains("st-running"));
            let chip = h.find_class(card, "sf-chip");
            assert_eq!(h.text(chip[0]), "Running");
            let sp = h.el("s1", "main.build.h.sp");
            assert_eq!(
                h.doc.node(sp).element().unwrap().tag.as_str(),
                "span",
                "inline kind"
            );
            assert!(!h.find_class(sp, "sf-dots").is_empty());
            let el = h.el("s1", "main.build.h.t");
            assert_eq!(h.attr(el, "role").as_deref(), Some("timer"));
            seen_running = true;
        }
    });
    assert!(seen_running);
    no_errors(&h, "python-progress");
    h.view.layout(&mut h.doc);
    layout_sane(&h);
    let card = h.el("s1", "main.build");
    assert!(h.class(card).contains("st-done"));
    assert_eq!(
        h.text(h.find_class(card, "sf-head")[0]),
        "cargo build --release"
    );
    let p = h.el("s1", "main.build.p");
    assert_eq!(h.attr(p, "role").as_deref(), Some("progressbar"));
    assert_eq!(h.attr(p, "aria-valuenow").as_deref(), Some("100"));
    assert_eq!(h.attr(p, "data-tone").as_deref(), Some("success"));
    // The stopped timer reads its stopped value.
    let t = h.el("s1", "main.build.h.t");
    assert_eq!(h.text(t), "2.8s");
    // The spinner was replaced by text with the same id, in the same place.
    let sp = h.el("s1", "main.build.h.sp");
    assert!(h.class(sp).contains("sf-text"));
    assert_eq!(h.doc.element_children(h.el("s1", "main.build.h"))[0], sp);
    // ansi: SGR colors as theme variables, one line per row.
    let log = h.el("s1", "main.build.log");
    let lines = h.find_class(log, "ln");
    assert_eq!(lines.len(), 7, "{}", h.text(log));
    assert!(h.text(log).contains("ok resolving dependencies"));
    let green = h.find_class(log, "ln")[0];
    let span = h.doc.element_children(green)[0];
    assert_eq!(h.view.written().style(span), Some("color: var(--ansi2);"));
}

#[test]
fn python_chat_dock_editor_status_md() {
    let mut checked_dock = false;
    let mut h = replay("python-chat.jsonl", |h, rec| {
        if rec.verb == "f" && rec.body["s"] == 2 {
            let ed = h.el("s1", "dock.ed");
            assert!(h.class(ed).contains("focused"), "focus op: {}", h.class(ed));
        }
        if rec.verb == "f" && rec.body["s"] == 10 {
            let key = h.pane.surfaces()[0].key();
            let dock = h
                .view
                .region(key, auk_views::Region::Dock)
                .expect("dock region");
            assert!(h.class(dock).contains("sf-dock"));
            let ed = h.el("s1", "dock.ed");
            assert_eq!(h.attr(ed, "role").as_deref(), Some("textbox"));
            assert!(!h.find_class(ed, "sf-ed-prompt").is_empty());
            // Empty again after send: the placeholder shows.
            assert_eq!(h.text(h.find_class(ed, "sf-ed-ph")[0]), "Message");
            let bar = h.el("s1", "dock.bar");
            let right = h.find_class(bar, "sf-st-r")[0];
            assert_eq!(h.text(right), "2 sent");
            checked_dock = true;
        }
    });
    assert!(checked_dock);
    no_errors(&h, "python-chat");
    h.view.layout(&mut h.doc);
    layout_sane(&h);
    let key = h.pane.surfaces()[0].key();
    let m1 = h.el("s1", "main.m1.0");
    let md = h.find_class(m1, "md");
    assert_eq!(md.len(), 1);
    assert!(!h.doc.element_children(md[0]).is_empty());
    let strong = &h.find_class(m1, "md")[0];
    let html = {
        let mut s = String::new();
        auk_views::html::element_html(&h.doc, h.view.written(), *strong, &mut s);
        s
    };
    assert!(html.contains("<strong>markdown</strong>"), "{html}");
    assert!(html.contains("<code>code</code>"), "{html}");
    // The surface closed (kept): the dock goes, main stays.
    assert!(h.view.region(key, auk_views::Region::Dock).is_none());
}

#[test]
fn docs_frames_md_card_code_shimmer() {
    let mut h = replay("docs-frames.jsonl", |_, _| {});
    no_errors(&h, "docs-frames");
    h.view.layout(&mut h.doc);
    layout_sane(&h);
    let m1 = h.el("s1", "m1");
    assert_eq!(h.text(m1), "Checking the build, then the tests.");
    let card = h.el("s1", "t9");
    assert!(h.class(card).contains("collapsible"));
    // `collapsed` went from true to false: the reset rule opens it.
    assert!(!h.class(card).contains("collapsed"), "{}", h.class(card));
    assert!(h.class(card).contains("st-done"));
    let code = h.el("s1", "o9");
    assert_eq!(h.find_class(code, "sf-ln").len(), 2);
    let m2 = h.el("s1", "m2");
    assert!(h.text(m2).contains("All 42 tests pass."));
}

#[test]
fn docs_regions_dock_layer_overlay_focus() {
    let mut h = replay("docs-regions.jsonl", |_, _| {});
    no_errors(&h, "docs-regions");
    h.view.layout(&mut h.doc);
    layout_sane(&h);
    let key = h.pane.surfaces()[0].key();
    let layer = h.view.region(key, auk_views::Region::Layer).unwrap();
    assert!(h.class(layer).contains("sf-layer"));
    let ov = h.el("s1", "ask");
    assert!(h.class(ov).contains("sz-sm") && h.class(ov).contains("at-top"));
    let card = h.find_class(ov, "sf-ov-card")[0];
    assert_eq!(h.attr(card, "role").as_deref(), Some("dialog"));
    assert_eq!(h.text(h.find_class(ov, "sf-ov-title")[0]), "Update serde");
    let ed = h.el("s1", "ed");
    assert!(h.class(ed).contains("focused"));
    let bar = h.el("s1", "bar");
    assert_eq!(h.text(h.find_class(bar, "sf-st-r")[0]), "ready");
    assert!(h.text(h.find_class(bar, "sf-st-l")[0]).ends_with("main"));
}

#[test]
fn docs_ask_and_ls() {
    for name in ["docs-ask.jsonl", "docs-ls.jsonl"] {
        let mut h = replay(name, |_, _| {});
        no_errors(&h, name);
        h.view.layout(&mut h.doc);
        layout_sane(&h);
    }
}

/// The whole omp session replays with no errors, and its transcript draws natively.
#[test]
fn omp_session_replays_without_errors() {
    let mut frames = 0;
    let mut kinds = std::collections::BTreeSet::new();
    let mut h = replay("omp-session.jsonl", |h, rec| {
        if rec.verb == "f" {
            frames += 1;
            for s in h.pane.surfaces() {
                let mut stack = vec![s.document().root()];
                while let Some(n) = stack.pop() {
                    kinds.insert(n.kind().to_owned());
                    stack.extend(n.children());
                }
            }
        }
    });
    assert!(frames > 500);
    no_errors(&h, "omp-session");
    h.view.layout(&mut h.doc);
    layout_sane(&h);
    let s = &h.pane.surfaces()[0];
    // The kinds omp 18.8.5 used in this session. (The 18.8.4 recording also had `code`, from a
    // grep result the model happened to run; `docs-frames` covers `code`.)
    for k in [
        "tool", "md", "diff", "ansi", "editor", "status", "section", "row", "icon", "card",
        "image", "spinner", "elapsed", "shimmer", "kbd", "meter",
    ] {
        assert!(kinds.contains(k), "omp used {k}");
    }
    // Every node in the document has an element.
    let key = s.key();
    let mut stack: Vec<_> = s.document().region("main").unwrap().children().collect();
    let mut checked = 0;
    while let Some(n) = stack.pop() {
        let el = h.view.node_element(key, n.id());
        assert!(el.is_some(), "{} ({}) has no element", n.id(), n.kind());
        let class = h.class(el.unwrap());
        assert!(class.starts_with(&format!("sf sf-{}", n.kind())), "{class}");
        checked += 1;
        stack.extend(n.children());
    }
    assert!(checked > 50);
}
