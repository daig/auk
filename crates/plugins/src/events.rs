//! Host events (`tern.on` in a host half, §9.7) and the window event names (§9.8).

use serde_json::{Value, json};

/// A daemon pane id as plugins see it (a Lua number; host ids stay below 2^44).
pub type PaneId = u64;

/// Every host event name `tern.on` accepts, in Tern's error-message order.
pub const HOST_EVENTS: &[&str] = &[
    "spawn",
    "command_started",
    "command_finished",
    "cwd",
    "title",
    "pane_exited",
];

/// Every window event name `tern.on` accepts in a window half.
pub const WINDOW_EVENTS: &[&str] = &[
    "window_start",
    "focus",
    "pane_created",
    "pane_closed",
    "tab_created",
    "tab_closed",
    "command_started",
    "command_finished",
    "cwd",
    "title",
];

/// A pane event the daemon sends to every host worker. (`spawn` is a request, not an event.)
#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    /// Shell integration reported a command start (OSC 133;C). `line` is as typed.
    CommandStarted {
        pane: PaneId,
        line: String,
        cwd: Option<String>,
    },
    /// The command finished (OSC 133;D), after a seen start.
    CommandFinished {
        pane: PaneId,
        line: String,
        status: i32,
        took_ms: f64,
    },
    /// A new usable working directory.
    Cwd { pane: PaneId, path: String },
    /// A new title.
    Title { pane: PaneId, title: String },
    /// A shell, command or plugin block ended.
    PaneExited { pane: PaneId, status: i32 },
}

impl HostEvent {
    /// The `tern.on` name, which is also the hook name budgets disable.
    pub fn name(&self) -> &'static str {
        match self {
            HostEvent::CommandStarted { .. } => "command_started",
            HostEvent::CommandFinished { .. } => "command_finished",
            HostEvent::Cwd { .. } => "cwd",
            HostEvent::Title { .. } => "title",
            HostEvent::PaneExited { .. } => "pane_exited",
        }
    }

    /// The pane the event concerns; effects from its handlers target that pane's windows.
    pub fn pane(&self) -> PaneId {
        match self {
            HostEvent::CommandStarted { pane, .. }
            | HostEvent::CommandFinished { pane, .. }
            | HostEvent::Cwd { pane, .. }
            | HostEvent::Title { pane, .. }
            | HostEvent::PaneExited { pane, .. } => *pane,
        }
    }

    /// The payload table handlers receive, as JSON (a nil `cwd` is left out).
    pub fn payload(&self) -> Value {
        match self {
            HostEvent::CommandStarted { pane, line, cwd } => {
                let mut v = json!({ "pane": pane, "line": line });
                if let Some(cwd) = cwd {
                    v["cwd"] = json!(cwd);
                }
                v
            }
            HostEvent::CommandFinished {
                pane,
                line,
                status,
                took_ms,
            } => {
                json!({ "pane": pane, "line": line, "status": status, "took_ms": took_ms })
            }
            HostEvent::Cwd { pane, path } => json!({ "pane": pane, "path": path }),
            HostEvent::Title { pane, title } => json!({ "pane": pane, "title": title }),
            HostEvent::PaneExited { pane, status } => json!({ "pane": pane, "status": status }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads() {
        let ev = HostEvent::CommandStarted {
            pane: 3,
            line: "ls".into(),
            cwd: None,
        };
        assert_eq!(ev.payload(), json!({"pane": 3, "line": "ls"}));
        assert_eq!(ev.name(), "command_started");
        let ev = HostEvent::CommandFinished {
            pane: 3,
            line: "ls".into(),
            status: 1,
            took_ms: 12.5,
        };
        assert_eq!(ev.payload()["took_ms"], 12.5);
        for ev in [
            HostEvent::Cwd {
                pane: 1,
                path: "/".into(),
            },
            HostEvent::Title {
                pane: 1,
                title: String::new(),
            },
            HostEvent::PaneExited { pane: 1, status: 0 },
        ] {
            assert!(HOST_EVENTS.contains(&ev.name()));
            assert_eq!(ev.pane(), 1);
        }
    }
}
