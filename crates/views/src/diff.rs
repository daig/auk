//! The `diff` sub-engine (§8.5): hunks from unified text or `hunks`, word-level emphasis on paired
//! lines (imara-diff over word tokens), fold rows between hunks, unified or split rows.

use imara_diff::{Algorithm, Diff, Interner, Token};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Ctx,
    Del,
    Add,
}

/// One diff line with its line numbers and emphasized byte ranges.
#[derive(Clone, Debug, PartialEq)]
pub struct DLine {
    pub op: Op,
    pub old: Option<u64>,
    pub new: Option<u64>,
    pub text: String,
    pub em: Vec<std::ops::Range<usize>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hunk {
    pub old_start: u64,
    pub new_start: u64,
    /// The function name from the `@@` header, if any.
    pub func: Option<String>,
    /// Whether the start lines were given (an `@@` header, `oldStart`): fold rows count the
    /// gap between known hunks of one file and say only `⋯` otherwise.
    pub known: bool,
    /// The first hunk of a later file of a multi-file diff (after its `diff`/`---`/`+++`
    /// headers): no gap is counted from the hunk before.
    pub new_file: bool,
    pub lines: Vec<DLine>,
}

/// Whether `lines[i]` is a file header of a unified diff: `diff …`, `index …` after it, or a
/// `--- ` line followed by `+++ ` (and that `+++ ` line). A removed line reading `-- x` is
/// content: no `+++ ` follows it.
fn file_header(lines: &[&str], i: usize) -> bool {
    let l = lines[i];
    if l.starts_with("diff ") {
        return true;
    }
    if l.starts_with("index ") {
        return i > 0 && lines[i - 1].starts_with("diff ");
    }
    if l.starts_with("--- ") || l == "---" {
        return lines.get(i + 1).is_some_and(|n| n.starts_with("+++"));
    }
    if l.starts_with("+++ ") || l == "+++" {
        return i > 0 && (lines[i - 1].starts_with("--- ") || lines[i - 1] == "---");
    }
    false
}

/// Parse a unified diff (file headers tolerated, before each file of a multi-file diff too).
pub fn parse_unified(text: &str) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut raw: Vec<(Op, String)> = Vec::new();
    let mut head: Option<(u64, u64, Option<String>, bool)> = None;
    // A file header came since the last hunk: the next one starts a later file.
    let mut new_file = false;
    let flush = |hunks: &mut Vec<Hunk>,
                 head: &Option<(u64, u64, Option<String>, bool)>,
                 raw: &mut Vec<(Op, String)>,
                 new_file: &mut bool| {
        if let Some((o, n, f, known)) = head {
            let mut h = number(*o, *n, f.clone(), std::mem::take(raw));
            h.known = *known;
            h.new_file = *new_file && !hunks.is_empty();
            *new_file = false;
            hunks.push(h);
        }
        raw.clear();
    };
    let lines: Vec<&str> = text.lines().collect();
    for (i, &line) in lines.iter().enumerate() {
        if let Some(rest) = line.strip_prefix("@@") {
            flush(&mut hunks, &head, &mut raw, &mut new_file);
            let (nums, func) = match rest.find("@@") {
                Some(i) => (&rest[..i], rest[i + 2..].trim()),
                None => (rest, ""),
            };
            let mut o = 1;
            let mut n = 1;
            for part in nums.split_whitespace() {
                let v = part[1..]
                    .split(',')
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(1);
                if part.starts_with('-') {
                    o = v;
                } else if part.starts_with('+') {
                    n = v;
                }
            }
            head = Some((o, n, (!func.is_empty()).then(|| func.to_owned()), true));
            continue;
        }
        if file_header(&lines, i) {
            // The end of a file's hunks: what follows is another file's.
            if head.is_some() {
                flush(&mut hunks, &head, &mut raw, &mut new_file);
                head = None;
                new_file = true;
            }
            continue;
        }
        if head.is_none() {
            // Text with no hunk header at all reads as one hunk.
            head = Some((1, 1, None, false));
        }
        if line.starts_with("\\") {
            continue;
        }
        let (op, t) = match line.chars().next() {
            Some('-') => (Op::Del, &line[1..]),
            Some('+') => (Op::Add, &line[1..]),
            Some(' ') => (Op::Ctx, &line[1..]),
            _ => (Op::Ctx, line),
        };
        raw.push((op, t.to_owned()));
    }
    flush(&mut hunks, &head, &mut raw, &mut new_file);
    hunks
}

