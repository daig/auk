//! Decorations: elements Tern adds that are not nodes, each `.sf-deco`.
//!
//! - `.sf-deco.sf-caption` (`.sf-drafted`, `.sf-caption-name`, `.sf-caption-draft`): the folded
//!   composer's caption, the first child of an `omp.editor*` col. Sheets show it in an unfocused
//!   pane (omp-composer.css); it names the session and, with a draft, shows the draft.
//! - `span.sf-deco.sf-thought-sum`: the summary on the first head of a chain of thoughts (two or
//!   more consecutive `omp.thinking*` sections): how many, and their summed `took`.
//! - `div.sf-deco.sf-work-head`: under the "Fold tool calls" setting, the head of a run of two or
//!   more consecutive work blocks among `main`'s children. The blocks of a run
//!   get `.sf-work`, folded ones `.sf-folded`.
//!
//! `div.sf-deco.sf-brand` (omp's app mark) is drawn by the `image` kind around its `omp` builtin.

use crate::common::{bool_prop, num_prop, str_prop};
use crate::spans;
use crate::state::SurfaceState;
use crate::view::{Cx, Mounts};
use auk_tsp::NodeRef;
use auk_ui::NodeId;
use serde_json::Value;
use std::collections::HashMap;

fn role<'a>(n: NodeRef<'a>) -> &'a str {
    str_prop(n.props(), "role").unwrap_or("")
}

// ---------------------------------------------------------------------------------------------
// The folded composer's caption

/// omp's composer: a col whose role is `omp.editor*`.
pub(crate) fn is_composer(n: NodeRef) -> bool {
    n.kind() == "col" && role(n).starts_with("omp.editor")
}

/// The composer col drew its caption `el` (its first child).
pub(crate) fn caption(cx: &mut Cx, col: NodeRef, el: NodeId) {
    cx.st.captions.insert(col.id().to_owned(), el);
    fill_caption(cx, col, el);
}

/// A field in a composer changed: its caption follows the draft.
pub(crate) fn field_changed(cx: &mut Cx, field: NodeRef) {
    let Some(col) = std::iter::successors(field.parent(), |n| n.parent()).find(|n| is_composer(*n))
    else {
        return;
    };
    if let Some(&el) = cx.st.captions.get(col.id())
        && cx.dom.doc.try_node(el).is_some()
    {
        fill_caption(cx, col, el);
    }
}

/// The draft: the composer's first field's text, on one line.
fn draft(col: NodeRef) -> String {
    let mut stack = vec![col];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "editor" | "input") {
            let t = crate::kinds::input::field_text(n);
            return t
                .split('\n')
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join(" ")
                .trim()
                .to_owned();
        }
        let kids: Vec<NodeRef> = n.children().collect();
        stack.extend(kids.into_iter().rev());
    }
    String::new()
}

fn fill_caption(cx: &mut Cx, col: NodeRef, el: NodeId) {
    let name = cx.sf.title().unwrap_or("").to_owned();
    let draft = draft(col);
    cx.dom.clear(el);
    cx.dom.set_class(
        el,
        if draft.is_empty() {
            "sf-deco sf-caption"
        } else {
            "sf-deco sf-caption sf-drafted"
        },
    );
    cx.dom.attr(el, "aria-hidden", "true");
    if !name.is_empty() {
        cx.dom.child_text(el, "span", "sf-caption-name", &name);
    }
    if !draft.is_empty() {
        cx.dom.child_text(el, "span", "sf-caption-draft", &draft);
    }
}

// ---------------------------------------------------------------------------------------------
// Chains of thoughts

pub(crate) fn is_thought(n: NodeRef) -> bool {
    n.kind() == "section" && role(n).starts_with("omp.thinking")
}

/// A thinking section drew its head: the summary it carries goes back on, and its chain is
/// summed again (its `took` may have changed).
pub(crate) fn thought_head(cx: &mut Cx, section: NodeRef, head: NodeId) {
    if !is_thought(section) {
        return;
    }
    cx.st.thought_heads.insert(section.id().to_owned(), head);
    if let Some(t) = cx.st.thought_sums.get(section.id()).cloned() {
        put_sum(cx, head, &t);
    }
    if let Some(p) = section.parent() {
        thought_chains(cx, p);
    }
}

/// The summaries over `parent`'s children: the first section of each chain of two or more
/// consecutive thinking sections shows one; the others none.
pub(crate) fn thought_chains(cx: &mut Cx, parent: NodeRef) {
    let kids: Vec<NodeRef> = parent
        .children()
        .filter(|c| !bool_prop(c.props(), "hidden"))
        .collect();
    let mut i = 0;
    while i < kids.len() {
        if !is_thought(kids[i]) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < kids.len() && is_thought(kids[j]) {
            j += 1;
        }
        let chain = &kids[i..j];
        let want = (chain.len() >= 2).then(|| thought_summary(chain));
        set_sum(cx, chain[0].id(), want);
        for k in &chain[1..] {
            set_sum(cx, k.id(), None);
        }
        i = j;
    }
}

