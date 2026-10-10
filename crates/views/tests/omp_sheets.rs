//! omp's sheets (amendment A24; research/omp-stylesheets.md §16.1-16.2): they parse, sit in Tern's
//! slot, keep every rule in a class bucket, and give omp's shared roles their look.

mod common;

use auk_ui::css::values::LengthPct;
use auk_ui::{ElementState, NodeId};
use auk_views::sheets::{BUILTIN, builtin_css};
use common::*;
use serde_json::{Value, json};

/// The omp sheets in Tern's order (book: Stylesheets), after the kit's and before the chrome.
const OMP: [&str; 11] = [
    "omp",
    "omp-pickers",
    "omp-prefs",
    "omp-transcript",
    "omp-tools",
    "omp-composer",
    "omp-panels",
    "omp-apps",
    "omp-chat-reader",
    "omp-chat-spine",
    "omp-chat-console",
];

#[test]
fn omp_sheets_parse_and_sit_between_the_kit_and_the_chrome() {
    for dark in [false, true] {
        let mut doc = auk_ui::Document::new();
        let errors = auk_views::install_sheets(&mut doc, dark);
        assert!(errors.is_empty(), "{errors:?}");
    }
    let names: Vec<&str> = BUILTIN.iter().map(|(n, _)| *n).collect();
    let at = names.iter().position(|n| *n == "surface-prefs").unwrap() + 1;
    assert_eq!(names[at..at + OMP.len()], OMP, "{names:?}");
    assert_eq!(names[at + OMP.len()..], ["chrome"]);
    let css = builtin_css();
    let starts: Vec<usize> = OMP
        .iter()
        .map(|n| css.find(&format!("/* {n} */\n")).unwrap())
        .collect();
    assert!(
        starts.windows(2).all(|w| w[0] < w[1]),
        "serialized pages keep the order"
    );
}

/// The selectors of a sheet's style rules, outside `@keyframes`.
fn selectors(css: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = css;
    // Comments out first; then walk blocks, descending into at-rules other than keyframes.
    let mut clean = String::new();
    while let Some(i) = rest.find("/*") {
        clean.push_str(&rest[..i]);
        rest = &rest[i + 2..];
        rest = &rest[rest.find("*/").map_or(rest.len(), |j| j + 2)..];
    }
    clean.push_str(rest);
    fn walk(s: &str, out: &mut Vec<String>) {
        let mut rest = s;
        while let Some(open) = rest.find('{') {
            let head = rest[..open].trim().to_owned();
            let mut depth = 0;
            let mut close = open;
            for (i, c) in rest[open..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            close = open + i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let body = &rest[open + 1..close];
            if head.starts_with("@keyframes") {
            } else if head.starts_with('@') {
                walk(body, out);
            } else {
                out.extend(head.split(',').map(|s| s.trim().to_owned()));
            }
            rest = &rest[close + 1..];
        }
    }
    walk(&clean, &mut out);
    out
}

/// The cascade buckets rules by the rightmost compound's first class (G1): a rule that starts
/// with an attribute, a pseudo-class, `*` or the `.sf` every node has is tried on every element.
/// Pseudo-element rules sit in the universal bucket anyway and are exempt.
#[test]
fn every_omp_rule_sits_in_a_class_bucket() {
    let mut bad = Vec::new();
    for (name, css) in BUILTIN.iter().filter(|(n, _)| OMP.contains(n)) {
        for sel in selectors(css) {
            if sel.contains("::") {
                continue;
            }
            let last = sel
                .rsplit([' ', '>', '+', '~'])
                .find(|s| !s.is_empty())
                .unwrap_or_default();
            let class = last
                .strip_prefix('.')
                .map(|c| c.split(['.', '[', ':']).next().unwrap_or_default());
            if !matches!(class, Some(c) if !c.is_empty() && c != "sf") {
                bad.push(format!("{name}: {sel}"));
            }
        }
    }
    assert!(bad.is_empty(), "rules outside a class bucket: {bad:#?}");
}

#[test]
fn the_selector_walk_sees_rules_in_at_rules_and_skips_keyframes() {
    let css = "/* a { } */ .a, .b > [x] { c: d } @media (min-width: 1px) { .c:hover { e: f } } \
               @keyframes k { from { g: h } }";
    assert_eq!(selectors(css), [".a", ".b > [x]", ".c:hover"]);
}

/// A pane with an omp session surface holding `main`.
fn omp_surface(main: Value, dark: bool) -> Harness {
    omp_surface_as("omp.session", main, dark)
}

/// A pane with an omp surface of `role` holding `main`.
fn omp_surface_as(role: &str, main: Value, dark: bool) -> Harness {
    let mut h = Harness::with(
        Default::default(),
        auk_views::ViewSettings {
            dark,
            ..Default::default()
        },
    );
    h.hello(&[]);
    h.apc(
        "o",
        &json!({"id": "s1", "mode": "flow", "role": role}).to_string(),
    );
    let frame = json!({"sf": "s1", "s": 1, "ops": [["add", "main", "s1", null,
        {"id": "main", "k": "col", "c": main}]]});
    h.apc("f", &frame.to_string());
    h.frame();
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    h
}

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
    c
}

