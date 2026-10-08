//! Every T0 and T1 kind renders through the TSP engine into the views and the UI engine, with its
//! props, children and terminal-owned behavior (research/elements.md §2). The surface is shared
//! with the `layout` suite (`crates/conformance/probes/kinds.json`).

mod common;

use auk_views::KINDS;
use common::Harness;
use serde_json::json;

fn harness() -> Harness {
    let mut h = Harness::new();
    h.hello(&["edit", "undo", "send"]);
    let (main, frame2) = auk_conformance::surfaces::kinds_surface();
    h.surface("s1", "inline", main);
    h.apc("f", &json!({"sf": "s1", "s": 2, "ops": frame2}).to_string());
    h.frame();
    h
}

#[test]
fn advertised_kinds_are_t0_and_t1() {
    let mut want: Vec<&str> = auk_tsp::T0_KINDS
        .iter()
        .chain(&auk_tsp::T1_KINDS)
        .copied()
        .collect();
    let mut have: Vec<&str> = KINDS.to_vec();
    want.sort_unstable();
    have.sort_unstable();
    assert_eq!(have, want);
    assert_eq!(auk_tsp::Config::default().kinds.len(), KINDS.len());
}

#[test]
fn every_t0_t1_kind_renders() {
    let h = harness();
    // The only error is the deliberate unknown `el` tag (el_attribute_allowlist_and_unknown_tags).
    let errors: Vec<_> = h
        .errors()
        .into_iter()
        .filter(|e| e["id"] != "el2")
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    let doc = h.pane.surfaces()[0].document();
    let mut used = std::collections::BTreeSet::new();
    let mut stack = vec![doc.root()];
    while let Some(n) = stack.pop() {
        used.insert(n.kind().to_owned());
        stack.extend(n.children());
    }
    for k in KINDS {
        assert!(used.contains(k), "the test covers {k}");
    }
    // Each node is .sf.sf-<kind> with data-id; inline kinds are spans.
    let mut stack: Vec<_> = doc.region("main").unwrap().children().collect();
    while let Some(n) = stack.pop() {
        let el = h.el("s1", n.id());
        let class = h.class(el);
        assert!(
            class.starts_with(&format!("sf sf-{}", n.kind())),
            "{} {class}",
            n.id()
        );
        assert_eq!(h.attr(el, "data-id").as_deref(), Some(n.id()));
        let tag = h.doc.node(el).element().unwrap().tag.as_str().to_owned();
        if matches!(
            n.kind(),
            "badge" | "kbd" | "icon" | "spinner" | "shimmer" | "elapsed" | "rate"
        ) {
            assert_eq!(tag, "span", "{}", n.kind());
        }
        stack.extend(n.children());
    }
}

#[test]
fn layout_kinds() {
    let h = harness();
    let c1 = h.el("s1", "c1");
    let class = h.class(c1);
    assert!(
        class.contains("gap-sm") && class.contains("al-start") && class.contains("js-between"),
        "{class}"
    );
    assert!(h.class(h.el("s1", "row0")).contains("sf-void"));
    let r1 = h.el("s1", "r1");
    assert!(h.class(r1).contains("wrap"));
    assert_eq!(h.view.written().style(r1), Some("flex-grow: 1;"));
    // card: head child drawn in the head, role icon, error chip, chevron.
    let card = h.el("s1", "card1");
    let head = h.find_class(card, "sf-card-head")[0];
    assert_eq!(h.attr(head, "role").as_deref(), Some("button"));
    assert_eq!(h.attr(head, "aria-expanded").as_deref(), Some("true"));
    let head_child = h.el("s1", "card1.h");
    assert_eq!(h.doc.parent(h.doc.parent(head_child).unwrap()), Some(head));
    assert_eq!(
        h.text(h.find_class(card, "sf-chip")[0]),
        auk_views::icons::glyph("x").unwrap().to_string() + "Failed"
    );
    assert!(
        !h.class(h.find_class(card, "sf-card-ic")[0])
            .contains("sf-hidden"),
        "lens.git → branch"
    );
    assert_eq!(h.attr(card, "data-tone").as_deref(), Some("error"));
    let body = h.find_class(card, "sf-card-body")[0];
    assert_eq!(h.doc.element_children(body), vec![h.el("s1", "card1.b")]);
    // section starts collapsed.
    assert!(h.class(h.el("s1", "sec1")).contains("collapsed"));
    // rule, spacer
    let rule = h.el("s1", "rule1");
    assert_eq!(h.attr(rule, "role").as_deref(), Some("separator"));
    assert_eq!(h.text(rule), "Older");
    assert!(h.class(h.el("s1", "spc1")).contains("sz-lg"));
    assert_eq!(h.doc.border_box(h.el("s1", "spc1")).unwrap().h, 20.0);
}

