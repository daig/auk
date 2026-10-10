//! The Rust features the omp sheets wait on (amendment A24; research/omp-stylesheets.md §2.5 K11,
//! §5 P6, §7.4, §8, §9 phase B): the Spindle mark in omp's working row, inline Markdown in omp's
//! composer, the decorations (`.sf-caption`, `.sf-thought-sum`, `.sf-work-head`, `.sf-brand`)
//! and the picker's row arrival. Constructed frames, and omp's recorded session.

mod common;

use auk_tsp::Config;
use auk_ui::NodeId;
use auk_views::{Outcome, PointerEvent, ViewSettings};
use common::*;
use serde_json::{Value, json};

fn center(h: &Harness, el: NodeId) -> (f32, f32) {
    let r = h.doc.border_box(el).expect("laid out");
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

fn click(h: &mut Harness, el: NodeId) -> Vec<Outcome> {
    let (x, y) = center(h, el);
    let out = h.view.click(
        &mut h.doc,
        &h.pane,
        PointerEvent {
            x,
            y,
            clicks: 1,
            ..Default::default()
        },
    );
    h.view.layout(&mut h.doc);
    out
}

/// Deliver outcomes' events through the engine; returns what the program heard.
fn deliver(h: &mut Harness, out: Vec<Outcome>) -> Vec<Value> {
    let before = h.sent.len();
    for o in out {
        if let Outcome::Event { sf, event, .. } = o {
            h.send(&sf, *event);
        }
    }
    h.sent[before..].to_vec()
}

/// The inline style the view wrote on `el`.
fn style(h: &Harness, el: NodeId) -> String {
    h.view.written().style(el).unwrap_or("").to_owned()
}

fn has(h: &Harness, el: NodeId, class: &str) -> bool {
    h.class(el).split(' ').any(|c| c == class)
}

fn one(h: &Harness, root: NodeId, class: &str) -> NodeId {
    let found = h.find_class(root, class);
    assert_eq!(found.len(), 1, "one .{class}, found {}", found.len());
    found[0]
}

fn shown(h: &Harness, el: NodeId) -> bool {
    std::iter::successors(Some(el), |&e| h.doc.parent(e)).all(|e| {
        h.doc
            .computed_style(e)
            .is_none_or(|s| s.display != auk_ui::style::Display::None)
    })
}

/// An omp session surface (title "Fix the parser") whose regions hold `main`, `dock` and
/// `layer`.
fn omp(settings: ViewSettings, features: &[&str], main: Value, dock: Value) -> Harness {
    let mut h = Harness::with(Config::default(), settings);
    h.hello(features);
    h.apc(
        "o",
        r#"{"id":"s1","mode":"flow","role":"omp.session","title":"Fix the parser"}"#,
    );
    let frame = json!({"sf": "s1", "s": 1, "ops": [
        ["add", "main", "s1", null, {"id": "main", "k": "col", "c": main}],
        ["add", "dock", "s1", null, {"id": "dock", "k": "col", "c": dock}]]});
    h.apc("f", &frame.to_string());
    h.frame();
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    h
}

// ---------------------------------------------------------------------------------------------
// The Spindle mark (book:8324-8326, 8360)

fn working_row() -> Value {
    json!([{"id": "w", "k": "row", "p": {"role": "omp.working", "gap": "sm"}, "c": [
        {"id": "w.sp", "k": "spinner", "p": {"style": "starburst", "tone": "accent"}},
        {"id": "w.l", "k": "text", "p": {"text": "Working…"}}
    ]}, {"id": "other", "k": "spinner", "p": {"style": "starburst"}}])
}

#[test]
fn a_starburst_in_omps_working_row_draws_the_spindle_mark_and_turns_it() {
    let mut h = omp(Default::default(), &[], json!([]), working_row());
    let sp = h.el("s1", "w.sp");
    let mark = one(&h, sp, "sf-spindle");
    assert_eq!(h.attr(mark, "aria-hidden").as_deref(), Some("true"));
    assert!(
        h.doc.children(mark).is_empty(),
        "the mark is painted, not text"
    );
    assert!(h.find_class(sp, "sf-spin").is_empty(), "no glyph strip");
    // Only under omp.working: another starburst keeps its glyphs.
    let other = h.el("s1", "other");
    assert!(h.find_class(other, "sf-spindle").is_empty());
    assert_eq!(h.find_class(other, "sf-spin").len(), 1);
    // The frame: 1.2em wide, --sf-lh tall, in the spinner's color.
    let b = h.doc.border_box(mark).unwrap();
    assert!((b.w - 1.2 * 13.0).abs() < 0.5, "{b:?}");
    assert!((b.h - 17.0).abs() < 0.5, "{b:?}");
    // It turns on the view's clock (not CSS keyframes).
    let st = h.doc.computed_style(mark).unwrap();
    assert!(st.animations.as_ref().is_none_or(|a| a.is_empty()));
    let mut seen = std::collections::HashSet::new();
    for _ in 0..12 {
        h.now += 50.0;
        assert!(
            h.view.tick(&mut h.doc, &h.pane, h.now).is_some(),
            "it moves"
        );
        h.view.layout(&mut h.doc);
        seen.insert(style(&h, mark));
    }
    assert!(seen.len() > 4, "{seen:?}");
    assert!(
        seen.iter().all(|s| s.starts_with("transform: scaleX(")),
        "{seen:?}"
    );
    assert!(h.doc.computed_style(mark).unwrap().transform.is_some());
}

#[test]
fn the_spindle_mark_rests_under_reduce_motion_and_under_a_sheet() {
    // Reduce Motion: the rest frame, and the clock stops for it.
    let still = ViewSettings {
        reduce_motion: true,
        ..Default::default()
    };
    let mut h = omp(still, &[], json!([]), working_row());
    let mark = one(&h, h.el("s1", "w.sp"), "sf-spindle");
    h.now += 300.0;
    h.frame();
    assert_eq!(style(&h, mark), "", "rest frame");
    let custom = h.doc.computed_style(mark).unwrap().custom.clone();
    assert_eq!(custom.get("--sf-spindle").map(|v| v.trim()), Some("still"));

    // Under a sheet (`.sf-covered`) it rests, and turns again once the sheet goes.
    let mut h = omp(Default::default(), &[], json!([]), working_row());
    let mark = one(&h, h.el("s1", "w.sp"), "sf-spindle");
    h.now += 100.0;
    h.frame();
    assert_ne!(style(&h, mark), "");
    h.frame_ops(
        "s1",
        2,
        json!([["add", "layer", "s1", null, {"id": "layer", "k": "col", "c": [
            {"id": "ov", "k": "overlay", "p": {"modal": true}}]}]]),
    );
    h.frame();
    for _ in 0..5 {
        h.now += 97.0;
        h.frame();
        assert_eq!(style(&h, mark), "", "rests under the sheet");
    }
    h.frame_ops("s1", 3, json!([["del", "ov"]]));
    h.frame();
    let mut moved = false;
    for _ in 0..6 {
        h.now += 97.0;
        h.frame();
        moved |= !style(&h, mark).is_empty();
    }
    assert!(moved, "it turns again");
}

// ---------------------------------------------------------------------------------------------
// Inline Markdown in omp's composer (book:9198-9201)

const DRAFT: &str = "# Plan\nFix **the** `parse` step, see [docs](https://x.y)\n- *first*";

fn composer(text: &str, omp_role: bool) -> Value {
    let role = if omp_role { "omp.editor" } else { "plain" };
    json!([{"id": "ed", "k": "col", "p": {"role": role}, "c": [
        {"id": "ed.line", "k": "row", "p": {"role": "omp.composer.line"}, "c": [
            {"id": "ed.in", "k": "editor", "p": {"text": text, "cursor": 0, "sendable": true}}
        ]}
    ]}])
}

fn field_text_el(h: &Harness) -> NodeId {
    one(h, h.el("s1", "ed.in"), "sf-ed-text")
}

#[test]
fn omps_composer_draws_its_markdown_inline_and_keeps_every_character() {
    let h = omp(Default::default(), &[], json!([]), composer(DRAFT, true));
    let ed = h.el("s1", "ed.in");
    assert!(has(&h, ed, "markdown"));
    let t = field_text_el(&h);
    // Every character stays, markers included, in order.
    assert_eq!(h.text(t), DRAFT);
    let texts = |class: &str| -> Vec<String> {
        h.find_class(t, class)
            .into_iter()
            .map(|e| h.text(e))
            .collect()
    };
    assert_eq!(texts("md-strong"), ["**", "the", "**"]);
    assert_eq!(texts("md-code"), ["`", "parse", "`"]);
    assert_eq!(texts("md-url"), ["https://x.y"]);
    assert_eq!(texts("md-em"), ["*", "first", "*"]);
    assert!(texts("md-h").concat() == "# Plan");
    let marks = texts("md-mark").concat();
    assert_eq!(marks, "# ****``[]()- **");
    // The looks: markers dim, strong heavier, emphasis italic.
    let strong = h.find_class(t, "md-strong")[1];
    assert!(h.doc.computed_style(strong).unwrap().font_weight >= 600.0);
    let em = h.find_class(t, "md-em")[1];
    assert_ne!(
        h.doc.computed_style(em).unwrap().font_style,
        auk_ui::style::FontStyle::Normal
    );

    // The same editor outside an omp.editor role draws plain text.
    let p = omp(Default::default(), &[], json!([]), composer(DRAFT, false));
    assert!(!has(&p, p.el("s1", "ed.in"), "markdown"));
    let pt = field_text_el(&p);
    assert!(p.find_class(pt, "md-mark").is_empty());
    assert_eq!(p.text(pt), DRAFT);
    // A code field in the composer stays code.
    let c = omp(
        Default::default(),
        &[],
        json!([]),
        json!([{"id": "ed", "k": "col", "p": {"role": "omp.editor"}, "c": [
            {"id": "ed.in", "k": "editor", "p": {"text": "**x**", "lang": "rust"}}]}]),
    );
    assert!(!has(&c, c.el("s1", "ed.in"), "markdown"));
}

/// The caret, a click's offset, the selection and every event the program receives are the
/// same as in a plain editor.
#[test]
fn markdown_in_the_composer_maps_the_caret_and_round_trips_edits_undo_and_send() {
    let features = ["edit", "undo", "send"];
    let run = |omp_role: bool| -> (Vec<Value>, Vec<(f32, f32)>, Vec<String>) {
        let mut h = omp(
            Default::default(),
            &features,
            json!([]),
            composer(DRAFT, omp_role),
        );
        h.frame_ops("s1", 2, json!([["focus", "ed.in"]]));
        h.frame();
        let mut carets = Vec::new();
        let mut sels = Vec::new();
        let caret = |h: &Harness| {
            // Relative to the field's text: the composer's frame pads the field.
            let c = one(h, h.el("s1", "ed.in"), "sf-caret");
            let b = h.doc.border_box(c).unwrap();
            let t = h.doc.border_box(field_text_el(h)).unwrap();
            (b.x - t.x, b.y - t.y)
        };
        let before = h.sent.len();
        // Caret positions as the program moves it, over markers and styled text.
        for (s, at) in [(3u64, 0u64), (4, 9), (5, 14), (6, 25), (7, 52)] {
            h.frame_ops("s1", s, json!([["set", "ed.in", {"cursor": at}]]));
            h.frame();
            carets.push(caret(&h));
        }
        // A click places the caret natively: the offset the program hears.
        let t = field_text_el(&h);
        let r = h.doc.border_box(t).unwrap();
        for (dx, dy) in [(5.5, 1.5), (13.5, 1.5), (30.5, 1.5)] {
            let out = h.view.click(
                &mut h.doc,
                &h.pane,
                PointerEvent {
                    x: r.x + dx * 8.0,
                    y: r.y + dy * 17.0,
                    clicks: 1,
                    ..Default::default()
                },
            );
            h.view.layout(&mut h.doc);
            deliver(&mut h, out);
        }
        // A selection across markers, replaced by typing; a delete; undo; send.
        h.view.select(&mut h.doc, &h.pane, 11, 20);
        h.frame();
        let sel = h.find_class(h.el("s1", "ed.in"), "sf-ed-sel");
        sels.push(sel.iter().map(|e| h.text(*e)).collect::<String>());
        let out = h.view.type_text(&mut h.doc, &h.pane, "*x*");
        deliver(&mut h, vec![out]);
        h.view.select(&mut h.doc, &h.pane, 0, 2);
        let out = h.view.delete_selection(&mut h.doc, &h.pane);
        deliver(&mut h, vec![out]);
        let out = h.view.undo(&h.pane);
        deliver(&mut h, vec![out]);
        let out = h.view.send(&h.pane, "ed.in", DRAFT).unwrap();
        deliver(&mut h, vec![out]);
        (h.sent[before..].to_vec(), carets, sels)
    };
    let (md_events, md_carets, md_sels) = run(true);
    let (plain_events, plain_carets, plain_sels) = run(false);
    assert_eq!(md_events, plain_events);
    assert_eq!(md_sels, plain_sels);
    assert_eq!(md_sels, ["**the** `"]);
    for (a, b) in md_carets.iter().zip(&plain_carets) {
        assert!(
            (a.0 - b.0).abs() < 0.01 && (a.1 - b.1).abs() < 0.01,
            "{md_carets:?} vs {plain_carets:?}"
        );
    }
    let kinds: Vec<&str> = md_events
        .iter()
        .map(|e| e["ev"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        kinds,
        ["edit", "edit", "edit", "edit", "edit", "undo", "send"]
    );
    assert_eq!(md_events[3]["text"], "*x*");
    assert_eq!(md_events[6]["text"], DRAFT);
}

// ---------------------------------------------------------------------------------------------
// The folded composer's caption (book:11584, §9 phase B)

#[test]
fn the_unfocused_composer_folds_to_a_caption_naming_the_session_and_the_draft() {
    let mut h = omp(Default::default(), &[], json!([]), composer("", true));
    let col = h.el("s1", "ed");
    let cap = h.doc.children(col)[0];
    assert!(
        has(&h, cap, "sf-deco") && has(&h, cap, "sf-caption"),
        "first child"
    );
    assert!(!has(&h, cap, "sf-drafted"));
    assert_eq!(h.text(one(&h, cap, "sf-caption-name")), "Fix the parser");
    assert!(h.find_class(cap, "sf-caption-draft").is_empty());
    let line = h.el("s1", "ed.line");
    assert!(!shown(&h, cap), "focused: hidden");
    assert!(shown(&h, line));
    // Unfocused: the caption stands in for the field's line.
    h.view.set_focused(&mut h.doc, &h.pane, false);
    h.frame();
    assert!(shown(&h, cap));
    assert!(!shown(&h, line));
    let bc = h.doc.border_box(cap).unwrap();
    assert!((bc.h - 17.0).abs() < 0.5, "one row: {bc:?}");
    // A draft: the caption follows it, on one line, and dims the name.
    h.frame_ops(
        "s1",
        2,
        json!([["set", "ed.in", {"text": "rename the\ncolumns", "cursor": 3}]]),
    );
    h.frame();
    let cap = h.doc.children(col)[0];
    assert!(has(&h, cap, "sf-drafted"));
    assert_eq!(
        h.text(one(&h, cap, "sf-caption-draft")),
        "rename the columns"
    );
    let name = one(&h, cap, "sf-caption-name");
    let draft = one(&h, cap, "sf-caption-draft");
    assert_eq!(
        h.doc.computed_style(name).unwrap().color,
        h.doc.computed_style(cap).unwrap().color,
        "the name dims to the caption's color"
    );
    assert!(h.doc.border_box(draft).unwrap().x > h.doc.border_box(name).unwrap().x);
    // Focused again: the field is back.
    h.view.set_focused(&mut h.doc, &h.pane, true);
    h.frame();
    assert!(!shown(&h, cap));
    assert!(shown(&h, line));
    // Only omp's composer has one.
    let p = omp(Default::default(), &[], json!([]), composer("x", false));
    assert!(p.find_class(p.el("s1", "ed"), "sf-caption").is_empty());
}

// ---------------------------------------------------------------------------------------------
// The summary on a chain of thoughts (book:11583)

fn thought(id: &str, took: Option<u64>) -> Value {
    let mut p = json!({"role": "omp.thinking", "collapsible": true, "collapsed": true, "head": format!("{id}/h")});
    if let Some(t) = took {
        p["took"] = json!(t);
    }
    json!({"id": id, "k": "section", "p": p, "c": [
        {"id": format!("{id}/h"), "k": "text", "p": {"text": "Thought"}},
        {"id": format!("{id}/b"), "k": "md", "p": {"text": "hmm"}}
    ]})
}

fn sums(h: &Harness, id: &str) -> Vec<String> {
    let head = one(h, h.el("s1", id), "sf-section-head");
    h.find_class(head, "sf-thought-sum")
        .into_iter()
        .map(|e| h.text(e))
        .collect()
}

#[test]
fn the_first_head_of_a_chain_of_thoughts_carries_its_summary() {
    let main = json!([{"id": "a", "k": "col", "p": {"role": "omp.assistant"}, "c": [
        thought("a.k0", Some(1200)), thought("a.k1", Some(3400)),
        {"id": "a.t", "k": "md", "p": {"text": "Done."}},
        thought("a.k2", Some(500))
    ]}]);
    let mut h = omp(Default::default(), &[], main, json!([]));
    assert_eq!(sums(&h, "a.k0"), ["2 thoughts · 4.6s"]);
    assert!(sums(&h, "a.k1").is_empty());
    assert!(sums(&h, "a.k2").is_empty(), "a lone thought has none");
    let s = one(
        &h,
        one(&h, h.el("s1", "a.k0"), "sf-section-head"),
        "sf-thought-sum",
    );
    assert!(has(&h, s, "sf-deco"));
    let left = format!("{:?}", h.doc.computed_style(s).unwrap().margin[3]);
    assert_eq!(left, "Length(Px(6.0))", "omp-transcript.css places it");
    // A third joins the chain; a took changes; the first one redraws: one summary, current.
    h.frame_ops(
        "s1",
        2,
        json!([["add", "a.k1b", "a", "a.t", thought("a.k1b", None)],
               ["set", "a.k1", {"took": 4400}],
               ["set", "a.k0", {"collapsed": false}]]),
    );
    h.frame();
    assert_eq!(sums(&h, "a.k0"), ["3 thoughts · 5.6s"]);
    assert!(sums(&h, "a.k1").is_empty() && sums(&h, "a.k1b").is_empty());
    // The chain breaks down to one: no summary.
    h.frame_ops("s1", 3, json!([["del", "a.k1"], ["del", "a.k1b"]]));
    h.frame();
    assert!(sums(&h, "a.k0").is_empty());
}

// ---------------------------------------------------------------------------------------------
// The work fold (book:9939-9945)

fn tool(id: &str, title: &str, took: Option<u64>) -> Value {
    let mut p = json!({"name": title.to_lowercase(), "title": title, "status": "done"});
    if let Some(t) = took {
        p["took"] = json!(t);
    }
    json!({"id": id, "k": "tool", "p": p})
}

fn fold_on() -> ViewSettings {
    ViewSettings {
        fold_work: true,
        ..Default::default()
    }
}

fn heads(h: &Harness) -> Vec<NodeId> {
    heads_in(h, "s1")
}

fn heads_in(h: &Harness, sf: &str) -> Vec<NodeId> {
    let k = h
        .pane
        .surfaces()
        .iter()
        .rev()
        .find(|s| s.id() == sf)
        .unwrap()
        .key();
    let main = h.view.region(k, auk_views::Region::Main).unwrap();
    h.find_class(main, "sf-work-head")
}

fn work_main() -> Value {
    json!([
        {"id": "u", "k": "text", "p": {"text": "Deploy it"}},
        tool("t1", "Deploy", Some(10_000)),
        {"id": "void", "k": "col", "p": {"role": "omp.assistant"}},
        tool("t2", "Deploy", Some(2_300)),
        {"id": "th", "k": "col", "p": {"role": "omp.assistant"}, "c": [thought("th.k0", Some(1_000))]},
        tool("t3", "Test", None),
        {"id": "ans", "k": "md", "p": {"text": "Deployed."}},
        tool("t4", "Read", Some(100)),
        {"id": "nest", "k": "col", "c": [tool("n1", "Read", None), tool("n2", "Read", None)]},
        tool("t5", "Read", Some(200)),
        tool("t6", "Grep", Some(300))
    ])
}

#[test]
fn work_runs_fold_under_one_head_only_with_the_setting() {
    // Off by default: nothing changes.
    let h = omp(Default::default(), &[], work_main(), json!([]));
    assert!(heads(&h).is_empty());
    assert!(!has(&h, h.el("s1", "t1"), "sf-work"));

    let mut h = omp(fold_on(), &[], work_main(), json!([]));
    let hs = heads(&h);
    assert_eq!(
        hs.len(),
        2,
        "t4 stands alone: nest is not work; t5 and t6 end main"
    );
    assert_eq!(
        h.text(hs[1])
            .trim_start_matches(|c: char| !c.is_ascii_alphabetic()),
        "Worked for 0.5s · Read · Grep"
    );
    assert_eq!(
        h.attr(hs[1], "aria-expanded").as_deref(),
        Some("true"),
        "still growing: open"
    );
    let head = hs[0];
    assert!(has(&h, head, "sf-deco"));
    assert_eq!(
        h.text(head)
            .trim_start_matches(|c: char| !c.is_ascii_alphabetic()),
        "Worked for 13.3s · Deploy ×2 · Test"
    );
    // The head sits before the run's first block; the run, followed by the answer, is folded.
    let main = h.doc.parent(head).unwrap();
    let kids = h.doc.children(main).to_vec();
    let at = kids.iter().position(|&k| k == head).unwrap();
    assert_eq!(kids[at + 1], h.el("s1", "t1"));
    assert_eq!(h.attr(head, "aria-expanded").as_deref(), Some("false"));
    for id in ["t1", "t2", "th", "t3"] {
        let el = h.el("s1", id);
        assert!(has(&h, el, "sf-work") && has(&h, el, "sf-folded"), "{id}");
        assert!(!shown(&h, el), "{id} folded away");
    }
    for id in ["u", "ans", "t4", "nest", "n1", "n2", "void"] {
        assert!(!has(&h, h.el("s1", id), "sf-work"), "{id}");
    }
    assert!(shown(&h, head));

    // Without nest, t4 joins the run at the end of main.
    h.frame_ops("s1", 2, json!([["del", "nest"]]));
    h.frame();
    let hs = heads(&h);
    assert_eq!(hs.len(), 2);
    let tail = hs[1];
    assert_eq!(
        h.text(tail)
            .trim_start_matches(|c: char| !c.is_ascii_alphabetic()),
        "Worked for 0.6s · Read ×2 · Grep"
    );
    assert_eq!(h.attr(tail, "aria-expanded").as_deref(), Some("true"));
    for id in ["t4", "t5", "t6"] {
        let el = h.el("s1", id);
        assert!(has(&h, el, "sf-work") && !has(&h, el, "sf-folded"), "{id}");
        assert!(shown(&h, el));
    }
    // A prop change on a block keeps its classes; a took change updates the head.
    h.frame_ops("s1", 3, json!([["set", "t5", {"took": 1200}]]));
    h.frame();
    assert!(has(&h, h.el("s1", "t5"), "sf-work"));
    assert!(h.text(heads(&h)[1]).contains("Worked for 1.6s"));
    // Another block follows: the run folds.
    h.frame_ops(
        "s1",
        4,
        json!([["add", "ans2", "main", null, {"id": "ans2", "k": "md", "p": {"text": "All read."}}]]),
    );
    h.frame();
    assert!(has(&h, h.el("s1", "t6"), "sf-folded"));
    // A click on a head unfolds its run, locally (no event); again folds it.
    let head = heads(&h)[0];
    let sent = h.sent.len();
    assert_eq!(click(&mut h, head), vec![Outcome::Redraw]);
    h.frame();
    assert_eq!(h.sent.len(), sent, "no event");
    let head = heads(&h)[0];
    assert_eq!(h.attr(head, "aria-expanded").as_deref(), Some("true"));
    assert!(shown(&h, h.el("s1", "t2")));
    assert!(!has(&h, h.el("s1", "t2"), "sf-folded"));
    // The user's choice holds through more frames.
    h.frame_ops("s1", 5, json!([["set", "t2", {"took": 2400}]]));
    h.frame();
    assert!(!has(&h, h.el("s1", "t2"), "sf-folded"));
    assert!(has(&h, h.el("s1", "t2"), "sf-work"));
    let head = heads(&h)[0];
    click(&mut h, head);
    h.frame();
    assert!(has(&h, h.el("s1", "t2"), "sf-folded"));

    // The setting goes off: heads and classes go.
    let off = ViewSettings::default();
    h.view.set_settings(&mut h.doc, &h.pane, off);
    h.frame();
    assert!(heads(&h).is_empty());
    for id in ["t1", "t2", "t5"] {
        let el = h.el("s1", id);
        assert!(!has(&h, el, "sf-work") && !has(&h, el, "sf-folded"), "{id}");
        assert!(shown(&h, el));
    }
}

/// omp's recorded session: its read, glob and thinking-only messages fold once its answer
/// follows; empty assistant messages neither join nor break the run.
#[test]
fn omps_recorded_session_folds_its_tool_work() {
    let mut first = true;
    let h = replay("omp-session.jsonl", |h, _| {
        if first {
            first = false;
            h.view.set_settings(&mut h.doc, &h.pane, fold_on());
        }
    });
    let texts: Vec<String> = heads_in(&h, "s:1")
        .iter()
        .map(|&e| {
            h.text(e)
                .trim_start_matches(|c: char| !c.is_ascii_alphabetic())
                .to_owned()
        })
        .collect();
    assert!(
        texts
            .iter()
            .any(|t| t == "Worked for 3.6s · Read ×3 · Glob"),
        "{texts:?}"
    );
    for id in ["x", "y", "10", "11", "12"] {
        let el = h.el("s:1", id);
        assert!(has(&h, el, "sf-folded"), "{id}: {}", h.class(el));
    }
    assert!(!has(&h, h.el("s:1", "13"), "sf-work"), "the answer");
}

// ---------------------------------------------------------------------------------------------
// omp's app mark (book:11585)

#[test]
fn the_omp_builtin_mark_sits_in_a_brand_decoration() {
    let h = omp(
        Default::default(),
        &[],
        json!([{"id": "logo", "k": "image", "p": {"builtin": "omp", "alt": "omp", "w": 128, "role": "omp.welcome.logo"}}]),
        json!([]),
    );
    let el = h.el("s1", "logo");
    let brand = one(&h, el, "sf-brand");
    assert!(has(&h, brand, "sf-deco"));
    assert_eq!(h.doc.parent(brand), Some(el));
    let svg = one(&h, brand, "sf-img-svg");
    assert!(h.doc.image(svg).is_some());
    let b = h.doc.border_box(svg).unwrap();
    assert!((b.w - 128.0).abs() < 0.01, "{b:?}");
}

// ---------------------------------------------------------------------------------------------
// P6: picker rows arrive (book:7938-7940)

fn items(ids: &[&str]) -> Value {
    Value::Array(
        ids.iter()
            .map(|i| json!({"id": i, "label": format!("Item {i}")}))
            .collect(),
    )
}

fn open_picker(settings: ViewSettings, ids: &[&str]) -> Harness {
    let mut h = Harness::with(Config::default(), settings);
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "t", "k": "text", "p": {"text": "hi"}}]),
    );
    h.frame_ops(
        "s1",
        2,
        json!([["add", "layer", "s1", null, {"id": "layer", "k": "col", "c": [
            {"id": "pk", "k": "picker", "p": {"items": items(ids), "selected": ids[0], "size": "md"}}
        ]}]]),
    );
    h.frame();
    h
}

