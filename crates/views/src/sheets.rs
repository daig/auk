//! The terminal's built-in sheets (§8.4 cascade step 2): kit base, theme variables (dark and
//! light), highlight, motion, the `surface*` sheets that give each kind its look, and the pane
//! chrome. Program sheets cascade after them.

use auk_ui::{Document, SheetKind};

/// `(name, css)` in cascade order.
pub const BUILTIN: [(&str, &str); 12] = [
    ("base", include_str!("css/base.css")),
    ("theme", include_str!("css/theme.css")),
    ("highlight", include_str!("css/highlight.css")),
    ("motion", include_str!("css/motion.css")),
    ("surface", include_str!("css/surface.css")),
    ("surface-content", include_str!("css/surface-content.css")),
    ("surface-data", include_str!("css/surface-data.css")),
    ("surface-list", include_str!("css/surface-list.css")),
    ("surface-input", include_str!("css/surface-input.css")),
    ("surface-agent", include_str!("css/surface-agent.css")),
    ("surface-el", include_str!("css/surface-el.css")),
    ("chrome", include_str!("css/chrome.css")),
];

/// Install the built-in sheets (once per document, before any program sheet) and set the
/// appearance. Returns parse errors, which should be none.
pub fn install_sheets(doc: &mut Document, dark: bool) -> Vec<String> {
    let mut errors = Vec::new();
    for (name, css) in BUILTIN {
        for e in doc.set_stylesheet(name, SheetKind::BuiltIn, None, css) {
            errors.push(format!("{name}: {e}"));
        }
    }
    set_theme(doc, dark);
    errors
}

/// The appearance: `data-theme` on the root and the engine's `light-dark()` choice.
pub fn set_theme(doc: &mut Document, dark: bool) {
    let root = doc.root();
    doc.set_attribute(root, "data-theme", if dark { "dark" } else { "light" });
    doc.set_dark(dark);
}

/// All built-in CSS, concatenated in cascade order (for serialized pages).
pub fn builtin_css() -> String {
    let mut s = String::new();
    for (name, css) in BUILTIN {
        s.push_str(&format!("/* {name} */\n{css}\n"));
    }
    s
}
