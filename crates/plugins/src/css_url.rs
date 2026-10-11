//! `url()` in plugin sheets: a plugin file's `url()` resolves against
//! the plugin's directory when the sheet is installed and is inlined as a `data:` URL, so the UI
//! engine only ever loads `data:` pictures. `data:` URLs pass everywhere, remote sheets included;
//! `url(#id)` references are kept as written.
//!
//! Refused, and left as written so the engine drops the declaration: absolute paths, other
//! schemes (`https:`, `file:`), `..`, links out of the plugin directory, missing files, files over
//! 16 MiB, types other than the picture formats, and any file `url()` in a remote host's sheet
//! (`dir` is `None`). No percent-decoding; `?query` and `#fragment` are ignored.

use std::collections::HashSet;
use std::path::{Component, Path};
use std::sync::{Mutex, OnceLock};

use base64::Engine as _;

/// The largest file a `url()` inlines (a blob's limit).
const MAX_FILE: u64 = 16 << 20;

/// `css` with each plugin file `url()` inlined as a `data:` URL, and a message for each refused
/// one (`plugin style sheet url() <url>: <why>`). `dir` is the plugin's directory, or `None` for a
/// remote host's sheet.
pub fn inline_urls(css: &str, dir: Option<&Path>) -> (String, Vec<String>) {
    let mut out = String::with_capacity(css.len());
    let mut refused = Vec::new();
    let mut rest = css;
    while let Some(at) = find_url(rest) {
        out.push_str(&rest[..at]);
        let after = &rest[at + 4..];
        let Some((target, len)) = url_arg(after) else {
            out.push_str(&rest[at..at + 4]);
            rest = after;
            continue;
        };
        let whole = &rest[at..at + 4 + len];
        match resolve(&target, dir) {
            Ok(Some(data)) => {
                out.push_str("url(\"");
                out.push_str(&data);
                out.push_str("\")");
            }
            Ok(None) => out.push_str(whole),
            Err(why) => {
                refused.push(format!("plugin style sheet url() {target}: {why}"));
                out.push_str(whole);
            }
        }
        rest = &rest[at + 4 + len..];
    }
    out.push_str(rest);
    (out, refused)
}

/// Like [`inline_urls`], logging each refused URL once per process.
pub fn inline_urls_logged(css: &str, dir: Option<&Path>, plugin: &str) -> String {
    static LOGGED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let (css, refused) = inline_urls(css, dir);
    if !refused.is_empty() {
        let mut logged = LOGGED
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for msg in refused {
            if logged.insert(format!("{plugin}\0{msg}")) {
                tracing::warn!(%plugin, "{msg}");
            }
        }
    }
    css
}

/// The byte offset of the next `url(`, ignoring case and comments.
fn find_url(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    while i + 4 <= b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            i = s[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
            continue;
        }
        if b[i..i + 4].eq_ignore_ascii_case(b"url(")
            && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'-'))
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// The argument after `url(` and the length through the closing `)`.
fn url_arg(s: &str) -> Option<(String, usize)> {
    let trimmed = s.trim_start();
    let lead = s.len() - trimmed.len();
    let (arg, used) = match trimmed.chars().next()? {
        q @ ('"' | '\'') => {
            let end = trimmed[1..].find(q)?;
            (trimmed[1..1 + end].to_owned(), end + 2)
        }
        _ => {
            let end = trimmed.find(')')?;
            (trimmed[..end].trim_end().to_owned(), end)
        }
    };
    let tail = &trimmed[used..];
    let close = tail.find(')')?;
    if !tail[..close].trim().is_empty() {
        return None;
    }
    Some((arg, lead + used + close + 1))
}