fn bg(h: &Harness, el: NodeId) -> [u8; 4] {
    h.doc
        .computed_style(el)
        .unwrap()
        .background_color
        .to_rgba8()
}

fn var(h: &Harness, el: NodeId, name: &str) -> String {
    let st = h.doc.computed_style(el).unwrap();
    st.custom
        .get(name)
        .map(|v| v.trim().to_owned())
        .unwrap_or_default()
}

fn btn(id: &str, tone: Option<&str>) -> Value {
    let mut p =
        json!({"role": "omp.btn", "gap": "xs", "align": "center", "actions": {"click": id}});
    if let Some(t) = tone {
        p["tone"] = t.into();
    }
    json!({"id": id, "k": "row", "p": p, "c": [{"id": format!("{id}.t"), "k": "text", "p": {"text": "Close"}}]})
}

/// T1.1: omp's regions carry the omp tokens; other programs' don't.
#[test]
fn omp_regions_carry_the_omp_tokens() {
    let h = omp_surface(json!([{"id": "t", "k": "text", "p": {"text": "hi"}}]), true);
    let main = h.el("s1", "main");
    assert!(
        h.class(main).starts_with("sf-region sf-main"),
        "{}",
        h.class(main)
    );
    assert_eq!(h.attr(main, "data-surface").as_deref(), Some("omp.session"));
    assert_eq!(var(&h, main, "--omp-meta-fs"), "11.5px");
    assert_eq!(var(&h, main, "--omp-btn-h"), "26px");
    let mut other = Harness::new();
    other.hello(&[]);
    other.surface(
        "s1",
        "flow",
        json!([{"id": "t", "k": "text", "p": {"text": "hi"}}]),
    );
    other.frame();
    assert_eq!(var(&other, other.el("s1", "main"), "--omp-meta-fs"), "");
}

/// T1.2: the button idiom, at rest and hovered, and its accent and error tones, in both themes.
#[test]
fn omp_buttons_are_26px_on_a_quiet_fill_with_accent_and_error_tones() {
    for dark in [true, false] {
        let mut h = omp_surface(
            json!([{"id": "acts", "k": "row", "p": {"role": "omp.actions"}, "c": [
                btn("close", None), btn("ok", Some("accent")), btn("rm", Some("error"))]}]),
            dark,
        );
        let close = h.el("s1", "close");
        assert!(h.class(close).contains("sf-act"), "{}", h.class(close));
        let r = h.doc.border_box(close).unwrap();
        assert!((r.h - 26.0).abs() < 0.5, "26px tall: {r:?}");
        let st = h.doc.computed_style(close).unwrap();
        assert_eq!(st.font_size, 12.0);
        let ring = st.box_shadow.as_ref().expect("a ring");
        assert!(ring[0].inset, "{ring:?}");
        let (l1, l2, l3) = (
            token(&mut h, close, "var(--l1)"),
            token(&mut h, close, "var(--l2)"),
            token(&mut h, close, "var(--l3)"),
        );
        assert_eq!(bg(&h, close), l1, "dark {dark}");
        h.doc.set_state(close, ElementState::HOVER, true);
        h.doc.update();
        assert_eq!(bg(&h, close), l2, "hovered, dark {dark}");
        h.doc.set_state(close, ElementState::ACTIVE, true);
        h.doc.update();
        assert_eq!(bg(&h, close), l3, "pressed, dark {dark}");

        let ok = h.el("s1", "ok");
        let fill = token(&mut h, ok, "var(--accent-fill)");
        assert_eq!(bg(&h, ok), fill);
        assert_eq!(
            h.doc.computed_style(ok).unwrap().color.to_rgba8(),
            [255, 255, 255, 255]
        );
        let rm = h.el("s1", "rm");
        let bad = token(&mut h, rm, "var(--bad)");
        assert_eq!(h.doc.computed_style(rm).unwrap().color.to_rgba8(), bad);
        assert!(
            bg(&h, rm)[3] > 0 && bg(&h, rm)[3] < 64,
            "a 10% tint: {:?}",
            bg(&h, rm)
        );
    }
}

/// T1.4: the quiet control: a 20px seg with 6px sides, whose hover fills with --l1.
#[test]
fn omp_quiet_controls_are_20px_segs_that_fill_on_hover() {
    let mut h = omp_surface(
        json!([{"id": "stop", "k": "row", "p": {"role": "omp.working.stop", "actions": {"click": "stop"}},
            "c": [{"id": "stop.t", "k": "text", "p": {"text": "esc to interrupt"}}]}]),
        true,
    );
    let stop = h.el("s1", "stop");
    let r = h.doc.border_box(stop).unwrap();
    assert!((r.h - 20.0).abs() < 0.5, "{r:?}");
    let st = h.doc.computed_style(stop).unwrap();
    assert_eq!(
        format!("{:?}", st.padding[1]),
        format!("{:?}", st.padding[3])
    );
    assert!(
        format!("{:?}", st.padding[1]).contains("6.0"),
        "{:?}",
        st.padding
    );
    assert_eq!(bg(&h, stop)[3], 0, "no fill at rest");
    let (l1, t1) = (
        token(&mut h, stop, "var(--l1)"),
        token(&mut h, stop, "var(--t1)"),
    );
    h.doc.set_state(stop, ElementState::HOVER, true);
    h.doc.update();
    assert_eq!(bg(&h, stop), l1);
    assert_eq!(h.doc.computed_style(stop).unwrap().color.to_rgba8(), t1);
}

