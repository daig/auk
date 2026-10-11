//! Virtualized lists an ancestor scrolls (a transcript's `main`): children out of the window
//! are neither styled nor laid out until they come into it,
//! a list growing at the end builds only the new boxes, and a measure loop can ask what layout
//! computed since it last looked.

mod common;

use auk_ui::{Document, NodeId, SheetKind};

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

const CSS: &str = ".main { display: flex; flex-direction: column; row-gap: 10px } \
                   .entry { height: 20px; color: rgb(0, 0, 255) } .entry.red { color: rgb(255, 0, 0) } \
                   .entry.gone { display: none } .box { position: relative } \
                   .box p { margin: 0 } \
                   .pin { position: absolute; top: 3px; left: 4px; width: 5px; height: 5px }";

/// A body holding a virtualized flex column of `n` entries 20px high, 10px apart, estimated
/// at 20px; the window an ancestor's scroll gives it starts at the top.
fn list(n: usize) -> (Document, NodeId, Vec<NodeId>) {
    let mut doc = doc_with(CSS);
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let main = el(&mut doc, body, "div", "main", None);
    let entries: Vec<NodeId> = (0..n)
        .map(|i| el(&mut doc, main, "div", "entry", Some(&format!("entry {i}"))))
        .collect();
    doc.set_virtual(main, Some(20.0));
    doc.set_virtual_window(main, Some((0.0, 600.0)));
    doc.update();
    (doc, main, entries)
}

fn color(doc: &Document, id: NodeId) -> [u8; 4] {
    doc.computed_style(id).unwrap().color.to_rgba8()
}

#[test]
fn children_out_of_the_window_are_styled_as_they_come_into_it() {
    let (mut doc, main, entries) = list(5000);
    let st = doc.last_update_stats();
    assert!(
        st.restyled < 200,
        "only what's near the window is styled: {st:?}"
    );
    assert!(doc.laid_out(entries[0]));
    assert!(doc.computed_style(entries[4000]).is_none());
    assert!(!doc.laid_out(entries[4000]));
    // Its estimated place: 30px per entry.
    assert_eq!(doc.border_box(entries[4000]).unwrap().y, 4000.0 * 30.0);

    // The window moves to entry 4000: styled and laid out there, and only there.
    doc.set_virtual_window(main, Some((120_000.0, 120_600.0)));
    doc.update();
    let st = doc.last_update_stats();
    assert!(st.restyled < 200, "{st:?}");
    assert!(doc.laid_out(entries[4000]));
    assert_eq!(color(&doc, entries[4000]), [0, 0, 255, 255]);
    assert_eq!(doc.border_box(entries[4000]).unwrap().y, 4000.0 * 30.0);
    assert!(
        !doc.laid_out(entries[0]),
        "the top is out of the window now"
    );

    // A change to an entry out of the window waits for it; back in view, it shows.
    doc.set_attribute(entries[0], "class", "entry red");
    doc.update();
    assert_eq!(doc.last_update_stats().restyled, 0);
    assert_eq!(
        color(&doc, entries[0]),
        [0, 0, 255, 255],
        "not restyled yet"
    );
    doc.set_virtual_window(main, Some((0.0, 600.0)));
    doc.update();
    assert_eq!(color(&doc, entries[0]), [255, 0, 0, 255]);

    // An entry that leaves the flow out of view, and comes back in view.
    doc.set_attribute(entries[10], "class", "entry gone");
    doc.update();
    doc.update();
    assert!(!doc.laid_out(entries[10]));
    assert_eq!(
        doc.border_box(entries[11]).unwrap().y,
        10.0 * 30.0,
        "entry 11 takes its place"
    );
    doc.set_attribute(entries[10], "class", "entry");
    doc.update();
    assert!(doc.laid_out(entries[10]));
    assert_eq!(doc.border_box(entries[11]).unwrap().y, 11.0 * 30.0);
}

