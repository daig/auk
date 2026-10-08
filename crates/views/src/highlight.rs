//! Syntax highlighting into the 11 coarse `tk-*` classes (§8.5, §18.4): syntect with the
//! two-face grammar set, incremental by line (a changed line re-highlights from there on; an
//! append keeps the highlighting of unchanged lines), and in the background ([`Background`]): a
//! render highlights at most a screenful of changed lines and a worker thread does the rest,
//! which the view draws in as it arrives.

use std::sync::OnceLock;
use syntect::parsing::{ParseState, Scope, ScopeStack, ScopeStackOp, SyntaxReference, SyntaxSet};

/// The 11 token classes (book:5123-5134).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tk {
    Comment,
    Keyword,
    Function,
    Variable,
    String,
    Number,
    Type,
    Operator,
    Punct,
    Inserted,
    Deleted,
}

impl Tk {
    pub fn class(self) -> &'static str {
        match self {
            Tk::Comment => "tk-comment",
            Tk::Keyword => "tk-keyword",
            Tk::Function => "tk-function",
            Tk::Variable => "tk-variable",
            Tk::String => "tk-string",
            Tk::Number => "tk-number",
            Tk::Type => "tk-type",
            Tk::Operator => "tk-operator",
            Tk::Punct => "tk-punct",
            Tk::Inserted => "tk-inserted",
            Tk::Deleted => "tk-deleted",
        }
    }
}

static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
static MAP: OnceLock<Vec<(Scope, Option<Tk>)>> = OnceLock::new();

/// The grammar set (loaded once; `warm` loads it ahead of first use).
pub fn syntaxes() -> &'static SyntaxSet {
    SYNTAXES.get_or_init(two_face::syntax::extra_newlines)
}

/// Load the grammars on a background thread, and compile the common languages' patterns (syntect
/// compiles them lazily on first use), so the first code block doesn't pay for it.
pub fn warm() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    // No threads in the browser: grammars load on first use there.
    if cfg!(target_arch = "wasm32") {
        return;
    }
    STARTED.call_once(|| {
        std::thread::spawn(|| {
            syntaxes();
            map();
            const SAMPLES: [(&str, &str); 14] = [
                (
                    "rust",
                    "fn main() { let x: u32 = 1; /* c */ println!(\"{x}\"); }",
                ),
                ("python", "def f(x):\n    return [i for i in x if i] # c\n"),
                ("ts", "const f = (x: number): string => `${x}`; // c"),
                ("js", "function f(x) { return x ? 1 : 'a'; }"),
                ("bash", "for f in *.rs; do echo \"$f\" | grep -v x; done"),
                ("json", "{\"a\": [1, true, null]}"),
                ("md", "# h\n*e* `c` [l](u)"),
                ("go", "func f(x int) string { return \"a\" }"),
                ("c", "int main(void) { return 0; }"),
                ("toml", "[a]\nb = 1"),
                ("yaml", "a: [1, 2]\nb: x"),
                ("diff", "@@ -1 +1 @@\n-a\n+b"),
                ("html", "<div class=\"a\">x</div>"),
                ("css", ".a { color: red; }"),
            ];
            for (lang, text) in SAMPLES {
                if let Some(s) = find(Some(lang), None) {
                    Highlighter::new(s).update(text);
                }
            }
        });
    });
}