/// T1.3 (constructed): an overlay's hint row is meta-sized, muted, and set off by a hairline.
#[test]
fn omp_overlay_hints_are_muted_meta_text_under_a_hairline() {
    let mut h = omp_surface(
        json!([{"id": "hints", "k": "row", "p": {"role": "omp.overlay.hints", "gap": "md", "wrap": true}, "c": [
            {"id": "h1", "k": "row", "c": [{"id": "h1.k", "k": "text", "p": {"text": "esc"}},
                {"id": "h1.t", "k": "text", "p": {"text": "close"}}]}]}]),
        false,
    );
    let hints = h.el("s1", "hints");
    let st = h.doc.computed_style(hints).unwrap();
    assert_eq!(st.font_size, 11.5);
    assert!(
        format!("{:?}", st.padding[0]).contains("8.0"),
        "{:?}",
        st.padding
    );
    assert!(st.box_shadow.is_some());
    let t3 = token(&mut h, hints, "var(--t3)");
    assert_eq!(h.doc.computed_style(hints).unwrap().color.to_rgba8(), t3);
}

// omp-transcript.css and omp-tools.css (§16.3-16.4), in both themes (T6.1) and still (T6.2).

/// A program palette for constructed surfaces (omp's own recorded one is titanium, F:4).
const PALETTE: &str = r##"{"dark":{"userMessageBg":"#203a5c","customMessageBg":"#5c2040"},"light":{"userMessageBg":"#c8dcff","customMessageBg":"#ffc8dc"}}"##;

/// omp's recorded session (`fixtures/tsp/omp-session.jsonl`, surface `s:1`) replayed in dark,
/// with `at` called after each line (1-based). `palette` is "Use program colors": omp sends its
/// palette (titanium in dark) at F:4.
fn omp_session(palette: bool, at: impl FnMut(&mut Harness, usize)) -> Harness {
    omp_session_in(true, palette, at)
}

/// The same, in light or dark. The appearance is set before the replay: switching it later
/// re-renders every mount, which on a long session can reach a freed element.
fn omp_session_in(dark: bool, palette: bool, mut at: impl FnMut(&mut Harness, usize)) -> Harness {
    let mut line = 0;
    replay("omp-session.jsonl", |h, _| {
        line += 1;
        if line == 1 {
            let s = auk_views::ViewSettings {
                dark,
                program_colors: palette,
                ..Default::default()
            };
            h.view.set_settings(&mut h.doc, &h.pane, s);
        }
        at(h, line);
    })
}

fn color(h: &Harness, el: NodeId) -> [u8; 4] {
    h.doc.computed_style(el).unwrap().color.to_rgba8()
}

fn px(v: &LengthPct) -> f32 {
    match v {
        LengthPct::Px(v) => *v,
        other => panic!("not a length: {other:?}"),
    }
}

/// The content box's right edge.
fn inner_right(h: &Harness, el: NodeId) -> f32 {
    let r = h.doc.border_box(el).unwrap();
    let st = h.doc.computed_style(el).unwrap();
    let border = match st.border_style[1] {
        auk_ui::style::BorderStyle::None => 0.0,
        _ => st.border_width[1],
    };
    r.x + r.w - px(&st.padding[1]) - border
}

/// T6.2: the omp sheets add no transitions and no animations.
fn assert_still(h: &Harness, el: NodeId, what: &str) {
    let st = h.doc.computed_style(el).unwrap();
    assert!(
        st.transitions
            .as_ref()
            .is_none_or(|t| t.iter().all(|t| t.duration == 0.0)),
        "{what}: {:?}",
        st.transitions
    );
    assert!(st.animations.is_none(), "{what}: {:?}", st.animations);
}

