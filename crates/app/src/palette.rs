//! The command palette (§9.8): built-in commands, the rows plugins add (shown while their
//! `available` says so), and "Open file…" for a typed path, whose open goes through
//! `route.open` with origin `palette`.

use crate::commands::{self, Cmd, REGISTRY};

/// What a palette row runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowAction {
    /// A built-in command (its overrides run first).
    Builtin(Cmd),
    /// A plugin action, `plugin.<p>.<id>`.
    Plugin(String),
    /// Open a path (origin `palette`).
    Open(String),
    /// Open a block of this type (`<plugin>.<block>`, §9.5).
    Block(String),
    /// Show a session of the window key ("Switch Session…").
    Session(u64),
    /// Rename a session to the typed name.
    RenameSession(u64, String),
}

/// A palette row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub label: String,
    pub group: String,
    pub icon: String,
    pub action: RowAction,
}

impl Row {
    /// The action's name, as the automation endpoint lists it.
    pub fn action_name(&self) -> String {
        match &self.action {
            RowAction::Builtin(c) => commands::builtin_id(*c),
            RowAction::Plugin(a) => a.clone(),
            RowAction::Open(p) => format!("open:{p}"),
            RowAction::Block(kind) => format!("block:{kind}"),
            RowAction::Session(id) => format!("session:{id}"),
            RowAction::RenameSession(id, _) => format!("rename-session:{id}"),
        }
    }
}

/// An open palette: the query, the matching rows and the selected one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Palette {
    pub query: String,
    pub selected: usize,
    /// Every row the palette offers, before filtering.
    pub all: Vec<Row>,
    /// What the palette lists, said in its empty query field, when it isn't every command
    /// ("Switch to a session").
    pub scope: Option<String>,
    /// The palette asks for a session's new name: (session, its current name).
    pub rename: Option<(u64, String)>,
}

/// The built-in rows, in registry order (the palette doesn't offer itself).
pub fn builtin_rows() -> Vec<Row> {
    REGISTRY
        .iter()
        .filter(|e| !matches!(e.cmd, Cmd::CommandPalette | Cmd::SelectTab(_)))
        .map(|e| Row {
            label: e.title.trim_end_matches('…').to_owned(),
            group: e.menu.to_owned(),
            icon: String::new(),
            action: RowAction::Builtin(e.cmd),
        })
        .collect()
}

fn matches(label: &str, query: &str) -> bool {
    let label = label.to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|w| label.contains(w))
}

impl Palette {
    pub fn new(all: Vec<Row>) -> Palette {
        Palette {
            query: String::new(),
            selected: 0,
            all,
            scope: None,
            rename: None,
        }
    }

    /// A palette asking for `session`'s new name: Return renames it to what was typed.
    pub fn rename_session(session: u64, name: &str) -> Palette {
        Palette {
            rename: Some((session, name.to_owned())),
            ..Palette::new(Vec::new())
        }
    }

    /// What the empty query field says, when not the default.
    pub fn placeholder(&self) -> Option<String> {
        match &self.rename {
            Some((_, name)) => Some(format!("Rename session \u{201c}{name}\u{201d} to\u{2026}")),
            None => self.scope.clone(),
        }
    }

    /// The rows the query shows: an open row for a typed path, then matching commands. Asking
    /// for a name, the one row renames to what was typed.
    pub fn rows(&self) -> Vec<Row> {
        let q = self.query.trim();
        let mut out = Vec::new();
        if let Some((session, _)) = &self.rename {
            if !q.is_empty() {
                out.push(Row {
                    label: format!("Rename to \u{201c}{q}\u{201d}"),
                    group: "Session".into(),
                    icon: String::new(),
                    action: RowAction::RenameSession(*session, q.to_owned()),
                });
            }
            return out;
        }
        if self.scope.is_none() && (q.starts_with('/') || q.starts_with('~')) {
            let path = match q.strip_prefix('~') {
                Some(rest) => format!(
                    "{}{rest}",
                    std::env::var("HOME")
                        .unwrap_or_default()
                        .trim_end_matches('/')
                ),
                None => q.to_owned(),
            };
            out.push(Row {
                label: format!("Open {q}"),
                group: "Open file".into(),
                icon: "file".into(),
                action: RowAction::Open(path),
            });
            return out;
        }
        out.extend(self.all.iter().filter(|r| matches(&r.label, q)).cloned());
        out
    }

    pub fn type_text(&mut self, text: &str) {
        self.query.push_str(text);
        self.selected = 0;
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.selected = 0;
    }

    pub fn step(&mut self, down: bool) {
        let n = self.rows().len();
        if n == 0 {
            self.selected = 0;
        } else if down {
            self.selected = (self.selected + 1) % n;
        } else {
            self.selected = (self.selected + n - 1) % n;
        }
    }

    /// The selected row.
    pub fn chosen(&self) -> Option<Row> {
        self.rows().into_iter().nth(self.selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_words_and_offers_an_open_row_for_paths() {
        let mut p = Palette::new(builtin_rows());
        p.type_text("split rig");
        let rows = p.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].action, RowAction::Builtin(Cmd::SplitRight));
        p.query = "/tmp/a.json".into();
        assert_eq!(
            p.chosen().map(|r| r.action),
            Some(RowAction::Open("/tmp/a.json".into()))
        );
    }

    #[test]
    fn renaming_a_session_offers_one_row_for_the_typed_name() {
        let mut p = Palette::rename_session(4, "brisk-otter");
        assert!(p.rows().is_empty());
        assert!(p.placeholder().unwrap().contains("brisk-otter"));
        p.type_text(" ops ");
        assert_eq!(
            p.chosen().map(|r| r.action),
            Some(RowAction::RenameSession(4, "ops".into()))
        );
    }
}
