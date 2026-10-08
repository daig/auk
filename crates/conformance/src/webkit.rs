//! The WebKit oracle for the layout suite (GOAL.md `layout`, spec §18.2): `webkit/boxes.swift`
//! lays a page out in a `WKWebView` and prints every element's border box. The tool is built fresh
//! with `swiftc` in each test process, and its boxes are compared with the UI engine's.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use serde_json::Value;

/// Tall enough that pages never scroll (a scrollbar would narrow the WebKit viewport).
pub const PAGE_HEIGHT: f32 = 3000.0;

/// An element's border box, from either engine. `path` is the element's child-index path from
/// `body` ("0.2.1"), so pages need no ids.
#[derive(Clone, Debug, PartialEq)]
pub struct BoxRec {
    pub path: String,
    pub tag: String,
    /// The class attribute (ours only; WebKit's boxes leave it empty).
    pub class: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl BoxRec {
    /// The largest difference of any edge position or size.
    pub fn distance(&self, other: &BoxRec) -> f32 {
        (self.x - other.x)
            .abs()
            .max((self.y - other.y).abs())
            .max((self.w - other.w).abs())
            .max((self.h - other.h).abs())
    }
}

/// The tool's source.
pub fn source() -> PathBuf {
    crate::probe::repo_root().join("crates/conformance/webkit/boxes.swift")
}

static TOOL: OnceLock<Result<PathBuf, String>> = OnceLock::new();

/// Builds `webkit-boxes` with `swiftc` into `dir` (a test's `CARGO_TARGET_TMPDIR`), once per
/// process, and returns its path. Panics naming `swiftc` when it is missing or the build fails.
///
/// Each process builds to a private name and renames the result into place, so test processes
/// running side by side never execute a half-written binary.
pub fn tool(dir: &Path) -> PathBuf {
    TOOL.get_or_init(|| {
        let src = source();
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let out = dir.join("webkit-boxes");
        let tmp = dir.join(format!("webkit-boxes.{}.tmp", std::process::id()));
        let status = Command::new("swiftc")
            .arg("-O")
            .arg("-o")
            .arg(&tmp)
            .arg(&src)
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => return Err(format!("swiftc failed building {}: {s}", src.display())),
            Err(e) => {
                return Err(format!(
                    "prerequisite missing: `swiftc` (Xcode command line tools) builds the WebKit \
                     layout oracle and could not run: {e}"
                ));
            }
        }
        std::fs::rename(&tmp, &out).map_err(|e| format!("install {}: {e}", out.display()))?;
        Ok(out)
    })
    .clone()
    .unwrap_or_else(|e| panic!("{e}"))
}

/// Lays `html` out in WebKit at `width` CSS px (and [`PAGE_HEIGHT`]) and returns its boxes in
/// document order. `files` are written next to the page first (fonts the page loads, say).
/// `dir` is the test's `CARGO_TARGET_TMPDIR`.
pub fn boxes(
    dir: &Path,
    name: &str,
    html: &str,
    width: f32,
    files: &[(&str, &[u8])],
) -> Vec<BoxRec> {
    let tool = tool(dir);
    let pages = dir.join(format!("webkit-pages-{}", std::process::id()));
    std::fs::create_dir_all(&pages).unwrap_or_else(|e| panic!("{}: {e}", pages.display()));
    for (file, bytes) in files {
        std::fs::write(pages.join(file), bytes).unwrap_or_else(|e| panic!("write {file}: {e}"));
    }
    let page = pages.join(format!("{name}.html"));
    std::fs::write(&page, html).unwrap_or_else(|e| panic!("{}: {e}", page.display()));
    let out = Command::new(&tool)
        .arg(&page)
        .arg("--width")
        .arg(width.to_string())
        .arg("--height")
        .arg(PAGE_HEIGHT.to_string())
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", tool.display()));
    assert!(
        out.status.success(),
        "webkit-boxes failed on {}: {}",
        page.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("webkit-boxes prints JSON");
    let num =
        |b: &Value, k: &str| b[k].as_f64().unwrap_or_else(|| panic!("box without `{k}`")) as f32;
    v["boxes"]
        .as_array()
        .expect("webkit-boxes prints `boxes`")
        .iter()
        .map(|b| BoxRec {
            path: b["path"].as_str().expect("a path").to_owned(),
            tag: b["tag"].as_str().expect("a tag").to_owned(),
            class: String::new(),
            x: num(b, "x"),
            y: num(b, "y"),
            w: num(b, "w"),
            h: num(b, "h"),
        })
        .collect()
}

/// The result of comparing our boxes with WebKit's.
#[derive(Clone, Debug, Default)]
pub struct Comparison {
    /// WebKit's box count.
    pub boxes: usize,
    /// Our box count.
    pub ours: usize,
    /// The largest difference over all compared boxes.
    pub worst: f32,
    /// One line per box beyond tolerance, or missing in ours.
    pub bad: Vec<String>,
    /// Boxes skipped as listed exceptions.
    pub skipped: usize,
}

/// Compares every WebKit box with ours at the same path. `tolerance` gives the allowed difference
/// for a WebKit box; `skip` names boxes excluded as listed exceptions.
pub fn compare(
    name: &str,
    webkit: &[BoxRec],
    ours: &[BoxRec],
    tolerance: impl Fn(&BoxRec) -> f32,
    skip: impl Fn(&BoxRec) -> bool,
) -> Comparison {
    let mut c = Comparison {
        boxes: webkit.len(),
        ours: ours.len(),
        ..Comparison::default()
    };
    for w in webkit {
        let Some(o) = ours.iter().find(|o| o.path == w.path) else {
            c.bad
                .push(format!("{name} {} <{}>: missing in ours", w.path, w.tag));
            continue;
        };
        if skip(o) {
            c.skipped += 1;
            continue;
        }
        let d = o.distance(w);
        c.worst = c.worst.max(d);
        // A hundredth of a pixel of slack for float rounding.
        if d > tolerance(w) + 0.01 {
            c.bad.push(format!(
                "{name} {:10} <{}{}>: ours ({:.2},{:.2} {:.2}x{:.2}) webkit ({:.2},{:.2} {:.2}x{:.2})",
                w.path,
                w.tag,
                if o.class.is_empty() { String::new() } else { format!(" class={:?}", o.class) },
                o.x,
                o.y,
                o.w,
                o.h,
                w.x,
                w.y,
                w.w,
                w.h
            ));
        }
    }
    c
}

/// Tolerance for pages in the real fonts: 2 CSS px or 2% of the box's size, whichever is larger.
pub fn real_font_tolerance(b: &BoxRec) -> f32 {
    (b.w.max(b.h) * 0.02).max(2.0)
}