/// `Some(data URL)` for a plugin file, `None` to keep the URL as written (`data:`, `#id`).
fn resolve(target: &str, dir: Option<&Path>) -> Result<Option<String>, String> {
    let lower = target.to_ascii_lowercase();
    if lower.starts_with("data:") || target.starts_with('#') {
        return Ok(None);
    }
    let Some(dir) = dir else {
        return Err("file url() is refused in a remote host's sheet".into());
    };
    // `?query` and `#fragment` are ignored; no percent-decoding.
    let path = target.split(['?', '#']).next().unwrap_or_default();
    if path.is_empty() {
        return Err("empty".into());
    }
    if let Some(colon) = path.find(':')
        && !path[..colon].contains('/')
    {
        return Err("only plugin files and data: URLs".into());
    }
    let rel = Path::new(path);
    if rel.is_absolute() || path.starts_with('/') || path.starts_with('\\') {
        return Err("absolute paths are refused".into());
    }
    if rel.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("`..` is refused".into());
    }
    let root = dir
        .canonicalize()
        .map_err(|e| format!("the plugin directory: {e}"))?;
    let file = root
        .join(rel)
        .canonicalize()
        .map_err(|_| "no such file".to_owned())?;
    if !file.starts_with(&root) {
        return Err("it leads out of the plugin directory".into());
    }
    let meta = std::fs::metadata(&file).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a file".into());
    }
    if meta.len() > MAX_FILE {
        return Err("over 16 MiB".into());
    }
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mime = match ext.as_str() {
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "tif" | "tiff" => "image/tiff",
        _ => return Err("not a picture".into()),
    };
    let bytes = std::fs::read(&file).map_err(|e| e.to_string())?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(Some(format!("data:{mime};base64,{b64}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_files_inline_and_everything_else_is_refused() {
        let root = std::env::temp_dir().join(format!("auk-css-url-{}", std::process::id()));
        let dir = root.join("plugin");
        std::fs::create_dir_all(dir.join("img")).unwrap();
        std::fs::write(
            dir.join("img/dot.svg"),
            "<svg xmlns='http://www.w3.org/2000/svg'/>",
        )
        .unwrap();
        std::fs::write(root.join("outside.png"), b"x").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("outside.png"), dir.join("link.png")).unwrap();
        let css = ".a { background-image: url(img/dot.svg?v=2) }\n\
                   .b { background: URL( \"img/dot.svg#frag\" ) }\n\
                   .c { background-image: url(data:image/png;base64,AAAA) }\n\
                   .d { filter: url(#blur) }\n\
                   /* url(img/dot.svg) in a comment */\n\
                   .e { background-image: url(/etc/hosts) }\n\
                   .f { background-image: url(https://example.com/x.png) }\n\
                   .g { background-image: url(../outside.png) }\n\
                   .h { background-image: url(link.png) }\n\
                   .i { background-image: url(missing.png) }\n\
                   .j { background-image: url(notes.txt) }";
        let (out, refused) = inline_urls(css, Some(&dir));
        let svg = base64::engine::general_purpose::STANDARD
            .encode("<svg xmlns='http://www.w3.org/2000/svg'/>");
        let data = format!("url(\"data:image/svg+xml;base64,{svg}\")");
        assert_eq!(out.matches(&data).count(), 2, "{out}");
        for kept in [
            "url(data:image/png;base64,AAAA)",
            "url(#blur)",
            "/* url(img/dot.svg) in a comment */",
            "url(/etc/hosts)",
            "url(https://example.com/x.png)",
            "url(../outside.png)",
            "url(missing.png)",
            "url(notes.txt)",
        ] {
            assert!(out.contains(kept), "{kept} in {out}");
        }
        let n = if cfg!(unix) { 6 } else { 5 };
        assert_eq!(refused.len(), n, "{refused:#?}");
        assert!(
            refused
                .iter()
                .all(|r| r.starts_with("plugin style sheet url() "))
        );
        // A remote host's sheet: data: only.
        let (out, refused) = inline_urls(css, None);
        assert!(!out.contains(&data));
        assert!(out.contains("url(data:image/png;base64,AAAA)"));
        assert!(refused.iter().any(|r| r.contains("remote host")));
        let _ = std::fs::remove_dir_all(&root);
    }
}
