//! Inline Markdown in omp's composer: an `editor` inside an `omp.editor*` role
//! draws its Markdown in place, as `md-*` spans over the text it already shows. Nothing is hidden
//! or replaced: every character, markers included, stays where it is, so the caret, selections,
//! clicks and what the program receives map exactly as in a plain editor. The spans only style.
//!
//! Recognized per line: ATX headings, block quotes, list markers and fenced code; inline code
//! spans, strong, emphasis, strikethrough and links (`[text](url)`). Markers carry `md-mark`.

use std::ops::Range;

const H: u16 = 1;
const QUOTE: u16 = 1 << 1;
const LI: u16 = 1 << 2;
const MARK: u16 = 1 << 3;
const STRONG: u16 = 1 << 4;
const EM: u16 = 1 << 5;
const CODE: u16 = 1 << 6;
const STRIKE: u16 = 1 << 7;
const LINK: u16 = 1 << 8;
const URL: u16 = 1 << 9;
const FENCE: u16 = 1 << 10;
const PRE: u16 = 1 << 11;

/// Class names by flag, in the order they're written.
const NAMES: [(u16, &str); 12] = [
    (H, "md-h"),
    (QUOTE, "md-quote"),
    (LI, "md-li"),
    (FENCE, "md-fence"),
    (PRE, "md-pre"),
    (STRONG, "md-strong"),
    (EM, "md-em"),
    (STRIKE, "md-strike"),
    (CODE, "md-code"),
    (LINK, "md-link"),
    (URL, "md-url"),
    (MARK, "md-mark"),
];

/// The styled runs of each line of `text` (split at `\n`), as byte ranges within the line and
/// their classes. Unstyled text has no run.
pub(crate) fn runs(text: &str) -> Vec<Vec<(Range<usize>, String)>> {
    let mut fence: Option<(u8, usize)> = None;
    text.split('\n')
        .map(|line| {
            let mut f = vec![0u16; line.len()];
            line_flags(line, &mut f, &mut fence);
            merge(&f)
        })
        .collect()
}

fn merge(f: &[u16]) -> Vec<(Range<usize>, String)> {
    let mut out: Vec<(Range<usize>, String)> = Vec::new();
    let mut i = 0;
    while i < f.len() {
        let v = f[i];
        let mut j = i + 1;
        while j < f.len() && f[j] == v {
            j += 1;
        }
        if v != 0 {
            let class: Vec<&str> = NAMES
                .iter()
                .filter(|(b, _)| v & b != 0)
                .map(|(_, n)| *n)
                .collect();
            out.push((i..j, class.join(" ")));
        }
        i = j;
    }
    out
}

fn set(f: &mut [u16], r: Range<usize>, flag: u16) {
    for x in &mut f[r] {
        *x |= flag;
    }
}

fn line_flags(line: &str, f: &mut [u16], fence: &mut Option<(u8, usize)>) {
    let b = line.as_bytes();
    let indent = b.iter().take_while(|c| **c == b' ').count();
    // Fenced code: the fence lines and everything between them.
    if indent < 4 {
        let rest = &b[indent..];
        let ch = rest.first().copied().filter(|c| *c == b'`' || *c == b'~');
        let n = ch.map_or(0, |c| rest.iter().take_while(|x| **x == c).count());
        match (*fence, ch) {
            (Some((c, len)), Some(x)) if x == c && n >= len => {
                *fence = None;
                set(f, 0..b.len(), FENCE | MARK);
                return;
            }
            (None, Some(c)) if n >= 3 => {
                *fence = Some((c, n));
                set(f, 0..b.len(), FENCE | MARK);
                return;
            }
            _ => {}
        }
    }
    if fence.is_some() {
        set(f, 0..b.len(), PRE);
        return;
    }
    let mut at = indent;
    let mut whole = 0u16;
    // Block quotes (possibly nested), then a heading or a list marker.
    while at < b.len() && b[at] == b'>' {
        let end = if b.get(at + 1) == Some(&b' ') {
            at + 2
        } else {
            at + 1
        };
        set(f, at..end, MARK);
        whole |= QUOTE;
        at = end;
    }
    let hashes = b[at..].iter().take_while(|c| **c == b'#').count();
    if (1..=6).contains(&hashes) && matches!(b.get(at + hashes), None | Some(b' ')) {
        let end = (at + hashes + 1).min(b.len());
        set(f, at..end, MARK);
        whole |= H;
        at = end;
    } else if let Some(end) = list_marker(b, at) {
        set(f, at..end, LI | MARK);
        at = end;
    }
    if whole != 0 {
        set(f, 0..b.len(), whole);
    }
    inline(line, at..b.len(), f);
}