#[test]
fn text_and_span_kinds() {
    let h = harness();
    let t = h.el("s1", "card1.h");
    let spans: Vec<String> = h
        .doc
        .element_children(t)
        .iter()
        .map(|e| h.class(*e))
        .collect();
    assert_eq!(
        spans,
        vec!["sf-t-strong", "sf-t-path", "sf-t-key", "sf-link"]
    );
    let path = h.doc.element_children(t)[1];
    assert_eq!(h.text(h.doc.element_children(path)[0]), "src/");
    let key = h.doc.element_children(t)[2];
    assert_eq!(h.text(key), "⌃R");
    let link = h.doc.element_children(t)[3];
    assert_eq!(
        h.attr(link, "data-href").as_deref(),
        Some("https://example.com")
    );
    // badge, icon, kbd
    assert_eq!(h.text(h.el("s1", "b1")), "ok");
    let ic = h.el("s1", "i1");
    assert_eq!(
        h.text(ic),
        auk_views::icons::glyph("x").unwrap().to_string()
    );
    assert_eq!(h.attr(ic, "aria-hidden").as_deref(), Some("true"));
    let k = h.el("s1", "k1");
    assert_eq!(h.text(k), "⌃O");
    assert_eq!(h.attr(k, "aria-label").as_deref(), Some("Control O"));
    // kv: key column as wide as the widest key in cells.
    let kv = h.el("s1", "sec1.kv");
    assert_eq!(
        h.view.written().style(kv),
        Some(
            "grid-template-columns: calc(7*var(--sf-cw)) minmax(0, 1fr); \
             --sf-kv-cols: calc(7*var(--sf-cw)) minmax(0, 1fr);"
        )
    );
    // Shimmer effect spans and Nerd Font runs.
    let tx2 = h.el("s1", "tx2");
    assert_eq!(h.find_class(tx2, "sf-sh").len(), 4);
    let icon = h.find_class(tx2, "sf-t-icon");
    assert_eq!(icon.len(), 1);
    assert!(h.class(icon[0]).contains("sf-t-muted"));
    assert!(h.class(tx2).contains("clamp"));
    // Middle truncation after layout (16 cells).
    let tx1 = h.el("s1", "tx1");
    let shown = h.text(tx1);
    assert!(
        shown.starts_with('…') && shown.ends_with("file.rs") && shown.chars().count() <= 16,
        "{shown}"
    );
    assert_eq!(
        h.attr(tx1, "title").as_deref(),
        Some("a/very/long/path/to/some/file.rs")
    );
}