/// T2.1-T2.3 (F:17): the user's message is a fill-only bubble at main's right edge, at most 85%
/// wide, with the time and Copy/Rewind in a 20px row at its foot, right-aligned, faint until the
/// bubble is hovered, and hovering moves nothing. With a palette the fill is userMessageBg.
#[test]
fn omp_user_message_is_a_right_aligned_bubble_with_its_tools_at_the_foot() {
    for palette in [false, true] {
        let mut fills = Vec::new();
        for dark in [false, true] {
            omp_session_in(dark, palette, |h, line| {
                if line != 17 {
                    return;
                }
                let at = format!("palette {palette}, dark {dark}");
                let (u, main) = (h.el("s:1", "u"), h.el("s:1", "main"));
                let st = h.doc.computed_style(u).unwrap();
                assert!(st.border_color.iter().all(|c| c.a == 0.0), "{at}: no ring");
                let ring = st.box_shadow.as_deref().map(|s| s[0]);
                let l1 = token(h, u, "var(--l1)");
                // Q-T2: only under a palette, a whisper ring.
                match ring {
                    None => assert!(!palette, "{at}"),
                    Some(s) => {
                        assert!(palette && s.inset && s.spread == 1.0, "{at}: {s:?}");
                        assert_eq!(s.color.to_rgba8(), l1, "{at}");
                    }
                }
                let want = if palette {
                    token(h, u, "var(--sf-tint-user)")
                } else {
                    l1
                };
                assert_eq!(bg(h, u), want, "{at}");
                if palette {
                    assert_ne!(want, l1, "{at}: the palette's fill");
                }
                fills.push(bg(h, u));
                let (ub, mb) = (
                    h.doc.border_box(u).unwrap(),
                    h.doc.border_box(main).unwrap(),
                );
                assert!(
                    (ub.x + ub.w - inner_right(h, main)).abs() < 0.5,
                    "{at}: at the right {ub:?} in {mb:?}"
                );
                assert!(ub.w <= 0.85 * mb.w + 0.5, "{at}: {ub:?} in {mb:?}");
                assert!(ub.x > mb.x + 8.0, "{at}: not full width");

                // T2.2: the tools row at the foot, right-aligned, 20px tall.
                let (tools, md) = (h.el("s:1", "u.tools"), h.el("s:1", "u.1"));
                let (tb, db) = (
                    h.doc.border_box(tools).unwrap(),
                    h.doc.border_box(md).unwrap(),
                );
                assert!(tb.y >= db.y + db.h - 0.5, "{at}: {tb:?} under {db:?}");
                assert!((tb.h - 20.0).abs() < 0.5, "{at}: {tb:?}");
                assert!(
                    (tb.x + tb.w - inner_right(h, u)).abs() <= 6.0,
                    "{at}: {tb:?} in {ub:?}"
                );
                let [time, copy, rewind] =
                    ["u.tools/0", "u.tools/1", "u.tools/2"].map(|id| h.el("s:1", id));
                let (t1, t3, t4, l2) = (
                    token(h, u, "var(--t1)"),
                    token(h, u, "var(--t3)"),
                    token(h, u, "var(--t4)"),
                    token(h, u, "var(--l2)"),
                );
                for el in [time, copy, rewind] {
                    assert_eq!(color(h, el), t4, "{at}: faint at rest");
                }
                let (cb, rb) = (
                    h.doc.border_box(copy).unwrap(),
                    h.doc.border_box(rewind).unwrap(),
                );
                assert!(
                    (rb.x + 6.0) - (cb.x + cb.w - 6.0) >= 12.0,
                    "{at}: labels apart: {cb:?} {rb:?}"
                );
                assert!((cb.h - 20.0).abs() < 0.5, "{at}: {cb:?}");

                // Hovering the bubble lifts the tools; hovering one fills it a step up.
                h.doc.set_state(u, ElementState::HOVER, true);
                h.doc.update();
                assert_eq!(color(h, copy), t3, "{at}");
                assert_eq!(color(h, rewind), t3, "{at}");
                assert_eq!(color(h, time), t4, "{at}");
                h.doc.set_state(copy, ElementState::HOVER, true);
                h.doc.update();
                assert_eq!(bg(h, copy), l2, "{at}");
                assert_eq!(color(h, copy), t1, "{at}");
                h.frame();
                // T2.3: the reveal moves nothing.
                assert_eq!(h.doc.border_box(u).unwrap(), ub, "{at}");
                h.doc.set_state(copy, ElementState::HOVER, false);
                h.doc.set_state(u, ElementState::HOVER, false);
                h.frame();
                for el in [u, tools, copy] {
                    assert_still(h, el, &at);
                }
            });
        }
        assert_eq!(fills.len(), 2, "palette {palette}");
        assert_ne!(fills[0], fills[1], "palette {palette}: themed");
    }
}

/// T2.4 and T2.6 (F:661, F:1093): an assistant turn's blocks sit 8px apart, and each finished
/// turn ends in a meta-sized, muted usage line with a 12px icon.
#[test]
fn omp_assistant_turns_breathe_and_end_in_a_quiet_usage_line() {
    let mut checked = Vec::new();
    omp_session(false, |h, line| {
        let usage = match line {
            661 => "13.turn-usage",
            1093 => "1f.turn-usage",
            _ => return,
        };
        checked.push(line);
        if line == 661 {
            let col = h.el("s:1", "13");
            let st = h.doc.computed_style(col).unwrap();
            assert_eq!(st.row_gap.as_ref().map(px), Some(8.0));
            let (k0, t1) = (
                h.doc.border_box(h.el("s:1", "13.k0")).unwrap(),
                h.doc.border_box(h.el("s:1", "13.t1")).unwrap(),
            );
            assert!((t1.y - (k0.y + k0.h) - 8.0).abs() < 0.5, "{k0:?} {t1:?}");
            assert_still(h, col, "13");
        }
        let row = h.el("s:1", usage);
        let st = h.doc.computed_style(row).unwrap();
        assert_eq!(st.font_size, 11.5, "{usage}");
        let t3 = token(h, row, "var(--t3)");
        assert_eq!(color(h, row), t3, "{usage}");
        let icon = h
            .doc
            .border_box(h.el("s:1", &format!("{usage}/icon")))
            .unwrap();
        assert!(
            (icon.w - 12.0).abs() < 0.5 && (icon.h - 12.0).abs() < 0.5,
            "{icon:?}"
        );
        assert_still(h, row, usage);
    });
    assert_eq!(checked, [661, 1093]);
}

