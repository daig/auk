//! `block`: the command-lens block (§8.2 T2, §9.6, research/elements.md §2 `block`). Tern draws
//! it around a lens view; it isn't advertised to programs.
//!
//! The block shows `native` when there is one, else `raw`. Native/Raw is a local choice kept by
//! the node's state key (its `key` among its siblings, else its id); only the shown child is in
//! the tree, the other waits detached in its slot. The toolbar holds the lens label, the
//! Native/Raw control (only with a native view) and Copy, which copies the raw output as plain
//! text at the capture's width. The foot shows `exit N` after a failing exit and the duration
//! once it passes a second.

use super::{begin, icon, part, slot};
use crate::common::{num_prop, str_prop};
use crate::state::state_key;
use crate::view::{Cx, Look, Mount};
use auk_tsp::NodeRef;

/// Wrappers the lens label skips, as the claim does (§9.6).
const WRAPPERS: [&str; 15] = [
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

/// Programs whose label names their subcommand too.
const SUBCOMMANDS: [&str; 14] = [
    "git",
    "cargo",
    "docker",
    "npm",
    "kubectl",
    "pnpm",
    "yarn",
    "go",
    "brew",
    "terraform",
    "helm",
    "gh",
    "bun",
    "podman",
];

/// Whether a child of a block is its `raw` child: the one with id `raw` (or ending `.raw`), else
/// the last `ansi` child.
pub(crate) fn is_raw(node: NodeRef, child: NodeRef) -> bool {
    let named = |c: NodeRef| c.id() == "raw" || c.id().ends_with(".raw");
    if node.children().any(named) {
        return named(child);
    }
    child.kind() == "ansi"
        && node
            .children()
            .filter(|c| c.kind() == "ansi")
            .last()
            .is_some_and(|last| last.id() == child.id())
}

/// The native child: the first child that isn't raw.
fn native(node: NodeRef) -> Option<NodeRef> {
    node.children().find(|c| !is_raw(node, *c))
}

/// The key of a block's Native/Raw choice.
pub(crate) fn view_key(node: NodeRef) -> String {
    format!("{}\u{1f}raw-view", state_key(node))
}

/// Whether the block shows its raw output.
pub(crate) fn shows_raw(cx_more: &std::collections::HashMap<String, bool>, node: NodeRef) -> bool {
    native(node).is_none() || cx_more.get(&view_key(node)).copied().unwrap_or(false)
}

/// The label of a command line: the program (no directories, assignments or wrappers) and, for
/// some programs, the subcommand.
pub fn lens_label(line: &str) -> String {
    let mut words = line.split_whitespace().peekable();
    // Assignments and wrappers (with their options) go first.
    while let Some(w) = words.peek() {
        let base = w.rsplit('/').next().unwrap_or(w);
        if w.contains('=') && !w.starts_with('=') || WRAPPERS.contains(&base) || w.starts_with('-')
        {
            words.next();
        } else {
            break;
        }
    }
    let Some(program) = words.next() else {
        return String::new();
    };
    let program = program.rsplit('/').next().unwrap_or(program).to_owned();
    if SUBCOMMANDS.contains(&program.as_str())
        && let Some(sub) = words.find(|w| !w.starts_with('-'))
    {
        return format!("{program} {sub}");
    }
    program
}

/// The duration in the foot: `2.4s`, `1m 5s`.
fn took_text(ms: f64) -> String {
    let s = ms / 1000.0;
    if s < 60.0 {
        format!("{:.1}s", s)
    } else {
        let m = (s / 60.0).floor();
        format!("{}m {}s", m as u64, (s - m * 60.0).floor() as u64)
    }
}

/// The raw output as plain text, as a terminal `cols` wide shows it (Copy).
pub fn raw_text(text: &str, cols: u16) -> String {
    let mut st = crate::ansi::AnsiState::new(cols.max(2));
    st.set(text, cols.max(2));
    st.text()
}

pub(crate) fn block(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    let p = node.props();
    let lens = str_prop(p, "lens").unwrap_or("");
    let role = str_prop(p, "role")
        .map(str::to_owned)
        .unwrap_or_else(|| format!("lens.{lens}"));
    let running = str_prop(p, "state") == Some("running");
    let exit = num_prop(p, "exit").map(|e| e as i64);
    let failed = !running && exit.is_some_and(|e| e != 0);
    let has_native = native(node).is_some();
    let raw = shows_raw(&cx.st.more, node);
    look.class_if(failed, "failed");
    look.attr("data-role", role.clone());
    look.attr("data-state", if running { "running" } else { "done" });
    look.attr("data-view", if raw { "raw" } else { "native" });

    // The toolbar.
    let bar = part(cx, m, "div", "sf-block-bar");
    let cmd = str_prop(p, "cmd").unwrap_or("");
    let line = str_prop(p, "line").unwrap_or(cmd);
    let label = cx.dom.child(bar, "span", "sf-block-lens");
    if !cmd.is_empty() {
        cx.dom.attr(label, "title", cmd);
    }
    let ic = cx.dom.child(label, "span", "sf-block-ic");
    cx.dom.attr(ic, "aria-hidden", "true");
    let icon_name = super::layout::role_icon(&role).unwrap_or("terminal");
    icon(cx, ic, icon_name);
    cx.dom.child_text(label, "span", "lbl", &lens_label(line));
    if has_native {
        let seg = cx.dom.child(bar, "div", "sf-block-seg");
        cx.dom.attr(seg, "role", "group");
        cx.dom.attr(seg, "aria-label", "Output");
        for (name, on) in [("Native", !raw), ("Raw", raw)] {
            let b = cx.dom.child_text(
                seg,
                "button",
                if on {
                    "sf-block-opt on"
                } else {
                    "sf-block-opt"
                },
                name,
            );
            cx.dom.attr(b, "data-view", &name.to_ascii_lowercase());
            cx.dom
                .attr(b, "aria-pressed", if on { "true" } else { "false" });
        }
    }
    let copy = cx.dom.child(bar, "button", "sf-block-copy");
    cx.dom.attr(copy, "aria-label", "Copy output");
    icon(cx, copy, "copy");

    // The body: the shown child's slot. The other slot stays detached.
    let body = part(cx, m, "div", "sf-block-body");
    let shown = slot(cx, m, usize::from(raw), "div", "sf-block-slot");
    let _ = slot(cx, m, usize::from(!raw), "div", "sf-block-slot");
    cx.dom.doc.append_child(body, shown);

    // The foot.
    let took = num_prop(p, "took").filter(|t| !running && *t > 1000.0);
    if failed || took.is_some() {
        let foot = part(cx, m, "div", "sf-block-foot");
        if failed && let Some(e) = exit {
            let b = cx
                .dom
                .child_text(foot, "span", "sf sf-badge", &format!("exit {e}"));
            cx.dom.attr(b, "data-tone", "error");
        }
        if let Some(t) = took {
            cx.dom
                .child_text(foot, "span", "sf-block-took", &took_text(t));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_skip_assignments_wrappers_and_directories() {
        assert_eq!(
            lens_label("FOO=1 sudo -E /usr/bin/kubectl get pods -A"),
            "kubectl get"
        );
        assert_eq!(lens_label("terraform plan"), "terraform plan");
        assert_eq!(
            lens_label("./bin/terraform -chdir=x plan"),
            "terraform plan"
        );
        assert_eq!(lens_label("du -sh ."), "du");
        assert_eq!(lens_label(""), "");
    }

    #[test]
    fn copy_text_is_plain_at_the_capture_width() {
        assert_eq!(
            raw_text("\x1b[31mred\x1b[0m\r\nnext\r\n", 20).trim_end(),
            "red\nnext"
        );
    }
}
