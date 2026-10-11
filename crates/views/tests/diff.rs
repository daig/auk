//! Split (side-by-side) `diff`: the Edits setting and `mode`, pairing
//! of removed and added lines, word emphasis and highlighting on both sides, fold rows, the
//! blank side, the layout of the halves, and omp's diffs drawn split.

mod common;

use auk_ui::NodeId;
use auk_views::ViewSettings;
use common::{Harness, replay};
use serde_json::json;

/// The cell width the harness lays out with.
const CW: f32 = 8.0;

fn split_on(h: &mut Harness) {
    let settings = ViewSettings {
        split_diffs: true,
        ..h.view.settings().clone()
    };
    h.view.set_settings(&mut h.doc, &h.pane, settings);
}

/// The window and the pane resized together, as a host does.
fn resize(h: &mut Harness, cols: u16) {
    h.doc.set_viewport(f32::from(cols) * CW, 800.0);
    h.view.set_cols(&mut h.doc, &h.pane, cols);
    h.frame();
}

fn is_split(h: &Harness, el: NodeId) -> bool {
    h.class(el).split(' ').any(|c| c == "split")
}

fn children_with(h: &Harness, el: NodeId, class: &str) -> Vec<NodeId> {
    h.doc
        .element_children(el)
        .into_iter()
        .filter(|&c| h.class(c).split(' ').any(|x| x == class))
        .collect()
}

const CHANGE: &str =
    "@@ -10,6 +10,5 @@ fn f()\n a\n-b one\n-c two\n-d three\n+b ONE\n e\n+f new\n g\n";

/// Under Split the decision is the block's own width, not the pane's: in a 115-cell pane a diff
/// at the top level is wider than 112 cells and splits, one inside a card (its padding) isn't
/// and stays unified. Widening the window alone (the pane's columns unchanged) re-decides after
/// layout; narrowing it goes back. Inline (the default) never splits.
#[test]
fn split_follows_the_blocks_width() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "top", "k": "diff", "p": {"text": CHANGE}},
            {"id": "card", "k": "card", "p": {"head": "src/f.rs"}, "c": [
                {"id": "inner", "k": "diff", "p": {"text": CHANGE}}
            ]}
        ]),
    );
    resize(&mut h, 115);
    let (top, inner) = (h.el("s1", "top"), h.el("s1", "inner"));
    assert!(
        !is_split(&h, top) && !is_split(&h, inner),
        "Inline: unified"
    );
    split_on(&mut h);
    h.frame();
    let w = |h: &Harness, el| h.doc.border_box(el).unwrap().w;
    assert!(
        w(&h, top) > 112.0 * CW && w(&h, inner) < 112.0 * CW,
        "{} {}",
        w(&h, top),
        w(&h, inner)
    );
    assert!(
        is_split(&h, top),
        "the top-level diff is wider than 112 cells"
    );
    assert!(!is_split(&h, inner), "the card's diff is narrower");
    assert!(!h.find_class(top, "sf-dr").is_empty());
    assert!(h.find_class(inner, "sf-dr").is_empty());
    // The window widens; the pane's columns haven't caught up yet.
    h.doc.set_viewport(140.0 * CW, 800.0);
    h.frame();
    assert!(is_split(&h, inner), "re-decided from the laid-out width");
    assert_eq!(h.find_class(inner, "sf-dr").len(), 7);
    // And narrows.
    resize(&mut h, 100);
    assert!(!is_split(&h, top) && !is_split(&h, inner));
    assert!(h.find_class(top, "sf-dr").is_empty());
    assert_eq!(h.find_class(top, "sf-dl").len(), 8);
    // An explicit mode wins over the width.
    h.frame_ops("s1", 2, json!([["set", "inner", {"mode": "split"}]]));
    h.frame();
    assert!(is_split(&h, h.el("s1", "inner")));
    resize(&mut h, 200);
    h.frame_ops("s1", 3, json!([["set", "top", {"mode": "unified"}]]));
    h.frame();
    assert!(!is_split(&h, h.el("s1", "top")));
    assert!(h.errors().is_empty(), "{:?}", h.errors());
}