/// T2.5 (F:47, F:153): thinking has a quiet 12px head; once finished it folds, and its thought
/// is italic in thinkingText (the palette's, else --t3).
#[test]
fn omp_thinking_folds_under_a_quiet_head_in_italic_thinking_text() {
    for palette in [false, true] {
        let mut checked = Vec::new();
        omp_session(palette, |h, line| {
            let sec = match line {
                47 | 153 => h.el("s:1", "11.k0"),
                _ => return,
            };
            checked.push(line);
            let head = h.find_class(sec, "sf-section-head")[0];
            assert_eq!(h.doc.computed_style(head).unwrap().font_size, 12.0);
            if line == 47 {
                return;
            }
            assert_eq!(h.attr(sec, "data-role").as_deref(), Some("omp.thinking"));
            let body = h.find_class(sec, "sf-section-body")[0];
            assert!(h.doc.border_box(body).is_none_or(|b| b.h == 0.0), "folded");
            let md = h.el("s:1", "11.k0/body");
            let st = h.doc.computed_style(md).unwrap();
            assert_eq!(st.font_style, auk_ui::style::FontStyle::Italic);
            let t3 = token(h, md, "var(--t3)");
            let want = if palette {
                token(h, md, "var(--sf-p-thinkingText)")
            } else {
                t3
            };
            assert_eq!(color(h, md), want, "palette {palette}");
            assert!(!palette || want != t3, "omp's thinkingText");
        });
        assert_eq!(checked, [47, 153]);
    }
}

/// T2.11 (F:5): the welcome card's lockup is 12px apart, and its tip sits under an --l1 rule.
#[test]
fn omp_welcome_card_sets_its_tip_under_a_hairline() {
    let mut seen = false;
    omp_session(false, |h, line| {
        if line != 5 {
            return;
        }
        seen = true;
        let lockup = h.el("s:1", "1.lockup");
        let st = h.doc.computed_style(lockup).unwrap();
        assert_eq!(st.column_gap.as_ref().map(px), Some(12.0));
        let tip = h.el("s:1", "1.tip");
        let l1 = token(h, tip, "var(--l1)");
        let st = h.doc.computed_style(tip).unwrap();
        assert_eq!(st.border_width[0], 1.0);
        assert_eq!(st.border_color[0].to_rgba8(), l1);
    });
    assert!(seen);
}

fn error_action(id: &str, label: &str) -> Value {
    json!({"id": id, "k": "row", "p": {"role": "omp.error.action", "gap": "xs", "actions": {"click": id}},
        "c": [{"id": format!("{id}.t"), "k": "text", "p": {"text": label}}]})
}

/// T2.7 (constructed, S:417729-417749): the error card's actions are 20px quiet controls, Retry
/// filled as the default.
#[test]
fn omp_error_actions_are_quiet_controls_with_retry_first() {
    for dark in [false, true] {
        let mut h = omp_surface(
            json!([{"id": "err", "k": "card", "p": {"role": "omp.error", "tone": "error"}, "c": [
                {"id": "err.msg", "k": "text", "p": {"role": "omp.error.message", "text": "overloaded"}},
                {"id": "err.acts", "k": "row", "p": {"role": "omp.error.actions", "gap": "sm"}, "c": [
                    error_action("retry", "Retry"), error_action("copy", "Copy error"),
                    error_action("switch", "Switch model")]}]}]),
            dark,
        );
        let err = h.el("s1", "err");
        let l1 = token(&mut h, err, "var(--l1)");
        for (i, id) in ["retry", "copy", "switch"].into_iter().enumerate() {
            let el = h.el("s1", id);
            let r = h.doc.border_box(el).unwrap();
            assert!((r.h - 20.0).abs() < 0.5, "{id}: {r:?}");
            let st = h.doc.computed_style(el).unwrap();
            assert_eq!((px(&st.padding[1]), px(&st.padding[3])), (6.0, 6.0));
            if i == 0 {
                assert_eq!(bg(&h, el), l1, "dark {dark}");
            } else {
                assert_eq!(bg(&h, el)[3], 0, "{id}");
            }
        }
        let msg = h.el("s1", "err.msg");
        assert_eq!(h.doc.computed_style(msg).unwrap().font_size, 12.0);
    }
}

