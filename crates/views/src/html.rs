//! Serialize a projected surface (its DOM plus every sheet that applies) to a standalone HTML page,
//! so the layout suite can lay the same DOM and CSS out in WebKit (GOAL.md `layout`).

use crate::dom::Written;
use auk_ui::{Document, NodeId};

fn esc(s: &str, attr: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            c => out.push(c),
        }
    }
    out
}

/// The HTML of an element subtree, with the class and inline style text the views wrote.
pub fn element_html(doc: &Document, written: &Written, el: NodeId, out: &mut String) {
    let n = doc.node(el);
    if let Some(t) = n.text() {
        out.push_str(&esc(t, false));
        return;
    }
    let Some(e) = n.element() else { return };
    let tag = e.tag.as_str();
    out.push('<');
    out.push_str(tag);
    let class = written.class.get(&el).cloned().unwrap_or_else(|| {
        e.classes
            .iter()
            .map(|c| c.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    });
    if !class.is_empty() {
        out.push_str(&format!(" class=\"{}\"", esc(&class, true)));
    }
    if let Some(id) = &e.id {
        out.push_str(&format!(" id=\"{}\"", esc(id.as_str(), true)));
    }
    for (k, v) in &e.attrs {
        out.push_str(&format!(" {}=\"{}\"", k.as_str(), esc(v, true)));
    }
    if let Some(s) = written.style(el) {
        out.push_str(&format!(" style=\"{}\"", esc(s, true)));
    }
    out.push('>');
    if matches!(tag, "hr" | "br" | "input" | "img") {
        return;
    }
    for &c in doc.children(el) {
        element_html(doc, written, c, out);
    }
    out.push_str(&format!("</{tag}>"));
}

/// A standalone page: the built-in sheets, the host's own CSS, each program sheet confined to its
/// scope (with `:where()` so the scope adds no specificity, as in the engine), and the body's
/// subtree. `body` should be the document's `body` element.
pub fn page(
    doc: &Document,
    written: &Written,
    body: NodeId,
    dark: bool,
    host_css: &str,
    program_sheets: &[(String, String)],
) -> String {
    let mut out = String::from("<!DOCTYPE html>\n");
    out.push_str(&format!(
        "<html data-theme=\"{}\"><head><meta charset=\"utf-8\">\n<style>\n{}\n{host_css}\n</style>\n",
        if dark { "dark" } else { "light" },
        crate::sheets::builtin_css()
    ));
    for (scope, css) in program_sheets {
        out.push_str(&format!(
            "<style>\n:where([data-scope=\"{scope}\"]) {{\n{css}\n}}\n</style>\n"
        ));
    }
    out.push_str("</head>");
    // The body's subtree is built with DOM calls, so the HTML parser's fix-ups (a `tbody` under
    // a `table` with `tr` children, `p` auto-closing) can't change it: WebKit lays out the very
    // DOM the engine laid out.
    let mut tree = String::new();
    json_tree(doc, written, body, &mut tree);
    out.push_str("<body><script>\n(function(){\nconst t = ");
    out.push_str(&tree);
    out.push_str(
        r#";
function build(n) {
  if (typeof n === 'string') return document.createTextNode(n);
  const e = document.createElement(n[0]);
  for (const [k, v] of n[1]) e.setAttribute(k, v);
  for (const c of n[2]) e.appendChild(build(c));
  return e;
}
const b = document.body;
for (const [k, v] of t[1]) b.setAttribute(k, v);
for (const c of t[2]) b.appendChild(build(c));
document.currentScript.remove();
})();
</script></body></html>
"#,
    );
    out
}

fn json_str(s: &str) -> String {
    serde_json::Value::String(s.to_owned())
        .to_string()
        .replace("</", "<\\/")
}

/// `[tag, [[name, value]…], [children…]]`, text nodes as strings.
fn json_tree(doc: &Document, written: &Written, el: NodeId, out: &mut String) {
    let n = doc.node(el);
    if let Some(t) = n.text() {
        out.push_str(&json_str(t));
        return;
    }
    let Some(e) = n.element() else {
        out.push_str("\"\"");
        return;
    };
    let mut attrs: Vec<(String, String)> = Vec::new();
    let class = written.class.get(&el).cloned().unwrap_or_else(|| {
        e.classes
            .iter()
            .map(|c| c.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    });
    if !class.is_empty() {
        attrs.push(("class".into(), class));
    }
    for (k, v) in &e.attrs {
        attrs.push((k.as_str().to_owned(), v.clone()));
    }
    if let Some(s) = written.style(el) {
        attrs.push(("style".into(), s.to_owned()));
    }
    out.push('[');
    out.push_str(&json_str(e.tag.as_str()));
    out.push_str(",[");
    for (i, (k, v)) in attrs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("[{},{}]", json_str(k), json_str(v)));
    }
    out.push_str("],[");
    for (i, &c) in doc.children(el).iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        json_tree(doc, written, c, out);
    }
    out.push_str("]]");
}

impl crate::PaneView {
    /// Every program sheet installed by this view, as `(scope, css)` in cascade order.
    pub fn program_sheets(&self) -> Vec<(String, String)> {
        self.surfaces
            .iter()
            .flat_map(|s| {
                s.sheets
                    .iter()
                    .map(|(_, css)| (s.scope.clone(), css.clone()))
            })
            .collect()
    }

    /// The page for `body` (an ancestor of the pane element): the DOM, the built-in sheets, the
    /// host's CSS and the program sheets.
    pub fn to_html(&self, doc: &Document, body: NodeId, host_css: &str) -> String {
        page(
            doc,
            &self.written,
            body,
            self.settings.dark,
            host_css,
            &self.program_sheets(),
        )
    }
}
