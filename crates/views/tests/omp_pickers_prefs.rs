//! omp's picker and settings sheets (amendment A24; research/omp-stylesheets.md §5, §6, §16.7):
//! the preview heading, the copy picker's blocks, the agents prompt, the unfocused pane and the
//! card fallback model browser; the status-line preview, the editor card's children, the picker
//! in the editor card, program accents, the fallback settings list and the unfocused and still
//! pane. Checked on omp 18.8.5's recordings where they hold the role, constructed frames
//! otherwise, in both themes where a colour is asserted, and with no motion on the probed nodes.

mod common;

use auk_tsp::Config;
use auk_ui::{Document, ElementState, NodeId};
use auk_views::ViewSettings;
use common::*;
use serde_json::{Value, json};

// ---------------------------------------------------------------------------------------------
// Helpers

/// What `value` (a token, e.g. `var(--l1)`) resolves to as a background where `el` sits.
fn token(h: &mut Harness, el: NodeId, value: &str) -> [u8; 4] {
    let probe = h.doc.create_element("div");
    h.doc.append_child(el, probe);
    h.doc
        .set_attribute(probe, "style", &format!("background-color: {value}"));
    h.doc.update();
    let c = h
        .doc
        .computed_style(probe)
        .unwrap()
        .background_color
        .to_rgba8();
    h.doc.remove(probe);
    h.doc.update();
    c
}

fn bg(h: &Harness, el: NodeId) -> [u8; 4] {
    h.doc
        .computed_style(el)
        .unwrap()
        .background_color
        .to_rgba8()
}

fn fg(h: &Harness, el: NodeId) -> [u8; 4] {
    h.doc.computed_style(el).unwrap().color.to_rgba8()
}

fn var(h: &Harness, el: NodeId, name: &str) -> String {
    let st = h.doc.computed_style(el).unwrap();
    st.custom
        .get(name)
        .map(|v| v.trim().to_owned())
        .unwrap_or_default()
}

/// A `--name: Npx` custom property as a number.
fn var_px(h: &Harness, el: NodeId, name: &str) -> f32 {
    let v = var(h, el, name);
    v.trim_end_matches("px")
        .parse()
        .unwrap_or_else(|_| panic!("{name} = {v:?}"))
}

fn first(h: &Harness, under: NodeId, class: &str) -> NodeId {
    *h.find_class(under, class)
        .first()
        .unwrap_or_else(|| panic!("a .{class}"))
}

/// The first element under `root` (itself included) whose `data-role` is `role`.
fn by_role(h: &Harness, root: NodeId, role: &str) -> NodeId {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if h.doc.node(n).is_element() && h.attr(n, "data-role").as_deref() == Some(role) {
            return n;
        }
        for &c in h.doc.children(n).iter().rev() {
            stack.push(c);
        }
    }
    panic!("no data-role={role}")
}

fn px(v: &impl std::fmt::Debug) -> String {
    format!("{v:?}")
}

/// T6.2: omp's sheets add no motion: no transition and no animation on a probed node.
fn still(h: &Harness, el: NodeId) {
    let st = h.doc.computed_style(el).unwrap();
    assert!(
        st.transitions
            .as_ref()
            .is_none_or(|t| t.iter().all(|x| x.duration == 0.0)),
        "{:?}: {:?}",
        h.class(el),
        st.transitions
    );
    assert!(
        st.animations.as_ref().is_none_or(|a| a.is_empty()),
        "{:?}: {:?}",
        h.class(el),
        st.animations
    );
}

fn set_dark(h: &mut Harness, dark: bool) {
    let s = ViewSettings {
        dark,
        ..h.view.settings().clone()
    };
    h.view.set_settings(&mut h.doc, &h.pane, s);
    h.frame();
}

/// An omp session surface (`mode`) holding `main`, and `layer` when given.
fn omp_surface(
    mode: &str,
    config: Config,
    main: Value,
    layer: Option<Value>,
    dark: bool,
) -> Harness {
    let mut h = Harness::with(
        config,
        ViewSettings {
            dark,
            ..Default::default()
        },
    );
    h.view.set_cols(&mut h.doc, &h.pane, 120);
    h.hello(&[]);
    h.apc(
        "o",
        &json!({"id": "s1", "mode": mode, "role": "omp.session"}).to_string(),
    );
    let mut ops = vec![json!(["add", "main", "s1", null, {"id": "main", "k": "col", "c": main}])];
    if let Some(l) = layer {
        ops.push(json!(["add", "layer", "s1", null, {"id": "layer", "k": "col", "c": l}]));
    }
    h.frame_ops("s1", 1, Value::Array(ops));
    h.frame();
    h.now += 1_000.0;
    h.frame();
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    h
}