/// Split pairs the i-th removed with the i-th added line of a change; an unpaired line's other
/// side is blank (`.nil`); context shows on both sides. Each half has its side's line number,
/// and the changed words are emphasized on both sides. Unified keeps one column with both
/// numbers, a change's removed lines first.
#[test]
fn split_pairs_removed_and_added_lines() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "s", "k": "diff", "p": {"text": CHANGE, "mode": "split"}},
            {"id": "u", "k": "diff", "p": {"text": CHANGE, "mode": "unified"}}
        ]),
    );
    h.frame();
    let s = h.el("s1", "s");
    let body = h.find_class(s, "sf-diff-body")[0];
    let rows = children_with(&h, body, "sf-dr");
    // (op, number, text) of each half: "" for the blank side.
    let half = |el: NodeId| -> (String, String, String) {
        let c = h.class(el);
        let op = ["ctx", "del", "add", "nil"]
            .into_iter()
            .find(|o| c.split(' ').any(|x| x == *o))
            .unwrap_or("?")
            .to_owned();
        assert!(c.split(' ').any(|x| x == "half"), "{c}");
        let num = h
            .find_class(el, "sf-dg")
            .first()
            .map(|&g| h.text(g))
            .unwrap_or_default();
        let text = h
            .find_class(el, "sf-dt")
            .first()
            .map(|&t| h.text(t))
            .unwrap_or_default();
        (op, num, text)
    };
    let got: Vec<_> = rows
        .iter()
        .map(|&r| {
            let k = h.doc.element_children(r);
            assert_eq!(k.len(), 2, "a row has two halves");
            (half(k[0]), half(k[1]))
        })
        .collect();
    let t = |op: &str, n: &str, s: &str| (op.to_owned(), n.to_owned(), s.to_owned());
    let nil = t("nil", "", "");
    assert_eq!(
        got,
        vec![
            (t("ctx", "10", "a"), t("ctx", "10", "a")),
            (t("del", "11", "b one"), t("add", "11", "b ONE")),
            (t("del", "12", "c two"), nil.clone()),
            (t("del", "13", "d three"), nil.clone()),
            (t("ctx", "14", "e"), t("ctx", "12", "e")),
            (nil.clone(), t("add", "13", "f new")),
            (t("ctx", "15", "g"), t("ctx", "14", "g")),
        ]
    );
    // Word emphasis on both sides of the pair.
    let k = h.doc.element_children(rows[1]);
    let em = |el| {
        h.find_class(el, "sf-em")
            .iter()
            .map(|&e| h.text(e))
            .collect::<Vec<_>>()
    };
    assert_eq!(em(k[0]), ["one"]);
    assert_eq!(em(k[1]), ["ONE"]);
    // Unified: one column, both numbers, removed before added.
    let u = h.el("s1", "u");
    let lines = h.find_class(u, "sf-dl");
    let order: Vec<(String, String)> = lines
        .iter()
        .map(|&l| {
            let g = h.find_class(l, "sf-dg");
            assert_eq!(g.len(), 2);
            (h.text(g[0]), h.text(g[1]))
        })
        .collect();
    let n = |a: &str, b: &str| (a.to_owned(), b.to_owned());
    assert_eq!(
        order,
        vec![
            n("10", "10"),
            n("11", ""),
            n("12", ""),
            n("13", ""),
            n("", "11"),
            n("14", "12"),
            n("", "13"),
            n("15", "14"),
        ]
    );
    assert!(h.find_class(u, "sf-dr").is_empty() && h.find_class(u, "half").is_empty());
}