/// T2.8 (constructed): under a palette a custom message takes customMessageBg, and a skill keeps
/// the user tint with an inset rail in the label colour.
#[test]
fn omp_custom_messages_take_their_tint_and_skills_a_rail() {
    for dark in [false, true] {
        let mut h = omp_surface(
            json!([
                {"id": "cu", "k": "card", "p": {"role": "omp.custom", "tone": "accent", "head": "note"},
                    "c": [{"id": "cu.t", "k": "text", "p": {"text": "from an extension"}}]},
                {"id": "sk", "k": "card", "p": {"role": "omp.skill", "tone": "user", "head": "skill"},
                    "c": [{"id": "sk.t", "k": "text", "p": {"text": "a skill"}}]}
            ]),
            dark,
        );
        h.apc("t", PALETTE);
        h.frame();
        let (cu, sk) = (h.el("s1", "cu"), h.el("s1", "sk"));
        let custom = token(&mut h, cu, "var(--sf-tint-custom)");
        assert!(custom[3] > 0, "the palette's tint");
        assert_eq!(bg(&h, cu), custom, "dark {dark}");
        let user = token(&mut h, sk, "var(--sf-tint-user)");
        assert_eq!(bg(&h, sk), user, "dark {dark}");
        let st = h.doc.computed_style(sk).unwrap();
        let rail = st.box_shadow.as_deref().expect("a rail")[0];
        assert!(rail.inset && rail.x == 2.0, "{rail:?}");
    }
}

/// T2.9 (constructed): a marker centres its label between two hairlines (their pixels are
/// checked in paint.rs).
#[test]
fn omp_markers_centre_their_label() {
    let h = omp_surface(
        json!([{"id": "mk", "k": "row", "p": {"role": "omp.marker.cache-miss", "gap": "sm"}, "c": [
            {"id": "mk.i", "k": "icon", "p": {"name": "warning"}},
            {"id": "mk.t", "k": "text", "p": {"text": "cache miss"}}]}]),
        true,
    );
    let mk = h.el("s1", "mk");
    let r = h.doc.border_box(mk).unwrap();
    let (i, t) = (
        h.doc.border_box(h.el("s1", "mk.i")).unwrap(),
        h.doc.border_box(h.el("s1", "mk.t")).unwrap(),
    );
    let mid = (i.x + t.x + t.w) / 2.0;
    assert!(
        (mid - (r.x + r.w / 2.0)).abs() < 1.0,
        "{i:?} {t:?} in {r:?}"
    );
    assert!(i.x > r.x + 40.0, "hairlines take the sides");
    assert_eq!(h.doc.computed_style(mk).unwrap().font_size, 11.5);
}

/// T2.10 (constructed, S:435738-435809): on the rewind screen the inactive branch is dimmed, the
/// accent one full; a picked prompt draws its bar as an inset shadow, a dropped one fades.
#[test]
fn omp_rewind_dims_other_branches_and_marks_picked_and_dropped_prompts() {
    let prompt = |id: &str, mark: &str| {
        json!({"id": id, "k": "card", "p": {"role": "omp.user", "tone": "user", "mark": mark},
            "c": [{"id": format!("{id}.md"), "k": "md", "p": {"text": "a prompt"}}]})
    };
    let branch = |id: &str, tone: Option<&str>| {
        let mut p = json!({"role": "omp.rewind.branch"});
        if let Some(t) = tone {
            p["tone"] = t.into();
        }
        json!({"id": id, "k": "col", "p": p, "c": [
            {"id": format!("{id}.h"), "k": "text", "p": {"role": "omp.rewind.branch.head", "text": id}}]})
    };
    for dark in [false, true] {
        let mut h = omp_surface_as(
            "omp.rewind",
            json!([
                {"id": "strip", "k": "row", "p": {"role": "omp.rewind.strip"}, "c": [
                    branch("b0", None), branch("b1", Some("accent"))]},
                prompt("pk", "pick"), prompt("dr", "drop")
            ]),
            dark,
        );
        let op = |h: &Harness, id: &str| h.doc.computed_style(h.el("s1", id)).unwrap().opacity;
        assert!((op(&h, "b0") - 0.6).abs() < 1e-3);
        assert_eq!(op(&h, "b1"), 1.0);
        assert!((op(&h, "dr") - 0.45).abs() < 1e-3);
        let pk = h.el("s1", "pk");
        let accent = token(&mut h, pk, "var(--accent)");
        let st = h.doc.computed_style(pk).unwrap();
        assert_eq!(st.border_width[3], 0.0);
        let bar = st.box_shadow.as_deref().expect("the pick bar")[0];
        assert!(bar.inset && bar.x == 2.0, "{bar:?}");
        assert_eq!(bar.color.to_rgba8(), accent, "dark {dark}");
        let head = h.el("s1", "b1.h");
        let st = h.doc.computed_style(head).unwrap();
        assert_eq!(st.border_color[2].to_rgba8(), accent);
    }
}

/// A tool call as omp sends one.
fn omp_tool(id: &str, role: &str, status: &str, frame: &str) -> Value {
    json!({"id": id, "k": "tool", "p": {"role": role, "name": "bash", "title": "Bash", "target": "ls",
        "targetKind": "command", "status": status, "frame": frame}})
}