/// The engine advertising `prefs`, `picker` and `aside`, as Tern does (as tests/prefs.rs).
fn prefs_config() -> Config {
    let mut config = Config::default();
    config.kinds.push("prefs".into());
    config.kinds.push("picker".into());
    if !config.features.iter().any(|f| f == "aside") {
        config.features.push("aside".into());
    }
    config
}

/// A one-section prefs page.
fn page() -> Value {
    json!({
        "title": "Settings",
        "pages": [{"id": "general", "label": "General", "icon": "sliders"}],
        "page": "general",
        "sections": [{"id": "run", "title": "Runs", "rows": [
            {"id": "auto", "label": "Deploy on push", "control": {"k": "switch", "on": true}}]}]
    })
}

/// The settings sheet (`v`) of omp's recordings, once laid out.
fn settings_sheet(h: &mut Harness) -> NodeId {
    h.view.set_cols(&mut h.doc, &h.pane, 120);
    h.frame();
    h.now += 1_000.0;
    h.frame();
    let key = h
        .pane
        .surfaces()
        .iter()
        .find(|x| x.id() == "s:1")
        .unwrap()
        .key();
    h.view.node_element(key, "v").expect("the settings sheet")
}

// ---------------------------------------------------------------------------------------------
// omp-pickers.css

/// T-P1: the model hub's preview heading (`fixtures/tsp/omp-picker.jsonl` s6) is the preview's
/// h1: sans 15px 600; the id line under it is --t3 and sits 2px below.
#[test]
fn omp_picker_preview_title_is_a_sans_heading_with_the_id_tucked_under() {
    let mut seen = false;
    let h = replay("omp-picker.jsonl", |h, rec| {
        if rec.verb != "f" || rec.body["s"] != 6 {
            return;
        }
        h.view.set_cols(&mut h.doc, &h.pane, 120);
        h.now += 1_000.0;
        h.frame();
        let pk = h.el("s:1", "v");
        let slot = first(h, pk, "pk-pv-slot");
        let title = by_role(h, slot, "omp.picker.title");
        let st = h.doc.computed_style(title).unwrap();
        assert_eq!(st.font_size, 15.0);
        assert_eq!(st.font_weight, 600.0);
        assert!(st.font_family.contains("system-ui"), "{}", st.font_family);
        let kids = h.doc.children(h.doc.parent(title).unwrap()).to_vec();
        let at = kids.iter().position(|&k| k == title).unwrap();
        let next = *kids[at + 1..]
            .iter()
            .find(|&&k| h.doc.node(k).is_element())
            .expect("the id line");
        assert!(h.class(next).contains("sf-act"), "{}", h.class(next));
        let t3 = token(h, next, "var(--t3)");
        assert_eq!(fg(h, next), t3);
        let (a, b) = (
            h.doc.border_box(title).unwrap(),
            h.doc.border_box(next).unwrap(),
        );
        assert!(((b.y - (a.y + a.h)) - 2.0).abs() < 0.5, "{a:?} {b:?}");
        // The id line copies on click: it fills on hover, as a clickable seg.
        let l1 = token(h, next, "var(--l1)");
        h.doc.set_state(next, ElementState::HOVER, true);
        h.doc.update();
        assert_eq!(bg(h, next), l1);
        h.doc.set_state(next, ElementState::HOVER, false);
        h.doc.update();
        still(h, title);
        still(h, next);
        seen = true;
    });
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    assert!(seen, "s6 drew the hub");
}