/// Both halves are highlighted from their side's text, with the word emphasis kept.
#[test]
fn split_highlights_both_sides() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "d", "k": "diff", "p": {
            "text": "@@ -1,2 +1,2 @@\n fn main() {\n-    let x = 1;\n+    let x = 2;\n",
            "lang": "rust", "mode": "split"
        }}]),
    );
    h.frame();
    h.view.finish_background(&mut h.doc);
    h.frame();
    let d = h.el("s1", "d");
    let rows = h.find_class(d, "sf-dr");
    assert_eq!(rows.len(), 2);
    let k = h.doc.element_children(rows[1]);
    for (side, el, want) in [("old", k[0], "1"), ("new", k[1], "2")] {
        let kw: Vec<_> = h
            .find_class(el, "tk-keyword")
            .iter()
            .map(|&e| h.text(e))
            .collect();
        assert!(kw.iter().any(|w| w == "let"), "{side}: {kw:?}");
        let em: Vec<_> = h
            .find_class(el, "sf-em")
            .iter()
            .map(|&e| h.text(e))
            .collect();
        assert_eq!(em, [want], "{side}");
    }
}

/// Fold rows sit between hunks (never before the first): `⋯ N unchanged lines` and the next
/// hunk's function, or `⋯` alone when the gap is unknown. In split they span the row. No hunks:
/// "No changes".
#[test]
fn split_fold_rows_and_no_changes() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "known", "k": "diff", "p": {"mode": "split",
                "text": "@@ -5,2 +5,2 @@\n a\n-b\n+B\n@@ -20,1 +20,1 @@ fn tail()\n-c\n+C\n"}},
            {"id": "unknown", "k": "diff", "p": {"mode": "split",
                "hunks": [{"lines": ["-a", "+b"]}, {"lines": ["-c", "+d"]}]}},
            {"id": "empty", "k": "diff", "p": {"text": "", "mode": "split"}}
        ]),
    );
    h.frame();
    let known = h.el("s1", "known");
    let body = h.find_class(known, "sf-diff-body")[0];
    let kids = h.doc.element_children(body);
    assert!(
        h.class(kids[0]).contains("sf-dr"),
        "no fold row before the first hunk"
    );
    let seps = children_with(&h, body, "sf-dsep");
    assert_eq!(seps.len(), 1);
    assert_eq!(h.text(seps[0]), "⋯ 13 unchanged linesfn tail()");
    assert_eq!(h.text(h.find_class(seps[0], "fn")[0]), "fn tail()");
    let (sr, br) = (
        h.doc.border_box(seps[0]).unwrap(),
        h.doc.border_box(body).unwrap(),
    );
    assert!((sr.w - br.w).abs() < 1.0, "the fold row spans both halves");
    let unknown = h.el("s1", "unknown");
    let seps = h.find_class(unknown, "sf-dsep");
    assert_eq!(seps.len(), 1);
    assert_eq!(h.text(seps[0]), "⋯");
    let empty = h.el("s1", "empty");
    assert!(h.find_class(empty, "sf-diff-body").is_empty());
    assert_eq!(
        h.text(h.find_class(empty, "sf-diff-empty")[0]),
        "No changes"
    );
}

