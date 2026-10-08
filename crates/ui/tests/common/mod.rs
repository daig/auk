//! Test support: a minimal HTML reader for probe pages, the WebKit box reader, and box
//! comparison. The app builds DOMs from TSP nodes; this reader exists only for tests.

#![allow(dead_code)]

use auk_ui::{Document, NodeId, SheetKind};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

pub fn ahem_bytes() -> Vec<u8> {
    std::fs::read(repo_root().join("fixtures/fonts/Ahem.ttf")).expect("Ahem.ttf")
}

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

/// Build a document's DOM from an HTML page. Returns the `body` element. Style elements are
/// installed as author sheets.
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
                    if let Some(pos) = stack.iter().rposition(|n| doc_tag(doc, *n) == name)
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
            if body.is_none() {
                let b = doc.create_element("body");
                doc.append_child(root, b);
                body = Some(b);
                stack = vec![root, b];
            }
            let parent_tag = doc_tag(doc, *stack.last().unwrap());
            if parent_tag == "p" && CLOSES_P.contains(&name.as_str()) {
                stack.pop();
            }
            if name == "tr" && doc_tag(doc, *stack.last().unwrap()) == "table" {
                let tbody = doc.create_element("tbody");
                doc.append_child(*stack.last().unwrap(), tbody);
                stack.push(tbody);
            }
            let el = doc.create_element(&name);
            for (k, v) in &attrs {
                doc.set_attribute(el, k, v);
            }
            doc.append_child(*stack.last().unwrap(), el);
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
        let parent = *stack.last().unwrap();
        let t = doc.create_text(&decode_entities(text));
        doc.append_child(parent, t);
    }
    let errors = doc.set_stylesheet("page", SheetKind::BuiltIn, None, &styles);
    assert!(errors.is_empty(), "probe page CSS errors: {errors:?}");
    body.expect("body")
}

fn doc_tag(doc: &Document, id: NodeId) -> String {
    doc.node(id)
        .element()
        .map(|e| e.tag.as_str().to_string())
        .unwrap_or_default()
}

/// A box from either engine.
#[derive(Clone, Debug)]
pub struct BoxRec {
    pub path: String,
    pub tag: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Our boxes, keyed like webkit-boxes: child-index paths of elements from `body`.
pub fn our_boxes(doc: &Document, body: NodeId) -> Vec<BoxRec> {
    let mut out = Vec::new();
    fn walk(doc: &Document, id: NodeId, path: String, out: &mut Vec<BoxRec>) {
        let r = doc.border_box(id).unwrap_or_default();
        let tag = doc
            .node(id)
            .element()
            .map(|e| e.tag.as_str().to_string())
            .unwrap_or_default();
        out.push(BoxRec {
            path: path.clone(),
            tag,
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
            walk(doc, c, p, out);
        }
    }
    walk(doc, body, String::new(), &mut out);
    out
}

/// Tall enough that probe pages never scroll (a scrollbar would narrow the WebKit viewport).
pub const PAGE_HEIGHT: f32 = 3000.0;

static TOOL: OnceLock<Result<PathBuf, String>> = OnceLock::new();

/// Build the WebKit box reader (fresh, once per test process).
pub fn webkit_tool() -> PathBuf {
    TOOL.get_or_init(|| {
        let src = repo_root().join("crates/conformance/webkit/boxes.swift");
        let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("webkit-boxes");
        let status = Command::new("swiftc")
            .arg("-O")
            .arg("-o")
            .arg(&out)
            .arg(&src)
            .status();
        match status {
            Ok(s) if s.success() => Ok(out),
            Ok(s) => Err(format!("swiftc failed building {}: {s}", src.display())),
            Err(e) => Err(format!(
                "swiftc is required for the WebKit layout comparison and could not run: {e}"
            )),
        }
    })
    .clone()
    .unwrap_or_else(|e| panic!("{e}"))
}

/// Lay a page out in WebKit (with Ahem next to it) and return its boxes.
pub fn webkit_boxes(name: &str, html: &str, width: f32) -> Vec<BoxRec> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("probes");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Ahem.ttf"), ahem_bytes()).unwrap();
    let page = dir.join(format!("{name}.html"));
    std::fs::write(&page, html).unwrap();
    let out = Command::new(webkit_tool())
        .arg(&page)
        .arg("--width")
        .arg(width.to_string())
        .arg("--height")
        .arg(PAGE_HEIGHT.to_string())
        .output()
        .expect("run webkit-boxes");
    assert!(
        out.status.success(),
        "webkit-boxes failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("webkit-boxes JSON");
    v["boxes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| BoxRec {
            path: b["path"].as_str().unwrap().to_string(),
            tag: b["tag"].as_str().unwrap().to_string(),
            x: b["x"].as_f64().unwrap() as f32,
            y: b["y"].as_f64().unwrap() as f32,
            w: b["w"].as_f64().unwrap() as f32,
            h: b["h"].as_f64().unwrap() as f32,
        })
        .collect()
}

/// A document with Ahem registered, laid out at a viewport width.
pub fn our_layout(html: &str, width: f32, height: f32) -> (Document, NodeId) {
    let mut doc = Document::new();
    doc.fonts_mut().register_font(ahem_bytes());
    let body = load_html(&mut doc, html);
    doc.set_viewport(width, height);
    doc.update();
    (doc, body)
}

/// Compare every element box with WebKit's. Returns the mismatch report lines.
pub fn compare(
    name: &str,
    html: &str,
    width: f32,
    tolerance: impl Fn(&BoxRec) -> f32,
) -> Vec<String> {
    let wk = webkit_boxes(name, html, width);
    let (doc, body) = our_layout(html, width, PAGE_HEIGHT);
    let ours = our_boxes(&doc, body);
    let mut bad = Vec::new();
    for w in &wk {
        let Some(o) = ours.iter().find(|o| o.path == w.path) else {
            bad.push(format!("{name} {} <{}>: missing in ours", w.path, w.tag));
            continue;
        };
        let tol = tolerance(w) + 0.01;
        if (o.x - w.x).abs() > tol
            || (o.y - w.y).abs() > tol
            || (o.w - w.w).abs() > tol
            || (o.h - w.h).abs() > tol
        {
            bad.push(format!(
                "{name} {:8} <{}>: ours ({:.2},{:.2} {:.2}x{:.2}) webkit ({:.2},{:.2} {:.2}x{:.2})",
                w.path, w.tag, o.x, o.y, o.w, o.h, w.x, w.y, w.w, w.h
            ));
        }
    }
    bad
}

/// Run a probe page from `tests/probes/<name>.html` and assert every box is within 1 px.
pub fn assert_probe(name: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/probes")
        .join(format!("{name}.html"));
    let html = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let bad = compare(name, &html, 800.0, |_| 1.0);
    assert!(
        bad.is_empty(),
        "{} boxes differ from WebKit by more than 1px:\n{}",
        bad.len(),
        bad.join("\n")
    );
}