/// T3.1 (F:838→1093, then constructed): an omp card tool draws a 1px --l2 ring instead of its
/// border, 12px round; an error's ring is red, a running tool's isn't, over the pending tint.
#[test]
fn omp_card_tools_draw_a_ring_red_on_error() {
    for dark in [false, true] {
        let mut checked = Vec::new();
        omp_session_in(dark, false, |h, line| {
            if line != 838 && line != 1093 {
                return;
            }
            checked.push(line);
            let tool = h.el("s:1", "1e");
            let l2 = token(h, tool, "var(--l2)");
            let st = h.doc.computed_style(tool).unwrap();
            assert!(st.border_color.iter().all(|c| c.a == 0.0));
            let ring = st.box_shadow.as_deref().expect("a ring")[0];
            assert!(ring.inset && ring.spread == 1.0, "{ring:?}");
            assert_eq!(ring.color.to_rgba8(), l2, "line {line}, dark {dark}");
            assert_eq!(px(&st.border_radius[0].0), 12.0);
            assert_still(h, tool, "1e");
        });
        assert_eq!(checked, [838, 1093]);
    }
    for dark in [false, true] {
        let mut h = omp_surface(
            json!([
                omp_tool("er", "omp.tool.bash", "error", "card"),
                omp_tool("ru", "omp.tool.bash", "running", "card")
            ]),
            dark,
        );
        let er = h.el("s1", "er");
        let red = token(
            &mut h,
            er,
            "color-mix(in srgb, var(--bad) 40%, transparent)",
        );
        let st = h.doc.computed_style(er).unwrap();
        assert_eq!(st.box_shadow.as_deref().unwrap()[0].color.to_rgba8(), red);
        let ru = h.el("s1", "ru");
        let (l2, pending) = (
            token(&mut h, ru, "var(--l2)"),
            token(&mut h, ru, "var(--sf-tint-pending, var(--sf-card-bg))"),
        );
        let st = h.doc.computed_style(ru).unwrap();
        assert_eq!(st.box_shadow.as_deref().unwrap()[0].color.to_rgba8(), l2);
        assert_eq!(bg(&h, ru), pending, "dark {dark}");
    }
}

/// T3.2 (F:23): an inline read draws no ring, and its head runs dense.
#[test]
fn omp_inline_tools_stay_ringless_and_dense() {
    let mut seen = false;
    omp_session(false, |h, line| {
        if line != 23 {
            return;
        }
        seen = true;
        let x = h.el("s:1", "x");
        assert!(h.class(x).contains("inline"), "{}", h.class(x));
        assert!(h.doc.computed_style(x).unwrap().box_shadow.is_none());
        let head = h.find_class(x, "sf-tool-head")[0];
        let st = h.doc.computed_style(head).unwrap();
        assert_eq!((px(&st.padding[0]), px(&st.padding[2])), (2.0, 2.0));
    });
    assert!(seen);
}

/// T3.3 (constructed): a tool nested in a task's body loses its ring, an error's too.
#[test]
fn omp_nested_tools_lose_their_ring() {
    let mut task = omp_tool("task", "omp.tool.task", "running", "card");
    task["c"] = json!([
        omp_tool("nb", "omp.tool.bash", "done", "card"),
        omp_tool("ne", "omp.tool.bash", "error", "card")
    ]);
    let h = omp_surface(json!([task]), true);
    let outer = h.doc.computed_style(h.el("s1", "task")).unwrap();
    assert!(outer.box_shadow.is_some());
    for id in ["nb", "ne"] {
        let st = h.doc.computed_style(h.el("s1", id)).unwrap();
        assert!(st.box_shadow.is_none(), "{id}: {:?}", st.box_shadow);
    }
}

/// T3.4 (F:37→43): a file row being read is muted with a live icon; once read it takes the tool
/// output colour and a muted icon.
#[test]
fn omp_tool_files_show_their_reading_state() {
    let mut checked = Vec::new();
    omp_session(false, |h, line| {
        if line != 37 && line != 43 {
            return;
        }
        checked.push(line);
        let (row, icon) = (h.el("s:1", "10.go"), h.el("s:1", "10.go/0"));
        let (t2, t3, live) = (
            token(h, row, "var(--t2)"),
            token(h, row, "var(--t3)"),
            token(h, row, "var(--live)"),
        );
        if line == 37 {
            assert_eq!(color(h, row), t3);
            assert_eq!(color(h, icon), live);
        } else {
            assert_eq!(color(h, row), t2);
            assert_eq!(color(h, icon), t3);
            assert_eq!(h.doc.border_box(icon).unwrap().w, 14.0);
        }
    });
    assert_eq!(checked, [37, 43]);
}