/// Split's geometry: a row's halves sit side by side on one line, each half the body's width;
/// a long line wraps inside its half (the diff never scrolls sideways) and its row grows, the
/// other half (blank, shaded with `--sf-shade`) as tall.
#[test]
fn split_halves_lay_out_side_by_side() {
    let mut h = Harness::new();
    h.hello(&[]);
    let long = "x".repeat(150);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "d", "k": "diff", "p": {"mode": "split", "hunks": [
            {"oldStart": 1, "newStart": 1, "lines": ["-short", "+short!", format!("+{long}")]}
        ]}}]),
    );
    h.frame();
    let d = h.el("s1", "d");
    let body = h.find_class(d, "sf-diff-body")[0];
    let br = h.doc.border_box(body).unwrap();
    let rows = h.find_class(d, "sf-dr");
    assert_eq!(rows.len(), 2);
    for &r in &rows {
        let k = h.doc.element_children(r);
        let (a, b) = (
            h.doc.border_box(k[0]).unwrap(),
            h.doc.border_box(k[1]).unwrap(),
        );
        assert!(
            (a.y - b.y).abs() < 0.5 && (a.h - b.h).abs() < 0.5,
            "{a:?} {b:?}"
        );
        assert!(a.x < b.x && (a.x + a.w - b.x).abs() < 1.0, "{a:?} {b:?}");
        assert!((a.w - br.w / 2.0).abs() < 2.0, "{} of {}", a.w, br.w);
    }
    let k = h.doc.element_children(rows[1]);
    let row = h.doc.border_box(rows[1]).unwrap();
    assert!(
        row.h > 2.5 * 17.0,
        "the long line wraps in its half: {row:?}"
    );
    assert!(h.doc.scroll_size(d).0 <= h.doc.border_box(d).unwrap().w + 0.5);
    assert!(h.class(k[0]).contains("nil"));
    let shade = h.doc.computed_style(k[0]).unwrap();
    assert!(shade.background_color.a > 0.0, "the blank side is shaded");
}

/// omp's diffs (the recorded session's edit tool cards) draw side by side under Split in a wide
/// pane, with removed and added lines paired; narrowed, they go back to unified.
#[test]
fn omp_session_diffs_draw_split_under_the_split_setting() {
    let mut h = replay("omp-session.jsonl", |_, _| {});
    let diffs = |h: &Harness| -> Vec<NodeId> {
        let mut out = Vec::new();
        for s in h.pane.surfaces() {
            let mut stack = vec![s.document().root()];
            while let Some(n) = stack.pop() {
                if n.kind() == "diff"
                    && let Some(el) = h.view.node_element(s.key(), n.id())
                {
                    out.push(el);
                }
                stack.extend(n.children());
            }
        }
        out
    };
    h.frame();
    let all = diffs(&h);
    assert!(!all.is_empty(), "omp drew diffs");
    assert!(all.iter().all(|&d| !is_split(&h, d)), "Inline by default");
    split_on(&mut h);
    resize(&mut h, 180);
    for &d in &diffs(&h) {
        assert!(is_split(&h, d), "{}", h.class(d));
        let rows = h.find_class(d, "sf-dr");
        assert!(!rows.is_empty());
        let paired = rows.iter().any(|&r| {
            let k = h.doc.element_children(r);
            h.class(k[0]).contains("del") && h.class(k[1]).contains("add")
        });
        assert!(paired, "a removed line beside its replacement");
        assert!(
            h.find_class(d, "sf-em").len() >= 2,
            "word emphasis on both sides"
        );
    }
    resize(&mut h, 100);
    for &d in &diffs(&h) {
        assert!(!is_split(&h, d));
        assert!(h.find_class(d, "sf-dr").is_empty());
    }
}

/// Outside a card a diff has its ring and `--code-bg`; inside one (directly or deeper) it sits
/// flush, with no ring or background of its own.
#[test]
fn a_diff_in_a_card_sits_flush() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "top", "k": "diff", "p": {"text": CHANGE}},
            {"id": "card", "k": "card", "p": {"head": "src/f.rs"}, "c": [
                {"id": "inner", "k": "diff", "p": {"text": CHANGE}},
                {"id": "col", "k": "col", "c": [{"id": "deep", "k": "diff", "p": {"text": CHANGE}}]}
            ]}
        ]),
    );
    h.frame();
    let top = h.doc.computed_style(h.el("s1", "top")).unwrap();
    assert_eq!(top.border_width, [1.0; 4]);
    assert!(top.background_color.a > 0.5, "{:?}", top.background_color);
    for id in ["inner", "deep"] {
        let s = h.doc.computed_style(h.el("s1", id)).unwrap();
        assert_eq!(s.border_width, [0.0; 4], "{id}");
        assert_eq!(s.background_color.a, 0.0, "{id}: {:?}", s.background_color);
    }
}
