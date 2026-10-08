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
