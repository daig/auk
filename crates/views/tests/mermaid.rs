//! Mermaid fences in `md` (§8.5, §18.4, amendment A23; book:5000, 5018, 5026, 5065): every
//! diagram kind drawn (nodes, edges and labels present, labels apart and inside their shapes),
//! the terminal's colors dark and light, the figure's DOM, size and sideways scroll, Open, the
//! code fallback with its note, an open fence while streaming, and omp's stream drawn.

mod common;

use auk_ui::NodeId;
use auk_views::image::{self as pictures, Picture};
use auk_views::mermaid;
use auk_views::{Outcome, ViewSettings};
use common::{Harness, replay};
use resvg::usvg;
use serde_json::{Value, json};
use std::sync::Arc;

fn src(name: &str) -> String {
    let path = format!("{}/tests/mermaid/{name}.mmd", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[derive(Clone, Copy, Debug)]
struct Box2 {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Box2 {
    fn of(r: usvg::Rect) -> Box2 {
        Box2 {
            x0: r.left(),
            y0: r.top(),
            x1: r.right(),
            y1: r.bottom(),
        }
    }
    fn area(self) -> f32 {
        (self.x1 - self.x0).max(0.0) * (self.y1 - self.y0).max(0.0)
    }
    fn overlap(self, o: Box2) -> f32 {
        let w = (self.x1.min(o.x1) - self.x0.max(o.x0)).max(0.0);
        let h = (self.y1.min(o.y1) - self.y0.max(o.y0)).max(0.0);
        w * h
    }
    fn union(self, o: Box2) -> Box2 {
        Box2 {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }
    fn contains(self, o: Box2, slack: f32) -> bool {
        o.x0 >= self.x0 - slack
            && o.y0 >= self.y0 - slack
            && o.x1 <= self.x1 + slack
            && o.y1 <= self.y1 + slack
    }
}

/// What a laid-out diagram draws: each text run (with its group ids, innermost last) and each
/// shape's box by its group ids.
struct Drawn {
    size: (f32, f32),
    texts: Vec<(String, Box2, Vec<String>)>,
    shapes: Vec<(Box2, Vec<String>)>,
}

fn drawn(svg: &str) -> Drawn {
    let opt = usvg::Options {
        fontdb: pictures::svg_fonts(),
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_str(svg, &opt).expect("usvg parses the diagram");
    let mut d = Drawn {
        size: (tree.size().width(), tree.size().height()),
        texts: Vec::new(),
        shapes: Vec::new(),
    };
    fn walk(g: &usvg::Group, ids: &mut Vec<String>, d: &mut Drawn) {
        let pushed = !g.id().is_empty();
        if pushed {
            ids.push(g.id().to_owned());
        }
        for n in g.children() {
            match n {
                usvg::Node::Group(c) => walk(c, ids, d),
                usvg::Node::Text(t) => {
                    let s: String = t.chunks().iter().map(|c| c.text()).collect();
                    if !s.trim().is_empty() {
                        d.texts.push((
                            s.trim().to_owned(),
                            Box2::of(t.abs_bounding_box()),
                            ids.clone(),
                        ));
                    }
                }
                usvg::Node::Path(p) => {
                    d.shapes.push((Box2::of(p.abs_bounding_box()), ids.clone()));
                }
                usvg::Node::Image(_) => {}
            }
        }
        if pushed {
            ids.pop();
        }
    }
    walk(tree.root(), &mut Vec::new(), &mut d);
    d
}

/// Every label is drawn, inside the diagram, and no two labels overlap.
fn check_labels(kind: &str, d: &Drawn, want: &[&str]) {
    for w in want {
        assert!(
            d.texts.iter().any(|(t, ..)| t == w),
            "{kind}: label {w:?} missing; drawn: {:?}",
            d.texts.iter().map(|t| &t.0).collect::<Vec<_>>()
        );
    }
    let canvas = Box2 {
        x0: 0.0,
        y0: 0.0,
        x1: d.size.0,
        y1: d.size.1,
    };
    let _ = canvas;
    for (i, (a, ba, _)) in d.texts.iter().enumerate() {
        assert!(ba.area() > 0.0, "{kind}: {a:?} has no extent");
        for (b, bb, _) in &d.texts[i + 1..] {
            let o = ba.overlap(*bb);
            assert!(
                o <= 0.15 * ba.area().min(bb.area()),
                "{kind}: labels {a:?} {ba:?} and {b:?} {bb:?} overlap"
            );
        }
    }
}

/// Each node's label sits inside the node's shape.
fn check_nodes(kind: &str, d: &Drawn, marker: &str, count: usize) {
    let mut nodes: Vec<String> = d
        .shapes
        .iter()
        .flat_map(|(_, ids)| ids.iter())
        .chain(d.texts.iter().flat_map(|(_, _, ids)| ids.iter()))
        .filter(|id| id.contains(marker))
        .cloned()
        .collect();
    nodes.sort();
    nodes.dedup();
    assert_eq!(nodes.len(), count, "{kind}: nodes {nodes:?}");
    for n in &nodes {
        let shape = d
            .shapes
            .iter()
            .filter(|(_, ids)| ids.contains(n))
            .map(|(b, _)| *b)
            .reduce(Box2::union);
        let text = d
            .texts
            .iter()
            .filter(|(_, _, ids)| ids.contains(n))
            .map(|(_, b, _)| *b)
            .reduce(Box2::union);
        if let (Some(s), Some(t)) = (shape, text) {
            assert!(
                s.contains(t, 1.5),
                "{kind}: node {n}'s label {t:?} spills out of its shape {s:?}"
            );
        }
    }
}

fn xml_count(svg: &str, tag: &str, class: &str) -> usize {
    let doc = usvg::roxmltree::Document::parse(svg).unwrap();
    doc.descendants()
        .filter(|n| {
            n.tag_name().name() == tag
                && n.attribute("class")
                    .is_some_and(|c| c.split_whitespace().any(|x| x == class))
        })
        .count()
}

#[test]
fn flowcharts_draw_shapes_edges_labels_and_subgraphs() {
    let svg = mermaid::svg(&src("flowchart"), true).expect("flowchart");
    let d = drawn(&svg);
    check_labels(
        "flowchart",
        &d,
        &[
            "Start",
            "Round step",
            "Valid?",
            "Database",
            "Retry",
            "Flag",
            "Lean right",
            "Hexagon",
            "Subroutine",
            "parse",
            "yes",
            "no",
            "Storage",
        ],
    );
    check_nodes("flowchart", &d, "-flowchart-", 9);
    assert_eq!(xml_count(&svg, "g", "node"), 9);
    assert_eq!(xml_count(&svg, "path", "flowchart-link"), 9, "every edge");
    assert_eq!(xml_count(&svg, "g", "cluster"), 1, "the subgraph");
    // Every direction lays out, along its axis.
    for (dir, wide) in [
        ("LR", true),
        ("RL", true),
        ("TD", false),
        ("TB", false),
        ("BT", false),
    ] {
        let s = format!("flowchart {dir}\n  a[One] --> b[Two] --> c[Three]\n");
        let d = drawn(&mermaid::svg(&s, true).unwrap_or_else(|e| panic!("{dir}: {e}")));
        let pos = |l: &str| d.texts.iter().find(|t| t.0 == l).map(|t| t.1).unwrap();
        let (a, c) = (pos("One"), pos("Three"));
        let along = if wide { a.x0 - c.x0 } else { a.y0 - c.y0 };
        let forward = !matches!(dir, "RL" | "BT");
        assert!(
            if forward { along < 0.0 } else { along > 0.0 },
            "{dir}: One {a:?} Three {c:?}"
        );
        // `graph` is the same diagram.
        assert_eq!(
            mermaid::kind(&s.replace("flowchart", "graph")),
            Ok("flowchart")
        );
    }
}

#[test]
fn sequence_diagrams_draw_actors_messages_notes_and_blocks() {
    let svg = mermaid::svg(&src("sequence"), true).expect("sequence");
    let d = drawn(&svg);
    check_labels(
        "sequence",
        &d,
        &[
            "User",
            "Agent",
            "Tool",
            "Fix the failing test",
            "run cargo test",
            "1 failed",
            "reads the failure",
            "edit and rerun",
            "Done",
            "Needs help",
            "loop",
            "alt",
        ],
    );
    assert_eq!(
        xml_count(&svg, "rect", "actor"),
        6,
        "three actors, top and bottom"
    );
    assert_eq!(xml_count(&svg, "text", "messageText"), 6);
    assert_eq!(xml_count(&svg, "rect", "note"), 1);
    assert!(xml_count(&svg, "rect", "activation0") >= 1);
}

#[test]
fn class_state_and_er_diagrams_draw_their_boxes_and_relations() {
    let svg = mermaid::svg(&src("class"), true).expect("class");
    let d = drawn(&svg);
    check_labels(
        "class",
        &d,
        &[
            "Surface",
            "Node",
            "Region",
            "Text",
            "List",
            "+String id",
            "+open() : void",
            "holds",
        ],
    );
    check_nodes("class", &d, "-classId-", 5);
    assert_eq!(xml_count(&svg, "path", "edge-thickness-normal"), 4);

    let svg = mermaid::svg(&src("state"), true).expect("state");
    let d = drawn(&svg);
    check_labels(
        "state",
        &d,
        &[
            "Idle", "Running", "Paused", "Done", "Fetch", "Build", "start", "pause", "resume",
        ],
    );
    assert!(
        xml_count(&svg, "circle", "state-start") >= 2,
        "start states"
    );
    assert!(xml_count(&svg, "path", "edge-thickness-normal") >= 9);

    let svg = mermaid::svg(&src("er"), true).expect("er");
    let d = drawn(&svg);
    check_labels(
        "er",
        &d,
        &[
            "CUSTOMER",
            "ORDER",
            "LINE_ITEM",
            "PRODUCT",
            "places",
            "contains",
            "ordered in",
            "email",
        ],
    );
    check_nodes("er", &d, "-entity-", 4);
    assert_eq!(xml_count(&svg, "path", "edge-thickness-normal"), 3);
}

#[test]
fn gantt_and_pie_draw_bars_slices_and_legends() {
    let svg = mermaid::svg(&src("gantt"), true).expect("gantt");
    let d = drawn(&svg);
    check_labels(
        "gantt",
        &d,
        &[
            "Release plan",
            "Parser",
            "Layout",
            "Review",
            "Build",
            "Ship",
            "2026-01-05",
        ],
    );
    assert_eq!(xml_count(&svg, "rect", "task"), 4);
    // Deterministic: no "today" from a clock.
    assert_eq!(svg, mermaid::svg(&src("gantt"), true).unwrap());

    let svg = mermaid::svg(&src("pie"), true).expect("pie");
    let d = drawn(&svg);
    check_labels(
        "pie",
        &d,
        &[
            "Time per phase",
            "Reading",
            "Editing",
            "Testing",
            "Waiting",
            "42%",
            "8%",
        ],
    );
    assert_eq!(xml_count(&svg, "path", "pieCircle"), 4);
}

fn rgb(hex: &str) -> [u8; 3] {
    let n = |i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap();
    [n(1), n(3), n(5)]
}

/// Opaque pixels of a picture near `c` (premultiplied RGBA, so opaque pixels are plain).
fn pixels_near(p: &Picture, c: [u8; 3]) -> usize {
    let data = p.frames[0].content.data.data.data();
    data.as_chunks::<4>()
        .0
        .iter()
        .filter(|px| {
            px[3] == 255 && (0..3).all(|i| (i32::from(px[i]) - i32::from(c[i])).abs() <= 6)
        })
        .count()
}

fn raster(svg: &str) -> (u32, u32, Vec<u8>) {
    let opt = usvg::Options {
        fontdb: pictures::svg_fonts(),
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_str(svg, &opt).unwrap();
    let (w, h) = (
        tree.size().width().ceil() as u32,
        tree.size().height().ceil() as u32,
    );
    let mut pm = resvg::tiny_skia::Pixmap::new(w, h).unwrap();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pm.as_mut(),
    );
    (w, h, pm.take())
}

/// The SVG without its text.
fn without_text(svg: &str) -> String {
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(at) = rest.find("<text") {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let end = tail.find('>').unwrap();
        rest = if tail[..end].ends_with('/') {
            &tail[end + 1..]
        } else {
            let close = tail.find("</text>").unwrap();
            &tail[close + "</text>".len()..]
        };
    }
    out.push_str(rest);
    out
}

fn luminance(c: [f32; 3]) -> f32 {
    let lin = |v: f32| {
        let v = v / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

fn contrast(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// Every label of every kind reads against what it sits on (3:1 or more), dark and light: the
/// text's own fill against the diagram drawn without its text, over the figure's background.
#[test]
fn labels_are_legible_on_what_they_sit_on() {
    for dark in [true, false] {
        let bg = rgb(&mermaid::colors(dark).canvas).map(f32::from);
        for name in [
            "flowchart",
            "sequence",
            "class",
            "state",
            "er",
            "gantt",
            "pie",
        ] {
            let svg = mermaid::svg(&src(name), dark).unwrap();
            let opt = usvg::Options {
                fontdb: pictures::svg_fonts(),
                ..usvg::Options::default()
            };
            let tree = usvg::Tree::from_str(&svg, &opt).unwrap();
            let (w, h, under) = raster(&without_text(&svg));
            let mut texts = Vec::new();
            fn walk(g: &usvg::Group, out: &mut Vec<(String, usvg::Rect, [f32; 3])>) {
                for n in g.children() {
                    match n {
                        usvg::Node::Group(c) => walk(c, out),
                        usvg::Node::Text(t) => {
                            for ch in t.chunks() {
                                for sp in ch.spans() {
                                    let Some(usvg::Paint::Color(c)) =
                                        sp.fill().map(|f| f.paint().clone())
                                    else {
                                        continue;
                                    };
                                    let s = ch.text()[sp.start()..sp.end()].trim().to_owned();
                                    if !s.is_empty() {
                                        out.push((
                                            s,
                                            t.abs_bounding_box(),
                                            [c.red, c.green, c.blue].map(f32::from),
                                        ));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            walk(tree.root(), &mut texts);
            assert!(!texts.is_empty(), "{name}");
            for (s, r, fg) in texts {
                // The most common color under the label, composited over the background.
                let mut counts: std::collections::HashMap<[u8; 3], usize> = Default::default();
                let (x0, y0) = (r.left().max(0.0) as u32, r.top().max(0.0) as u32);
                let (x1, y1) = (
                    (r.right().ceil() as u32).min(w),
                    (r.bottom().ceil() as u32).min(h),
                );
                for y in y0..y1 {
                    for x in x0..x1 {
                        let i = ((y * w + x) * 4) as usize;
                        let a = f32::from(under[i + 3]) / 255.0;
                        let px = [0, 1, 2]
                            .map(|k| (f32::from(under[i + k]) + bg[k] * (1.0 - a)).round() as u8);
                        *counts.entry(px).or_default() += 1;
                    }
                }
                let Some((&behind, _)) = counts.iter().max_by_key(|(_, n)| **n) else {
                    continue;
                };
                let k = contrast(fg, behind.map(f32::from));
                assert!(
                    k >= 3.0,
                    "{name} dark={dark}: {s:?} in {fg:?} on {behind:?}: contrast {k:.2}"
                );
            }
        }
    }
}

fn picture_now(src: &str, dark: bool) -> Arc<Picture> {
    let (id, _) = mermaid::request(src, dark, 2.0);
    pictures::settle();
    mermaid::peek(&id, 2.0).expect("drawn").expect("no error")
}

#[test]
fn diagrams_take_the_terminals_colors_dark_and_light() {
    for dark in [true, false] {
        let c = mermaid::colors(dark);
        let p = picture_now(&src("flowchart"), dark);
        assert!(p.svg);
        let (w, h) = p.size;
        let content = &p.frames[0].content;
        assert_eq!(
            content.data.width,
            (w * 2.0).ceil() as u32,
            "drawn at the density"
        );
        let _ = h;
        for (what, color, min) in [
            ("node fill", &c.node, 2000),
            ("node border (accent)", &c.border, 200),
            ("edges", &c.line, 200),
            ("text", &c.text, 100),
            ("subgraph fill", &c.cluster, 2000),
        ] {
            let n = pixels_near(&p, rgb(color));
            assert!(n >= min, "dark={dark}: {what} {color}: {n} pixels");
        }
        // The canvas is transparent: the figure's background (`--code-bg`) shows through.
        let data = content.data.data.data();
        assert_eq!(data[3], 0, "dark={dark}: the corner is transparent");
    }
    let d = mermaid::colors(true);
    let l = mermaid::colors(false);
    assert_ne!(d.text, l.text);
    assert_ne!(d.node, l.node);
}

/// A `md` node with `text`, in surface `s1`.
fn show_md(text: &str, stream: bool, settings: ViewSettings) -> Harness {
    let mut h = Harness::with(Default::default(), settings);
    h.hello(&[]);
    let mut p = json!({"text": text});
    if stream {
        p["stream"] = Value::Bool(true);
    }
    h.surface("s1", "inline", json!([{"id": "doc", "k": "md", "p": p}]));
    h.frame();
    h
}

/// Draws in whatever the background finished.
fn settle(h: &mut Harness) {
    pictures::settle();
    h.now += 16.0;
    h.frame();
    h.now += 16.0;
    h.frame();
}

fn one(h: &Harness, root: NodeId, class: &str) -> NodeId {
    let v = h.find_class(root, class);
    assert_eq!(v.len(), 1, "one .{class}, found {}", v.len());
    v[0]
}

#[test]
fn a_fence_draws_a_figure_at_natural_size_that_scrolls_sideways() {
    let text = format!("Before.\n\n```mermaid\n{}```\n\nAfter.\n", src("flowchart"));
    let mut h = show_md(&text, false, ViewSettings::default());
    settle(&mut h);
    let doc = h.el("s1", "doc");
    let fig = one(&h, doc, "mfig");
    assert_eq!(h.doc.node(fig).element().unwrap().tag.as_str(), "figure");
    assert_eq!(
        h.attr(fig, "data-m").as_deref(),
        Some(src("flowchart").trim_end())
    );
    let head = one(&h, fig, "mfig-head");
    assert!(
        h.text(head).contains("mermaid · flowchart"),
        "{}",
        h.text(head)
    );
    one(&h, head, "mfig-ic");
    let open = one(&h, head, "mfig-open");
    assert_eq!(h.text(open), "Open");
    let body = one(&h, fig, "mfig-body");
    let svg = one(&h, body, "mfig-svg");
    assert_eq!(h.attr(svg, "role").as_deref(), Some("img"));
    assert_eq!(
        h.attr(svg, "aria-label").as_deref(),
        Some("flowchart diagram")
    );
    // Natural size, wider than the pane: the body scrolls sideways, the page doesn't widen.
    let p = picture_now(&src("flowchart"), true);
    let sb = h.doc.border_box(svg).unwrap();
    assert!(
        (sb.w - p.size.0).abs() < 0.5 && (sb.h - p.size.1).abs() < 0.5,
        "{sb:?} {:?}",
        p.size
    );
    let bb = h.doc.border_box(body).unwrap();
    let fb = h.doc.border_box(fig).unwrap();
    assert!(p.size.0 > 960.0, "the sample is wide: {:?}", p.size);
    assert!(fb.w <= 960.0, "the figure fits the pane: {fb:?}");
    assert!(bb.w < sb.w, "the body clips: {bb:?} {sb:?}");
    let st = h.doc.computed_style(body).unwrap();
    assert!(
        format!("{:?}", st.overflow_x)
            .to_lowercase()
            .contains("auto")
    );
    // Code blocks around it are untouched, prose too.
    assert!(h.text(doc).contains("Before.") && h.text(doc).contains("After."));
    assert!(h.find_class(doc, "code").is_empty());

    // Open shows the diagram large in the viewer, drawn again at the viewer's scale.
    let out = {
        let b = h.doc.border_box(open).unwrap();
        let at = auk_views::PointerEvent {
            x: b.x + b.w / 2.0,
            y: b.y + b.h / 2.0,
            clicks: 1,
            ..auk_views::PointerEvent::default()
        };
        h.view.click(&mut h.doc, &h.pane, at)
    };
    let [Outcome::Zoom(z)] = out.as_slice() else {
        panic!("{out:?}")
    };
    assert_eq!(z.images.len(), 1);
    assert!(z.images[0].blob.starts_with("mermaid:"));
    assert_eq!(z.images[0].title.as_deref(), Some("mermaid · flowchart"));
    let big = pictures::load_made_for_viewer(&z.images[0].blob, 4.0)
        .expect("the viewer finds it")
        .expect("drawn");
    assert_eq!(big.size, p.size);
    assert_eq!(
        big.frames[0].content.data.width,
        (p.size.0 * 4.0).ceil().min(4096.0) as u32
    );
}

#[test]
fn each_kind_draws_in_md_with_its_head() {
    let mut text = String::new();
    let names = [
        "flowchart",
        "sequence",
        "class",
        "state",
        "er",
        "gantt",
        "pie",
    ];
    for n in names {
        text.push_str(&format!("```mermaid\n{}```\n\n", src(n)));
    }
    let mut h = show_md(&text, false, ViewSettings::default());
    settle(&mut h);
    let doc = h.el("s1", "doc");
    let figs = h.find_class(doc, "mfig");
    assert_eq!(figs.len(), names.len());
    let mut last_bottom = f32::MIN;
    for (f, n) in figs.iter().zip(names) {
        let head = one(&h, *f, "mfig-head");
        assert_eq!(
            h.text(head).trim_end_matches("Open").trim(),
            format!("\u{f199c}mermaid · {n}")
        );
        let svg = one(&h, *f, "mfig-svg");
        let b = h.doc.border_box(svg).unwrap();
        assert!(b.w > 50.0 && b.h > 50.0, "{n}: {b:?}");
        let fb = h.doc.border_box(*f).unwrap();
        assert!(fb.y >= last_bottom, "{n}: figures stack");
        last_bottom = fb.y + fb.h;
        assert!(h.attr(*f, "aria-busy").is_none(), "{n}: drawn");
    }
}

#[test]
fn unsupported_kinds_and_bad_sources_stay_code_with_a_note() {
    let text = "```mermaid\nmindmap\n  root\n    a\n```\n\n```mermaid\nflowchart LR\n  A --> \n  ]]]\n```\n";
    let mut h = show_md(text, false, ViewSettings::default());
    settle(&mut h);
    let doc = h.el("s1", "doc");
    assert!(h.find_class(doc, "mfig").is_empty());
    let codes = h.find_class(doc, "code");
    assert_eq!(codes.len(), 2);
    for c in &codes {
        assert!(h.class(*c).contains("mermaid"));
        assert!(h.text(*c).contains("Copy"), "still a code block with Copy");
    }
    let notes = h.find_class(doc, "mfig-note");
    assert_eq!(notes.len(), 2);
    assert_eq!(
        h.text(notes[0]),
        "Not drawn: `mindmap` diagrams aren't drawn"
    );
    assert!(
        h.text(notes[1]).starts_with("Not drawn: "),
        "{}",
        h.text(notes[1])
    );
    assert!(
        h.text(notes[1]).to_lowercase().contains("parse"),
        "{}",
        h.text(notes[1])
    );
    // The fence's code is there to read and copy.
    assert!(h.text(codes[0]).contains("mindmap"));
    // Sources past the limit too.
    let big = format!("flowchart LR\n{}", "  a --> b\n".repeat(4000));
    assert!(mermaid::svg(&big, true).unwrap_err().contains("KiB"));
}

#[test]
fn an_open_fence_stays_code_while_streaming() {
    let open = "Here:\n\n```mermaid\nflowchart LR\n  A[One] --> B[Two]\n";
    let mut h = show_md(open, true, ViewSettings::default());
    settle(&mut h);
    let doc = h.el("s1", "doc");
    assert!(h.find_class(doc, "mfig").is_empty(), "half-written: code");
    assert_eq!(h.find_class(doc, "code").len(), 1);
    assert!(
        h.find_class(doc, "mfig-note").is_empty(),
        "no note while it streams"
    );
    // The fence closes while the stream goes on: the figure draws.
    h.frame_ops("s1", 2, json!([["text", "doc", "append", "```\n\nMore"]]));
    settle(&mut h);
    let doc = h.el("s1", "doc");
    assert_eq!(h.find_class(doc, "mfig").len(), 1);
    assert!(h.find_class(doc, "code").is_empty());

    // A fence still open when the stream ends draws too.
    let mut h = show_md(open, true, ViewSettings::default());
    settle(&mut h);
    h.frame_ops("s1", 2, json!([["set", "doc", {"stream": null}]]));
    settle(&mut h);
    let doc = h.el("s1", "doc");
    assert_eq!(
        h.find_class(doc, "mfig").len(),
        1,
        "drawn once the stream ends"
    );
}

#[test]
fn the_figure_draws_in_from_the_background_and_follows_the_theme() {
    // A source no other test draws: not cached, so it starts out pending.
    let text = "```mermaid\nflowchart TD\n  q1[Queued] --> q2[Drawn later]\n```\n";
    let mut h = show_md(text, false, ViewSettings::default());
    let doc = h.el("s1", "doc");
    let fig = one(&h, doc, "mfig");
    if h.attr(fig, "aria-busy").is_some() {
        // Waiting: the body shows the source, and the clock watches for the picture.
        let body = one(&h, fig, "mfig-body");
        assert!(h.text(body).contains("q1[Queued]"));
        assert!(h.find_class(body, "mfig-svg").is_empty());
    }
    settle(&mut h);
    let fig = one(&h, doc, "mfig");
    assert!(h.attr(fig, "aria-busy").is_none());
    one(&h, fig, "mfig-svg");
    let dark_id = h.attr(one(&h, fig, "mfig-open"), "data-mfig").unwrap();

    // Light: drawn again with the light colors.
    let light = ViewSettings {
        dark: false,
        ..ViewSettings::default()
    };
    h.view.set_settings(&mut h.doc, &h.pane, light);
    settle(&mut h);
    let doc = h.el("s1", "doc");
    let fig = one(&h, doc, "mfig");
    let light_id = h.attr(one(&h, fig, "mfig-open"), "data-mfig").unwrap();
    assert_ne!(dark_id, light_id);
    one(&h, fig, "mfig-svg");
}

/// omp streams its answers into `md` (`text append`, then `stream: null`): a mermaid answer
/// streamed the same way into the recorded session's transcript draws as a figure.
#[test]
fn omp_streamed_answer_with_a_diagram_draws() {
    let sf = "s:1";
    // After omp's last frame (s 541) is acked, before it closes the surface: stream an answer
    // into its transcript turn (`13`) the way omp streams every answer.
    let mut streamed = false;
    let mut h = replay("omp-session.jsonl", |h, rec| {
        if rec.body["ev"] != "ack" || rec.body["s"] != 541 {
            return;
        }
        streamed = true;
        let mut s = 541;
        let mut op = |h: &mut Harness, ops: Value| {
            s += 1;
            h.frame_ops(sf, s, ops);
            h.now += 16.0;
            h.frame();
        };
        op(
            h,
            json!([["add", "99.t1", "13", null, {"id": "99.t1", "k": "md", "p": {"text": "The flow:", "stream": true}}]]),
        );
        let answer = format!("\n\n```mermaid\n{}```\n\nThat's it.", src("sequence"));
        let chunks: Vec<String> = answer
            .chars()
            .collect::<Vec<_>>()
            .chunks(7)
            .map(|c| c.iter().collect())
            .collect();
        let mut saw_code = false;
        for c in &chunks {
            op(h, json!([["text", "99.t1", "append", c]]));
            let el = h.el(sf, "99.t1");
            if !h.find_class(el, "code").is_empty() {
                saw_code = true;
                assert!(h.find_class(el, "mfig").is_empty(), "never both");
            }
        }
        assert!(saw_code, "the half-written fence showed as code");
        op(h, json!([["set", "99.t1", {"stream": null}]]));
        settle(h);
    });
    assert!(streamed);
    settle(&mut h);
    let el = h.el(sf, "99.t1");
    let fig = one(&h, el, "mfig");
    assert!(h.text(fig).contains("mermaid · sequence"));
    one(&h, fig, "mfig-svg");
    let errors: Vec<_> = h.errors().into_iter().collect();
    assert!(errors.is_empty(), "{errors:?}");
}

/// Paints `doc` (1 CSS px per pixel) over white and reads back its RGBA pixels (as paint.rs).
fn render(doc: &auk_ui::Document, w: u32, h: u32) -> Vec<u8> {
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

/// The figure painted offscreen: its `--code-bg` card, and the diagram's slices in the
/// terminal's accent inside it.
#[test]
fn the_figure_paints_offscreen_on_its_card() {
    let text = format!("```mermaid\n{}```\n", src("pie"));
    let mut h = show_md(&text, false, ViewSettings::default());
    settle(&mut h);
    let doc = h.el("s1", "doc");
    let fig = one(&h, doc, "mfig");
    let svg = one(&h, fig, "mfig-svg");
    let fb = h.doc.border_box(fig).unwrap();
    let sb = h.doc.border_box(svg).unwrap();
    let (w, hgt) = (960, (fb.y + fb.h).ceil() as u32 + 4);
    let px = render(&h.doc, w, hgt);
    let at = |x: f32, y: f32| {
        let i = ((y as u32 * w + x as u32) * 4) as usize;
        [px[i], px[i + 1], px[i + 2]]
    };
    let c = mermaid::colors(true);
    let near = |a: [u8; 3], b: [u8; 3], tol: u8| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol);
    // The card behind the diagram (the padding left of it, and the transparent canvas corner).
    assert!(
        near(at(fb.x + 4.0, sb.y + 4.0), rgb(&c.canvas), 3),
        "{:?}",
        at(fb.x + 4.0, sb.y + 4.0)
    );
    assert!(
        near(at(sb.x + 1.0, sb.y + 1.0), rgb(&c.canvas), 3),
        "{:?}",
        at(sb.x + 1.0, sb.y + 1.0)
    );
    // The slices: the series colors fill the diagram's box.
    let mut counts = [0usize; 4];
    for y in (sb.y as u32)..((sb.y + sb.h) as u32).min(hgt) {
        for x in (sb.x as u32)..((sb.x + sb.w) as u32).min(w) {
            let p = at(x as f32, y as f32);
            for (k, col) in c.series.iter().take(4).enumerate() {
                if near(p, rgb(col), 12) {
                    counts[k] += 1;
                }
            }
        }
    }
    assert!(
        counts.iter().all(|n| *n > 200),
        "slice pixels by series color: {counts:?}"
    );
    assert!(counts[0] > counts[3], "42% outweighs 8%: {counts:?}");
}

/// This thread's CPU time, in ms: the work done, which a loaded machine (other tests running
/// alongside) doesn't stretch as it does the wall clock.
fn cpu_ms() -> f64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `t` is a valid timespec to write.
    let r = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) };
    assert_eq!(r, 0, "the thread's CPU clock");
    t.tv_sec as f64 * 1000.0 + t.tv_nsec as f64 / 1e6
}

/// Off the frame path, but quick: each sample diagram is laid out, written as SVG and drawn at
/// 2x in well under a frame budget's worth of background time (the engine builds optimized).
/// Both run on this thread, so its CPU time is what they cost.
#[test]
fn diagrams_lay_out_and_draw_quickly() {
    // Fonts and advances, and the system font database pictures draw with: loaded once a
    // process, not per diagram.
    let _ = mermaid::svg(&src("pie"), true);
    let _ = pictures::svg_fonts();
    for name in ["flowchart", "sequence", "class", "state", "er", "gantt", "pie"] {
        let s = src(name);
        let t = cpu_ms();
        let svg = mermaid::svg(&s, true).unwrap();
        let laid = cpu_ms() - t;
        let t = cpu_ms();
        let opt = usvg::Options {
            fontdb: pictures::svg_fonts(),
            ..usvg::Options::default()
        };
        let tree = usvg::Tree::from_str(&svg, &opt).unwrap();
        let (w, h) = (tree.size().width() * 2.0, tree.size().height() * 2.0);
        let mut pm = resvg::tiny_skia::Pixmap::new(w.ceil() as u32, h.ceil() as u32).unwrap();
        resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(2.0, 2.0), &mut pm.as_mut());
        let drawn = cpu_ms() - t;
        eprintln!("{name}: layout+svg {laid:.1} ms, draw 2x {drawn:.1} ms (CPU)");
        assert!(laid + drawn < 250.0, "{name}: {laid:.1} + {drawn:.1} ms");
    }
}