#[test]
fn rich_kinds() {
    let h = harness();
    // md: per-block elements, GFM, callout, highlight, literal HTML.
    let md = h.el("s1", "md1");
    let blocks = h.doc.element_children(md);
    assert_eq!(blocks.len(), 7, "{}", h.text(md));
    assert!(blocks.iter().all(|b| h.class(*b) == "md"));
    assert_eq!(h.find_class(md, "md-task").len(), 2);
    assert_eq!(h.find_class(md, "md-task on").len(), 0);
    assert_eq!(h.find_class(md, "callout").len(), 1);
    assert!(h.class(h.find_class(md, "callout")[0]).contains("warning"));
    assert_eq!(h.find_class(md, "md-sev").len(), 1);
    assert_eq!(h.find_class(md, "md-swatch").len(), 1);
    assert_eq!(h.find_class(md, "tk-keyword").len(), 1);
    assert_eq!(h.text(h.find_class(md, "md-html")[0]), "<div>raw</div>");
    let links: Vec<String> = h
        .find_class(md, "sf-link")
        .iter()
        .filter_map(|l| h.attr(*l, "data-href"))
        .collect();
    assert_eq!(links, vec!["https://example.com", "src/lib.rs:12"]);
    // code: header, numbers from `start`, marked line with an emphasized range.
    let code = h.el("s1", "code1");
    assert!(h.class(code).contains("numbers"));
    assert_eq!(h.text(h.find_class(code, "sf-code-path")[0]), "src/main.rs");
    let lines = h.find_class(code, "sf-ln");
    assert_eq!(lines.len(), 3);
    assert_eq!(h.text(h.find_class(lines[0], "sf-gut")[0]), "10");
    assert!(h.class(lines[1]).contains("mk"));
    assert_eq!(h.attr(lines[1], "data-tone").as_deref(), Some("warning"));
    assert_eq!(h.text(h.find_class(lines[1], "sf-em")[0]), "x");
    // diff: fold separators, numbers, word emphasis; split mode.
    let d = h.el("s1", "diff1");
    let seps = h.find_class(d, "sf-dsep");
    assert_eq!(seps.len(), 1, "a gap between the hunks");
    assert!(h.text(seps[0]).starts_with("⋯ 16 unchanged lines"));
    assert_eq!(h.find_class(d, "del").len(), 2);
    assert_eq!(h.find_class(d, "add").len(), 2);
    assert_eq!(
        h.text(h.find_class(h.find_class(d, "del")[0], "sf-em")[0]),
        "1"
    );
    let d2 = h.el("s1", "diff2");
    assert!(h.class(d2).contains("split"));
    assert_eq!(h.find_class(d2, "sf-dr").len(), 1);
    // ansi: bold, OSC 8 link, preview clamp with "N more lines".
    let an = h.el("s1", "an1");
    let lns = h.find_class(an, "ln");
    assert_eq!(lns.len(), 2);
    assert_eq!(h.find_class(an, "b").len(), 1);
    let l = h.find_class(an, "sf-link");
    assert_eq!(h.attr(l[0], "data-href").as_deref(), Some("https://x.y"));
    let more = h.find_class(an, "sf-ansi-more")[0];
    assert_eq!(h.text(more), "2 more lines");
    // rows: exact width, fallback marker.
    let rows = h.el("s1", "rows1");
    assert_eq!(h.text(h.find_class(rows, "sf-rows-mark")[0]), "fallback");
    assert_eq!(h.find_class(rows, "ln").len(), 2);
}

#[test]
fn field_kinds() {
    let h = harness();
    let ed = h.el("s1", "ed1");
    let class = h.class(ed);
    assert!(
        class.contains("focused") && class.contains("capped"),
        "{class}"
    );
    assert_eq!(h.attr(ed, "data-mode").as_deref(), Some("insert"));
    assert_eq!(h.attr(ed, "aria-multiline").as_deref(), Some("true"));
    assert_eq!(h.text(h.find_class(ed, "sf-ed-sel")[0]), "hello");
    assert_eq!(h.text(h.find_class(ed, "sf-t-typo")[0]), "world");
    assert_eq!(h.find_class(ed, "sf-caret").len(), 1);
    assert_eq!(h.text(h.find_class(ed, "sf-ed-mode")[0]), "INSERT");
    let inp = h.el("s1", "in1");
    assert_eq!(h.attr(inp, "aria-multiline").as_deref(), Some("false"));
    assert_eq!(h.attr(inp, "aria-label").as_deref(), Some("Filter"));
    assert_eq!(h.text(h.find_class(inp, "sf-ed-ph")[0]), "Filter");
}