/// An entry styled only as it comes into view matches selectors that need its ancestors (by
/// tag, class and id, from a universal subject too) as one styled with the whole document does.
#[test]
fn children_styled_coming_into_view_match_selectors_through_their_ancestors() {
    let mut doc = doc_with(&format!(
        "{CSS} body.on .main .entry {{ color: rgb(0, 128, 0) }} \
         #m > .entry.odd {{ color: rgb(255, 0, 0) }} .dim * {{ opacity: 0.5 }}"
    ));
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let outer = el(&mut doc, body, "div", "", None);
    let main = el(&mut doc, outer, "div", "main", None);
    doc.set_attribute(main, "id", "m");
    let entries: Vec<NodeId> = (0..2000)
        .map(|i| {
            let class = if i % 2 == 1 { "entry odd" } else { "entry" };
            el(&mut doc, main, "div", class, Some(&format!("entry {i}")))
        })
        .collect();
    doc.set_virtual(main, Some(20.0));
    doc.set_virtual_window(main, Some((0.0, 600.0)));
    doc.update();
    assert_eq!(color(&doc, entries[0]), [0, 0, 255, 255]);
    // Ancestors change while the far entries wait out of view.
    doc.set_attribute(body, "class", "on");
    doc.set_attribute(outer, "class", "dim");
    doc.update();
    assert_eq!(color(&doc, entries[0]), [0, 128, 0, 255]);
    assert!(doc.computed_style(entries[1500]).is_none());
    doc.set_virtual_window(main, Some((1500.0 * 30.0, 1500.0 * 30.0 + 600.0)));
    doc.update();
    assert!(doc.laid_out(entries[1500]));
    assert_eq!(color(&doc, entries[1500]), [0, 128, 0, 255]);
    assert_eq!(color(&doc, entries[1501]), [255, 0, 0, 255]);
    assert_eq!(doc.computed_style(entries[1500]).unwrap().opacity, 0.5);
}

#[test]
fn a_list_growing_at_the_end_builds_only_the_new_boxes() {
    let (mut doc, main, mut entries) = list(5000);
    doc.set_virtual_window(main, Some((5000.0 * 30.0 - 600.0, 5000.0 * 30.0)));
    doc.update();
    for i in 0..10 {
        entries.push(el(
            &mut doc,
            main,
            "div",
            "entry",
            Some(&format!("new {i}")),
        ));
    }
    let end = entries.len() as f32 * 30.0;
    doc.set_virtual_window(main, Some((end - 600.0, end)));
    doc.update();
    let st = doc.last_update_stats();
    assert!(
        st.restyled <= 40,
        "the new entries and their neighbours: {st:?}"
    );
    let last = *entries.last().unwrap();
    assert!(doc.laid_out(last));
    assert_eq!(doc.border_box(last).unwrap().y, 5009.0 * 30.0);
    assert_eq!(doc.border_box(main).unwrap().h, 5010.0 * 30.0 - 10.0);

    // Inserted in the middle: every box is rebuilt, in order.
    let mid = doc.create_element("div");
    doc.set_attribute(mid, "class", "entry");
    doc.insert_before(main, mid, Some(entries[2500]));
    doc.update();
    assert_eq!(doc.border_box(entries[2500]).unwrap().y, 2501.0 * 30.0);
    assert_eq!(doc.border_box(last).unwrap().y, 5010.0 * 30.0);
}