/// T3.5 (F:839): bash output is in toolOutput (the palette's, else --t2).
#[test]
fn omp_bash_output_takes_the_tool_output_colour() {
    for palette in [false, true] {
        let mut seen = false;
        omp_session(palette, |h, line| {
            if line != 839 {
                return;
            }
            seen = true;
            let out = h.el("s:1", "1e.output");
            let t2 = token(h, out, "var(--t2)");
            let want = if palette {
                token(h, out, "var(--sf-p-toolOutput)")
            } else {
                t2
            };
            assert_eq!(color(h, out), want, "palette {palette}");
            assert!(!palette || want != t2, "omp's toolOutput");
        });
        assert!(seen);
    }
}

/// T3.6, T3.7 (constructed): the user's own shell command sits at the right like their message;
/// the think tool's md is italic and muted; an eval prompt glyph is two cells wide.
#[test]
fn omp_user_shell_commands_think_and_eval_prompts() {
    for dark in [false, true] {
        let mut h = omp_surface(
            json!([
                omp_tool("ub", "omp.bash", "done", "card"),
                {"id": "th", "k": "md", "p": {"role": "omp.think", "text": "considering"}},
                {"id": "ev", "k": "row", "p": {"role": "omp.tool.eval.input"}, "c": [
                    {"id": "ev.p", "k": "icon", "p": {"name": "chev", "role": "omp.tool.eval.prompt"}},
                    {"id": "ev.c", "k": "text", "p": {"text": "1 + 1"}}]}
            ]),
            dark,
        );
        let (ub, main) = (h.el("s1", "ub"), h.el("s1", "main"));
        let r = h.doc.border_box(ub).unwrap();
        assert!((r.x + r.w - inner_right(&h, main)).abs() < 0.5, "{r:?}");
        let l1 = token(&mut h, ub, "var(--l1)");
        assert_eq!(bg(&h, ub), l1, "dark {dark}");
        let th = h.el("s1", "th");
        let t3 = token(&mut h, th, "var(--t3)");
        let st = h.doc.computed_style(th).unwrap();
        assert_eq!(st.font_style, auk_ui::style::FontStyle::Italic);
        assert_eq!(color(&h, th), t3);
        let cw: f32 = var(&h, main, "--sf-cw")
            .trim_end_matches("px")
            .parse()
            .unwrap();
        let p = h.doc.border_box(h.el("s1", "ev.p")).unwrap();
        assert!((p.w - 2.0 * cw).abs() < 0.5, "{p:?}, cw {cw}");
    }
}

/// T3.8: roleless tools (an SDK program's) compute the same styles with and without the omp
/// transcript and tool sheets, on a plain surface and on an omp one.
#[test]
fn roleless_tools_look_the_same_without_the_omp_sheets() {
    fn styles(h: &Harness, el: NodeId, out: &mut Vec<auk_ui::style::ComputedStyle>) {
        out.push(h.doc.computed_style(el).unwrap().clone());
        for c in h.doc.element_children(el) {
            styles(h, c, out);
        }
    }
    let tool = |id: &str, p: Value, c: Value| json!({"id": id, "k": "tool", "p": p, "c": c});
    let body = |id: &str| json!([{"id": format!("{id}.o"), "k": "text", "p": {"text": "out"}}]);
    let main = json!([
        tool(
            "a",
            json!({"name": "read", "title": "Read", "target": "a.py", "status": "done", "frame": "inline"}),
            body("a")
        ),
        tool(
            "b",
            json!({"name": "bash", "title": "Run", "target": "ls", "status": "running", "frame": "card"}),
            body("b")
        ),
        tool(
            "c",
            json!({"name": "bash", "title": "Run", "target": "false", "status": "error", "frame": "card",
            "collapsible": true}),
            body("c")
        ),
        tool(
            "d",
            json!({"name": "edit", "title": "Edit", "target": "b.py", "status": "done", "frame": "card",
            "collapsible": true, "collapsed": true, "preview": {"lines": 2}}),
            body("d")
        ),
        tool(
            "e",
            json!({"name": "task", "title": "Task", "status": "running", "frame": "card"}),
            json!([
            {"id": "e.n", "k": "tool", "p": {"name": "bash", "title": "Run", "status": "error", "frame": "card"}}])
        )
    ]);
    for dark in [false, true] {
        let mut plain = Harness::with(
            Default::default(),
            auk_views::ViewSettings {
                dark,
                ..Default::default()
            },
        );
        plain.hello(&[]);
        plain.surface("s1", "inline", main.clone());
        plain.frame();
        let omp = omp_surface(main.clone(), dark);
        for (what, mut h) in [("plain", plain), ("omp", omp)] {
            let root = h.el("s1", "main");
            let mut with = Vec::new();
            styles(&h, root, &mut with);
            for name in ["omp-transcript", "omp-tools"] {
                h.doc
                    .set_stylesheet(name, auk_ui::SheetKind::BuiltIn, None, "");
            }
            h.doc.update();
            let mut without = Vec::new();
            styles(&h, root, &mut without);
            assert_eq!(with.len(), without.len());
            assert!(with.len() > 20, "{what}: {}", with.len());
            for (i, (a, b)) in with.iter().zip(&without).enumerate() {
                assert!(a == b, "{what}, dark {dark}: element {i} differs");
            }
        }
    }
}