#[test]
fn bar_list_and_overlay_kinds() {
    let h = harness();
    let st = h.el("s1", "st1");
    assert_eq!(h.attr(st, "role").as_deref(), Some("group"));
    let l = h.find_class(st, "sf-st-l")[0];
    let r = h.find_class(st, "sf-st-r")[0];
    assert_eq!(h.doc.element_children(l), vec![h.el("s1", "seg1")]);
    assert_eq!(h.doc.element_children(r), vec![h.el("s1", "seg2")]);
    assert!(h.class(h.el("s1", "seg1")).contains("sf-act"));
    // list: selection, filter marks (substring first, else fuzzy), max scroller.
    let ls = h.el("s1", "ls1");
    assert_eq!(h.attr(ls, "role").as_deref(), Some("listbox"));
    let it1 = h.el("s1", "it1");
    let it2 = h.el("s1", "it2");
    assert!(h.class(it2).contains("sel"));
    assert_eq!(h.attr(it2, "aria-selected").as_deref(), Some("true"));
    let marks: Vec<String> = h
        .find_class(it1, "sf-mark")
        .iter()
        .map(|m| h.text(*m))
        .collect();
    assert_eq!(marks, vec!["l", "g", "n"]);
    assert_eq!(h.text(h.find_class(it1, "kbd")[0]), "⏎");
    assert!(!h.find_class(ls, "max").is_empty());
    // tabs
    let tabs = h.el("s1", "tb1");
    let on = h.find_class(tabs, "on");
    assert_eq!(h.attr(on[0], "data-tab").as_deref(), Some("commits"));
    // table: grid, meter cell, data-key.
    let t = h.el("s1", "tab1");
    assert_eq!(h.find_class(t, "sf-tbl-h").len(), 3);
    assert_eq!(h.find_class(t, "sf-tbl-c").len(), 6);
    let mt = h.find_class(t, "sf-meter")[0];
    assert_eq!(h.attr(mt, "data-level").as_deref(), Some("warn"));
    // tree: open item shows its child; closed one mounts none.
    let tr = h.el("s1", "tr1");
    assert_eq!(h.attr(tr, "role").as_deref(), Some("tree"));
    let items = h.find_class(tr, "sf-tree-node");
    assert_eq!(items.len(), 3);
    assert_eq!(h.attr(items[0], "aria-expanded").as_deref(), Some("true"));
    // overlay: modal dim, dialog, anchored to a node (placed after layout).
    let ov = h.el("s1", "ov1");
    assert!(h.class(ov).contains("modal") && h.class(ov).contains("at-node"));
    assert_eq!(h.find_class(ov, "sf-ov-dim").len(), 1);
    let card = h.find_class(ov, "sf-ov-card")[0];
    assert!(
        h.view.written().style(card).unwrap_or("").contains("top:"),
        "anchored"
    );
    // toast: the node is hidden; the pane's toast layer shows it.
    let toast = h.el("s1", "toast1");
    assert_eq!(h.attr(toast, "aria-hidden").as_deref(), Some("true"));
    let layer = h.find_class(h.view.element(), "toast");
    assert_eq!(layer.len(), 1);
    assert!(h.class(layer[0]).contains("info"));
    assert_eq!(
        h.text(layer[0])
            .trim_start_matches(|c: char| !c.is_ascii_alphabetic()),
        "Saveddisk ok"
    );
}