/// T-P2: the copy picker's blocks (`fixtures/tsp/omp-picker-tree-copy.jsonl` s12, focus on the
/// preview, `18.b0` focused): tiles with an --l2 border and no tree hairline; the focused one is
/// ringed in --accent. In both themes.
#[test]
fn omp_copy_picker_blocks_are_tiles_and_the_focused_one_is_ringed() {
    let mut seen = Vec::new();
    let h = replay("omp-picker-tree-copy.jsonl", |h, rec| {
        if rec.verb != "f" || rec.body["s"] != 12 {
            return;
        }
        h.view.set_cols(&mut h.doc, &h.pane, 120);
        h.now += 1_000.0;
        h.frame();
        let mut colours = Vec::new();
        for dark in [true, false] {
            set_dark(h, dark);
            let whole = h.el("s:1", "18.whole");
            let focused = h.el("s:1", "18.b0");
            assert_eq!(
                h.attr(whole, "data-role").as_deref(),
                Some("omp.picker.block")
            );
            assert_eq!(
                h.attr(focused, "data-role").as_deref(),
                Some("omp.picker.block.focused")
            );
            let st = h.doc.computed_style(whole).unwrap().clone();
            assert_eq!(st.border_width, [1.0; 4]);
            let l2 = token(h, whole, "var(--l2)");
            assert_eq!(st.border_color[0].to_rgba8(), l2, "dark {dark}");
            let well = token(h, whole, "var(--well)");
            assert_eq!(bg(h, whole), well);
            let body = first(h, whole, "sf-section-body");
            let sb = h.doc.computed_style(body).unwrap();
            assert_eq!(sb.border_width[3], 0.0, "no tree hairline");
            let (wb, bb) = (
                h.doc.border_box(whole).unwrap(),
                h.doc.border_box(body).unwrap(),
            );
            assert!(
                (bb.x - (wb.x + 9.0)).abs() < 0.5,
                "no indent: {wb:?} {bb:?}"
            );
            let head = first(h, whole, "sf-section-head");
            assert_eq!(h.doc.computed_style(head).unwrap().font_size, 12.0);
            // Hover lifts an unfocused tile.
            let l3 = token(h, whole, "var(--l3)");
            h.doc.set_state(whole, ElementState::HOVER, true);
            h.doc.update();
            assert_eq!(
                h.doc.computed_style(whole).unwrap().border_color[0].to_rgba8(),
                l3
            );
            h.doc.set_state(whole, ElementState::HOVER, false);
            h.doc.update();
            // The focused block: the picker's accent ring, its head in --accent-ink.
            let accent = token(h, focused, "var(--accent)");
            let fs = h.doc.computed_style(focused).unwrap();
            assert_eq!(fs.border_color[0].to_rgba8(), accent);
            let ring = fs.box_shadow.as_ref().expect("a ring");
            assert!(ring[0].inset && ring[0].spread == 1.0, "{ring:?}");
            let fhead = first(h, focused, "sf-section-head");
            let ink = token(h, fhead, "var(--accent-ink)");
            assert_eq!(fg(h, fhead), ink);
            still(h, whole);
            still(h, focused);
            colours.push((l2, well, accent));
        }
        set_dark(h, true);
        seen.push(colours);
    });
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    let [colours] = &seen[..] else {
        panic!("s12 once: {}", seen.len())
    };
    assert_ne!(colours[0].0, colours[1].0, "--l2 follows the theme");
    assert_ne!(colours[0].1, colours[1].1, "--well follows the theme");
}

/// T-P3 (constructed: the agents hub's preview is unrecorded): the system prompt's body stops
/// at 14 lines and scrolls.
#[test]
fn omp_agents_prompt_scrolls_after_fourteen_lines() {
    let text: String = (0..60).map(|i| format!("line {i}\n")).collect();
    let h = omp_surface(
        "flow",
        Config::default(),
        json!([{"id": "p", "k": "section", "p": {"head": "System prompt", "role": "omp.agents.prompt"},
            "c": [{"id": "p.c", "k": "code", "p": {"lang": "md", "wrap": true, "text": text}}]}]),
        None,
        true,
    );
    let p = h.el("s1", "p");
    let body = first(&h, p, "sf-section-body");
    let lh = var_px(&h, body, "--sf-lh");
    let r = h.doc.border_box(body).unwrap();
    assert!(r.h <= 14.0 * lh + 0.5, "{r:?} at lh {lh}");
    assert!(r.h >= 10.0 * lh, "{r:?}");
    let st = h.doc.computed_style(body).unwrap();
    assert_eq!(format!("{:?}", st.overflow_y), "Auto");
    still(&h, body);
}

