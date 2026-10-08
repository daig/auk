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
        }
    }

    /// The rows the query shows: an open row for a typed path, then matching commands.
    pub fn rows(&self) -> Vec<Row> {
        let q = self.query.trim();
        let mut out = Vec::new();
        if q.starts_with('/') || q.starts_with('~') {
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
}