#[test]
fn motion_kinds() {
    let h = harness();
    let sp = h.el("s1", "sp1");
    assert_eq!(h.attr(sp, "aria-label").as_deref(), Some("Fetching"));
    assert_eq!(h.text(h.find_class(sp, "lbl")[0]), "Fetching");
    assert_eq!(h.text(h.el("s1", "e1")), "4m 05s");
    assert_eq!(h.text(h.el("s1", "ra1")), "42.2 tok/s");
    let sh = h.el("s1", "sh1");
    assert!(h.class(sh).contains("kitt"));
    assert_eq!(h.find_class(sh, "sf-sh").len(), 8);
    let p1 = h.el("s1", "pr1");
    assert_eq!(h.attr(p1, "aria-valuenow").as_deref(), Some("50"));
    let p2 = h.el("s1", "pr2");
    assert!(h.class(p2).contains("indeterminate"));
    let m1 = h.el("s1", "mt1");
    assert_eq!(h.attr(m1, "data-level").as_deref(), Some("warn"));
    assert_eq!(h.text(h.find_class(m1, "sf-mt-total")[0]), "200K");
    assert_eq!(h.find_class(m1, "sf-mt-mark").len(), 1);
    let m2 = h.el("s1", "mt2");
    assert_eq!(h.find_class(m2, "sf-mt-part").len(), 2);
    let m3 = h.el("s1", "mt3");
    assert_eq!(h.find_class(m3, "on").len(), 2);
}

#[test]
fn agent_kinds() {
    let h = harness();
    let t = h.el("s1", "tool1");
    let class = h.class(t);
    assert!(
        class.contains("st-running") && class.contains("collapsible"),
        "{class}"
    );
    assert_eq!(h.attr(t, "data-tk").as_deref(), Some("command"));
    assert_eq!(h.attr(t, "title"), None, "tool title is content");
    assert_eq!(h.text(h.find_class(t, "sf-tool-title")[0]), "Run");
    let target = h.find_class(t, "sf-tool-target")[0];
    assert_eq!(h.text(h.find_class(target, "cmd")[0]), "cargo");
    assert_eq!(h.find_class(target, "flag").len(), 2);
    assert_eq!(h.text(h.find_class(t, "sf-tool-time")[0]), "1.2s");
    assert_eq!(h.text(h.find_class(t, "sf-tool-intent")[0]), "Testing");
    // Body children of different kinds get separators.
    let md = h.el("s1", "tool1.md");
    assert!(h.class(md).contains("sf-tool-hr"));
    let t2 = h.el("s1", "tool2");
    assert_eq!(h.text(h.find_class(t2, "sf-tool-time")[0]), "0.4s");
    assert!(
        h.class(h.find_class(t2, "sf-tool-time")[0])
            .contains("fast")
    );
    assert_eq!(h.text(h.find_class(t2, "sf-tool-exit")[0]), "exit 1");
    // agent
    let a = h.el("s1", "ag1");
    assert_eq!(h.view.written().style(a), Some("--sf-depth: 1;"));
    assert_eq!(h.text(h.find_class(a, "sf-agent-name")[0]), "runner-3");
    assert!(
        h.class(h.find_class(a, "sf-agent-meter")[0])
            .contains("warn")
    );
    assert_eq!(h.text(h.find_class(a, "cost")[0]), "$0.42");
    assert_eq!(h.text(h.find_class(a, "tokens")[0]), "12.3K");
    let retry = h.find_class(a, "sf-agent-retry")[0];
    assert!(
        h.text(retry).starts_with("Retry 1/3 in 4.0s"),
        "{}",
        h.text(retry)
    );
    assert_eq!(
        h.doc.element_children(h.find_class(a, "sf-agent-kids")[0]),
        vec![h.el("s1", "ag1.k")]
    );
    // checklist modes
    let ck = h.el("s1", "ck1");
    assert_eq!(
        h.attr(ck, "aria-label").as_deref(),
        Some("Todos, 1 of 3 done")
    );
    assert_eq!(h.find_class(ck, "sf-ck-item").len(), 3);
    assert_eq!(h.text(h.find_class(ck, "sf-ck-note")[0]), "waiting on CI");
    let hud = h.el("s1", "ck2");
    assert!(h.class(hud).contains("hud"));
    assert_eq!(h.text(h.find_class(hud, "t")[0]), "Tag");
    let rem = h.el("s1", "ck3");
    assert_eq!(h.text(h.find_class(rem, "t")[0]), "1 todo still open");
}