#[test]
fn absolutely_positioned_boxes_deep_in_a_subtree_find_their_containing_block() {
    // A positioned container with many children: what each subtree hands up is cached, and
    // must follow what changes under it.
    let mut doc = doc_with(CSS);
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let bx = el(&mut doc, body, "div", "box", None);
    let rows: Vec<NodeId> = (0..50)
        .map(|i| {
            let r = el(&mut doc, bx, "div", "", None);
            el(&mut doc, r, "p", "", Some(&format!("row {i}")));
            r
        })
        .collect();
    doc.update();
    // Added deep in row 20.
    let inner = doc.element_children(rows[20])[0];
    let pin = el(&mut doc, inner, "span", "pin", None);
    doc.update();
    let b = doc.border_box(pin).unwrap();
    assert_eq!((b.x, b.y, b.w), (4.0, 3.0, 5.0), "against the box: {b:?}");
    // A style change makes a deep element absolute.
    let p = doc.element_children(rows[30])[0];
    doc.set_attribute(p, "class", "pin");
    doc.update();
    let b = doc.border_box(p).unwrap();
    assert_eq!((b.x, b.y), (4.0, 3.0), "{b:?}");
    // And back: in the flow again, below rows 0 to 29.
    doc.set_attribute(p, "class", "");
    doc.update();
    assert_eq!(doc.border_box(p).unwrap().y, 30.0 * 10.0);
}

#[test]
fn layout_says_which_boxes_it_computed_since_a_count() {
    let mut doc = doc_with(".a { width: 100px }");
    let root = doc.root();
    let body = el(&mut doc, root, "body", "", None);
    let a = el(&mut doc, body, "div", "a", None);
    let p = el(&mut doc, a, "p", "", None);
    let span = el(&mut doc, p, "span", "", Some("inline text"));
    let other = el(&mut doc, body, "div", "", Some("other"));
    doc.update();
    let before = doc.layout_count();
    doc.update();
    assert_eq!(doc.layout_count(), before, "nothing to lay out");
    assert!(!doc.laid_out_since(span, before));
    // A change inside `p`: its formatting context is laid out again, and the inline box with
    // it; the sibling comes from its cache.
    let t = doc.children(span)[0];
    doc.set_text(t, "longer inline text");
    doc.update();
    assert!(doc.layout_count() > before);
    assert!(doc.laid_out_since(span, before));
    assert!(doc.laid_out_since(p, before));
    assert!(!doc.laid_out_since(other, before));
    // `relayout` drops a box's cache: the next layout computes it again.
    let now = doc.layout_count();
    doc.relayout(other);
    doc.update();
    assert!(doc.laid_out_since(other, now));
    assert!(doc.laid_out(other));
}

/// A window kept on the end (a view following a growing transcript) lays out the end as the
/// list grows, not where the end was; and a layout that changes nothing near the window
/// computes nothing, however long the list.
#[test]
fn a_window_kept_on_the_end_follows_the_end_as_the_list_grows() {
    let (mut doc, main, mut entries) = list(2000);
    // The last 600px, from the end.
    doc.set_virtual_window_at(main, Some((600.0, 0.0)), true);
    doc.update();
    assert!(doc.laid_out(entries[1999]));
    assert!(!doc.laid_out(entries[0]));
    assert!(!doc.laid_out(entries[1900]));
    // Grows by 500: the new end is laid out, the old one goes back to estimates.
    for i in 2000..2500 {
        entries.push(el(
            &mut doc,
            main,
            "div",
            "entry",
            Some(&format!("entry {i}")),
        ));
    }
    doc.update();
    assert!(doc.laid_out(entries[2499]));
    assert!(!doc.laid_out(entries[1999]));
    let b = doc.border_box(entries[2499]).unwrap();
    assert_eq!(b.y, 2499.0 * 30.0);
    // Restyling one entry near the top restyles and lays out little.
    doc.set_attribute(entries[3], "class", "entry red");
    doc.update();
    let st = doc.last_update_stats();
    assert!(st.restyled <= 2 && st.laid_out < 20, "{st:?}");
    assert_eq!(doc.border_box(entries[2499]).unwrap().y, 2499.0 * 30.0);
    // Back to a window from the top.
    doc.set_virtual_window_at(main, Some((0.0, 600.0)), false);
    doc.update();
    assert!(doc.laid_out(entries[0]));
    assert!(!doc.laid_out(entries[2499]));
}
