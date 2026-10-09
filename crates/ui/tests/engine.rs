//! The engine through its public API: cascade and scoping, Tern's errors, state pseudo-classes,
//! generated content, incremental restyle and relayout, virtualization and scroll anchoring.

mod common;

use auk_ui::css::values::Color;
use auk_ui::{Document, ElementState, NodeId, SheetKind};

fn doc_with(css: &str) -> Document {
    let mut doc = Document::new();
    doc.fonts_mut().register_font(common::ahem_bytes());
    let errors = doc.set_stylesheet(
        "test",
        SheetKind::BuiltIn,
        None,
        &format!("html {{ font: 10px/1 Ahem; }} body {{ margin: 0 }} {css}"),
    );
    assert!(errors.is_empty(), "{errors:?}");
    doc.set_viewport(800.0, 600.0);
    doc
}

fn el(doc: &mut Document, parent: NodeId, tag: &str, class: &str, text: Option<&str>) -> NodeId {
    let e = doc.create_element(tag);
    if !class.is_empty() {
        doc.set_attribute(e, "class", class);
    }
    if let Some(t) = text {
        let tn = doc.create_text(t);
        doc.append_child(e, tn);
    }
    doc.append_child(parent, e);
    e
}

fn color(doc: &Document, id: NodeId) -> [u8; 4] {
    doc.computed_style(id).unwrap().color.to_rgba8()
}

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const GREEN: [u8; 4] = [0, 128, 0, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];

#[test]
fn program_sheets_are_scoped_to_their_regions() {
    let mut doc = doc_with(".x.y { color: green }");
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let region = el(&mut doc, body, "main", "sf-region sf-main", None);
    doc.set_attribute(region, "data-scope", "sf1");
    let inside = el(&mut doc, region, "div", "x", Some("in"));
    let both = el(&mut doc, region, "div", "x y", Some("both"));
    let outside = el(&mut doc, body, "div", "x", Some("out"));
    let other = el(&mut doc, body, "main", "sf-region sf-main", None);
    doc.set_attribute(other, "data-scope", "sf2");
    let other_x = el(&mut doc, other, "div", "x", None);
    let errors = doc.set_stylesheet(
        "app",
        SheetKind::Program,
        Some("sf1"),
        ".x { color: red } .sf-main { color: blue } .sf-main .x { background: blue } :root { color: blue }",
    );
    assert!(errors.is_empty(), "{errors:?}");
    doc.update();
    assert_eq!(color(&doc, inside), RED);
    assert_eq!(color(&doc, outside), BLACK, "outside the scope");
    assert_eq!(color(&doc, other_x), BLACK, "another surface");
    assert_eq!(
        color(&doc, region),
        BLACK,
        "the region element itself is outside the scope"
    );
    assert!(
        doc.computed_style(inside)
            .unwrap()
            .background_color
            .is_transparent(),
        "the region can't be matched as an ancestor"
    );
    assert_eq!(color(&doc, root), BLACK, ":root never matches");
    // The scope adds no specificity: the built-in `.x.y` (0,2,0) beats the program's `.x` (0,1,0).
    assert_eq!(color(&doc, both), GREEN);
}

#[test]
fn program_sheet_errors_use_terns_format() {
    let mut doc = Document::new();
    let errors = doc.set_stylesheet(
        "app",
        SheetKind::Program,
        Some("sf1"),
        ".a {\n  background: url(x.png);\n  position: fixed;\n  width: 3rem;\n  bogus: 1;\n}\n@import \"x.css\";\n@font-face { font-family: X; }\n",
    );
    let joined = errors.join("; ");
    assert_eq!(errors.len(), 5, "{joined}");
    assert!(
        joined.starts_with("2:3: url() is not allowed in `background: url(x.png)`"),
        "{joined}"
    );
    assert!(
        joined.contains("3:3: position: fixed is not allowed in `position: fixed`"),
        "{joined}"
    );
    assert!(
        joined.contains("4:3: unsupported length unit `rem` in `width: 3rem`"),
        "{joined}"
    );
    assert!(
        joined.contains("5:3: unknown property `bogus` in `bogus: 1`"),
        "{joined}"
    );
    assert!(joined.contains("7:1: @import is not supported"), "{joined}");
}

#[test]
fn cascade_order_importance_inheritance_and_variables() {
    let mut doc = doc_with(
        ".p { color: red; --gap: 6px; --w: calc(var(--gap) * 10); font-size: 20px }
         #i { color: blue }
         .c { color: green !important; }
         .v { width: var(--w); height: var(--missing, 2em); margin-left: var(--nope); padding: var(--gap) }
         .cur { border: 2px solid currentColor; color: rgb(0, 0, 255) }",
    );
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let p = el(&mut doc, body, "div", "p", None);
    let inherits = el(&mut doc, p, "span", "", Some("x"));
    let id_wins = el(&mut doc, p, "div", "", None);
    doc.set_attribute(id_wins, "id", "i");
    let important = el(&mut doc, p, "div", "c", None);
    doc.set_attribute(important, "id", "i");
    doc.set_inline_style(important, "color: black");
    let inline = el(&mut doc, p, "div", "", None);
    doc.set_inline_style(inline, "color: blue");
    let v = el(&mut doc, p, "div", "v", None);
    let cur = el(&mut doc, p, "div", "cur", None);
    doc.update();
    assert_eq!(color(&doc, inherits), RED);
    assert_eq!(color(&doc, id_wins), BLUE);
    assert_eq!(
        color(&doc, important),
        GREEN,
        "!important beats id and inline style"
    );
    assert_eq!(color(&doc, inline), BLUE);
    let b = doc.border_box(v).unwrap();
    assert_eq!(
        (b.w, b.h),
        (60.0 + 12.0, 40.0 + 12.0),
        "var(), calc(), fallback and em"
    );
    assert_eq!(
        doc.computed_style(v).unwrap().margin[3],
        auk_ui::style::LengthPctAuto::ZERO,
        "unresolved var() is unset"
    );
    assert_eq!(
        doc.computed_style(cur).unwrap().border_color[0],
        Color::rgb8(0, 0, 255),
        "currentColor"
    );
}