#[test]
fn el_attribute_allowlist_and_unknown_tags() {
    let mut h = harness();
    let e = h.el("s1", "el1");
    let class = h.class(e);
    assert!(
        class.contains("box") && !class.contains("sf-evil"),
        "{class}"
    );
    assert_eq!(h.attr(e, "data-x").as_deref(), Some("1"));
    assert_eq!(h.attr(e, "aria-hidden").as_deref(), Some("true"));
    assert_eq!(h.attr(e, "role").as_deref(), Some("note"));
    assert_eq!(h.attr(e, "onclick"), None);
    assert_eq!(h.attr(e, "style"), None);
    assert_eq!(h.text(e), "hellobold");
    let u = h.el("s1", "el2");
    assert_eq!(h.doc.node(u).element().unwrap().tag.as_str(), "div");
    // The replica reports the unknown tag to the program at op time (§7.7): once per node
    // version, so a redraw or a change elsewhere sends nothing and a change to the node sends it
    // again.
    let tag_errors = |h: &Harness| -> Vec<serde_json::Value> {
        h.errors()
            .into_iter()
            .filter(|e| e.get("id").is_some())
            .cloned()
            .collect()
    };
    assert_eq!(
        tag_errors(&h),
        vec![json!({"ev": "error", "sf": "s1", "id": "el2", "msg": "unknown el tag marquee"})]
    );
    h.frame();
    h.frame_ops("s1", 3, json!([["set", "el1", {"class": "box two"}]]));
    assert_eq!(
        tag_errors(&h).len(),
        1,
        "a redraw or another node sends nothing"
    );
    h.frame_ops("s1", 4, json!([["set", "el2", {"class": "again"}]]));
    assert_eq!(
        tag_errors(&h).len(),
        2,
        "a change to the node sends it again"
    );
    // An input that is neither a checkbox nor a radio draws as an empty div.
    h.frame_ops(
        "s1",
        5,
        json!([
            ["add", "bad1", "main", null, {"id": "bad1", "k": "el", "p": {"tag": "input", "type": "text", "text": "x"}}],
            ["add", "bad2", "main", null, {"id": "bad2", "k": "el", "p": {"tag": "input"}}],
            ["set", "el2", {"tag": "section"}]
        ]),
    );
    h.frame();
    let msgs: Vec<_> = tag_errors(&h)[2..]
        .iter()
        .map(|e| e["msg"].clone())
        .collect();
    assert_eq!(
        msgs,
        vec![
            json!("unknown el tag input type text"),
            json!("unknown el tag input")
        ]
    );
    let i1 = h.el("s1", "bad1");
    assert_eq!(h.doc.node(i1).element().unwrap().tag.as_str(), "div");
    assert_eq!(h.text(i1), "");
    assert!(!h.class(i1).contains("sf-check"));
    // Programs that don't listen hear nothing.
    h.apc("o", r#"{"id":"quiet","listen":false}"#);
    h.frame_ops(
        "quiet",
        1,
        json!([["add", "main", "quiet", null, {"id": "main", "k": "col", "c": [{"id": "q", "k": "el", "p": {"tag": "blink"}}]}]]),
    );
    assert_eq!(tag_errors(&h).len(), 4);
}

#[test]
fn mark_prop_sets_data_mark_pick_draws_a_bar_and_drop_dims() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "p", "k": "text", "p": {"text": "picked", "mark": "pick"}},
            {"id": "d", "k": "card", "p": {"title": "dropped", "mark": "drop"}},
            {"id": "o", "k": "text", "p": {"text": "other", "mark": "star"}},
            {"id": "n", "k": "text", "p": {"text": "plain"}}
        ]),
    );
    h.frame();
    let (p, d, o, n) = (
        h.el("s1", "p"),
        h.el("s1", "d"),
        h.el("s1", "o"),
        h.el("s1", "n"),
    );
    assert_eq!(h.attr(p, "data-mark").as_deref(), Some("pick"));
    assert_eq!(h.attr(d, "data-mark").as_deref(), Some("drop"));
    assert_eq!(
        h.attr(o, "data-mark").as_deref(),
        Some("star"),
        "other strings only set the attribute"
    );
    assert_eq!(h.attr(n, "data-mark"), None);
    let style = |el| h.doc.computed_style(el).unwrap().clone();
    let (sp, sd, so, sn) = (style(p), style(d), style(o), style(n));
    let bar = |s: &auk_ui::style::ComputedStyle| {
        s.border_style[3] != auk_ui::style::BorderStyle::None && s.border_width[3] > 0.0
    };
    assert!(bar(&sp), "pick: an accent bar on the left");
    assert!(!bar(&sn));
    assert!(sd.opacity < 1.0, "drop dims");
    assert_eq!(sn.opacity, 1.0);
    assert!(!bar(&so));
    assert_eq!(so.opacity, 1.0);
    // Clearing the prop clears the attribute.
    h.frame_ops("s1", 2, json!([["set", "p", {"mark": null}]]));
    h.frame();
    assert_eq!(h.attr(p, "data-mark"), None);
    assert!(!bar(h.doc.computed_style(p).unwrap()));
}

