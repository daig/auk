//! omp's sheets (amendment A24; research/omp-stylesheets.md §16.1-16.2): they parse, sit in Tern's
//! slot, keep every rule in a class bucket, and give omp's shared roles their look.

mod common;

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
    let mut h = Harness::with(
        Default::default(),
        auk_views::ViewSettings {
            dark,
            ..Default::default()
        },
    );
    h.hello(&[]);
    h.apc("o", r#"{"id":"s1","mode":"flow","role":"omp.session"}"#);
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
