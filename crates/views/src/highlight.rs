//! Syntax highlighting into the 11 coarse `tk-*` classes (§8.5, §18.4): syntect with the
//! two-face grammar set, incremental by line (a changed line re-highlights from there on; an
//! append keeps the highlighting of unchanged lines).

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
        }
    }

    pub fn name(&self) -> &str {
        &self.syntax.name
    }

    /// Bring the highlighting up to `text`. Returns the first line whose highlighting changed;
    /// lines before it are unchanged.
    pub fn update(&mut self, text: &str) -> usize {
        let new: Vec<&str> = text.split('\n').collect();
        let first = self
            .lines
            .iter()
            .zip(&new)
            .take_while(|(a, b)| a.as_str() == **b)
            .count();
        // The last old line may have been incomplete; re-highlight from it.
        let first = first.min(self.lines.len().saturating_sub(1)).min(new.len());
        self.lines.truncate(first);
        self.runs.truncate(first);
        self.states.truncate(first);
        let (mut ps, mut stack) = match self.states.last() {
            Some(_) if first > 0 => self.after(first - 1),
            _ => (ParseState::new(self.syntax), ScopeStack::new()),
        };
        for line in &new[first..] {
            self.states.push((ps.clone(), stack.clone()));
            let with_nl = format!("{line}\n");
            let ops = ps.parse_line(&with_nl, syntaxes()).unwrap_or_default();
            self.runs.push(runs_for(line, &ops, &mut stack));
            self.lines.push((*line).to_owned());
        }
        first
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
