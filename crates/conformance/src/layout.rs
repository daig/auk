//! The UI engine's side of the layout comparison: probe pages read
//! into an `auk_ui::Document`, laid out, and their element boxes keyed like `webkit-boxes`.
//!
//! The probe pages live in `crates/conformance/probes`; each exercises one layout mode in the Ahem
//! test font (`fixtures/fonts/Ahem.ttf`), whose glyphs are exact squares, so text metrics agree
//! with WebKit exactly. The reader here is a minimal HTML reader for those pages only: the app
//! builds its DOMs from TSP nodes.

use std::path::{Path, PathBuf};

use auk_ui::SheetKind;
pub use auk_ui::{Document, NodeId};

use crate::probe::repo_root;
use crate::webkit::{self, BoxRec, Comparison, PAGE_HEIGHT};

/// The probe pages' directory.
pub fn probes_dir() -> PathBuf {
    repo_root().join("crates/conformance/probes")
}

/// A probe page's HTML.
pub fn probe_html(name: &str) -> String {
    let path = probes_dir().join(format!("{name}.html"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The Ahem test font.
pub fn ahem_bytes() -> Vec<u8> {
    let path = repo_root().join("fixtures/fonts/Ahem.ttf");
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The width probe pages are laid out at.
pub const PROBE_WIDTH: f32 = 800.0;

const VOID: &[&str] = &[
    "input", "hr", "br", "img", "meta", "link", "col", "area", "base", "wbr",
];
const CLOSES_P: &[&str] = &[
    "div",
    "p",
    "ul",
    "ol",
    "dl",
    "table",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "pre",
    "section",
    "header",
    "footer",
    "nav",
    "aside",
    "main",
    "article",
    "figure",
    "blockquote",
    "form",
    "hr",
];

fn decode_entities(s: &str) -> String {
    s.replace("&nbsp;", "\u{a0}")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

fn parse_attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'/' {
            i += 1;
        }
        if start == i {
            break;
        }
        let name = s[start..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                i += 1;
                let vs = i;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
                value = s[vs..i].to_string();
                i += 1;
            } else {
                let vs = i;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = s[vs..i].to_string();
            }
        }
        out.push((name, decode_entities(&value)));
    }
    out
}

fn tag_of(doc: &Document, id: NodeId) -> String {
    doc.node(id)
        .element()
        .map(|e| e.tag.as_str().to_string())
        .unwrap_or_default()
}

/// Builds a document's DOM from a probe page and returns its `body`. The page's `style` elements
/// are installed as one sheet; CSS errors panic, since a probe must use only what the engine
/// accepts.
pub fn load_html(doc: &mut Document, html: &str) -> NodeId {
    let root = doc.root();
    let mut styles = String::new();
    let mut stack: Vec<NodeId> = vec![root];
    let mut body: Option<NodeId> = None;
    let mut in_head = false;
    let mut rest = html;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("<!--") {
            rest = r.find("-->").map(|i| &r[i + 3..]).unwrap_or("");
            continue;
        }
        if rest.starts_with("<!") {
            rest = rest.find('>').map(|i| &rest[i + 1..]).unwrap_or("");
            continue;
        }
        if let Some(r) = rest.strip_prefix("</") {
            let end = r.find('>').unwrap_or(r.len());
            let name = r[..end].trim().to_ascii_lowercase();
            rest = r.get(end + 1..).unwrap_or("");
            match name.as_str() {
                "head" => in_head = false,
                "html" | "body" => {}
                _ => {
                    // Pop to the matching element.
                    if let Some(pos) = stack.iter().rposition(|n| tag_of(doc, *n) == name)
                        && pos > 0
                    {
                        stack.truncate(pos);
                    }
                }
            }
            continue;
        }
        if rest.starts_with('<') && rest[1..].starts_with(|c: char| c.is_ascii_alphabetic()) {
            let end = rest.find('>').unwrap_or(rest.len());
            let inner = &rest[1..end];
            rest = rest.get(end + 1..).unwrap_or("");
            let name_end = inner
                .find(|c: char| c.is_ascii_whitespace() || c == '/')
                .unwrap_or(inner.len());
            let name = inner[..name_end].to_ascii_lowercase();
            let attrs = parse_attrs(&inner[name_end..]);
            match name.as_str() {
                "html" => continue,
                "head" => {
                    in_head = true;
                    continue;
                }
                "style" => {
                    let close = rest.find("</style>").unwrap_or(rest.len());
                    styles.push_str(&rest[..close]);
                    styles.push('\n');
                    rest = rest.get(close + 8..).unwrap_or("");
                    continue;
                }
                "title" => {
                    let close = rest.find("</title>").unwrap_or(rest.len());
                    rest = rest.get(close + 8..).unwrap_or("");
                    continue;
                }
                "meta" | "link" => continue,
                "body" => {
                    in_head = false;
                    let b = doc.create_element("body");
                    for (k, v) in &attrs {
                        doc.set_attribute(b, k, v);
                    }
                    doc.append_child(root, b);
                    body = Some(b);
                    stack = vec![root, b];
                    continue;
                }
                _ => {}
            }
            let parent = match body {
                Some(_) => *stack.last().expect("the stack holds the root"),
                None => {
                    let b = doc.create_element("body");
                    doc.append_child(root, b);
                    body = Some(b);
                    stack = vec![root, b];
                    b
                }
            };
            if tag_of(doc, parent) == "p" && CLOSES_P.contains(&name.as_str()) {
                stack.pop();
            }
            let parent = *stack.last().expect("the stack holds the root");
            if name == "tr" && tag_of(doc, parent) == "table" {
                let tbody = doc.create_element("tbody");
                doc.append_child(parent, tbody);
                stack.push(tbody);
            }
            let el = doc.create_element(&name);
            for (k, v) in &attrs {
                doc.set_attribute(el, k, v);
            }
            doc.append_child(*stack.last().expect("the stack holds the root"), el);
            if !VOID.contains(&name.as_str()) && !inner.trim_end().ends_with('/') {
                stack.push(el);
            }
            continue;
        }
        let end = rest[1..].find('<').map(|i| i + 1).unwrap_or(rest.len());
        let text = &rest[..end];
        rest = &rest[end..];
        if in_head || body.is_none() {
            continue;
        }
        let parent = *stack.last().expect("the stack holds the root");
        let t = doc.create_text(&decode_entities(text));
        doc.append_child(parent, t);
    }
    let errors = doc.set_stylesheet("page", SheetKind::BuiltIn, None, &styles);
    assert!(errors.is_empty(), "probe page CSS errors: {errors:?}");
    body.expect("the page has a body")
}

/// Our element boxes under `body`, keyed like `webkit-boxes`. `class` reads an element's class
/// text (the views keep what they wrote; probe pages have none worth reporting).
pub fn our_boxes(doc: &Document, body: NodeId, class: &dyn Fn(NodeId) -> String) -> Vec<BoxRec> {
    fn walk(
        doc: &Document,
        id: NodeId,
        path: String,
        class: &dyn Fn(NodeId) -> String,
        out: &mut Vec<BoxRec>,
    ) {
        let r = doc.border_box(id).unwrap_or_default();
        out.push(BoxRec {
            path: path.clone(),
            tag: tag_of(doc, id),
            class: class(id),
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
        });
        for (i, c) in doc.element_children(id).into_iter().enumerate() {
            let p = if path.is_empty() {
                i.to_string()
            } else {
                format!("{path}.{i}")
            };
            walk(doc, c, p, class, out);
        }
    }
    let mut out = Vec::new();
    walk(doc, body, String::new(), class, &mut out);
    out
}

/// A document with Ahem registered, holding `html`, laid out at `width` × [`PAGE_HEIGHT`].
pub fn our_layout(html: &str, width: f32) -> (Document, NodeId) {
    let mut doc = Document::new();
    doc.fonts_mut().register_font(ahem_bytes());
    let body = load_html(&mut doc, html);
    doc.set_viewport(width, PAGE_HEIGHT);
    doc.update();
    (doc, body)
}

/// Lays `html` out in both engines at [`PROBE_WIDTH`] (Ahem beside the page for WebKit) and
/// compares every box. `dir` is the test's `CARGO_TARGET_TMPDIR`.
pub fn compare_page(
    dir: &Path,
    name: &str,
    html: &str,
    tolerance: impl Fn(&BoxRec) -> f32,
) -> Comparison {
    let ahem = ahem_bytes();
    let wk = webkit::boxes(dir, name, html, PROBE_WIDTH, &[("Ahem.ttf", &ahem)]);
    let (doc, body) = our_layout(html, PROBE_WIDTH);
    let ours = our_boxes(&doc, body, &|_| String::new());
    webkit::compare(name, &wk, &ours, tolerance, |_| false)
}

/// Asserts that probe page `name` matches WebKit within `tolerance` on every box, and returns the
/// comparison. With `PROBE_DIFF=1` in the environment, every box differing by more than
/// 0.05 px is printed first (a diagnostic for engine work).
pub fn assert_page(
    dir: &Path,
    name: &str,
    html: &str,
    tolerance: impl Fn(&BoxRec) -> f32,
) -> Comparison {
    if std::env::var_os("PROBE_DIFF").is_some() {
        for line in compare_page(dir, name, html, |_| 0.05).bad {
            eprintln!("{line}");
        }
    }
    let c = compare_page(dir, name, html, tolerance);
    assert!(c.boxes > 1, "{name}: WebKit reported {} boxes", c.boxes);
    assert_eq!(
        c.boxes, c.ours,
        "{name}: both engines see the same elements"
    );
    assert!(
        c.bad.is_empty(),
        "{} of {} boxes differ from WebKit beyond tolerance:\n{}",
        c.bad.len(),
        c.boxes,
        c.bad.join("\n")
    );
    eprintln!(
        "{name}: {} boxes, worst difference {:.2} px",
        c.boxes, c.worst
    );
    c
}

/// Asserts that a probe page in Ahem matches WebKit within 1 CSS px on every box.
pub fn assert_probe(dir: &Path, name: &str) -> Comparison {
    assert_page(dir, name, &probe_html(name), |_| 1.0)
}