/// T-P4: the model hub (`fixtures/tsp/omp-picker.jsonl`, s11: the list focused, a query typed)
/// in an unfocused pane: no caret, and the selection loses its accent edge for an --l2 fill.
#[test]
fn omp_picker_in_an_unfocused_pane_hides_its_caret_and_quiets_the_selection() {
    let mut seen = false;
    let h = replay("omp-picker.jsonl", |h, rec| {
        if rec.verb != "f" || rec.body["s"] != 11 {
            return;
        }
        h.view.set_cols(&mut h.doc, &h.pane, 120);
        h.now += 1_000.0;
        h.frame();
        let pk = h.el("s:1", "v");
        let sheet = first(h, pk, "pk-sheet");
        assert!(
            h.class(sheet).split(' ').any(|c| c == "f-list"),
            "{}",
            h.class(sheet)
        );
        let caret = first(h, pk, "caret");
        let sel = *h
            .find_class(pk, "sel")
            .iter()
            .find(|&&r| h.class(r).contains("pk-row"))
            .expect("a selected row");
        let visible =
            |h: &Harness, el| format!("{:?}", h.doc.computed_style(el).unwrap().visibility);
        assert_eq!(visible(h, caret), "Visible");
        assert!(
            h.doc.computed_style(sel).unwrap().box_shadow.is_some(),
            "the accent edge"
        );
        h.view.set_focused(&mut h.doc, &h.pane, false);
        h.frame();
        assert!(h.class(h.doc.parent(pk).unwrap()).contains("sf-unfocused"));
        assert_eq!(visible(h, caret), "Hidden");
        let st = h.doc.computed_style(sel).unwrap();
        assert!(st.box_shadow.is_none(), "{:?}", st.box_shadow);
        let l2 = token(h, sel, "var(--l2)");
        assert_eq!(bg(h, sel), l2);
        h.view.set_focused(&mut h.doc, &h.pane, true);
        h.frame();
        assert_eq!(visible(h, caret), "Visible");
        seen = true;
    });
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    assert!(seen);
}

/// The card fallback's Switch Model card as omp 18.8.5 sent it with `picker` advertised
/// (`long_model.jsonl` line 1096, the list cut to two models).
fn model_picker_card() -> Value {
    let item = |id: &str, name: &str| {
        json!({"id": format!("w.list/{id}"), "k": "item", "p": {"label": [{"t": "xai-oauth/", "s": "dim"}, {"t": name}],
            "value": [{"t": "46  2.4s 32t/s  500k", "s": "dim"}, {"t": " $2/6", "s": "dim"}]}})
    };
    json!([{"id": "layer.ov-v", "k": "overlay", "p": {"anchor": "bottom", "size": "lg", "modal": true}, "c": [
        {"id": "v", "k": "card", "p": {"role": "omp.overlay.model-picker", "head": "Switch Model"}, "c": [
            {"id": "v.0", "k": "text", "p": {"wrap": "word", "spans": [{"t": "Session-only switch — role models stay unchanged", "s": "muted"}]}},
            {"id": "w", "k": "col", "p": {"gap": "sm"}, "c": [
                {"id": "w.0", "k": "row", "p": {"gap": "sm", "align": "center"}, "c": [
                    {"id": "w.0/0", "k": "text", "p": {"spans": [{"t": "\u{f002}", "s": "accent icon"}]}},
                    {"id": "x", "k": "input", "p": {"text": "", "cursor": 0, "sendable": false, "prompt": "> "}}]},
                {"id": "w.list", "k": "list", "p": {"role": "omp.model-browser.list", "selected": "w.list/a", "virtual": true, "grow": 1},
                    "c": [item("a", "grok-4.7"), item("b", "grok-4.6")]},
                {"id": "w.detail", "k": "col", "p": {"role": "omp.model-browser.detail", "gap": "none"}, "c": [
                    {"id": "w.detail/0", "k": "row", "p": {"gap": "sm", "align": "center", "wrap": true}, "c": [
                        {"id": "w.detail/0/0", "k": "text", "p": {"spans": [{"t": "Grok 4.7", "s": "strong"}]}}]},
                    {"id": "w.detail/1", "k": "text", "p": {"wrap": "word", "spans": [{"t": "500k ctx · 500k out · $2/6 per M", "s": "muted"}]}}]}]},
            {"id": "v.hints", "k": "row", "p": {"role": "omp.overlay.hints", "gap": "md", "wrap": true}, "c": [
                {"id": "v.hints/4", "k": "row", "p": {"gap": "xs", "align": "center"}, "c": [
                    {"id": "v.hints/4/0", "k": "kbd", "p": {"keys": ["escape"]}},
                    {"id": "v.hints/4/1", "k": "text", "p": {"spans": [{"t": "close", "s": "muted"}]}}]}]}]}]}])
}