#[test]
fn state_pseudo_classes_and_generated_content() {
    let mut doc = doc_with(
        ".b { color: black } .b:hover { color: red } .b:hover + .s { color: blue }
         .b:checked { color: green }
         .q::before { content: \"ab\"; } .q::after { content: attr(data-n); color: red }",
    );
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let b = el(&mut doc, body, "div", "b", Some("x"));
    let s = el(&mut doc, body, "div", "s", None);
    let q = el(&mut doc, body, "span", "q", Some("x"));
    doc.set_attribute(q, "data-n", "cde");
    doc.update();
    assert_eq!(color(&doc, b), BLACK);
    assert_eq!(
        doc.border_box(q).unwrap().w,
        60.0,
        "::before, text and ::after"
    );
    doc.set_state(b, ElementState::HOVER, true);
    doc.update();
    assert_eq!(color(&doc, b), RED);
    assert_eq!(color(&doc, s), BLUE);
    doc.set_state(b, ElementState::HOVER, false);
    doc.set_state(b, ElementState::CHECKED, true);
    doc.update();
    assert_eq!(color(&doc, b), GREEN);
    assert_eq!(color(&doc, s), BLACK);
    doc.set_attribute(q, "data-n", "c");
    doc.update();
    assert_eq!(
        doc.border_box(q).unwrap().w,
        40.0,
        "attr() follows the attribute"
    );
}

#[test]
fn mutation_is_incremental() {
    let mut doc = doc_with(".row { display: flex; gap: 4px } .cell { padding: 2px }");
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let mut rows = Vec::new();
    for i in 0..200 {
        let r = el(&mut doc, body, "div", "row", None);
        el(&mut doc, r, "div", "cell", Some(&format!("row {i}")));
        el(&mut doc, r, "div", "cell", Some("second cell"));
        rows.push(r);
    }
    doc.update();
    assert!(doc.last_update_stats().restyled > 600);
    let before = doc.border_box(rows[150]).unwrap();

    // Change one text node: no restyle, one inline context rebuilt.
    let cell = doc.element_children(rows[10])[0];
    let text = doc.children(cell)[0];
    doc.set_text(text, "a much longer text that wraps? no, it fits");
    doc.update();
    let st = doc.last_update_stats();
    assert_eq!(st.restyled, 0, "{st:?}");
    assert_eq!(st.inline_rebuilt, 1, "{st:?}");
    assert!(
        st.laid_out < 100,
        "only the changed path is laid out again: {st:?}"
    );
    assert_eq!(doc.border_box(rows[150]).unwrap(), before);

    // Insert a row near the top: later rows move down by its height.
    let new_row = doc.create_element("div");
    doc.set_attribute(new_row, "class", "row");
    let c = doc.create_element("div");
    doc.set_attribute(c, "class", "cell");
    doc.append_child(new_row, c);
    let t = doc.create_text("inserted");
    doc.append_child(c, t);
    doc.insert_before(body, new_row, Some(rows[5]));
    doc.update();
    let st = doc.last_update_stats();
    assert!(st.restyled < 10, "{st:?}");
    assert_eq!(doc.border_box(rows[150]).unwrap().y, before.y + 14.0);

    // Remove it again.
    doc.remove(new_row);
    doc.update();
    assert_eq!(doc.border_box(rows[150]).unwrap(), before);

    // A class change restyles that subtree only.
    doc.set_attribute(rows[20], "class", "row big");
    doc.update();
    let st = doc.last_update_stats();
    assert!(st.restyled <= 6, "{st:?}");
}

#[test]
fn virtualized_lists_lay_out_only_the_window_and_anchor_scrolling() {
    let mut doc = doc_with(
        ".main { height: 400px; overflow: auto; row-gap: 10px } .entry { padding: 5px } .tall { height: 100px }",
    );
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let main = el(&mut doc, body, "div", "main", None);
    let mut entries = Vec::new();
    for i in 0..10_000 {
        entries.push(el(
            &mut doc,
            main,
            "div",
            "entry",
            Some(&format!("entry {i} with some text")),
        ));
    }
    doc.set_virtual(main, Some(20.0));
    doc.update();
    let st = doc.last_update_stats();
    assert!(
        st.laid_out < 200,
        "only visible entries are laid out: {st:?}"
    );
    assert_eq!(doc.border_box(entries[0]).unwrap().h, 20.0);
    assert_eq!(doc.border_box(entries[1]).unwrap().y, 30.0);

    // Scroll to the middle and lay out again.
    doc.set_scroll_offset(main, 0.0, 150_000.0);
    doc.update();
    let (_, sy) = doc.scroll_offset(main);
    assert_eq!(sy, 150_000.0);
    let first_visible = entries.iter().position(|e| {
        let b = doc.border_box(*e).unwrap();
        b.y + b.h > 0.0
    });
    let i = first_visible.unwrap();
    assert!((4990..=5010).contains(&i), "{i}");
    let anchor_y = doc.border_box(entries[i + 2]).unwrap().y;

    // An entry just above the viewport (laid out in the overscan) grows: the visible content
    // stays put.
    assert!(doc.border_box(entries[i - 3]).unwrap().y < 0.0);
    doc.set_attribute(entries[i - 3], "class", "entry tall");
    doc.update();
    doc.set_scroll_offset(main, 0.0, doc.scroll_offset(main).1);
    let after = doc.border_box(entries[i + 2]).unwrap().y;
    assert!(
        (after - anchor_y).abs() < 0.5,
        "anchored: {anchor_y} -> {after}"
    );
}

#[test]
fn sticky_boxes_follow_the_scroll_within_their_container() {
    let mut doc = doc_with(
        ".s { height: 100px; overflow: auto } .h { position: sticky; top: 0; height: 10px } .c { height: 200px }",
    );
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let s = el(&mut doc, body, "div", "s", None);
    let sec = el(&mut doc, s, "section", "", None);
    let h = el(&mut doc, sec, "div", "h", None);
    el(&mut doc, sec, "div", "c", None);
    el(&mut doc, s, "div", "c", None);
    doc.update();
    assert_eq!(doc.border_box(h).unwrap().y, 0.0);
    doc.set_scroll_offset(s, 0.0, 50.0);
    doc.update();
    assert_eq!(
        doc.border_box(h).unwrap().y,
        0.0,
        "stuck to the top while scrolled"
    );
    doc.set_scroll_offset(s, 0.0, 300.0);
    doc.update();
    // The section ends at 210; the header can't leave it.
    assert_eq!(doc.border_box(h).unwrap().y, 200.0 - 300.0);
}

#[test]
fn measure_then_layout_loop() {
    // Clamp a paragraph to two lines by shrinking its text until it fits, the way a kind's
    // measure-then-relayout code would.
    let mut doc = doc_with(".p { width: 100px }");
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let p = el(
        &mut doc,
        body,
        "p",
        "p",
        Some("one two three four five six seven eight nine"),
    );
    let text = doc.children(p)[0];
    let passes = doc.update_with(20, |doc| {
        if doc.line_count(p) <= 2 {
            return false;
        }
        let t = doc.node(text).text().unwrap().to_string();
        let shorter = t
            .rsplit_once(' ')
            .map(|(a, _)| a.to_string())
            .unwrap_or_default();
        doc.set_text(text, &shorter);
        true
    });
    assert!(passes > 1);
    assert_eq!(doc.line_count(p), 2);
    assert_eq!(doc.border_box(p).unwrap().h, 20.0);
    let (min, max) = doc.intrinsic_widths(p);
    assert_eq!(min, 100.0, "the fixed width");
    assert_eq!(max, 100.0);
}

#[test]
fn transitions_and_keyframe_animations_run_on_the_document_clock() {
    let mut doc = doc_with(
        ".a { opacity: 1; transition: opacity 100ms linear }
         .a.off { opacity: 0 }
         .c { color: rgb(0, 0, 0); transition: color 100ms linear }
         .c.on { color: rgb(200, 100, 0) }
         .d { transition: transform 100ms linear 50ms }
         .d.moved { transform: translateX(100px) }
         @keyframes glow { from { background-color: rgb(0, 0, 0) } to { background-color: rgb(200, 0, 0) } }
         .b { animation: glow 1s linear 2 alternate }
         .f { opacity: 0.2; animation: fade 200ms linear forwards }
         @keyframes fade { to { opacity: 1 } }
         .p { animation: glow 1s linear infinite; animation-play-state: paused }",
    );
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let a = el(&mut doc, body, "div", "a", Some("x"));
    let c = el(&mut doc, body, "div", "c", None);
    let span = el(&mut doc, c, "span", "", Some("inherits"));
    let d = el(&mut doc, body, "div", "d", Some("x"));
    doc.set_time(0.0);
    doc.update();
    assert!(!doc.animating());
    let opacity = |doc: &Document| doc.computed_style(a).unwrap().opacity;
    // A transition starts from the value shown when the style changes.
    doc.set_attribute(a, "class", "a off");
    doc.set_attribute(c, "class", "c on");
    doc.set_attribute(d, "class", "d moved");
    doc.update();
    assert_eq!(opacity(&doc), 1.0);
    assert!(doc.animating());
    doc.set_time(50.0);
    doc.update();
    assert!((opacity(&doc) - 0.5).abs() < 0.01, "{}", opacity(&doc));
    // Color moves too, and descendants inherit what is shown.
    assert_eq!(color(&doc, span), [100, 50, 0, 255]);
    // A delayed transition holds its start value through the delay.
    let tx = |doc: &Document| format!("{:?}", doc.computed_style(d).unwrap().transform);
    assert!(tx(&doc).contains("Px(0.0)"), "{}", tx(&doc));
    doc.set_time(100.0);
    doc.update();
    assert!(tx(&doc).contains("Px(50.0)"), "{}", tx(&doc));
    // Reversing mid-way starts from where it is.
    doc.set_attribute(a, "class", "a");
    doc.update();
    assert_eq!(opacity(&doc), 0.0, "the opacity transition had finished");
    doc.set_time(150.0);
    doc.update();
    assert!((opacity(&doc) - 0.5).abs() < 0.01);
    doc.set_time(400.0);
    doc.update();
    assert_eq!(opacity(&doc), 1.0);
    assert_eq!(color(&doc, span), [200, 100, 0, 255]);
    assert!(!doc.animating(), "everything settled");

    // Keyframes: two alternating iterations, then the cascaded value; `forwards` keeps the end.
    let b = el(&mut doc, body, "div", "b", None);
    let f = el(&mut doc, body, "div", "f", None);
    let p = el(&mut doc, body, "div", "p", None);
    doc.set_time(1000.0);
    doc.update();
    let bg = |doc: &Document, id| doc.computed_style(id).unwrap().background_color.to_rgba8();
    assert_eq!(bg(&doc, b), [0, 0, 0, 255]);
    assert_eq!(bg(&doc, p), [0, 0, 0, 255], "paused at its start");
    doc.set_time(1250.0);
    doc.update();
    assert_eq!(bg(&doc, b), [50, 0, 0, 255]);
    assert_eq!(bg(&doc, p), [0, 0, 0, 255]);
    doc.set_time(1750.0);
    doc.update();
    assert_eq!(bg(&doc, b), [150, 0, 0, 255]);
    assert_eq!(
        doc.computed_style(f).unwrap().opacity,
        1.0,
        "filled forwards"
    );
    // The second iteration runs backwards.
    doc.set_time(2250.0);
    doc.update();
    assert_eq!(bg(&doc, b), [150, 0, 0, 255]);
    doc.set_time(3100.0);
    doc.update();
    assert!(
        doc.computed_style(b)
            .unwrap()
            .background_color
            .is_transparent(),
        "done"
    );
    assert_eq!(doc.computed_style(f).unwrap().opacity, 1.0);

    // Reduce Motion: transitions jump and animations don't run.
    doc.set_media_preferences(true, false, false);
    doc.update();
    doc.set_attribute(a, "class", "a off");
    let g = el(&mut doc, body, "div", "b", None);
    doc.update();
    assert_eq!(opacity(&doc), 0.0);
    assert!(
        doc.computed_style(g)
            .unwrap()
            .background_color
            .is_transparent()
    );
    assert!(!doc.animating());
}

/// A17: a finished animation stays finished through later restyles; it runs again only when
/// the element's animation names change or the element is inserted again.
#[test]
fn finished_animations_dont_replay_on_restyle() {
    let mut doc = doc_with(
        "@keyframes pop { from { opacity: 0 } to { opacity: 1 } }
         @keyframes pop2 { from { opacity: 0 } to { opacity: 1 } }
         .card { animation: pop 220ms linear }
         .wrap.focused .card { color: rgb(255, 0, 0) }
         .wrap.other .card { animation-name: pop2 }",
    );
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let wrap = el(&mut doc, body, "div", "wrap", None);
    let card = el(&mut doc, wrap, "div", "card", Some("x"));
    let opacity = |doc: &Document| doc.computed_style(card).unwrap().opacity;
    doc.set_time(0.0);
    doc.update();
    assert_eq!(opacity(&doc), 0.0, "starts");
    doc.set_time(110.0);
    doc.update();
    assert!((opacity(&doc) - 0.5).abs() < 0.01, "{}", opacity(&doc));
    doc.set_time(300.0);
    doc.update();
    assert_eq!(opacity(&doc), 1.0, "finished");
    assert!(!doc.animating());
    // An ancestor's class changes the card's style: the animation stays finished.
    doc.set_time(1000.0);
    doc.set_attribute(wrap, "class", "wrap focused");
    doc.update();
    assert_eq!(color(&doc, card), [255, 0, 0, 255], "restyled");
    assert_eq!(opacity(&doc), 1.0, "not replayed");
    assert!(!doc.animating());
    doc.set_attribute(wrap, "class", "wrap");
    doc.update();
    assert_eq!(opacity(&doc), 1.0);
    assert!(!doc.animating());
    // A new animation name starts.
    doc.set_time(2000.0);
    doc.set_attribute(wrap, "class", "wrap other");
    doc.update();
    assert_eq!(opacity(&doc), 0.0, "pop2 starts");
    assert!(doc.animating());
    doc.set_time(2300.0);
    doc.update();
    assert_eq!(opacity(&doc), 1.0);
    // And back to `pop`, which is no longer listed as finished.
    doc.set_time(3000.0);
    doc.set_attribute(wrap, "class", "wrap");
    doc.update();
    assert_eq!(opacity(&doc), 0.0, "pop starts again");
    doc.set_time(3300.0);
    doc.update();
    assert_eq!(opacity(&doc), 1.0);
    assert!(!doc.animating());
    // Removed and inserted again: new to CSS, so it runs.
    doc.set_time(4000.0);
    doc.detach(card);
    doc.update();
    doc.append_child(wrap, card);
    doc.update();
    assert_eq!(opacity(&doc), 0.0, "re-inserted");
    assert!(doc.animating());
}

/// A17: an element's finished animation is forgotten on detach even while another animation
/// still runs on it; and turning reduced motion off doesn't replay what was listed meanwhile,
/// while a looping animation resumes.
#[test]
fn finished_animations_beside_running_ones_and_across_reduced_motion() {
    let mut doc = doc_with(
        "@keyframes pop { from { opacity: 0 } to { opacity: 1 } }
         @keyframes spin { from { width: 0px } to { width: 100px } }
         .card { animation: pop 220ms linear, spin 1s linear infinite }
         .plain { animation: pop 220ms linear }",
    );
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let wrap = el(&mut doc, body, "div", "", None);
    let card = el(&mut doc, wrap, "div", "card", Some("x"));
    let opacity = |doc: &Document, id| doc.computed_style(id).unwrap().opacity;
    doc.set_time(0.0);
    doc.update();
    doc.set_time(300.0);
    doc.update();
    assert_eq!(opacity(&doc, card), 1.0, "pop finished, spin runs");
    assert!(doc.animating());
    doc.detach(card);
    doc.update();
    doc.append_child(wrap, card);
    doc.set_time(400.0);
    doc.update();
    assert_eq!(opacity(&doc, card), 0.0, "re-inserted: pop runs again");
    doc.detach(card);
    doc.update();

    // Under reduced motion nothing moves; turned off, `pop` stays done and a loop resumes.
    doc.set_media_preferences(true, false, false);
    let plain = el(&mut doc, wrap, "div", "plain", Some("y"));
    doc.append_child(wrap, card);
    doc.set_time(1000.0);
    doc.update();
    assert_eq!(opacity(&doc, plain), 1.0);
    assert!(!doc.animating());
    doc.set_media_preferences(false, false, false);
    doc.set_time(2000.0);
    doc.update();
    assert_eq!(opacity(&doc, plain), 1.0, "not replayed");
    assert_eq!(opacity(&doc, card), 1.0, "not replayed beside a loop");
    assert!(doc.animating(), "the loop resumes");
}

/// §8.4 "Light and dark": `light-dark()` follows the appearance (the document's, or the nearest
/// `color-scheme`, which the theme sets under `data-theme`); the three supported media features
/// match the user's preferences; `prefers-color-scheme` is rejected.
#[test]
fn light_dark_and_the_supported_media_features_match_and_prefers_color_scheme_is_rejected() {
    let mut doc = doc_with(
        ".ld { color: light-dark(rgb(255, 0, 0), rgb(0, 0, 255)) }
         [data-theme=dark] { color-scheme: dark }
         [data-theme=light] { color-scheme: light }
         @media (prefers-reduced-motion: reduce) { .rm { color: rgb(255, 0, 0) } }
         @media (prefers-reduced-motion) { .rm2 { color: rgb(255, 0, 0) } }
         @media (prefers-reduced-transparency: reduce) { .rt { color: rgb(255, 0, 0) } }
         @media (prefers-contrast: more) { .pc { color: rgb(255, 0, 0) } }
         @media screen and (prefers-contrast: more) and (prefers-reduced-motion: reduce) { .both { color: rgb(255, 0, 0) } }",
    );
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let ld = el(&mut doc, body, "div", "ld", Some("x"));
    let dark_theme = el(&mut doc, body, "div", "", None);
    doc.set_attribute(dark_theme, "data-theme", "dark");
    let in_dark = el(&mut doc, dark_theme, "div", "ld", Some("x"));
    let light_theme = el(&mut doc, body, "div", "", None);
    doc.set_attribute(light_theme, "data-theme", "light");
    let in_light = el(&mut doc, light_theme, "div", "ld", Some("x"));
    let [rm, rm2, rt, pc, both] =
        ["rm", "rm2", "rt", "pc", "both"].map(|c| el(&mut doc, body, "div", c, Some("x")));
    doc.update();
    assert_eq!(color(&doc, ld), RED, "light by default");
    assert_eq!(color(&doc, in_dark), BLUE, "data-theme=dark");
    assert_eq!(color(&doc, in_light), RED);
    doc.set_dark(true);
    doc.update();
    assert_eq!(color(&doc, ld), BLUE, "the appearance turned dark");
    assert_eq!(color(&doc, in_light), RED, "a light theme stays light");

    // No preferences: none of the media rules match.
    for e in [rm, rm2, rt, pc, both] {
        assert_eq!(color(&doc, e), BLACK);
    }
    doc.set_media_preferences(true, false, false);
    doc.update();
    assert_eq!(
        [rm, rm2, rt, pc, both].map(|e| color(&doc, e)),
        [RED, RED, BLACK, BLACK, BLACK],
        "reduced motion"
    );
    doc.set_media_preferences(false, true, false);
    doc.update();
    assert_eq!(
        [rm, rm2, rt, pc, both].map(|e| color(&doc, e)),
        [BLACK, BLACK, RED, BLACK, BLACK],
        "reduced transparency"
    );
    doc.set_media_preferences(true, false, true);
    doc.update();
    assert_eq!(
        [rm, rm2, rt, pc, both].map(|e| color(&doc, e)),
        [RED, RED, BLACK, RED, RED],
        "more contrast and reduced motion"
    );

    // prefers-color-scheme is rejected (an error, and the rule never applies), in every sheet
    // kind; so are other values of the supported features.
    for (kind, scope) in [
        (SheetKind::BuiltIn, None),
        (SheetKind::Plugin, None),
        (SheetKind::Program, Some("sf1")),
    ] {
        let errors = doc.set_stylesheet(
            "scheme",
            kind,
            scope,
            "@media (prefers-color-scheme: dark) { .ld { color: rgb(0, 128, 0) } }
             @media (prefers-contrast: less) { .pc { color: rgb(0, 128, 0) } }",
        );
        assert_eq!(errors.len(), 2, "{kind:?}: {errors:?}");
        assert!(errors.iter().all(|e| e.contains("unsupported media query")), "{errors:?}");
        doc.update();
        assert_eq!(color(&doc, ld), BLUE);
        assert_eq!(color(&doc, pc), RED);
        doc.remove_stylesheet("scheme", scope);
    }
}

/// §8.4 cascade step 3: plugin sheets and program sheets apply in the order each was first
/// installed, whatever their kind; replacing a sheet's text keeps its place.
#[test]
fn plugin_and_program_sheets_cascade_in_first_install_order() {
    let mut doc = doc_with("");
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let region = el(&mut doc, body, "main", "sf-region sf-main", None);
    doc.set_attribute(region, "data-scope", "sf1");
    let x = el(&mut doc, region, "div", "x", Some("x"));
    // A plugin sheet, then a program sheet: the program's rule (same specificity) is later.
    assert!(doc.set_stylesheet("plugin:a", SheetKind::Plugin, None, ".x { color: rgb(255, 0, 0) }").is_empty());
    assert!(doc.set_stylesheet("app", SheetKind::Program, Some("sf1"), ".x { color: rgb(0, 0, 255) }").is_empty());
    doc.update();
    assert_eq!(color(&doc, x), BLUE, "the later-installed program sheet");
    // A second plugin sheet installed after the program's comes later still.
    assert!(doc.set_stylesheet("plugin:b", SheetKind::Plugin, None, ".x { color: rgb(0, 128, 0) }").is_empty());
    doc.update();
    assert_eq!(color(&doc, x), GREEN);
    // Replacing the first plugin sheet's and the program sheet's text keeps their places.
    doc.set_stylesheet("plugin:a", SheetKind::Plugin, None, ".x { color: rgb(255, 0, 0) } .y { color: red }");
    doc.set_stylesheet("app", SheetKind::Program, Some("sf1"), ".x { color: rgb(0, 0, 255) } .z { color: red }");
    doc.update();
    assert_eq!(color(&doc, x), GREEN, "replaced sheets keep their first-install order");
    // Specificity still decides first; !important declarations go in the same order.
    doc.set_stylesheet("plugin:a", SheetKind::Plugin, None, ".sf-main .x { color: rgb(255, 0, 0) }");
    doc.update();
    assert_eq!(color(&doc, x), RED, "more specific beats a later sheet");
    doc.set_stylesheet("plugin:a", SheetKind::Plugin, None, ".x { color: rgb(255, 0, 0) !important }");
    doc.set_stylesheet("plugin:b", SheetKind::Plugin, None, ".x { color: rgb(0, 128, 0) !important }");
    doc.update();
    assert_eq!(color(&doc, x), GREEN, "!important in the same order");
    // Removed and installed again: a new install, so it goes last.
    doc.remove_stylesheet("plugin:a", None);
    doc.set_stylesheet("plugin:a", SheetKind::Plugin, None, ".x { color: rgb(255, 0, 0) !important }");
    doc.update();
    assert_eq!(color(&doc, x), RED);
}

/// §8.4 the engine's feature set: program sheets get Tern's errors for the subsequent-sibling
/// combinator `~` and `:has` (the whole rule dropped, "unsupported selector"), for grid areas
/// and the line longhands ("unknown property"), and for `repeat(auto-fill | auto-fit, …)` and
/// the areas form of `grid-template` ("invalid value"). The rest of each sheet still applies,
/// `~=` attribute selectors are fine, and the terminal's own sheets keep `~` and the grid features.
#[test]
fn program_sheets_refuse_sibling_tilde_has_grid_areas_and_auto_fill_with_terns_errors() {
    let css = "\
.a ~ .b { color: red; }
.a:has(.c) { color: red; }
.ok { color: green; }
[data-k~=\"x\"] { color: blue; }
.g {
  display: grid;
  grid-template-areas: \"head head\" \"side main\";
  grid-area: head;
  grid-template-columns: repeat(auto-fill, 40px);
  grid-template-rows: repeat(auto-fit, 20px);
  grid-template: \"x y\" 10px / 1fr 1fr;
  grid-auto-flow: column;
  grid-row-start: 2;
  grid-column-end: 3;
  grid-template-columns: repeat(2, 30px);
  grid-column: 1 / span 2;
}
";
    let mut doc = doc_with("");
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let region = el(&mut doc, body, "div", "sf-region sf-main", None);
    doc.set_attribute(region, "data-scope", "sf1");
    let a = el(&mut doc, region, "div", "a", None);
    let b = el(&mut doc, region, "div", "b", Some("b"));
    el(&mut doc, a, "div", "c", None);
    let ok = el(&mut doc, region, "div", "ok", Some("ok"));
    let k = el(&mut doc, region, "div", "", Some("k"));
    doc.set_attribute(k, "data-k", "w x");
    let g = el(&mut doc, region, "div", "g", None);
    let errors = doc.set_stylesheet("app", SheetKind::Program, Some("sf1"), css);
    let want = [
        "1:1: unsupported selector in `.a ~ .b`",
        "2:1: unsupported selector in `.a:has(.c)`",
        "7:3: unknown property `grid-template-areas` in `grid-template-areas: \"head head\" \"side main\"`",
        "8:3: unknown property `grid-area` in `grid-area: head`",
        "9:3: invalid value for `grid-template-columns` in `grid-template-columns: repeat(auto-fill, 40px)`",
        "10:3: invalid value for `grid-template-rows` in `grid-template-rows: repeat(auto-fit, 20px)`",
        "11:3: invalid value for `grid-template` in `grid-template: \"x y\" 10px / 1fr 1fr`",
        "12:3: unknown property `grid-auto-flow` in `grid-auto-flow: column`",
        "13:3: unknown property `grid-row-start` in `grid-row-start: 2`",
        "14:3: unknown property `grid-column-end` in `grid-column-end: 3`",
    ];
    assert_eq!(errors, want, "{errors:#?}");
    doc.update();
    assert_eq!(color(&doc, b), BLACK, "the `~` rule is dropped");
    assert_eq!(color(&doc, a), BLACK, "the `:has` rule is dropped");
    assert_eq!(color(&doc, ok), GREEN);
    assert_eq!(color(&doc, k), BLUE, "`~=` is an attribute selector");
    // What Tern supports still applies: two 30px columns, the item spans both.
    let item = el(&mut doc, g, "div", "", Some("x"));
    doc.set_inline_style(item, "grid-column: 1 / span 2; height: 5px");
    doc.update();
    assert_eq!(doc.border_box(item).unwrap().w, 60.0);

    // Plugin sheets follow the same rules; the terminal's own sheets may use `~` and the grid
    // features.
    let plugin = doc.set_stylesheet("plug", SheetKind::Plugin, None, ".p ~ .q { color: red } .r { grid-area: x }");
    assert_eq!(
        plugin,
        [
            "1:1: unsupported selector in `.p ~ .q`",
            "1:29: unknown property `grid-area` in `grid-area: x`"
        ]
    );
    let own = doc.set_stylesheet(
        "own",
        SheetKind::BuiltIn,
        None,
        ".p ~ .q { color: red } .t { grid-template-areas: \"a b\"; grid-template-columns: repeat(auto-fill, 10px); grid-area: a; }",
    );
    assert!(own.is_empty(), "{own:?}");
}

/// §8.4 units: `vw` and `vh` are viewport percentages in program sheets, and follow the
/// viewport when it changes.
#[test]
fn vw_and_vh_units_resolve_against_the_viewport_in_program_sheets() {
    let mut doc = doc_with("");
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let region = el(&mut doc, body, "div", "sf-region sf-main", None);
    doc.set_attribute(region, "data-scope", "sf1");
    let v = el(&mut doc, region, "div", "v", None);
    let errors = doc.set_stylesheet(
        "app",
        SheetKind::Program,
        Some("sf1"),
        ".v { width: 50vw; height: 25vh; margin-left: calc(10vw + 2px); }",
    );
    assert!(errors.is_empty(), "{errors:?}");
    doc.update();
    let r = doc.border_box(v).unwrap();
    assert_eq!((r.x, r.w, r.h), (82.0, 400.0, 150.0), "at 800x600");
    doc.set_viewport(1000.0, 400.0);
    doc.update();
    let r = doc.border_box(v).unwrap();
    assert_eq!((r.x, r.w, r.h), (102.0, 500.0, 100.0), "at 1000x400");
}

/// A7: `box-shadow` parses (inset, spread, several shadows, any component order, `var()`,
/// `color-mix()`, the `currentColor` default) and computes to px and resolved colors; `none`
/// clears it, and malformed lists are invalid.
#[test]
fn box_shadow_parses_and_computes_with_vars_color_mix_and_current_color() {
    use auk_ui::style::Shadow;
    let mut doc = doc_with(
        ".s { color: rgb(0, 0, 255); --c: rgb(0, 128, 0);
              box-shadow: inset 2px 0 0 rgb(255, 0, 0), 1px 2px 3px 4px; }
         .v { box-shadow: inset 0 0 0 1px var(--c); }
         .m { box-shadow: color-mix(in srgb, rgb(255, 0, 0) 50%, transparent) 0.5em -1px; }
         .n { box-shadow: none; }",
    );
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let s = el(&mut doc, body, "div", "s", None);
    let v = el(&mut doc, s, "div", "v", None);
    let m = el(&mut doc, s, "div", "m", None);
    let n = el(&mut doc, s, "div", "s n", None);
    doc.update();
    let shadows = |id: NodeId| -> Vec<Shadow> {
        doc.computed_style(id)
            .unwrap()
            .box_shadow
            .as_deref()
            .cloned()
            .unwrap_or_default()
    };
    let rgba = |sh: &Shadow| sh.color.to_rgba8();
    let got = shadows(s);
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got[0].inset);
    assert_eq!(
        (got[0].x, got[0].y, got[0].blur, got[0].spread),
        (2.0, 0.0, 0.0, 0.0)
    );
    assert_eq!(rgba(&got[0]), RED);
    assert!(!got[1].inset);
    assert_eq!(
        (got[1].x, got[1].y, got[1].blur, got[1].spread),
        (1.0, 2.0, 3.0, 4.0)
    );
    assert_eq!(rgba(&got[1]), BLUE, "no color is currentColor");
    let got = shadows(v);
    assert_eq!(got.len(), 1);
    assert_eq!((got[0].inset, got[0].spread), (true, 1.0));
    assert_eq!(rgba(&got[0]), GREEN, "var() resolves (and --c inherits)");
    let got = shadows(m);
    assert_eq!(
        (got[0].x, got[0].y),
        (5.0, -1.0),
        "em against the 10px font"
    );
    let [r, g, b, a] = rgba(&got[0]);
    assert_eq!(
        (r, g, b),
        (255, 0, 0),
        "color first, color-mix with transparent"
    );
    assert!((126..=129).contains(&a), "half alpha: {a}");
    assert!(shadows(n).is_empty(), "none clears it");
    assert!(doc.computed_style(n).unwrap().box_shadow.is_none());

    let errors = doc.set_stylesheet(
        "app",
        SheetKind::Program,
        Some("sf1"),
        ".a { box-shadow: 1px; }\n.b { box-shadow: 1px 1px -2px red; }\n.c { box-shadow: 10% 1px; }\n.d { box-shadow: inset inset 1px 1px; }\n.e { box-shadow: 1px 1px, none; }\n.f { box-shadow: red 1px 1px blue; }\n.ok { box-shadow: 1px 1px 2px -1px red, inset 0 0 0 1px; }\n",
    );
    let want = [
        "1:6: invalid value for `box-shadow` in `box-shadow: 1px`",
        "2:6: invalid value for `box-shadow` in `box-shadow: 1px 1px -2px red`",
        "3:6: invalid value for `box-shadow` in `box-shadow: 10% 1px`",
        "4:6: invalid value for `box-shadow` in `box-shadow: inset inset 1px 1px`",
        "5:6: invalid value for `box-shadow` in `box-shadow: 1px 1px, none`",
        "6:6: invalid value for `box-shadow` in `box-shadow: red 1px 1px blue`",
    ];
    assert_eq!(errors, want, "{errors:#?}");
}