/// "3 thoughts · 5.7s": how many, and their summed `took` when they have one.
pub fn thought_summary_text(n: usize, took_ms: f64) -> String {
    if took_ms > 0.0 {
        format!(
            "{n} thoughts · {}",
            crate::motion::format_duration(took_ms, false)
        )
    } else {
        format!("{n} thoughts")
    }
}

fn thought_summary(chain: &[NodeRef]) -> String {
    let took: f64 = chain
        .iter()
        .filter_map(|s| num_prop(s.props(), "took"))
        .filter(|t| *t > 0.0)
        .sum();
    thought_summary_text(chain.len(), took)
}

fn set_sum(cx: &mut Cx, id: &str, want: Option<String>) {
    if cx.st.thought_sums.get(id) == want.as_ref() {
        return;
    }
    match &want {
        Some(t) => cx.st.thought_sums.insert(id.to_owned(), t.clone()),
        None => cx.st.thought_sums.remove(id),
    };
    let Some(&head) = cx.st.thought_heads.get(id) else {
        return;
    };
    if cx.dom.doc.try_node(head).is_none() {
        return;
    }
    let old: Vec<NodeId> = cx
        .dom
        .doc
        .children(head)
        .iter()
        .copied()
        .filter(|&c| cx.dom.class_of(c).split(' ').any(|x| x == "sf-thought-sum"))
        .collect();
    for c in old {
        cx.dom.doc.remove(c);
    }
    if let Some(t) = want {
        put_sum(cx, head, &t);
    }
}

fn put_sum(cx: &mut Cx, head: NodeId, text: &str) {
    cx.dom
        .child_text(head, "span", "sf-deco sf-thought-sum", text);
}

// ---------------------------------------------------------------------------------------------
// The work fold

enum Block {
    Work {
        took: Option<f64>,
        label: Option<String>,
    },
    /// Takes no room and neither joins nor breaks a run (hidden, an empty col).
    Skip,
    Other,
}