/// T-P5 (constructed from `long_model.jsonl`:1096; the recording isn't in fixtures/): the list
/// keeps ten lines to browse, and the detail is set off by an --l2 hairline with 8/10 padding.
#[test]
fn omp_model_browser_card_keeps_a_browsable_list_and_a_ruled_detail() {
    let mut colours = Vec::new();
    for dark in [true, false] {
        let mut h = omp_surface(
            "inline",
            Config::default(),
            json!([{"id": "t", "k": "text", "p": {"text": "transcript"}}]),
            Some(model_picker_card()),
            dark,
        );
        let list = h.el("s1", "w.list");
        let lh = var_px(&h, list, "--sf-lh");
        let r = h.doc.border_box(list).unwrap();
        assert!(r.h >= 10.0 * lh - 0.5, "{r:?} at lh {lh}");
        let detail = h.el("s1", "w.detail");
        let st = h.doc.computed_style(detail).unwrap().clone();
        assert_eq!(st.border_width[0], 1.0);
        assert_eq!(px(&st.padding[0]), "Px(8.0)");
        assert_eq!(px(&st.padding[3]), "Px(10.0)");
        let l2 = token(&mut h, detail, "var(--l2)");
        assert_eq!(st.border_color[0].to_rgba8(), l2);
        let d = h.doc.border_box(detail).unwrap();
        assert!(
            d.y >= r.y + r.h - 0.5,
            "the detail sits under the list: {r:?} {d:?}"
        );
        still(&h, list);
        still(&h, detail);
        colours.push(l2);
    }
    assert_ne!(colours[0], colours[1]);
}

// ---------------------------------------------------------------------------------------------
// omp-prefs.css

/// T-R1: omp's /settings (`fixtures/tsp/omp-settings.jsonl` s6): the status-line preview is a
/// well in the terminal font; its strip is 24px tall at the meta size.
#[test]
fn omp_settings_status_preview_is_a_terminal_font_well() {
    let mut seen = false;
    let h = replay("omp-settings.jsonl", |h, rec| {
        if rec.verb != "f" || rec.body["sf"] != "s:1" || rec.body["s"] != 6 {
            return;
        }
        let pf = settings_sheet(h);
        let col = by_role(h, pf, "omp.prefs.preview.status");
        assert!(h.class(h.doc.parent(col).unwrap()).contains("pf-preview"));
        let st = h.doc.computed_style(col).unwrap();
        assert!(st.font_family.contains("Menlo"), "{}", st.font_family);
        assert_eq!(st.border_width, [1.0; 4]);
        let page = token(h, col, "var(--page)");
        assert_eq!(bg(h, col), page);
        let strip = first(h, col, "sf-status");
        assert_eq!(h.attr(strip, "data-role").as_deref(), Some("omp.status"));
        let ss = h.doc.computed_style(strip).unwrap();
        assert_eq!(ss.font_size, 11.5);
        assert!(h.doc.border_box(strip).unwrap().h >= 23.5);
        let t3 = token(h, strip, "var(--t3)");
        assert_eq!(fg(h, strip), t3);
        still(h, col);
        still(h, strip);
        seen = true;
    });
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    assert!(seen);
}

/// A screen surface whose `main` is a prefs page (`pf`) with `kids`.
fn prefs_page(kids: Value, dark: bool) -> Harness {
    omp_surface(
        "screen",
        prefs_config(),
        json!([{"id": "pf", "k": "prefs", "p": page(), "c": kids}]),
        None,
        dark,
    )
}

/// T-R2 (constructed): a card in the editor column drops its frame (the glass card is it), and
/// the column is terminal content.
#[test]
fn omp_prefs_editor_card_child_drops_its_frame() {
    let h = prefs_page(
        json!([{"id": "ed", "k": "col", "p": {"role": "omp.prefs.editor"}, "c": [
            {"id": "c", "k": "card", "p": {"head": "Edit"}, "c": [{"id": "c.t", "k": "text", "p": {"text": "value"}}]}]}]),
        true,
    );
    let ed = h.el("s1", "ed");
    assert!(h.class(h.doc.parent(ed).unwrap()).contains("pf-editor"));
    assert!(
        h.doc
            .computed_style(ed)
            .unwrap()
            .font_family
            .contains("Menlo")
    );
    let card = h.el("s1", "c");
    let st = h.doc.computed_style(card).unwrap();
    assert!(
        st.border_color.iter().all(|c| c.to_rgba8()[3] == 0),
        "{:?}",
        st.border_color
    );
    assert_eq!(bg(&h, card)[3], 0);
    still(&h, ed);
    still(&h, card);
}

