//! Every T0 and T1 kind renders through the TSP engine into the views and the UI engine, with its
//! props, children and terminal-owned behavior (research/elements.md §2).

#![recursion_limit = "512"]

mod common;

use auk_views::KINDS;
use common::Harness;
use serde_json::json;

fn all_kinds() -> serde_json::Value {
    json!([
        {"id": "c1", "k": "col", "p": {"gap": "sm", "align": "start", "justify": "between"}, "c": [
            {"id": "r1", "k": "row", "p": {"gap": "md", "wrap": true, "grow": 1}, "c": [
                {"id": "b1", "k": "badge", "p": {"text": "ok", "tone": "success"}},
                {"id": "i1", "k": "icon", "p": {"name": "status.error", "tone": "error"}},
                {"id": "k1", "k": "kbd", "p": {"keys": ["ctrl", "o"]}},
                {"id": "sp1", "k": "spinner", "p": {"style": "braille", "label": "Fetching"}},
                {"id": "e1", "k": "elapsed", "p": {"age": 245000}},
                {"id": "ra1", "k": "rate", "p": {"value": 42.25, "unit": "tok/s"}},
                {"id": "sh1", "k": "shimmer", "p": {"text": "Thinking", "mode": "kitt"}}
            ]},
            {"id": "row0", "k": "row"}
        ]},
        {"id": "card1", "k": "card", "p": {"head": "card1.h", "status": "error", "tone": "error", "collapsible": true, "role": "lens.git.log", "key": "build"}, "c": [
            {"id": "card1.h", "k": "text", "p": {"spans": [{"t": "Build ", "s": "strong"}, {"t": "src/main.rs", "s": "path"}, " ", {"t": "ctrl+r", "s": "key"}, {"t": "docs", "href": "https://example.com"}]}},
            {"id": "card1.b", "k": "text", "p": {"text": "body"}}
        ]},
        {"id": "sec1", "k": "section", "p": {"head": [{"t": "Env"}], "collapsible": true, "collapsed": true}, "c": [
            {"id": "sec1.kv", "k": "kv", "p": {"items": [{"k": "context", "v": "prod"}, {"k": "ns", "v": [{"t": "default", "s": "accent"}]}]}}
        ]},
        {"id": "rule1", "k": "rule", "p": {"label": "Older"}},
        {"id": "spc1", "k": "spacer", "p": {"size": "lg"}},
        {"id": "pr1", "k": "progress", "p": {"value": 0.5, "label": "1/2"}},
        {"id": "pr2", "k": "progress", "p": {"label": "waiting"}},
        {"id": "mt1", "k": "meter", "p": {"value": 0.78, "thresholds": {"warn": 0.7, "bad": 0.9}, "label": "78%", "total": "200K", "marks": [{"at": 0.85, "icon": "context", "title": "compacts"}]}},
        {"id": "mt2", "k": "meter", "p": {"parts": [{"value": 0.2, "token": "accent"}, {"value": 0.3, "token": "success", "hatch": true}], "style": "bar"}},
        {"id": "mt3", "k": "meter", "p": {"value": 0.5, "style": "blocks", "steps": 4}},
        {"id": "an1", "k": "ansi", "p": {"text": "\x1b[1mbold\x1b[0m and \x1b]8;;https://x.y\x1b\\link\x1b]8;;\x1b\\\nline2\nline3\nline4", "preview": {"lines": 2}}},
        {"id": "rows1", "k": "rows", "p": {"lines": ["\x1b[31mred\x1b[0m row", "second"], "cols": 20}},
        {"id": "el1", "k": "el", "p": {"tag": "div", "class": "box sf-evil", "attrs": {"data-x": "1", "aria-hidden": true, "onclick": "x", "style": "color:red", "role": "note"}, "text": "hello"}, "c": [
            {"id": "el1.b", "k": "el", "p": {"tag": "strong", "text": "bold"}}
        ]},
        {"id": "el2", "k": "el", "p": {"tag": "marquee", "text": "unknown"}},
        {"id": "md1", "k": "md", "p": {"text": "# Title\n\nSome *em* and **strong** with `#ff0000` and https://example.com and src/lib.rs:12.\n\n- [x] done\n- [ ] todo\n\n> [!WARNING]\n> careful\n\n| a | b |\n|:--|--:|\n| high | 2 |\n\n```rust\nfn main() {}\n```\n\n<div>raw</div>\n"}},
        {"id": "code1", "k": "code", "p": {"text": "fn main() {\n    let x = 42;\n}", "lang": "rust", "path": "src/main.rs", "numbers": true, "start": 10, "marks": [{"line": 11, "tone": "warning", "ranges": [[8, 9]]}]}},
        {"id": "diff1", "k": "diff", "p": {"text": "@@ -1,3 +1,3 @@ fn main()\n ctx\n-let x = 1;\n+let x = 2;\n ctx\n@@ -20,1 +20,1 @@\n-a\n+b\n", "lang": "rust"}},
        {"id": "diff2", "k": "diff", "p": {"hunks": [{"oldStart": 1, "newStart": 1, "lines": ["-old line", "+new line"]}], "mode": "split"}},
        {"id": "ed1", "k": "editor", "p": {"text": "hello world", "cursor": 5, "anchor": 0, "prompt": [{"t": "> ", "s": "accent"}], "decor": [{"from": 6, "to": 11, "s": "typo"}], "mode": "insert", "maxLines": 3}},
        {"id": "in1", "k": "input", "p": {"text": "", "placeholder": "Filter", "ghost": ""}},
        {"id": "st1", "k": "status", "c": [
            {"id": "seg1", "k": "seg", "p": {"icon": "branch", "text": "main", "tone": "accent", "actions": {"click": "branch"}}},
            {"id": "seg2", "k": "seg", "p": {"text": "ready", "side": "right", "priority": 2}}
        ]},
        {"id": "ls1", "k": "list", "p": {"selected": "it2", "filter": "lgn", "max": {"lines": 4}}, "c": [
            {"id": "it1", "k": "item", "p": {"label": "fix-login", "detail": "origin/fix-login", "value": "↑2", "icon": "branch", "hint": ["enter"]}},
            {"id": "it2", "k": "item", "p": {"label": "main"}}
        ]},
        {"id": "tb1", "k": "tabs", "p": {"items": [{"id": "files", "label": "Files"}, {"id": "commits", "label": "Commits"}], "active": "commits"}},
        {"id": "tab1", "k": "table", "p": {"cols": [{"id": "pod", "head": "Pod", "grow": 1}, {"id": "cpu", "head": "CPU", "align": "end", "priority": 1}, {"id": "mem", "head": "Memory"}], "rows": [{"id": "api", "cells": {"pod": "api-7d9f", "cpu": "120m", "mem": {"meter": {"value": 0.72, "thresholds": {"warn": 0.7}}}}}, {"id": "w", "cells": {"pod": "worker", "cpu": "80m", "mem": "1Gi"}}]}},
        {"id": "tr1", "k": "tree", "p": {"nodes": [{"id": "src", "label": "src", "icon": "folder", "open": true, "children": [{"id": "lib", "label": "lib.rs", "icon": "file-code"}]}, {"id": "docs", "label": "docs", "children": [{"id": "x", "label": "x.md"}]}]}},
        {"id": "tool1", "k": "tool", "p": {"name": "bash", "title": "Run", "target": "cargo test -p auk-views --lib | tail", "targetKind": "command", "status": "running", "age": 1200, "intent": "Testing", "collapsible": true, "meta": [[{"t": "+8", "s": "ins"}]], "badges": ["prod"], "tools": [{"id": "copy", "label": "Copy", "keys": "cmd+c"}]}, "c": [
            {"id": "tool1.out", "k": "ansi", "p": {"text": "running 3 tests"}},
            {"id": "tool1.md", "k": "md", "p": {"text": "done"}}
        ]},
        {"id": "tool2", "k": "tool", "p": {"name": "read", "title": "Read", "target": "src/lib.rs", "targetKind": "path", "status": "done", "took": 420, "exit": 1}},
        {"id": "ag1", "k": "agent", "p": {"name": "runner-3", "badges": ["linux"], "task": "Build and test", "status": "running", "model": "opus", "stats": {"tools": 14, "requests": 9, "done": 0.4, "context": 0.8, "tokens": 12345, "cost": 0.42, "age": 72000}, "tool": {"name": "bash", "intent": "Running tests", "age": 8000}, "retry": {"attempt": 1, "max": 3, "delay": 5000, "age": 1000, "error": "overloaded"}, "depth": 1, "collapsible": true}, "c": [
            {"id": "ag1.k", "k": "text", "p": {"text": "child"}}
        ]},
        {"id": "ck1", "k": "checklist", "p": {"phases": [{"id": "p1", "title": "Ship", "items": [{"id": "a", "text": "Tag", "status": "done"}, {"id": "b", "text": "Notes", "status": "active", "note": "waiting on CI"}, {"id": "c", "text": "Blog", "status": "blocked"}]}]}},
        {"id": "ck2", "k": "checklist", "p": {"mode": "hud", "phases": [{"id": "p1", "title": "Ship", "items": [{"id": "a", "text": "Tag", "status": "active"}]}]}},
        {"id": "ck3", "k": "checklist", "p": {"mode": "reminder", "phases": [{"id": "p1", "title": "Ship", "items": [{"id": "a", "text": "Tag", "status": "pending"}]}]}},
        {"id": "tx1", "k": "text", "p": {"text": "a/very/long/path/to/some/file.rs", "wrap": "none", "truncate": "middle", "max": {"w": "16ch"}}},
        {"id": "tx2", "k": "text", "p": {"spans": [{"t": "glow", "fx": "shimmer"}, {"t": " nerd", "s": "muted"}], "lines": 2}},
        {"id": "toast1", "k": "toast", "p": {"text": "Saved", "sub": "disk ok\nmore", "tone": "info"}}
    ])
}

fn harness() -> Harness {
    let mut h = Harness::new();
    h.hello(&["edit", "undo", "send"]);
    h.surface("s1", "inline", all_kinds());
    h.apc("f", &json!({"sf": "s1", "s": 2, "ops": [
        ["add", "layer", "s1", null, {"id": "layer", "k": "col", "c": [
            {"id": "ov1", "k": "overlay", "p": {"anchor": {"node": "ed1", "side": "bottom"}, "size": "sm", "modal": true, "head": [{"t": "Pick"}], "role": "omp.picker"}, "c": [
                {"id": "ov1.t", "k": "text", "p": {"text": "inside"}}
            ]}
        ]}],
        ["focus", "ed1"]
    ]}).to_string());
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
    assert!(h.errors().is_empty(), "{:?}", h.errors());
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
        Some("grid-template-columns: calc(7*var(--sf-cw)) minmax(0, 1fr);")
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
    let reports = h.view.take_reports();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].2, "unknown el tag marquee");
    assert!(h.view.take_reports().is_empty(), "once per node version");
}