/// Scope prefixes to classes, most specific first. `None` means plain.
fn map() -> &'static [(Scope, Option<Tk>)] {
    MAP.get_or_init(|| {
        let s = |x: &str| Scope::new(x).expect("scope");
        vec![
            (s("comment"), Some(Tk::Comment)),
            (s("punctuation.definition.comment"), Some(Tk::Comment)),
            (s("markup.inserted"), Some(Tk::Inserted)),
            (s("markup.deleted"), Some(Tk::Deleted)),
            (s("string"), Some(Tk::String)),
            (s("constant.character"), Some(Tk::String)),
            (s("punctuation.definition.string"), Some(Tk::String)),
            (s("constant.numeric"), Some(Tk::Number)),
            (s("constant.language"), Some(Tk::Number)),
            (s("constant.other"), Some(Tk::Number)),
            (s("keyword.operator"), Some(Tk::Operator)),
            (s("keyword"), Some(Tk::Keyword)),
            (s("storage.type"), Some(Tk::Keyword)),
            (s("storage.modifier"), Some(Tk::Keyword)),
            (s("storage"), Some(Tk::Keyword)),
            (s("variable.language"), Some(Tk::Keyword)),
            (s("entity.name.function"), Some(Tk::Function)),
            (s("support.function"), Some(Tk::Function)),
            (s("variable.function"), Some(Tk::Function)),
            (s("meta.function-call"), Some(Tk::Function)),
            (s("entity.name.type"), Some(Tk::Type)),
            (s("entity.name.class"), Some(Tk::Type)),
            (s("entity.name.struct"), Some(Tk::Type)),
            (s("entity.name.enum"), Some(Tk::Type)),
            (s("entity.name.trait"), Some(Tk::Type)),
            (s("entity.other.inherited-class"), Some(Tk::Type)),
            (s("support.type"), Some(Tk::Type)),
            (s("support.class"), Some(Tk::Type)),
            (s("entity.name.tag"), Some(Tk::Keyword)),
            (s("entity.other.attribute-name"), Some(Tk::Function)),
            (s("variable.parameter"), Some(Tk::Variable)),
            (s("variable"), Some(Tk::Variable)),
            (s("punctuation"), Some(Tk::Punct)),
            (s("markup.heading"), Some(Tk::Keyword)),
            (s("markup.bold"), Some(Tk::Type)),
            (s("markup.raw"), Some(Tk::String)),
        ]
    })
}

fn classify(stack: &[Scope]) -> Option<Tk> {
    let m = map();
    for scope in stack.iter().rev() {
        for (prefix, tk) in m {
            if prefix.is_prefix_of(*scope) {
                return *tk;
            }
        }
    }
    None
}

/// The grammar for a `lang` (a name or a file extension) or a `path`.
pub fn find(lang: Option<&str>, path: Option<&str>) -> Option<&'static SyntaxReference> {
    let ss = syntaxes();
    if let Some(l) = lang.filter(|l| !l.is_empty()) {
        let lower = l.to_ascii_lowercase();
        if matches!(lower.as_str(), "text" | "plain" | "plaintext" | "txt") {
            return None;
        }
        let alias = match lower.as_str() {
            "ts" | "typescript" => "ts",
            "tsx" => "tsx",
            "js" | "javascript" | "node" => "js",
            "sh" | "shell" | "bash" | "zsh" | "console" => "bash",
            "py" | "python3" => "py",
            "rs" => "rs",
            "yml" => "yaml",
            "md" | "markdown" => "md",
            "c++" | "cpp" => "cpp",
            "golang" => "go",
            other => other,
        };
        if let Some(s) = ss
            .find_syntax_by_token(alias)
            .or_else(|| ss.find_syntax_by_extension(alias))
            .or_else(|| ss.find_syntax_by_name(l))
        {
            return Some(s);
        }
    }
    let path = path?;
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or(name);
    ss.find_syntax_by_extension(ext)
        .or_else(|| ss.find_syntax_by_extension(name))
}

/// One highlighted line: runs of `(class, byte range)` covering the line (without its newline).
pub type Runs = Vec<(Option<Tk>, std::ops::Range<usize>)>;

/// An incremental highlighter for one text.
pub struct Highlighter {
    syntax: &'static SyntaxReference,
    lines: Vec<String>,
    runs: Vec<Runs>,
    /// Parser state at the start of each line.
    states: Vec<(ParseState, ScopeStack)>,
    /// The text's lines after `lines`, not highlighted yet.
    pending: std::collections::VecDeque<String>,
}