/// A7: `box-shadow` is paint-only (no relayout) and transitions shadow by shadow.
#[test]
fn box_shadow_changes_repaint_without_relayout_and_transition() {
    let mut doc = doc_with(
        ".t { width: 10px; height: 10px; box-shadow: 0 0 0 0 rgb(0, 0, 0);
              transition: box-shadow 100ms linear }
         .t.on { box-shadow: 0 0 0 10px rgb(200, 0, 0) }",
    );
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let t = el(&mut doc, body, "div", "t", None);
    doc.set_time(0.0);
    doc.update();
    doc.set_attribute(t, "class", "t on");
    doc.update();
    assert_eq!(
        doc.last_update_stats().laid_out,
        0,
        "a shadow change doesn't lay out: {:?}",
        doc.last_update_stats()
    );
    doc.set_time(50.0);
    doc.update();
    let sh = doc
        .computed_style(t)
        .unwrap()
        .box_shadow
        .as_deref()
        .unwrap()[0];
    assert!((sh.spread - 5.0).abs() < 0.01, "{sh:?}");
    assert_eq!(sh.color.to_rgba8(), [100, 0, 0, 255]);
    doc.set_time(200.0);
    doc.update();
    let sh = doc
        .computed_style(t)
        .unwrap()
        .box_shadow
        .as_deref()
        .unwrap()[0];
    assert_eq!(sh.spread, 10.0);
}