fn row(h: &Harness, id: &str) -> NodeId {
    h.find_class(h.el("s1", "pk"), "pk-row")
        .into_iter()
        .find(|r| h.attr(*r, "data-v").as_deref() == Some(id))
        .unwrap_or_else(|| panic!("no row {id}"))
}

fn arrival(h: &Harness, el: NodeId) -> Option<(String, String)> {
    let class = h.class(el);
    let a = class
        .split(' ')
        .find(|c| *c == "arr-a" || *c == "arr-b")?
        .to_owned();
    Some((a, style(h, el)))
}

#[test]
fn picker_rows_that_arrive_stagger_in_and_never_replay() {
    let ids: Vec<String> = (0..10).map(|i| format!("i{i}")).collect();
    let idr: Vec<&str> = ids.iter().map(String::as_str).collect();
    let mut h = open_picker(Default::default(), &idr[..3]);
    // First show: every row arrives, 24ms apart.
    for (i, id) in idr[..3].iter().enumerate() {
        let r = row(&h, id);
        let (a, style) = arrival(&h, r).expect("arriving");
        assert_eq!(a, "arr-a");
        assert!(style.contains(&format!("--i: {i}")), "{style}");
        let st = h.doc.computed_style(r).unwrap();
        let anim = &st.animations.as_ref().expect("animated")[0];
        assert_eq!(&*anim.name, "pk-row-in");
        assert!((anim.duration - 0.18).abs() < 1e-4);
        assert!(
            (anim.delay - 0.024 * i as f32).abs() < 1e-4,
            "{}",
            anim.delay
        );
    }
    h.now += 1000.0;
    h.frame();
    let kept = row(&h, "i1");
    // Seven more arrive: the first new ones take arr-b; past eight, none stagger.
    h.frame_ops("s1", 3, json!([["set", "pk", {"items": items(&idr)}]]));
    h.frame();
    assert_eq!(row(&h, "i1"), kept, "a row that stays keeps its element");
    assert_eq!(
        arrival(&h, kept).unwrap().0,
        "arr-a",
        "and its finished arrival"
    );
    for (i, id) in idr[3..].iter().enumerate() {
        let r = row(&h, id);
        let (a, style) = arrival(&h, r).expect("arriving");
        assert_eq!(a, "arr-b");
        assert!(style.contains(&format!("--i: {i}")), "{style}");
    }
    // The kept row's finished animation doesn't replay on the restyle.
    let op = h.doc.computed_style(kept).unwrap().opacity;
    assert_eq!(op, 1.0);
    // A selection change rebuilds two rows: they don't arrive.
    h.now += 1000.0;
    h.frame();
    h.frame_ops("s1", 4, json!([["set", "pk", {"selected": "i2"}]]));
    h.frame();
    assert!(arrival(&h, row(&h, "i2")).is_none());
    assert!(arrival(&h, row(&h, "i0")).is_none());
    assert!(!h.doc.animating(), "nothing moves");

    // More than eight at once: the first eight stagger, the rest don't.
    let h = open_picker(Default::default(), &idr);
    let arriving = idr
        .iter()
        .filter(|id| arrival(&h, row(&h, id)).is_some())
        .count();
    assert_eq!(arriving, 8);

    // Under Reduce Motion (.sf-still) nothing moves.
    let still = ViewSettings {
        reduce_motion: true,
        ..Default::default()
    };
    let h = open_picker(still, &idr[..3]);
    let r = row(&h, "i0");
    assert!(arrival(&h, r).is_some());
    let st = h.doc.computed_style(r).unwrap();
    assert!(
        st.animations.as_ref().is_none_or(|a| a.is_empty()),
        "{:?}",
        st.animations
    );
    assert_eq!(st.opacity, 1.0);
}