/// T-R3: a picker child of a prefs sheet in the layer is framed by the editor card: no
/// backdrop, the sheet inside the card. surface-prefs.css draws this (A23); the omp sheets must
/// leave it whole (T-D1b).
#[test]
fn omp_prefs_picker_child_fills_the_editor_card_without_a_backdrop() {
    let items: Vec<Value> = (0..30)
        .map(|i| json!({"id": format!("t{i}"), "label": format!("theme {i}")}))
        .collect();
    let mut h = omp_surface(
        "inline",
        prefs_config(),
        json!([{"id": "t", "k": "text", "p": {"text": "transcript"}}]),
        Some(json!([{"id": "pf", "k": "prefs", "p": page(), "c": [
            {"id": "pk", "k": "picker", "p": {"size": "md", "title": "Theme", "query": "", "items": items, "selected": "t3"}}]}])),
        true,
    );
    let pf = h.el("s1", "pf");
    let card = first(&h, pf, "pf-editor");
    assert_eq!(h.class(card), "pf-editor on pk");
    let picker = h.el("s1", "pk");
    let dim = first(&h, picker, "pk-dim");
    assert_eq!(
        format!("{:?}", h.doc.computed_style(dim).unwrap().display),
        "None"
    );
    let c = h.doc.border_box(card).unwrap();
    let s = h.doc.border_box(first(&h, picker, "pk-sheet")).unwrap();
    assert!(
        s.x >= c.x - 1.0
            && s.y >= c.y - 1.0
            && s.x + s.w <= c.x + c.w + 1.0
            && s.y + s.h <= c.y + c.h + 1.0,
        "sheet {s:?} in card {c:?}"
    );
    // No backdrop pixels: the bar's empty middle reads the same as with no dim layer, and a
    // dim layer forced on would darken it.
    let fill = h.doc.border_box(first(&h, picker, "pk-bar-fill")).unwrap();
    let (x, y) = (fill.x + fill.w / 2.0, fill.y + fill.h / 2.0);
    let (w, ht) = (960u32, 800u32);
    let a = pixel(&render(&h.doc, w, ht), w, x, y);
    h.doc.set_attribute(dim, "style", "display: block");
    h.doc.update();
    let b = pixel(&render(&h.doc, w, ht), w, x, y);
    let lum = |p: [u8; 3]| p.iter().map(|&v| v as u32).sum::<u32>();
    assert!(
        lum(b) + 6 < lum(a),
        "a dim layer would darken {a:?} to {b:?}"
    );
}

/// T-R4, the engine check: under "Use program colors" (`.sf-pal`) a custom property naming
/// another resolves per element at computed time, so the kit's accents follow omp's palette
/// (`fixtures/tsp/omp-settings.jsonl` s6): an on switch fills with the program's fill; without
/// the setting, with the theme's.
#[test]
fn omp_settings_switches_follow_the_program_accent_under_program_colors() {
    let mut seen = false;
    let h = replay("omp-settings.jsonl", |h, rec| {
        if rec.verb != "f" || rec.body["sf"] != "s:1" || rec.body["s"] != 6 {
            return;
        }
        let pf = settings_sheet(h);
        let region = h.doc.parent(pf).unwrap();
        assert!(h.class(region).contains("sf-pal"), "{}", h.class(region));
        let sw = *h
            .find_class(pf, "pf-sw")
            .iter()
            .find(|&&s| h.class(s).split(' ').any(|c| c == "on"))
            .expect("an on switch");
        let fill = var(h, sw, "--pf-accent-fill");
        assert!(fill.starts_with('#'), "{fill:?}");
        let b = bg(h, sw);
        assert_eq!(format!("#{:02x}{:02x}{:02x}", b[0], b[1], b[2]), fill);
        let theme = token(h, region, "var(--accent-fill)");
        assert_ne!(b, theme, "the program's fill, not the theme's");
        let s = ViewSettings {
            program_colors: false,
            ..h.view.settings().clone()
        };
        h.view.set_settings(&mut h.doc, &h.pane, s);
        // The switch's fill transitions: let it finish.
        h.frame();
        h.now += 1_000.0;
        h.frame();
        assert!(!h.class(region).contains("sf-pal"));
        let theme = token(h, region, "var(--accent-fill)");
        assert_eq!(bg(h, sw), theme);
        let s = ViewSettings {
            program_colors: true,
            ..h.view.settings().clone()
        };
        h.view.set_settings(&mut h.doc, &h.pane, s);
        h.frame();
        seen = true;
    });
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    assert!(seen);
}

