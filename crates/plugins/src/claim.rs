//! Lens claims (§9.6 "Claiming a command" and "Matching"): reading a command line as a simple
//! command, and matching it against the catalog's lens globs.
//!
//! Pure Rust with no Luau, so every replica (windows, the web tab) makes the same decision from
//! the same catalog without running plugin code. Nothing here depends on a window half.

use crate::catalog::Catalog;

/// Wrapper programs skipped before the real program, with their options (§9.6).
pub const WRAPPERS: &[&str] = &[
    "sudo",
    "doas",
    "exec",
    "time",
    "nice",
    "nohup",
    "env",
    "command",
    "builtin",
    "caffeinate",
    "npx",
    "bunx",
    "pnpx",
    "uvx",
    "pipx",
];

/// The options of `wrapper` that take the next word as their value.
fn option_takes_value(wrapper: &str, option: &str) -> bool {
    let list: &[&str] = match wrapper {
        "sudo" => &[
            "-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-U", "-T", "-R", "--user",
            "--group", "--chdir", "--close-from", "--host", "--prompt", "--role", "--type",
            "--other-user", "--command-timeout", "--chroot",
        ],
        "doas" => &["-u", "-C"],
        "exec" => &["-a"],
        "time" => &["-f", "-o", "--format", "--output"],
        "nice" => &["-n", "--adjustment"],
        "env" => &[
            "-u", "-C", "-S", "--unset", "--chdir", "--split-string", "-P",
        ],
        "caffeinate" => &["-t", "-w"],
        "npx" | "pnpx" | "bunx" => &["-p", "--package", "-c", "--call"],
        "uvx" => &[
            "--from", "--with", "-p", "--python", "--with-requirements", "--index", "--index-url",
        ],
        "pipx" => &["--spec", "--python", "--index-url", "--pip-args"],
        _ => &[],
    };
    list.contains(&option)
}

/// A command line read as a simple command: what lens globs see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// The program, after assignments and wrappers, without its directories.
    pub program: String,
    /// Its arguments, unquoted, with stderr and stdin redirections removed.
    pub args: Vec<String>,
}