impl std::fmt::Debug for Highlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Highlighter")
            .field("syntax", &self.syntax.name)
            .field("lines", &self.lines.len())
            .finish()
    }
}

impl Highlighter {
    pub fn new(syntax: &'static SyntaxReference) -> Highlighter {
        Highlighter {
            syntax,
            lines: Vec::new(),
            runs: Vec::new(),
            states: Vec::new(),
            pending: Default::default(),
        }
    }

    pub fn name(&self) -> &str {
        &self.syntax.name
    }

    /// Bring the highlighting up to `text`. Returns the first line whose highlighting changed;
    /// lines before it are unchanged.
    pub fn update(&mut self, text: &str) -> usize {
        let first = self.begin(text);
        self.step(usize::MAX);
        first
    }

    /// Takes `text` as the text to highlight without highlighting anything yet: lines up to the
    /// first changed one keep their highlighting, the rest wait for [`step`](Self::step).
    /// Returns the first line whose highlighting will change.
    pub fn begin(&mut self, text: &str) -> usize {
        let new: Vec<&str> = text.split('\n').collect();
        let first = self
            .lines
            .iter()
            .chain(self.pending.iter())
            .zip(&new)
            .take_while(|(a, b)| a.as_str() == **b)
            .count();
        // The last old line may have been incomplete; re-highlight from it.
        let total = self.lines.len() + self.pending.len();
        let first = first.min(total.saturating_sub(1)).min(new.len());
        let keep = first.min(self.lines.len());
        self.lines.truncate(keep);
        self.runs.truncate(keep);
        self.states.truncate(keep);
        self.pending = new[keep..].iter().map(|l| (*l).to_owned()).collect();
        first
    }

    /// Highlights up to `max` waiting lines. Returns how many it did.
    pub fn step(&mut self, max: usize) -> usize {
        if self.pending.is_empty() || max == 0 {
            return 0;
        }
        let n = self.lines.len();
        let (mut ps, mut stack) = match self.states.last() {
            Some(_) if n > 0 => self.after(n - 1),
            _ => (ParseState::new(self.syntax), ScopeStack::new()),
        };
        let mut done = 0;
        while done < max {
            let Some(line) = self.pending.pop_front() else {
                break;
            };
            self.states.push((ps.clone(), stack.clone()));
            let with_nl = format!("{line}\n");
            let ops = ps.parse_line(&with_nl, syntaxes()).unwrap_or_default();
            self.runs.push(runs_for(&line, &ops, &mut stack));
            self.lines.push(line);
            done += 1;
        }
        done
    }

    /// Whether every line of the text is highlighted.
    pub fn done(&self) -> bool {
        self.pending.is_empty()
    }

    /// The highlighted lines: the text's first lines, up to where the work has reached.
    pub fn highlighted(&self) -> usize {
        self.lines.len()
    }

    /// The parser state after line `i` (re-parsing it from its saved start state).
    fn after(&self, i: usize) -> (ParseState, ScopeStack) {
        let (mut ps, mut stack) = self.states[i].clone();
        let with_nl = format!("{}\n", self.lines[i]);
        let ops = ps.parse_line(&with_nl, syntaxes()).unwrap_or_default();
        for (_, op) in &ops {
            let _ = stack.apply(op);
        }
        (ps, stack)
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, i: usize) -> (&str, &Runs) {
        (&self.lines[i], &self.runs[i])
    }
}

/// Lines a render highlights itself before handing the rest to the background: about a
/// screenful, so what shows first is highlighted.
pub const RENDER_LINES: usize = 120;
/// Lines the worker highlights per turn of its lock.
const WORKER_CHUNK: usize = 256;

/// A text highlighted in the background (§8.5 "background, incremental"). [`Background::update`]
/// runs on the render path and does at most [`RENDER_LINES`] lines; a worker thread highlights
/// the rest a chunk at a time, and the view draws them in on its clock ([`Background::lines`]).
/// Without threads (the browser) [`Background::pump`] does the chunks from the view's clock.
#[derive(Clone, Debug)]
pub struct Background(std::sync::Arc<std::sync::Mutex<Highlighter>>);