/// T-R5 (constructed: the fallback list shows only without `prefs`): a heading item isn't
/// dimmed and reads as a section head; the search row is a well. In both themes.
#[test]
fn omp_settings_fallback_headings_are_section_heads_not_dimmed_items() {
    let mut colours = Vec::new();
    for dark in [true, false] {
        let mut h = omp_surface(
            "flow",
            Config::default(),
            json!([{"id": "set", "k": "col", "p": {"role": "omp.settings", "gap": "sm"}, "c": [
                {"id": "q", "k": "row", "p": {"role": "omp.settings.search", "gap": "sm", "align": "center"}, "c": [
                    {"id": "q.i", "k": "text", "p": {"spans": [{"t": "\u{f002}", "s": "accent icon"}]}},
                    {"id": "q.in", "k": "input", "p": {"text": "", "cursor": 0}},
                    {"id": "q.n", "k": "text", "p": {"text": "42"}}]},
                {"id": "l", "k": "list", "c": [
                    {"id": "h", "k": "item", "p": {"role": "omp.settings.heading", "disabled": true,
                        "label": [{"t": "Appearance", "s": "strong"}]}},
                    {"id": "i", "k": "item", "p": {"label": "Theme", "value": "titanium"}}]},
                {"id": "hint", "k": "text", "p": {"spans": [{"t": "esc to close", "s": "muted"}]}}]}]),
            None,
            dark,
        );
        let set = h.el("s1", "set");
        assert_eq!(
            px(&h.doc.computed_style(set).unwrap().row_gap),
            "Some(Px(6.0))"
        );
        let head = h.el("s1", "h");
        assert!(h.class(head).contains("disabled"), "{}", h.class(head));
        assert_eq!(h.doc.computed_style(head).unwrap().opacity, 1.0);
        let label = first(&h, head, "sf-item-label");
        let st = h.doc.computed_style(label).unwrap();
        assert_eq!(st.font_size, 11.0);
        assert_eq!(format!("{:?}", st.text_transform), "Uppercase");
        let t3 = token(&mut h, label, "var(--t3)");
        assert_eq!(fg(&h, label), t3);
        h.doc.set_state(head, ElementState::HOVER, true);
        h.doc.update();
        assert_eq!(bg(&h, head)[3], 0, "no hover fill on a heading");
        let q = h.el("s1", "q");
        assert!((h.doc.border_box(q).unwrap().h - 28.0).abs() < 0.5);
        let well = token(&mut h, q, "var(--well)");
        assert_eq!(bg(&h, q), well);
        still(&h, head);
        still(&h, q);
        colours.push((t3, well));
    }
    assert_ne!(colours[0].0, colours[1].0);
    assert_ne!(colours[0].1, colours[1].1);
}

/// R6 (constructed): the composer-shape sample takes no pointer; the snapcompact image is
/// framed by a hairline.
#[test]
fn omp_shape_previews_are_inert_samples() {
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
    let h = omp_surface(
        "flow",
        Config::default(),
        json!([
            {"id": "cs", "k": "col", "p": {"role": "omp.preview.composer-shape", "gap": "xs"}, "c": [
                {"id": "cs.0", "k": "text", "p": {"spans": [{"t": "Preview:", "s": "muted"}]}},
                {"id": "cs.e", "k": "editor", "p": {"text": "hello", "readonly": true, "maxLines": 1}},
                {"id": "cs.l", "k": "text", "p": {"text": "boxed"}}]},
            {"id": "sc", "k": "col", "p": {"role": "omp.preview.snapcompact-shape"}, "c": [
                {"id": "sc.0", "k": "text", "p": {"text": "caption"}},
                {"id": "sc.i", "k": "image", "p": {"src": format!("data:image/png;base64,{png}"), "w": 8, "h": 8}}]}]),
        None,
        true,
    );
    let ed = h.el("s1", "cs.e");
    assert!(h.class(ed).contains("readonly"), "{}", h.class(ed));
    assert!(!h.doc.computed_style(ed).unwrap().pointer_events);
    let img = h.el("s1", "sc.i");
    assert_eq!(h.doc.computed_style(img).unwrap().border_width, [1.0; 4]);
    still(&h, img);
}