impl Command {
    /// The text globs match: the program and its arguments joined by single spaces.
    pub fn text(&self) -> String {
        let mut text = self.program.clone();
        for arg in &self.args {
            text.push(' ');
            text.push_str(arg);
        }
        text
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// A word, unquoted. `subst` marks command substitution (`$(` or a backquote) outside single
    /// quotes; `assign` marks a `NAME=` prefix written unquoted.
    Word {
        text: String,
        subst: bool,
        assign: bool,
    },
    /// A redirection: whether it is dropped (stderr, stdin) and whether it consumes the next word.
    Redirect { drop: bool, takes_word: bool },
}

/// Why a line is not a simple command.
struct NotSimple;

fn is_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Splits `line` the way a POSIX shell would for a simple command, refusing anything else.
fn lex(line: &str) -> Result<Vec<Token>, NotSimple> {
    let chars: Vec<char> = line.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut word = String::new();
    let mut in_word = false;
    let mut quoted = false;
    let mut subst = false;
    let mut assign_at: Option<usize> = None;

    let flush = |tokens: &mut Vec<Token>,
                 word: &mut String,
                 in_word: &mut bool,
                 quoted: &mut bool,
                 subst: &mut bool,
                 assign_at: &mut Option<usize>| {
        if *in_word {
            let assign = assign_at.is_some_and(|at| is_name(&word[..at]));
            tokens.push(Token::Word {
                text: std::mem::take(word),
                subst: *subst,
                assign,
            });
        }
        *in_word = false;
        *quoted = false;
        *subst = false;
        *assign_at = None;
    };

    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' => {
                flush(
                    &mut tokens,
                    &mut word,
                    &mut in_word,
                    &mut quoted,
                    &mut subst,
                    &mut assign_at,
                );
                i += 1;
            }
            '\n' | ';' | '|' | '(' | ')' => return Err(NotSimple),
            '&' => {
                if chars.get(i + 1) == Some(&'>') {
                    // `&>` and `&>>` send stdout to a file.
                    return Err(NotSimple);
                }
                // `&&` and a trailing `&` make a list.
                return Err(NotSimple);
            }
            '\'' => {
                in_word = true;
                quoted = true;
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    word.push(chars[i]);
                    i += 1;
                }
                if i >= chars.len() {
                    return Err(NotSimple);
                }
                i += 1;
            }
            '"' => {
                in_word = true;
                quoted = true;
                i += 1;
                loop {
                    let Some(&d) = chars.get(i) else {
                        return Err(NotSimple);
                    };
                    match d {
                        '"' => {
                            i += 1;
                            break;
                        }
                        '\\' if matches!(chars.get(i + 1), Some('"' | '\\' | '$' | '`' | '\n')) => {
                            if chars[i + 1] != '\n' {
                                word.push(chars[i + 1]);
                            }
                            i += 2;
                        }
                        '$' if chars.get(i + 1) == Some(&'(') => {
                            subst = true;
                            word.push(d);
                            i += 1;
                        }
                        '`' => {
                            subst = true;
                            word.push(d);
                            i += 1;
                        }
                        _ => {
                            word.push(d);
                            i += 1;
                        }
                    }
                }
            }
            '\\' => {
                in_word = true;
                quoted = true;
                if let Some(&d) = chars.get(i + 1) {
                    if d != '\n' {
                        word.push(d);
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            '$' if chars.get(i + 1) == Some(&'(') => {
                // `$(…)` stays part of the word, nested parentheses included.
                in_word = true;
                subst = true;
                let mut depth = 0;
                while let Some(&d) = chars.get(i) {
                    word.push(d);
                    i += 1;
                    match d {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                if depth != 0 {
                    return Err(NotSimple);
                }
            }
            '`' => {
                in_word = true;
                subst = true;
                word.push('`');
                i += 1;
            }
            '<' | '>' => {
                // A word of digits glued to the operator is its file descriptor.
                let fd = if in_word && !quoted && !word.is_empty() && word.chars().all(|d| d.is_ascii_digit()) {
                    let fd = word.parse::<u32>().ok();
                    word.clear();
                    in_word = false;
                    fd
                } else {
                    flush(
                        &mut tokens,
                        &mut word,
                        &mut in_word,
                        &mut quoted,
                        &mut subst,
                        &mut assign_at,
                    );
                    None
                };
                let mut op = String::from(c);
                i += 1;
                while let Some(&d) = chars.get(i) {
                    if matches!(d, '<' | '>' | '&' | '|' | '-') && op.len() < 3 {
                        // `-` only continues `<<-`.
                        if d == '-' && op != "<<" {
                            break;
                        }
                        op.push(d);
                        i += 1;
                    } else {
                        break;
                    }
                }
                let redirect = classify(&op, fd, &chars, &mut i)?;
                tokens.push(redirect);
            }
            '=' => {
                if in_word && !quoted && assign_at.is_none() {
                    assign_at = Some(word.len());
                }
                word.push(c);
                in_word = true;
                i += 1;
            }
            _ => {
                in_word = true;
                word.push(c);
                i += 1;
            }
        }
    }
    flush(
        &mut tokens,
        &mut word,
        &mut in_word,
        &mut quoted,
        &mut subst,
        &mut assign_at,
    );
    Ok(tokens)
}

/// Classifies a redirection operator. For `>&N` / `<&N`, the target digits glued to the operator
/// are consumed here.
fn classify(op: &str, fd: Option<u32>, chars: &[char], i: &mut usize) -> Result<Token, NotSimple> {
    let input = op.starts_with('<');
    if op.starts_with("<<") && !op.starts_with("<<<") {
        return Err(NotSimple); // heredoc
    }
    let dup = op.ends_with('&');
    let mut takes_word = true;
    if dup {
        // `2>&1`, `<&0`, `>&-`: the target is glued.
        let start = *i;
        while *i < chars.len() && (chars[*i].is_ascii_digit() || chars[*i] == '-') {
            *i += 1;
        }
        if *i > start {
            takes_word = false;
        }
    }
    let drop = if input {
        // stdin (or another descriptor read from): removed.
        true
    } else {
        // Output: stdout (no fd, or 1) is never lensed; other descriptors are removed.
        match fd {
            None | Some(1) => return Err(NotSimple),
            Some(_) => true,
        }
    };
    Ok(Token::Redirect { drop, takes_word })
}

/// Reads `line` as a simple command: leading assignments and wrappers (with their options) are
/// skipped, the program loses its directories, and stderr and stdin redirections are removed.
/// `None` for pipelines, lists, stdout redirections, subshells, heredocs, command substitution in
/// the program word, and empty lines.
pub fn normalize(line: &str) -> Option<Command> {
    let tokens = lex(line).ok()?;
    // Drop redirections and their targets.
    let mut words: Vec<(String, bool, bool)> = Vec::new();
    let mut skip_next = false;
    for token in tokens {
        match token {
            Token::Redirect { drop, takes_word } => {
                debug_assert!(drop);
                skip_next = takes_word;
            }
            Token::Word {
                text,
                subst,
                assign,
            } => {
                if skip_next {
                    skip_next = false;
                    continue;
                }
                words.push((text, subst, assign));
            }
        }
    }
    let mut at = 0;
    // Leading `VAR=value` assignments.
    while at < words.len() && words[at].2 {
        at += 1;
    }
    // Wrappers and their options, possibly chained (`sudo nice -n 5 du`).
    while at < words.len() && !words[at].1 && WRAPPERS.contains(&words[at].0.as_str()) {
        let wrapper = words[at].0.clone();
        at += 1;
        while at < words.len() {
            let word = &words[at].0;
            if word == "--" {
                at += 1;
                break;
            }
            if word.starts_with('-') && word.len() > 1 {
                let takes = option_takes_value(&wrapper, word);
                at += if takes { 2 } else { 1 };
                continue;
            }
            if words[at].2 {
                at += 1; // `env FOO=1 cmd`, `sudo FOO=1 cmd`
                continue;
            }
            break;
        }
    }
    let (program, subst, _) = words.get(at)?;
    if *subst || program.is_empty() {
        return None;
    }
    let program = program.rsplit('/').next().unwrap_or(program).to_owned();
    if program.is_empty() {
        return None;
    }
    let args = words[at + 1..].iter().map(|w| w.0.clone()).collect();
    Some(Command { program, args })
}

/// Whether `pattern` (`*` any run, `?` one character, the rest literal, case-sensitive) matches
/// the whole of `text`.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || (p[pi] != '*' && p[pi] == t[ti])) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// Whether a file name matches a block's `files` glob: like [`glob_match`], but ASCII
/// case-insensitive.
pub fn file_glob_match(pattern: &str, name: &str) -> bool {
    glob_match(&pattern.to_ascii_lowercase(), &name.to_ascii_lowercase())
}

/// What the replica knows when a command starts (§9.6 "Preconditions").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preconditions {
    /// Shell integration reported the command line.
    pub shell_integration: bool,
    /// The `command_lenses` setting.
    pub lenses_on: bool,
    /// The pane is on its main screen.
    pub main_screen: bool,
    /// A program surface is open in the pane.
    pub surface_open: bool,
}

impl Preconditions {
    /// Whether a lens may claim at all.
    pub fn allow(self) -> bool {
        self.shell_integration && self.lenses_on && self.main_screen && !self.surface_open
    }
}

/// The command a lens claimed, as `open` receives it (`LensRun` in `tern.d.luau`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LensRun {
    /// The command line the claim read (alias-expanded when the shell reported that).
    pub line: String,
    /// The line as typed.
    pub typed: String,
    /// The shell's directory, when known.
    pub cwd: Option<String>,
    /// The program, after wrappers and assignments, without its directories.
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
}

