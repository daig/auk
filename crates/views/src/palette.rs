//! Program palettes (§7.12, book:9955-10010): `--sf-p-*` variables, semantic and tone overrides,
//! `Bg` fills as 60% tints, and contrast correction (4.5:1 body, 3:1 quiet tokens) against the
//! surface background and every tint over it.

use crate::common::fmt_num;
use auk_tsp::Palette;

/// The surface backgrounds of the built-in theme (`--page`), dark and light.
pub const PAGE_DARK: [u8; 3] = [0x15, 0x16, 0x1a];
pub const PAGE_LIGHT: [u8; 3] = [0xfb, 0xfb, 0xfc];

const FILLS: [(&str, &str); 9] = [
    ("toolSuccessBg", "--sf-tint-success"),
    ("toolErrorBg", "--sf-tint-error"),
    ("toolPendingBg", "--sf-tint-pending"),
    ("userMessageBg", "--sf-tint-user"),
    ("customMessageBg", "--sf-tint-custom"),
    ("cardBg", "--sf-tint-neutral"),
    ("infoBg", "--sf-tint-info"),
    ("selectedBg", "--sf-sel"),
    ("statusLineBg", "--sf-status-bg"),
];

/// Semantic span colors and their palette sources, first present wins.
const SEMANTIC: [(&str, &[&str]); 14] = [
    ("accent", &["accent"]),
    ("success", &["success"]),
    ("warning", &["warning"]),
    ("error", &["error"]),
    ("muted", &["muted"]),
    ("dim", &["dim"]),
    ("ins", &["toolDiffAdded", "success"]),
    ("del", &["toolDiffRemoved", "error"]),
    ("link", &["mdLink", "accent"]),
    ("path", &["statusLinePath", "mdLinkUrl"]),
    ("code", &["mdCode"]),
    ("num", &["syntaxNumber"]),
    ("info", &["syntaxType"]),
    ("key", &[]),
];

/// Tones the palette reaches.
const TONES: [(&str, &str); 5] = [
    ("--sf-accent", "accent"),
    ("--sf-ok", "success"),
    ("--sf-warn", "warning"),
    ("--sf-bad", "error"),
    ("--sf-muted", "muted"),
];

/// Highlight variables from `syntax*` tokens.
const SYNTAX: [(&str, &str); 9] = [
    ("--tk-comment", "syntaxComment"),
    ("--tk-keyword", "syntaxKeyword"),
    ("--tk-function", "syntaxFunction"),
    ("--tk-variable", "syntaxVariable"),
    ("--tk-string", "syntaxString"),
    ("--tk-number", "syntaxNumber"),
    ("--tk-type", "syntaxType"),
    ("--tk-operator", "syntaxOperator"),
    ("--tk-punct", "syntaxPunctuation"),
];

/// Quiet tokens get 3:1; everything else 4.5:1.
fn is_quiet(token: &str) -> bool {
    matches!(
        token,
        "muted"
            | "dim"
            | "thinkingOff"
            | "syntaxComment"
            | "mdHr"
            | "border"
            | "borderMuted"
            | "borderAccent"
            | "mdCodeBlockBorder"
            | "mdQuoteBorder"
            | "statusLineSep"
    )
}

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn lin(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG relative luminance.
pub fn luminance(c: [u8; 3]) -> f64 {
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

pub fn contrast(a: [u8; 3], b: [u8; 3]) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

fn mix(a: [u8; 3], b: [u8; 3], t: f64) -> [u8; 3] {
    let m = |x: u8, y: u8| {
        (x as f64 + (y as f64 - x as f64) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2])]
}

/// Move `c` toward white (dark backgrounds) or black until it reaches `ratio` against every
/// background in `bgs`.
pub fn correct(c: [u8; 3], bgs: &[[u8; 3]], ratio: f64, dark: bool) -> [u8; 3] {
    let target = if dark { [255, 255, 255] } else { [0, 0, 0] };
    let ok = |x: [u8; 3]| bgs.iter().all(|b| contrast(x, *b) >= ratio);
    if ok(c) {
        return c;
    }
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..20 {
        let mid = (lo + hi) / 2.0;
        if ok(mix(c, target, mid)) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    mix(c, target, hi)
}

/// The inline variables a palette sets on the region elements, for the appearance `dark`.
pub fn variables(p: &Palette, dark: bool) -> String {
    let variant = if dark {
        if p.dark.is_empty() { &p.light } else { &p.dark }
    } else if p.light.is_empty() {
        &p.dark
    } else {
        &p.light
    };
    let page = if dark { PAGE_DARK } else { PAGE_LIGHT };
    // Tints: fills at 60% over the page.
    let mut bgs = vec![page];
    let mut out = String::new();
    for (token, var) in FILLS {
        if let Some((_, v)) = variant.iter().find(|(k, _)| k == token)
            && let Some(c) = parse_hex(v)
        {
            bgs.push(mix(page, c, 0.6));
            out.push_str(&format!("{var}: rgb({} {} {} / 60%); ", c[0], c[1], c[2]));
            if token == "selectedBg" {
                out.push_str(&format!(
                    "--sf-mark-bg: rgb({} {} {} / 60%); ",
                    c[0], c[1], c[2]
                ));
            }
        }
    }
    let mut corrected: Vec<(String, [u8; 3])> = Vec::new();
    for (k, v) in variant {
        if k.ends_with("Bg") {
            continue;
        }
        let Some(c) = parse_hex(v) else { continue };
        let ratio = if is_quiet(k) { 3.0 } else { 4.5 };
        let c = correct(c, &bgs, ratio, dark);
        corrected.push((k.clone(), c));
        if k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            out.push_str(&format!("--sf-p-{k}: {}; ", hex(c)));
        }
    }
    let get = |k: &str| corrected.iter().find(|(n, _)| n == k).map(|(_, c)| hex(*c));
    for (sem, sources) in SEMANTIC {
        if let Some(c) = sources.iter().find_map(|s| get(s)) {
            out.push_str(&format!("--sf-c-{sem}: {c}; "));
        }
    }
    for (var, token) in TONES {
        if let Some(c) = get(token) {
            out.push_str(&format!("{var}: {c}; "));
        }
    }
    for (var, token) in SYNTAX {
        if let Some(c) = get(token) {
            out.push_str(&format!("{var}: {c}; "));
        }
    }
    if let Some(c) = get("text") {
        out.push_str(&format!("--sf-text: {c}; "));
    }
    let _ = fmt_num;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn palette_variables_and_contrast() {
        let body = json!({"dark": {"accent": "#00b4ff", "mdLink": "#1a1a40", "muted": "#9ca3b0", "toolSuccessBg": "#0f2a1c", "pageBg": "#000000"}});
        let p = Palette::parse(body.as_object().unwrap());
        let v = variables(&p, true);
        assert!(v.contains("--sf-p-accent: #00b4ff;"), "{v}");
        assert!(v.contains("--sf-tint-success: rgb(15 42 28 / 60%)"), "{v}");
        assert!(!v.contains("pageBg"), "{v}");
        assert!(v.contains("--sf-accent: #00b4ff;"));
        assert!(v.contains("--sf-c-link: "));
        // The dark link was too dark: corrected to at least 4.5:1.
        let link = v.split("--sf-c-link: ").nth(1).unwrap()[..7].to_owned();
        assert!(
            contrast(parse_hex(&link).unwrap(), PAGE_DARK) >= 4.5,
            "{link}"
        );
        // Light appearance with only a dark variant uses the dark one.
        assert!(variables(&p, false).contains("--sf-p-accent"));
    }
}