/// Hunks from the `hunks` prop: `[{oldStart?, newStart?, lines}]`.
pub fn parse_hunks(v: &Value) -> Vec<Hunk> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|h| {
            let o = h.as_object()?;
            // Both default to 1 (book: diff `hunks`).
            let start = o.get("oldStart").and_then(Value::as_u64);
            let old = start.unwrap_or(1);
            let new = o.get("newStart").and_then(Value::as_u64).unwrap_or(1);
            let raw = o
                .get("lines")?
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                // `\ No newline at end of file` lines are skipped.
                .filter(|l| !l.starts_with('\\'))
                .map(|l| match l.chars().next() {
                    Some('-') => (Op::Del, l[1..].to_owned()),
                    Some('+') => (Op::Add, l[1..].to_owned()),
                    Some(' ') => (Op::Ctx, l[1..].to_owned()),
                    _ => (Op::Ctx, l.to_owned()),
                })
                .collect();
            let mut h = number(old, new, None, raw);
            h.known = start.is_some();
            Some(h)
        })
        .collect()
}

fn number(old: u64, new: u64, func: Option<String>, raw: Vec<(Op, String)>) -> Hunk {
    let (mut o, mut n) = (old, new);
    let mut lines: Vec<DLine> = raw
        .into_iter()
        .map(|(op, text)| {
            let l = DLine {
                op,
                old: (op != Op::Add).then_some(o),
                new: (op != Op::Del).then_some(n),
                text,
                em: Vec::new(),
            };
            if op != Op::Add {
                o += 1;
            }
            if op != Op::Del {
                n += 1;
            }
            l
        })
        .collect();
    emphasize(&mut lines);
    Hunk {
        old_start: old,
        new_start: new,
        func,
        known: true,
        new_file: false,
        lines,
    }
}

/// Words, whitespace runs and single punctuation characters, as byte ranges.
fn tokens(s: &str) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let class = |c: char| {
            if c.is_alphanumeric() || c == '_' {
                0
            } else if c.is_whitespace() {
                1
            } else {
                2
            }
        };
        let k = class(c);
        let mut end = i + c.len_utf8();
        if k != 2 {
            while let Some(&(j, d)) = it.peek() {
                if class(d) != k {
                    break;
                }
                end = j + d.len_utf8();
                it.next();
            }
        }
        out.push(i..end);
    }
    out
}

/// Word-level emphasis on paired lines: the i-th deleted line of a run pairs with the i-th
/// added line. Skipped when a line passes 1000 bytes or the pair is mostly rewritten.
fn emphasize(lines: &mut [DLine]) {
    let mut i = 0;
    while i < lines.len() {
        if lines[i].op != Op::Del {
            i += 1;
            continue;
        }
        let ds = i;
        while i < lines.len() && lines[i].op == Op::Del {
            i += 1;
        }
        let as_ = i;
        while i < lines.len() && lines[i].op == Op::Add {
            i += 1;
        }
        let pairs = (as_ - ds).min(i - as_);
        for k in 0..pairs {
            let (d, a) = (ds + k, as_ + k);
            if lines[d].text.len() > 1000 || lines[a].text.len() > 1000 {
                continue;
            }
            let (de, ae) = word_diff(&lines[d].text, &lines[a].text);
            let changed = |em: &[std::ops::Range<usize>], len: usize| {
                em.iter().map(|r| r.len()).sum::<usize>() as f64 / len.max(1) as f64
            };
            if changed(&de, lines[d].text.len()) > 0.6 && changed(&ae, lines[a].text.len()) > 0.6 {
                continue;
            }
            lines[d].em = de;
            lines[a].em = ae;
        }
    }
}