/// A7: Tern's engine has automatic table layout only and no captions (book:10679), and no
/// floats (book:10796): `table-layout` and `float` are unknown properties and `display:
/// table-caption` is an invalid value, in every sheet kind.
#[test]
fn table_layout_table_caption_and_float_are_rejected_as_in_tern() {
    let css = ".a { table-layout: fixed; }\n.b { display: table-caption; }\n.c { float: left; clear: both; }\n";
    for kind in [SheetKind::Program, SheetKind::Plugin, SheetKind::BuiltIn] {
        let mut doc = Document::new();
        let errors = doc.set_stylesheet("x", kind, Some("sf1"), css);
        assert_eq!(
            errors,
            [
                "1:6: unknown property `table-layout` in `table-layout: fixed`",
                "2:6: invalid value for `display` in `display: table-caption`",
                "3:6: unknown property `float` in `float: left`",
                "3:19: unknown property `clear` in `clear: both`",
            ],
            "{kind:?}"
        );
    }
    // The UA sheet has no `caption` rule: a `caption` element is an inline box.
    let mut doc = doc_with("");
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let table = el(&mut doc, body, "table", "", None);
    let caption = el(&mut doc, table, "caption", "", Some("c"));
    doc.update();
    assert_eq!(
        doc.computed_style(caption).unwrap().display,
        auk_ui::style::Display::Inline
    );
}