/// §8.5 Markdown (omp's dialect): GFM strikethrough, GFM tables with their alignments drawn as
/// table layout, and math delimiters (inline `$…$` and display `$$…$$`) kept as their TeX.
#[test]
fn md_gfm_strikethrough_tables_and_math_delimiters() {
    let mut h = Harness::new();
    h.hello(&[]);
    let text = "Some ~~old~~ new text.\n\n| Name | Size | Kind |\n| :--- | ---: | :---: |\n| alpha | 10 | dir |\n| beta | 200 | file |\n\nInline $a^2 + b^2$ math.\n\n$$\n\\int_0^1 x\\,dx\n$$\n";
    h.surface("s1", "inline", json!([{"id": "md", "k": "md", "p": {"text": text}}]));
    h.frame();
    let md = h.el("s1", "md");
    let tag = |h: &Harness, e| {
        h.doc
            .node(e)
            .element()
            .map(|el| el.tag.as_str().to_owned())
            .unwrap_or_default()
    };
    let by_tag = |h: &Harness, name: &str| -> Vec<auk_ui::NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![md];
        while let Some(n) = stack.pop() {
            if tag(h, n) == name {
                out.push(n);
            }
            stack.extend(h.doc.children(n).iter().rev().copied());
        }
        out
    };
    // Strikethrough: a `del`, drawn struck through.
    let del = by_tag(&h, "del");
    assert_eq!(del.len(), 1);
    assert_eq!(h.text(del[0]), "old");
    let style = h.doc.computed_style(del[0]).unwrap();
    assert!(
        format!("{:?}", style.text_decoration).to_lowercase().contains("through"),
        "{:?}",
        style.text_decoration
    );
    // The table: a head row and two body rows, alignments as classes, laid out as a table.
    assert_eq!(h.find_class(md, "md-table").len(), 1);
    let th = by_tag(&h, "th");
    let td = by_tag(&h, "td");
    assert_eq!(th.iter().map(|&c| h.text(c)).collect::<Vec<_>>(), ["Name", "Size", "Kind"]);
    assert_eq!(
        td.iter().map(|&c| h.text(c)).collect::<Vec<_>>(),
        ["alpha", "10", "dir", "beta", "200", "file"]
    );
    assert_eq!(by_tag(&h, "thead").len(), 1);
    assert_eq!(by_tag(&h, "tbody").len(), 1);
    assert_eq!(by_tag(&h, "tr").len(), 3);
    assert_eq!(
        th.iter().map(|&c| h.class(c)).collect::<Vec<_>>(),
        ["ta-start", "ta-end", "ta-center"]
    );
    let b = |e| h.doc.border_box(e).unwrap();
    // Cells of a column share an x and a width; cells of a row share a y.
    for col in 0..3 {
        let (head, r1, r2) = (b(th[col]), b(td[col]), b(td[3 + col]));
        assert!((head.x - r1.x).abs() < 0.5 && (r1.x - r2.x).abs() < 0.5, "column {col}");
        assert!((head.w - r1.w).abs() < 0.5 && (r1.w - r2.w).abs() < 0.5, "column {col}");
    }
    assert!((b(td[0]).y - b(td[2]).y).abs() < 0.5 && b(td[3]).y > b(td[0]).y);
    assert!(b(th[1]).x > b(th[0]).x && b(th[2]).x > b(th[1]).x);
    // Right-aligned: the size column's text sits at the cell's end.
    let align = |e| format!("{:?}", h.doc.computed_style(e).unwrap().text_align).to_lowercase();
    assert!(align(td[1]).contains("right") || align(td[1]).contains("end"), "{}", align(td[1]));
    assert!(align(td[2]).contains("center"), "{}", align(td[2]));
    // Math delimiters: the TeX, kept (Math rendering is T2, §8.2), inline and display.
    let inline = h.find_class(md, "math");
    assert_eq!(inline.len(), 1);
    assert_eq!(h.attr(inline[0], "data-tex").as_deref(), Some("a^2 + b^2"));
    assert_eq!(h.text(inline[0]), "a^2 + b^2");
    let display = h.find_class(md, "math-display");
    assert_eq!(display.len(), 1);
    assert_eq!(h.attr(display[0], "data-tex").as_deref().map(str::trim), Some("\\int_0^1 x\\,dx"));
    assert!(!h.text(md).contains('$'), "the delimiters aren't shown");
}