fn word_diff(a: &str, b: &str) -> (Vec<std::ops::Range<usize>>, Vec<std::ops::Range<usize>>) {
    let ta = tokens(a);
    let tb = tokens(b);
    let mut interner: Interner<&str> = Interner::new(ta.len() + tb.len());
    let ka: Vec<Token> = ta.iter().map(|r| interner.intern(&a[r.clone()])).collect();
    let kb: Vec<Token> = tb.iter().map(|r| interner.intern(&b[r.clone()])).collect();
    let mut diff = Diff::default();
    diff.compute_with(Algorithm::Histogram, &ka, &kb, interner.num_tokens());
    let ranges = |toks: &[std::ops::Range<usize>], changed: &dyn Fn(u32) -> bool| {
        let mut out: Vec<std::ops::Range<usize>> = Vec::new();
        for (i, r) in toks.iter().enumerate() {
            if changed(i as u32) {
                match out.last_mut() {
                    Some(l) if l.end == r.start => l.end = r.end,
                    _ => out.push(r.clone()),
                }
            }
        }
        out
    };
    (
        ranges(&ta, &|i| diff.is_removed(i)),
        ranges(&tb, &|i| diff.is_added(i)),
    )
}

/// Count of changed lines, for `+N −M` summaries.
pub fn counts(hunks: &[Hunk]) -> (usize, usize) {
    let mut add = 0;
    let mut del = 0;
    for h in hunks {
        for l in &h.lines {
            match l.op {
                Op::Add => add += 1,
                Op::Del => del += 1,
                Op::Ctx => {}
            }
        }
    }
    (add, del)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unified_and_hunks() {
        let h = parse_unified(
            "--- a/x\n+++ b/x\n@@ -3,2 +3,2 @@ fn main()\n ctx\n-let x = 1;\n+let x = 2;\n",
        );
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].func.as_deref(), Some("fn main()"));
        let l = &h[0].lines;
        assert_eq!((l[1].old, l[1].new), (Some(4), None));
        assert_eq!((l[2].old, l[2].new), (None, Some(4)));
        assert_eq!(&l[1].text[l[1].em[0].clone()], "1");
        assert_eq!(&l[2].text[l[2].em[0].clone()], "2");
        let h =
            parse_hunks(&json!([{"oldStart": 36, "newStart": 36, "lines": ["-a b c", "+a B c"]}]));
        assert_eq!(&h[0].lines[1].text[h[0].lines[1].em[0].clone()], "B");
        assert_eq!(counts(&h), (1, 1));
        // Whether the start lines were given: an `@@` header or `oldStart`.
        assert!(h[0].known);
        assert!(!parse_hunks(&json!([{"lines": ["-a", "+b"]}]))[0].known);
        assert!(!parse_unified("-a\n+b\n")[0].known);
        assert!(parse_unified("@@ -3 +3 @@\n-a\n+b\n")[0].known);
    }

    /// `hunks`: `newStart` defaults to 1 (not to `oldStart`), and `\ No newline` lines are
    /// skipped (book: diff).
    #[test]
    fn hunks_default_new_start_and_skip_no_newline_markers() {
        let h = parse_hunks(&json!([{"oldStart": 10, "lines": [" a", "-b", "\\ No newline at end of file", "+c"]}]));
        assert_eq!(h[0].lines.len(), 3, "{:?}", h[0].lines);
        assert_eq!((h[0].lines[0].old, h[0].lines[0].new), (Some(10), Some(1)));
        let h = parse_unified("@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n");
        assert_eq!(h[0].lines.len(), 2);
    }

    /// A multi-file unified diff: each file's `diff`/`index`/`---`/`+++` headers end the hunk
    /// before and aren't drawn as lines; the next file's first hunk counts no gap from it. A
    /// removed line reading `-- x` stays content.
    #[test]
    fn multi_file_unified_diffs_skip_every_files_headers() {
        let text = "diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -3,2 +3,2 @@\n ctx\n--- dashes\n+new\n\
                    diff --git a/y b/y\nindex 3..4 100644\n--- a/y\n+++ b/y\n@@ -40 +40 @@ fn y()\n-old\n+new\n";
        let h = parse_unified(text);
        assert_eq!(h.len(), 2, "{h:?}");
        let texts: Vec<&str> = h[0].lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["ctx", "-- dashes", "new"]);
        assert!(!h[0].new_file && h[1].new_file);
        assert_eq!(h[1].old_start, 40);
        assert_eq!(h[1].lines.len(), 2);
    }
}
