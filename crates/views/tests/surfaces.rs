//! A pane's surfaces arranged with the real terminal (§6.2, §7.3): the shared compositor every
//! window uses ([`PaneSurfaces`]), fed by `auk-term` and the `auk-tsp` engine in stream order.

use auk_term::{Event, Terminal};
use auk_tsp::{Config, MemoryBlobs, TspPane};
use auk_views::compose::Scroll;
use auk_views::{Arranged, PaneSurfaces, ViewSettings};
use serde_json::{Value, json};

const CELL: f32 = 17.0;

struct Pane {
    term: Terminal,
    tsp: TspPane,
    sf: PaneSurfaces,
    now: f64,
}

impl Pane {
    fn new() -> Pane {
        Pane::with_rows(12)
    }

    fn with_rows(rows: u16) -> Pane {
        let term = Terminal::new(80, rows, auk_term::Config::default());
        let mut tsp = TspPane::with(
            Config::default(),
            auk_views::sheet_checker(),
            Box::new(MemoryBlobs::default()),
        );
        tsp.on_resize(&term);
        Pane {
            term,
            tsp,
            sf: PaneSurfaces::new(ViewSettings::default(), 1.0),
            now: 1_000_000.0,
        }
    }

    /// Feeds program output the way a replica does: TSP strings go to the engine in order.
    fn feed(&mut self, bytes: &[u8]) {
        for event in self.term.feed(bytes) {
            if let Event::Tsp(payload) = event {
                self.tsp.on_apc(&mut self.term, &payload);
            }
        }
        let changes = self.tsp.take_changes();
        self.sf.sync(&self.tsp, &changes, self.now);
        let _ = self.tsp.take_output();
    }

    fn apc(&mut self, verb: &str, body: &Value) {
        self.feed(format!("\x1b_tsp;{verb};{body}\x1b\\").as_bytes());
    }

    fn arrange(&mut self) -> Arranged {
        let h = f32::from(self.term.rows()) * CELL;
        self.sf
            .layout(&self.term, &self.tsp, (640.0, h), true, self.now);
        self.sf
            .arrange(&self.term, &self.tsp, CELL, h, 1.0, Scroll::default())
    }

    /// Lays out and arranges as hosts do: what the arrangement shows lays out too.
    fn arrange_shown(&mut self) -> Arranged {
        let h = f32::from(self.term.rows()) * CELL;
        self.sf
            .layout(&self.term, &self.tsp, (640.0, h), true, self.now);
        self.sf
            .arrange_shown(&self.term, &self.tsp, CELL, h, 1.0, Scroll::default())
    }

    /// The element of node `id` in surface `sf`.
    fn el(&self, sf: &str, id: &str) -> auk_ui::NodeId {
        let key = self.tsp.find(sf, false).expect("surface").key();
        self.sf.view().node_element(key, id).expect("mounted")
    }
}

/// The height the arranged flow gives the line holding `needle`.
fn height_of(p: &mut Pane, needle: &str) -> f32 {
    let lines = p.term.history_len() + usize::from(p.term.rows());
    let line = (0..lines)
        .find(|&l| p.term.text(l..l + 1).contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle:?}"));
    let arranged = p.arrange();
    arranged.composed.slot_of(line).map_or(0.0, |s| s.height)
}

#[test]
fn stray_output_under_a_live_inline_surface_stays_hidden_until_it_closes_or_suspends() {
    let mut p = Pane::new();
    p.feed(b"$ run\r\n");
    p.apc(
        "q",
        &json!({"q": "hello", "v": [1], "app": "t", "features": []}),
    );
    p.apc("o", &json!({"id": "s1", "mode": "inline"}));
    p.apc(
        "f",
        &json!({"sf": "s1", "s": 1, "ops": [["add", "main", "s1", null, {"id": "main", "k": "col", "c": [{"id": "t", "k": "text", "p": {"text": "surface"}}]}]]}),
    );
    // Output that isn't TSP goes to the grid below the anchor, and stays hidden.
    p.feed(b"stray output\r\n");
    assert_eq!(height_of(&mut p, "$ run"), CELL, "lines above show");
    assert_eq!(height_of(&mut p, "stray output"), 0.0);
    let arranged = p.arrange();
    assert_eq!(arranged.blocks.len(), 1, "the surface block shows");

    // Suspended: the pane goes back to the grid.
    p.apc("f", &json!({"sf": "s1", "s": 2, "ops": [["suspend"]]}));
    assert_eq!(height_of(&mut p, "stray output"), CELL);
    p.apc("f", &json!({"sf": "s1", "s": 3, "ops": [["resume"]]}));
    assert_eq!(height_of(&mut p, "stray output"), 0.0);

    // Closed (kept): the stray output shows below the kept document.
    p.apc("x", &json!({"id": "s1"}));
    assert_eq!(height_of(&mut p, "stray output"), CELL);
    assert_eq!(p.arrange().blocks.len(), 1, "main stays");

    // Flow surfaces never hide what follows them.
    p.apc("o", &json!({"id": "f1", "mode": "flow"}));
    p.apc(
        "f",
        &json!({"sf": "f1", "s": 1, "ops": [["add", "main", "f1", null, {"id": "main", "k": "col", "c": [{"id": "t", "k": "text", "p": {"text": "flow"}}]}]]}),
    );
    p.feed(b"after flow\r\n");
    assert_eq!(height_of(&mut p, "after flow"), CELL);
}