/// §8.5 Diff: with `mode` absent or `auto`, the Edits setting decides (unified by default; with
/// Split, side by side only when wider than 112 cells); an explicit mode wins.
#[test]
fn diff_mode_absent_or_auto_follows_the_edits_setting() {
    let mut h = Harness::new();
    h.hello(&[]);
    let text = "@@ -1,2 +1,2 @@\n-old line\n+new line\n same\n";
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "absent", "k": "diff", "p": {"text": text}},
            {"id": "auto", "k": "diff", "p": {"text": text, "mode": "auto"}},
            {"id": "unified", "k": "diff", "p": {"text": text, "mode": "unified"}},
            {"id": "split", "k": "diff", "p": {"text": text, "mode": "split"}}
        ]),
    );
    h.frame();
    let split = |h: &Harness| {
        ["absent", "auto", "unified", "split"]
            .map(|id| h.class(h.el("s1", id)).split(' ').any(|c| c == "split"))
    };
    // The default setting: unified, at any width.
    for cols in [80, 200] {
        h.view.set_cols(&mut h.doc, &h.pane, cols);
        h.frame();
        assert_eq!(split(&h), [false, false, false, true], "{cols} cols");
    }
    // Split: side by side when wider than 112 cells.
    let settings = auk_views::ViewSettings {
        split_diffs: true,
        ..h.view.settings().clone()
    };
    h.view.set_settings(&mut h.doc, &h.pane, settings);
    for (cols, wide) in [(112, false), (113, true), (200, true), (80, false)] {
        h.view.set_cols(&mut h.doc, &h.pane, cols);
        h.frame();
        assert_eq!(split(&h), [wide, wide, false, true], "{cols} cols");
    }
}
