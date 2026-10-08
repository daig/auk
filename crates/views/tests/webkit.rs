//! The `el`-plus-stylesheet fixtures (the docs' and the Python SDK's recorded frames) laid out by
//! our engine and by WebKit from the same serialized page: every element box within 2 CSS px or
//! 2% of its size, whichever is larger (GOAL.md `layout`). Exceptions are listed below.

mod common;

use auk_ui::{Document, NodeId};
use common::{HOST_CSS, Harness, replay};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

/// Tall enough that pages never scroll (a scrollbar would narrow the WebKit viewport).
const PAGE_HEIGHT: f32 = 3000.0;
const WIDTH: f32 = 960.0;

/// Boxes we don't compare, with the reason.
const EXCEPTIONS: &[(&str, &str)] = &[];

#[derive(Clone, Debug)]
struct BoxRec {
    path: String,
    tag: String,
    class: String,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

static TOOL: OnceLock<Result<PathBuf, String>> = OnceLock::new();

/// Build the WebKit box reader fresh, once per test process.
fn webkit_tool() -> PathBuf {
    TOOL.get_or_init(|| {
        let src = common::repo_root().join("crates/conformance/webkit/boxes.swift");
        let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("views-webkit-boxes");
        match Command::new("swiftc")
            .arg("-O")
            .arg("-o")
            .arg(&out)
            .arg(&src)
            .status()
        {
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

fn webkit_boxes(name: &str, html: &str) -> Vec<BoxRec> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("views-pages");
    std::fs::create_dir_all(&dir).unwrap();
    let page = dir.join(format!("{name}.html"));
    std::fs::write(&page, html).unwrap();
    let out = Command::new(webkit_tool())
        .arg(&page)
        .arg("--width")
        .arg(WIDTH.to_string())
        .arg("--height")
        .arg(PAGE_HEIGHT.to_string())
        .output()
        .expect("run webkit-boxes");
    assert!(
        out.status.success(),
        "webkit-boxes: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("webkit-boxes JSON");
    v["boxes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| BoxRec {
            path: b["path"].as_str().unwrap().to_owned(),
            tag: b["tag"].as_str().unwrap().to_owned(),
            class: String::new(),
            x: b["x"].as_f64().unwrap() as f32,
            y: b["y"].as_f64().unwrap() as f32,
            w: b["w"].as_f64().unwrap() as f32,
            h: b["h"].as_f64().unwrap() as f32,
        })
        .collect()
}

fn our_boxes(h: &Harness, body: NodeId) -> Vec<BoxRec> {
    fn walk(h: &Harness, doc: &Document, id: NodeId, path: String, out: &mut Vec<BoxRec>) {
        let r = doc.border_box(id).unwrap_or_default();
        out.push(BoxRec {
            path: path.clone(),
            tag: doc
                .node(id)
                .element()
                .map(|e| e.tag.as_str().to_owned())
                .unwrap_or_default(),
            class: h.class(id),
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
            walk(h, doc, c, p, out);
        }
    }
    let mut out = Vec::new();
    walk(h, &h.doc, body, String::new(), &mut out);
    out
}

fn compare(name: &str) -> (usize, Vec<String>) {
    let mut h = replay(&format!("{name}.jsonl"), |_, _| {});
    h.doc.set_viewport(WIDTH, PAGE_HEIGHT);
    h.view.layout(&mut h.doc);
    let html = h.view.to_html(&h.doc, h.body, HOST_CSS);
    let wk = webkit_boxes(name, &html);
    let ours = our_boxes(&h, h.body);
    let mut bad = Vec::new();
    for w in &wk {
        let Some(o) = ours.iter().find(|o| o.path == w.path) else {
            bad.push(format!("{name} {} <{}>: missing in ours", w.path, w.tag));
            continue;
        };
        if EXCEPTIONS
            .iter()
            .any(|(c, _)| o.class.split(' ').any(|x| x == *c))
        {
            continue;
        }
        let tol = (w.w.max(w.h) * 0.02).max(2.0) + 0.01;
        if (o.x - w.x).abs() > tol
            || (o.y - w.y).abs() > tol
            || (o.w - w.w).abs() > tol
            || (o.h - w.h).abs() > tol
        {
            bad.push(format!(
                "{name} {:10} <{} class={:?}>: ours ({:.2},{:.2} {:.2}x{:.2}) webkit ({:.2},{:.2} {:.2}x{:.2})",
                w.path, w.tag, o.class, o.x, o.y, o.w, o.h, w.x, w.y, w.w, w.h
            ));
        }
    }
    assert_eq!(wk.len(), ours.len(), "{name}: the same element count");
    // How close: the largest difference of any box edge or size.
    let worst = wk
        .iter()
        .filter_map(|w| ours.iter().find(|o| o.path == w.path).map(|o| (w, o)))
        .map(|(w, o)| {
            (o.x - w.x)
                .abs()
                .max((o.y - w.y).abs())
                .max((o.w - w.w).abs())
                .max((o.h - w.h).abs())
        })
        .fold(0.0f32, f32::max);
    eprintln!("{name}: {} boxes, worst difference {worst:.2} px", wk.len());
    let sized = wk.iter().filter(|w| w.w > 0.0 && w.h > 0.0).count();
    assert!(sized * 2 > wk.len(), "{name}: most boxes have a size");
    (wk.len(), bad)
}

fn check(name: &str) {
    let (n, bad) = compare(name);
    assert!(n > 5, "{name}: {n} boxes");
    assert!(
        bad.is_empty(),
        "{} of {n} boxes differ from WebKit:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[test]
fn docs_ask_matches_webkit() {
    check("docs-ask");
}

#[test]
fn docs_ls_matches_webkit() {
    check("docs-ls");
}

#[test]
fn python_ask_matches_webkit() {
    check("python-ask");
}

#[test]
fn python_table_matches_webkit() {
    check("python-table");
}