#[test]
fn overlays_sit_in_the_visible_pane_over_long_content_and_their_lists_lay_out() {
    let mut p = Pane::with_rows(30);
    let vh = 30.0 * CELL;
    p.apc(
        "q",
        &json!({"q": "hello", "v": [1], "app": "t", "features": []}),
    );
    // A transcript many times the pane's height.
    let lines: Vec<Value> = (0..150)
        .map(|i| json!({"id": format!("t{i}"), "k": "text", "p": {"text": format!("line {i}")}}))
        .collect();
    p.apc("o", &json!({"id": "s1", "mode": "flow"}));
    p.apc(
        "f",
        &json!({"sf": "s1", "s": 1, "ops": [["add", "main", "s1", null, {"id": "main", "k": "col", "c": lines}]]}),
    );
    p.arrange_shown();
    let pane = p.sf.view().element();
    assert!(
        p.sf.doc().border_box(pane).unwrap().h > vh * 3.0,
        "long content"
    );

    // omp's model picker: a modal overlay at the bottom, a card holding a virtual list.
    let items: Vec<Value> = (0..80)
        .map(|i| json!({"id": format!("l/{i}"), "k": "item", "p": {"label": format!("model {i}")}}))
        .collect();
    p.apc(
        "f",
        &json!({"sf": "s1", "s": 2, "ops": [["add", "layer", "s1", null, {"id": "layer", "k": "col", "c": [
            {"id": "ov", "k": "overlay", "p": {"anchor": "bottom", "size": "lg", "modal": true, "head": "Switch Model"}, "c": [
                {"id": "l", "k": "list", "p": {"virtual": true, "selected": "l/0"}, "c": items}
            ]}
        ]}]]}),
    );
    let arranged = p.arrange_shown();
    assert!(arranged.layers);
    let doc = p.sf.doc();
    let top = p.sf.pane_top();
    let find = |el: auk_ui::NodeId, class: &str| -> auk_ui::NodeId {
        let mut stack = vec![el];
        while let Some(e) = stack.pop() {
            if doc
                .attribute(e, "class")
                .unwrap_or("")
                .split(' ')
                .any(|c| c == class)
                || p.sf
                    .view()
                    .written()
                    .class(e)
                    .is_some_and(|c| c.split(' ').any(|c| c == class))
            {
                return e;
            }
            stack.extend(doc.children(e).iter().rev().copied());
        }
        panic!("no .{class}")
    };
    let ov = p.el("s1", "ov");
    let card = find(ov, "sf-ov-card");
    let body = find(ov, "sf-ov-body");
    let c = doc.border_box(card).unwrap();
    // In the visible area (the layer's top is the content area's top), 16px from its bottom.
    assert!(c.y - top >= 0.0 && c.h > 0.0, "card {c:?}");
    assert!(
        (c.y + c.h - top - (vh - 16.0)).abs() < 1.0,
        "16px from the bottom: {c:?} in {vh}"
    );
    // Taller than the room it has: its body scrolls.
    assert!(doc.max_scroll(body).1 > 0.0, "the body scrolls");
    // The list's rows in view are laid out (the first ones), the far ones aren't.
    let b = doc.border_box(body).unwrap();
    let first = p.el("s1", "l/0");
    assert!(doc.laid_out(first), "the first row is laid out");
    let r = doc.border_box(first).unwrap();
    assert!(
        r.y >= b.y && r.y + r.h <= b.y + b.h + 1.0,
        "row {r:?} in body {b:?}"
    );
    assert!(!doc.laid_out(p.el("s1", "l/79")), "the last row isn't");
    // A press on the card lands on it: the hosts' mapping agrees with the layout.
    let at =
        p.sf.doc_point(&arranged, c.x + c.w / 2.0, c.y - top + 10.0, vh, 1.0)
            .expect("a document point");
    let hit = auk_ui::hit_test(doc, at.0, at.1).expect("a hit");
    assert!(
        std::iter::successors(Some(hit), |&e| doc.parent(e)).any(|e| e == card),
        "the press lands in the card"
    );

    // Revealing a far row scrolls the card's body to it (not the pane), and lays it out.
    p.apc(
        "f",
        &json!({"sf": "s1", "s": 3, "ops": [["reveal", "l/70"]]}),
    );
    p.arrange_shown();
    let doc = p.sf.doc();
    assert!(doc.scroll_offset(body).1 > 0.0, "the body scrolled");
    let far = p.el("s1", "l/70");
    assert!(doc.laid_out(far), "the revealed row is laid out");
    let b = doc.border_box(body).unwrap();
    let r = doc.border_box(far).unwrap();
    assert!(
        r.y >= b.y - 1.0 && r.y + r.h <= b.y + b.h + 1.0,
        "row {r:?} in body {b:?}"
    );
    let (_, pv) = p.sf.parts();
    assert_eq!(pv.take_pane_reveal(), None, "the pane doesn't scroll");

    // To the list's end and back to its top: the rows in view are laid out each time (the
    // window isn't kept on the end, which only the flow's view rect would move).
    for (s, row) in [(4, "l/79"), (5, "l/0")] {
        p.apc("f", &json!({"sf": "s1", "s": s, "ops": [["reveal", row]]}));
        p.arrange_shown();
        let doc = p.sf.doc();
        let el = p.el("s1", row);
        assert!(doc.laid_out(el), "{row} is laid out");
        let b = doc.border_box(body).unwrap();
        let r = doc.border_box(el).unwrap();
        assert!(
            r.y >= b.y - 1.0 && r.y + r.h <= b.y + b.h + 1.0,
            "{row} {r:?} in body {b:?}"
        );
    }
    assert!(p.sf.doc().scroll_offset(body).1 < CELL, "back at the top");
    assert!(!p.sf.view().end_windows_left_view(p.sf.doc()));
}