/// A plugin lens's claim on a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// The plugin id.
    pub plugin: String,
    /// The lens id within the plugin.
    pub lens: String,
    /// The command as the lens sees it.
    pub command: Command,
}

impl Claim {
    /// The lens block's role, `lens.plugin.<plugin>.<lens>`.
    pub fn role(&self) -> String {
        format!("lens.plugin.{}.{}", self.plugin, self.lens)
    }

    /// The `run` table `open` receives for `line` (the line the claim read) and `typed`.
    pub fn run(&self, line: &str, typed: &str, cwd: Option<&str>) -> LensRun {
        LensRun {
            line: line.to_owned(),
            typed: typed.to_owned(),
            cwd: cwd.map(str::to_owned),
            program: self.command.program.clone(),
            args: self.command.args.clone(),
        }
    }
}

impl Catalog {
    /// The plugin lens that claims `line` (alias-expanded when the shell reported that): Ready
    /// plugins in id order, the first plugin with a matching glob wins, and within it its first
    /// matching lens in manifest order. A claim suppresses the built-in lenses.
    pub fn claim(&self, line: &str, pre: Preconditions) -> Option<Claim> {
        if !pre.allow() {
            return None;
        }
        self.claim_command(line)
    }

    /// [`Catalog::claim`] without the preconditions (rehydration re-checks only the globs).
    pub fn claim_command(&self, line: &str) -> Option<Claim> {
        let command = normalize(line)?;
        let text = command.text();
        let (plugin, rule) = self
            .lens_claims()
            .find(|(_, rule)| rule.patterns.iter().any(|p| glob_match(p, &text)))?;
        Some(Claim {
            plugin: plugin.id.clone(),
            lens: rule.id.clone(),
            command,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &str) -> Option<String> {
        normalize(line).map(|c| c.text())
    }

    #[test]
    fn normalization_table() {
        // research/plugins.md §7.2 (book:1057-1076).
        assert_eq!(text("sudo -u root du -sh /var").as_deref(), Some("du -sh /var"));
        assert_eq!(text("LC_ALL=C du -sh \"my dir\"").as_deref(), Some("du -sh my dir"));
        assert_eq!(text("du -sh src 2>/dev/null").as_deref(), Some("du -sh src"));
        assert_eq!(text("du -sh src | sort -h"), None);
        assert_eq!(text("du -sh src > out.txt"), None);
    }

    #[test]
    fn simple_command_rules() {
        assert_eq!(text("/usr/bin/du -sh .").as_deref(), Some("du -sh ."));
        assert_eq!(text("make 2>&1").as_deref(), Some("make"));
        assert_eq!(text("make 2> err.log").as_deref(), Some("make"));
        assert_eq!(text("wc -l < input.txt").as_deref(), Some("wc -l"));
        assert_eq!(text("nice -n 5 time make all").as_deref(), Some("make all"));
        assert_eq!(text("env -u X FOO=1 terraform plan").as_deref(), Some("terraform plan"));
        assert_eq!(text("npx -p pkg tsc --noEmit").as_deref(), Some("tsc --noEmit"));
        assert_eq!(text("echo 'a b'  c\\ d").as_deref(), Some("echo a b c d"));
        assert_eq!(text("ls $(pwd)").as_deref(), Some("ls $(pwd)"));
        for never in [
            "a && b",
            "a || b",
            "a; b",
            "a &",
            "(cd x && ls)",
            "cat <<EOF",
            "make >&2",
            "make &> log",
            "make 1> log",
            "$(which du) -sh",
            "`which du`",
            "",
            "FOO=1",
        ] {
            assert_eq!(text(never), None, "{never:?}");
        }
    }

    #[test]
    fn globs() {
        assert!(glob_match("du *", "du -sh"));
        assert!(!glob_match("du *", "du"));
        assert!(glob_match("du", "du"));
        assert!(!glob_match("kubectl get *", "kubectl get"));
        assert!(!glob_match("kubectl get *", "Kubectl get pods"));
        assert!(glob_match("terraform -chdir=* plan *", "terraform -chdir=infra plan -out x"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(glob_match("*", ""));
        assert!(file_glob_match("*.json", "DATA.JSON"));
    }
}