/// T-R7: omp's /settings (`fixtures/tsp/omp-settings.jsonl`): unfocused, the focus ring is a
/// quiet --l3 ring (s6) and the text draft's caret is hidden (s13); under reduce motion
/// (`.sf-still`) the switches snap.
#[test]
fn omp_settings_unfocused_quiets_the_ring_and_still_stops_the_switches() {
    let mut seen = Vec::new();
    let h = replay("omp-settings.jsonl", |h, rec| {
        if rec.verb != "f" || rec.body["sf"] != "s:1" {
            return;
        }
        let s = rec.body["s"].as_u64().unwrap();
        if s == 6 {
            let pf = settings_sheet(h);
            let ring = first(h, pf, "focus");
            assert!(h.class(ring).contains("pf-row"), "{}", h.class(ring));
            let accent = token(h, ring, "var(--accent)");
            let shadow = |h: &Harness| {
                h.doc
                    .computed_style(ring)
                    .unwrap()
                    .box_shadow
                    .as_ref()
                    .unwrap()[0]
                    .color
                    .to_rgba8()
            };
            assert_eq!(shadow(h), accent);
            h.view.set_focused(&mut h.doc, &h.pane, false);
            h.frame();
            let l3 = token(h, ring, "var(--l3)");
            assert_eq!(shadow(h), l3);
            h.view.set_focused(&mut h.doc, &h.pane, true);
            h.frame();
            // Still: the switches and their knobs don't slide.
            let sw = first(h, pf, "pf-sw");
            let knob = first(h, sw, "knob");
            let moving = |h: &Harness, el| {
                h.doc
                    .computed_style(el)
                    .unwrap()
                    .transitions
                    .as_ref()
                    .is_some_and(|t| t.iter().any(|x| x.duration > 0.0))
            };
            assert!(moving(h, sw) && moving(h, knob));
            let st = ViewSettings {
                reduce_motion: true,
                ..h.view.settings().clone()
            };
            h.view.set_settings(&mut h.doc, &h.pane, st);
            h.frame();
            assert!(!moving(h, sw) && !moving(h, knob));
            let st = ViewSettings {
                reduce_motion: false,
                ..h.view.settings().clone()
            };
            h.view.set_settings(&mut h.doc, &h.pane, st);
            h.frame();
            seen.push(s);
        }
        if s == 13 {
            let pf = settings_sheet(h);
            let caret = first(h, pf, "pf-caret");
            let vis =
                |h: &Harness| format!("{:?}", h.doc.computed_style(caret).unwrap().visibility);
            assert_eq!(vis(h), "Visible");
            h.view.set_focused(&mut h.doc, &h.pane, false);
            h.frame();
            assert_eq!(vis(h), "Hidden");
            h.view.set_focused(&mut h.doc, &h.pane, true);
            h.frame();
            seen.push(s);
        }
    });
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    assert_eq!(seen, [6, 13]);
}

// ---------------------------------------------------------------------------------------------
// Offscreen rendering (as in tests/prefs.rs)

fn render(doc: &Document, w: u32, h: u32) -> Vec<u8> {
    let mut scene = vello::Scene::new();
    auk_ui::paint(doc, &mut scene);
    let instance =
        vello::wgpu::Instance::new(vello::wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(
        instance.request_adapter(&vello::wgpu::RequestAdapterOptions::default()),
    )
    .expect("a wgpu adapter for offscreen rendering");
    let (device, queue) =
        pollster::block_on(adapter.request_device(&vello::wgpu::DeviceDescriptor::default()))
            .expect("a wgpu device");
    let mut renderer =
        vello::Renderer::new(&device, vello::RendererOptions::default()).expect("a renderer");
    let texture = device.create_texture(&vello::wgpu::TextureDescriptor {
        label: Some("target"),
        size: vello::wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: vello::wgpu::TextureDimension::D2,
        format: vello::wgpu::TextureFormat::Rgba8Unorm,
        usage: vello::wgpu::TextureUsages::STORAGE_BINDING | vello::wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    renderer
        .render_to_texture(
            &device,
            &queue,
            &scene,
            &view,
            &vello::RenderParams {
                base_color: vello::peniko::Color::new([1.0, 1.0, 1.0, 1.0]),
                width: w,
                height: h,
                antialiasing_method: vello::AaConfig::Area,
            },
        )
        .expect("render");
    let padded = (w * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&vello::wgpu::BufferDescriptor {
        label: None,
        size: (padded * h) as u64,
        usage: vello::wgpu::BufferUsages::MAP_READ | vello::wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        texture.as_image_copy(),
        vello::wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: vello::wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: None,
            },
        },
        vello::wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([enc.finish()]);
    let slice = buffer.slice(..);
    slice.map_async(vello::wgpu::MapMode::Read, |_| {});
    device
        .poll(vello::wgpu::PollType::wait_indefinitely())
        .expect("poll");
    let data = slice.get_mapped_range().expect("mapped");
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for row in 0..h {
        let start = (row * padded) as usize;
        out.extend_from_slice(&data[start..start + (w * 4) as usize]);
    }
    out
}

fn pixel(px: &[u8], w: u32, x: f32, y: f32) -> [u8; 3] {
    let i = ((y as u32 * w + x as u32) * 4) as usize;
    [px[i], px[i + 1], px[i + 2]]
}