#[test]
fn an_overlay_anchored_to_a_node_sits_where_the_node_shows_or_hides_when_it_doesnt() {
    let mut p = Pane::with_rows(30);
    let vh = 30.0 * CELL;
    p.apc(
        "q",
        &json!({"q": "hello", "v": [1], "app": "t", "features": []}),
    );
    // Long, but not so many children that `main` virtualizes.
    let lines: Vec<Value> = (0..50)
        .map(|i| json!({"id": format!("t{i}"), "k": "text", "p": {"text": format!("line {i}\nmore\nmore")}}))
        .collect();
    p.apc("o", &json!({"id": "s1", "mode": "flow"}));
    p.apc(
        "f",
        &json!({"sf": "s1", "s": 1, "ops": [["add", "main", "s1", null, {"id": "main", "k": "col", "c": lines}]]}),
    );
    let layer = |target: &str| {
        json!(["add", "layer", "s1", null, {"id": "layer", "k": "col", "c": [
            {"id": "ov", "k": "overlay", "p": {"anchor": {"node": target}}, "c": [
                {"id": "hint", "k": "text", "p": {"text": "a hint"}}
            ]}
        ]}])
    };
    // The pane follows the end: the last text shows near the pane's bottom.
    p.apc("f", &json!({"sf": "s1", "s": 2, "ops": [layer("t49")]}));
    let arranged = p.arrange_shown();
    let top = p.sf.pane_top();
    let ov = p.el("s1", "ov");
    let card = p.sf.doc().element_children(ov)[0];
    let c = p.sf.doc().border_box(card).unwrap();
    assert!(arranged.blocks[0].height > vh * 3.0, "long content");
    // Where the target shows: its block's place plus its offset in the block.
    let (i, y) = arranged.composed.blocks[0];
    let region = p.sf.doc().border_box(arranged.blocks[i].el).unwrap();
    let t = p.sf.doc().border_box(p.el("s1", "t49")).unwrap();
    let shown = y + (t.y - region.y);
    assert!(
        shown > vh / 2.0 && shown < vh,
        "the target shows low in the pane: {shown}"
    );
    // Above it (no room below), 4px away.
    assert!(
        (c.y + c.h - top - (shown - 4.0)).abs() < 1.0,
        "beside the target where it shows: {c:?}, target at {shown}"
    );
    assert!(
        !p.sf
            .view()
            .written()
            .class(ov)
            .unwrap_or("")
            .contains("lost")
    );
    // Anchored to a line scrolled out of view: the card hides.
    p.apc(
        "f",
        &json!({"sf": "s1", "s": 3, "ops": [["del", "layer"], layer("t0")]}),
    );
    p.arrange_shown();
    let ov = p.el("s1", "ov");
    assert!(
        p.sf.view()
            .written()
            .class(ov)
            .unwrap_or("")
            .split(' ')
            .any(|c| c == "lost"),
        "lost: {:?}",
        p.sf.view().written().class(ov)
    );
}