impl Background {
    pub fn new(syntax: &'static SyntaxReference) -> Background {
        Background(std::sync::Arc::new(std::sync::Mutex::new(
            Highlighter::new(syntax),
        )))
    }

    /// Locks the highlighter (to read lines).
    pub fn lock(&self) -> std::sync::MutexGuard<'_, Highlighter> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Takes a new text: highlights the first changed lines now and queues the rest. Returns the
    /// first changed line.
    pub fn update(&self, text: &str) -> usize {
        let (first, done) = {
            let mut h = self.lock();
            let first = h.begin(text);
            h.step(RENDER_LINES);
            (first, h.done())
        };
        if !done {
            self.queue();
        }
        first
    }

    /// Without a worker, highlights a chunk on the caller's thread. Returns whether work is left.
    pub fn pump(&self) -> bool {
        let mut h = self.lock();
        if cfg!(target_arch = "wasm32") {
            h.step(WORKER_CHUNK);
        }
        !h.done()
    }

    fn queue(&self) {
        if cfg!(target_arch = "wasm32") {
            return;
        }
        static WORKER: OnceLock<
            std::sync::mpsc::Sender<std::sync::Weak<std::sync::Mutex<Highlighter>>>,
        > = OnceLock::new();
        let tx = WORKER.get_or_init(|| {
            let (tx, rx) =
                std::sync::mpsc::channel::<std::sync::Weak<std::sync::Mutex<Highlighter>>>();
            std::thread::Builder::new()
                .name("auk-highlight".into())
                .spawn(move || {
                    for job in rx {
                        // A text dropped meanwhile is skipped; each turn holds the lock briefly.
                        while let Some(h) = job.upgrade() {
                            let mut h = h.lock().unwrap_or_else(|e| e.into_inner());
                            if h.step(WORKER_CHUNK) == 0 {
                                break;
                            }
                        }
                    }
                })
                .expect("the highlight worker");
            tx
        });
        let _ = tx.send(std::sync::Arc::downgrade(&self.0));
    }
}

fn runs_for(line: &str, ops: &[(usize, ScopeStackOp)], stack: &mut ScopeStack) -> Runs {
    let mut runs: Runs = Vec::new();
    let mut at = 0;
    let push = |runs: &mut Runs, tk: Option<Tk>, r: std::ops::Range<usize>| {
        if r.is_empty() {
            return;
        }
        match runs.last_mut() {
            Some((t, last)) if *t == tk && last.end == r.start => last.end = r.end,
            _ => runs.push((tk, r)),
        }
    };
    for (pos, op) in ops {
        let pos = (*pos).min(line.len());
        if pos > at {
            push(&mut runs, classify(stack.as_slice()), at..pos);
            at = pos;
        }
        let _ = stack.apply(op);
    }
    if at < line.len() {
        push(&mut runs, classify(stack.as_slice()), at..line.len());
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_tokens() {
        let syn = find(Some("rust"), None).expect("rust grammar");
        let mut h = Highlighter::new(syn);
        h.update("fn main() {\n    let x = 42; // hi\n}");
        let (l, runs) = h.line(1);
        let class_of = |needle: &str| {
            let at = l.find(needle).unwrap();
            runs.iter()
                .find(|(_, r)| r.contains(&at))
                .and_then(|(t, _)| *t)
        };
        assert_eq!(class_of("let"), Some(Tk::Keyword));
        assert_eq!(class_of("42"), Some(Tk::Number));
        assert_eq!(class_of("// hi"), Some(Tk::Comment));
        // Appending keeps earlier lines.
        assert_eq!(
            h.update("fn main() {\n    let x = 42; // hi\n}\nfn b() {}"),
            2
        );
        assert!(find(Some("py"), None).is_some());
        assert!(find(None, Some("src/lib.ts")).is_some());
        assert!(find(Some("text"), None).is_none());
    }
}