fn label(n: NodeRef) -> Option<String> {
    let p = n.props();
    let title = spans::spans_text(&spans::parse_spans(p.get("title").filter(|t| !t.is_null())));
    if !title.is_empty() {
        return Some(title);
    }
    str_prop(p, "name")
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn classify(n: NodeRef) -> Block {
    let p = n.props();
    if bool_prop(p, "hidden") {
        return Block::Skip;
    }
    let grows = num_prop(p, "grow").is_some_and(|g| g > 0.0);
    if matches!(n.kind(), "col" | "row") && n.child_count() == 0 && !grows {
        return Block::Skip;
    }
    // Tool nodes and omp's tool cards.
    if n.kind() == "tool" || (n.kind() == "card" && role(n).starts_with("omp.tool")) {
        return Block::Work {
            took: num_prop(p, "took"),
            label: label(n),
        };
    }
    // omp's thinking-only messages.
    if matches!(n.kind(), "col" | "card") {
        let shown: Vec<NodeRef> = n
            .children()
            .filter(|c| !bool_prop(c.props(), "hidden"))
            .collect();
        if !shown.is_empty() && shown.iter().all(|c| is_thought(*c)) {
            let took: Vec<f64> = shown
                .iter()
                .filter_map(|c| num_prop(c.props(), "took"))
                .collect();
            return Block::Work {
                took: (!took.is_empty()).then(|| took.iter().sum()),
                label: None,
            };
        }
    }
    Block::Other
}

/// "Worked for 12.3s · Deploy ×2 · Test": the summed `took`, then each title (else name) with
/// its count, in order of appearance.
pub fn work_summary_text(took_ms: Option<f64>, labels: &[(String, usize)]) -> String {
    let mut s = match took_ms {
        Some(t) => format!("Worked for {}", crate::motion::format_duration(t, false)),
        None => "Worked".to_owned(),
    };
    for (l, n) in labels {
        s.push_str(" · ");
        s.push_str(l);
        if *n > 1 {
            s.push_str(&format!(" ×{n}"));
        }
    }
    s
}

/// The fold state key of the run starting at block `first`.
pub(crate) fn work_key(first: &str) -> String {
    format!("\u{1f}work\u{1f}{first}")
}

/// Folds the runs of work among `main`'s children: heads go into `direct` (main's child
/// elements, in child order) before each run; blocks get their classes.
pub(crate) fn fold_work(cx: &mut Cx, main: NodeRef, mounts: &Mounts, direct: &mut Vec<NodeId>) {
    // With the setting off and no run left over from when it was on, there is nothing to fold
    // or unfold: main's children needn't be looked at.
    if !cx.set.fold_work && cx.st.work.is_empty() && cx.st.work_heads.is_empty() {
        return;
    }
    let kids: Vec<NodeRef> = main.children().collect();
    let mut runs: Vec<(Vec<usize>, bool)> = Vec::new();
    let mut blocks: Vec<Option<(Option<f64>, Option<String>)>> = Vec::with_capacity(kids.len());
    if cx.set.fold_work && direct.len() == kids.len() {
        let mut cur: Vec<usize> = Vec::new();
        for (i, k) in kids.iter().enumerate() {
            match classify(*k) {
                Block::Work { took, label } => {
                    blocks.push(Some((took, label)));
                    cur.push(i);
                }
                Block::Skip => blocks.push(None),
                Block::Other => {
                    blocks.push(None);
                    if cur.len() >= 2 {
                        runs.push((std::mem::take(&mut cur), true));
                    }
                    cur.clear();
                }
            }
        }
        if cur.len() >= 2 {
            runs.push((cur, false));
        }
    }
    let mut work: HashMap<String, bool> = HashMap::new();
    let mut heads: HashMap<String, NodeId> = HashMap::new();
    let mut inserts: Vec<(usize, NodeId)> = Vec::new();
    for (run, followed) in &runs {
        let first = kids[run[0]].id();
        // A run another block follows starts folded; one still growing at the end, open. The
        // user's toggle holds until that changes.
        let folded = SurfaceState::flip(
            &mut cx.st.folds,
            &work_key(first),
            Some(&Value::Bool(*followed)),
            *followed,
        );
        let mut took: Option<f64> = None;
        let mut labels: Vec<(String, usize)> = Vec::new();
        for &i in run {
            if let Some((t, l)) = &blocks[i] {
                if let Some(t) = t {
                    took = Some(took.unwrap_or(0.0) + t.max(0.0));
                }
                if let Some(l) = l {
                    match labels.iter_mut().find(|(x, _)| x == l) {
                        Some(e) => e.1 += 1,
                        None => labels.push((l.clone(), 1)),
                    }
                }
            }
            work.insert(kids[i].id().to_owned(), folded);
        }
        let head = cx
            .st
            .work_heads
            .remove(first)
            .filter(|&h| cx.dom.doc.try_node(h).is_some())
            .unwrap_or_else(|| cx.dom.el("div", "sf-deco sf-work-head"));
        draw_head(cx, head, first, folded, &work_summary_text(took, &labels));
        heads.insert(first.to_owned(), head);
        inserts.push((run[0], head));
    }
    for (_, h) in std::mem::replace(&mut cx.st.work_heads, heads) {
        if cx.dom.doc.try_node(h).is_some() {
            cx.dom.doc.remove(h);
        }
    }
    // Classes on the blocks whose part in a run changed.
    let old = std::mem::take(&mut cx.st.work);
    for id in old.keys().chain(work.keys()) {
        let want = work.get(id).copied();
        if old.get(id).copied() == want {
            continue;
        }
        let Some(m) = mounts.get(id) else { continue };
        let kept: Vec<String> = cx
            .dom
            .class_of(m.el)
            .split(' ')
            .filter(|c| !c.is_empty() && *c != "sf-work" && *c != "sf-folded")
            .map(str::to_owned)
            .collect();
        let mut class = kept.join(" ");
        if let Some(f) = want {
            class.push_str(if f { " sf-work sf-folded" } else { " sf-work" });
        }
        cx.dom.set_class(m.el, &class);
    }
    cx.st.work = work;
    for (at, head) in inserts.into_iter().rev() {
        direct.insert(at, head);
    }
}

/// The classes a block in a run adds to its node element.
pub(crate) fn work_class(st: &SurfaceState, id: &str) -> Option<&'static str> {
    st.work
        .get(id)
        .map(|&f| if f { "sf-work sf-folded" } else { "sf-work" })
}

fn draw_head(cx: &mut Cx, head: NodeId, first: &str, folded: bool, text: &str) {
    cx.dom.set_class(head, "sf-deco sf-work-head");
    cx.dom.attr(head, "role", "button");
    cx.dom.attr(head, "tabindex", "0");
    cx.dom.attr(head, "data-work", first);
    cx.dom
        .attr(head, "aria-expanded", if folded { "false" } else { "true" });
    cx.dom.clear(head);
    crate::kinds::layout::chevron(cx, head, folded);
    cx.dom.child_text(head, "span", "sf-work-t", text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_read_as_the_book_shows_them() {
        assert_eq!(
            work_summary_text(Some(12_345.0), &[("Deploy".into(), 2), ("Test".into(), 1)]),
            "Worked for 12.3s · Deploy ×2 · Test"
        );
        assert_eq!(
            work_summary_text(None, &[("Read".into(), 3)]),
            "Worked · Read ×3"
        );
        assert_eq!(thought_summary_text(3, 5_700.0), "3 thoughts · 5.7s");
        assert_eq!(thought_summary_text(2, 0.0), "2 thoughts");
    }
}