/// The end of a list marker (`- `, `* `, `+ `, `1. `, `1) `, an optional `[ ]`/`[x]` task box)
/// starting at `at`.
fn list_marker(b: &[u8], at: usize) -> Option<usize> {
    let mut end = match b.get(at)? {
        b'-' | b'*' | b'+' => at + 1,
        c if c.is_ascii_digit() => {
            let d = b[at..].iter().take_while(|c| c.is_ascii_digit()).count();
            if d > 9 || !matches!(b.get(at + d), Some(b'.' | b')')) {
                return None;
            }
            at + d + 1
        }
        _ => return None,
    };
    if b.get(end) != Some(&b' ') {
        return None;
    }
    end += 1;
    if b.len() >= end + 4
        && b[end] == b'['
        && matches!(b[end + 1], b' ' | b'x' | b'X')
        && b[end + 2] == b']'
        && b[end + 3] == b' '
    {
        end += 4;
    }
    Some(end)
}

/// Inline spans over `r` of `line`.
fn inline(line: &str, r: Range<usize>, f: &mut [u16]) {
    let b = line.as_bytes();
    let mut i = r.start;
    while i < r.end {
        let c = b[i];
        // Code spans: a run of backticks to the next run of the same length.
        if c == b'`' {
            let n = b[i..r.end].iter().take_while(|x| **x == b'`').count();
            let mut j = i + n;
            let mut close = None;
            while j < r.end {
                if b[j] == b'`' {
                    let m = b[j..r.end].iter().take_while(|x| **x == b'`').count();
                    if m == n {
                        close = Some(j);
                        break;
                    }
                    j += m;
                } else {
                    j += 1;
                }
            }
            match close {
                Some(j) => {
                    set(f, i..j + n, CODE);
                    set(f, i..i + n, MARK);
                    set(f, j..j + n, MARK);
                    i = j + n;
                }
                None => i += n,
            }
            continue;
        }
        // Links: `[text](url)`.
        if c == b'['
            && let Some(close) = find(b, i + 1, r.end, b"](")
            && let Some(end) = find(b, close + 2, r.end, b")")
            && close > i + 1
        {
            set(f, i..end + 1, LINK);
            set(f, i..i + 1, MARK);
            set(f, close..close + 2, MARK);
            set(f, end..end + 1, MARK);
            set(f, close + 2..end, URL);
            inline(line, i + 1..close, f);
            i = end + 1;
            continue;
        }
        // Strong, strikethrough (doubled delimiters), then emphasis (single ones).
        let pair = |d: &[u8]| b[i..r.end].starts_with(d);
        let mut matched = false;
        for (d, flag) in [(&b"**"[..], STRONG), (b"__", STRONG), (b"~~", STRIKE)] {
            if pair(d)
                && opens(b, i + 2, r.end)
                && let Some(j) = find(b, i + 3, r.end, d)
                && b[j - 1] != b' '
            {
                set(f, i..j + 2, flag);
                set(f, i..i + 2, MARK);
                set(f, j..j + 2, MARK);
                inline(line, i + 2..j, f);
                i = j + 2;
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }
        if (c == b'*' || c == b'_')
            && opens(b, i + 1, r.end)
            && (c == b'*' || i == r.start || !b[i - 1].is_ascii_alphanumeric())
            && let Some(j) = find_single(b, i + 2, r.end, c)
            && b[j - 1] != b' '
            && (c == b'*' || !b.get(j + 1).is_some_and(u8::is_ascii_alphanumeric))
        {
            set(f, i..j + 1, EM);
            set(f, i..i + 1, MARK);
            set(f, j..j + 1, MARK);
            inline(line, i + 1..j, f);
            i = j + 1;
            continue;
        }
        // Step a whole character (the flags are per byte).
        i += line[i..].chars().next().map_or(1, char::len_utf8);
    }
}

/// Content may start at `at`: a character that isn't a space.
fn opens(b: &[u8], at: usize, end: usize) -> bool {
    at < end && b[at] != b' '
}

fn find(b: &[u8], from: usize, end: usize, pat: &[u8]) -> Option<usize> {
    if from >= end || end - from < pat.len() {
        return None;
    }
    (from..=end - pat.len()).find(|&k| &b[k..k + pat.len()] == pat)
}

/// The next lone `c` (not doubled) from `from`.
fn find_single(b: &[u8], from: usize, end: usize, c: u8) -> Option<usize> {
    let mut k = from;
    while k < end {
        if b[k] == c {
            if b.get(k + 1) == Some(&c) && k + 1 < end {
                k += 2;
                continue;
            }
            return Some(k);
        }
        k += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(text: &str) -> Vec<Vec<(String, String)>> {
        let lines: Vec<&str> = text.split('\n').collect();
        runs(text)
            .into_iter()
            .zip(lines)
            .map(|(rs, l)| rs.into_iter().map(|(r, c)| (l[r].to_owned(), c)).collect())
            .collect()
    }

    #[test]
    fn inline_spans_keep_their_markers() {
        let s = spans("a **bold** and *em* `x` ~~gone~~");
        assert_eq!(
            s[0],
            vec![
                ("**".into(), "md-strong md-mark".into()),
                ("bold".into(), "md-strong".into()),
                ("**".into(), "md-strong md-mark".into()),
                ("*".into(), "md-em md-mark".into()),
                ("em".into(), "md-em".into()),
                ("*".into(), "md-em md-mark".into()),
                ("`".into(), "md-code md-mark".into()),
                ("x".into(), "md-code".into()),
                ("`".into(), "md-code md-mark".into()),
                ("~~".into(), "md-strike md-mark".into()),
                ("gone".into(), "md-strike".into()),
                ("~~".into(), "md-strike md-mark".into()),
            ]
        );
    }

    #[test]
    fn blocks_headings_quotes_lists_and_fences() {
        let s = spans("# Title\n> quote\n- item\n```rs\nlet *a* = 1;\n```\nafter");
        assert_eq!(s[0][0], ("# ".into(), "md-h md-mark".into()));
        assert_eq!(s[0][1], ("Title".into(), "md-h".into()));
        assert_eq!(s[1][0], ("> ".into(), "md-quote md-mark".into()));
        assert_eq!(s[2][0], ("- ".into(), "md-li md-mark".into()));
        assert_eq!(s[3], vec![("```rs".into(), "md-fence md-mark".into())]);
        assert_eq!(
            s[4],
            vec![("let *a* = 1;".into(), "md-pre".into())],
            "nothing inline inside a fence"
        );
        assert_eq!(s[5], vec![("```".into(), "md-fence md-mark".into())]);
        assert!(s[6].is_empty());
    }

    #[test]
    fn links_and_what_stays_plain() {
        let s = spans("see [docs](https://x.y) now");
        assert_eq!(
            s[0],
            vec![
                ("[".into(), "md-link md-mark".into()),
                ("docs".into(), "md-link".into()),
                ("](".into(), "md-link md-mark".into()),
                ("https://x.y".into(), "md-link md-url".into()),
                (")".into(), "md-link md-mark".into()),
            ]
        );
        for plain in [
            "2 * 3 * 4",
            "snake_case_name",
            "a ** b",
            "`open",
            "#tag",
            "-x",
        ] {
            assert!(spans(plain)[0].is_empty(), "{plain}: {:?}", spans(plain));
        }
        // Multibyte text steps by characters.
        assert_eq!(spans("é *ü*")[0][1], ("ü".into(), "md-em".into()));
    }
}
