//! The daemon's event stream (§11.1, `auk events [--filter k,…]`): one JSON object per line, such
//! as `{"event":"pane_spawned","pane":3,"window":"main","by":2}`.
//!
//! `by` says who caused the event: a client id (a window's layout edit, focus), `"cli"` (a
//! control-endpoint command), `"plugin"` (a block a plugin opened) or `null` (the daemon itself:
//! a process exiting, output from a pane). The kinds:
//!
//! | Event | Fields |
//! | --- | --- |
//! | `pane_spawned` | `pane`, `window`, `kind` |
//! | `pane_exited` | `pane`, `status` |
//! | `pane_closed` | `pane` (closed by `close` or a window) |
//! | `layout_changed` | `window` |
//! | `focus_changed` | `pane` |
//! | `title_changed` | `pane`, `title` |
//! | `cwd_changed` | `pane`, `cwd` |
//! | `command_started` | `pane`, `cmdline` |
//! | `command_finished` | `pane`, `status` |
//! | `client_connected` | `client`, `kind` |
//! | `client_attached` | `client`, `window` |
//! | `client_left` | `client` |
//! | `lagged` | `missed`: a slow reader lost that many events |
//!
//! A control connection that sends `events [--filter k,…]` gets `{"ok":true,"value":
//! {"streaming":true}}` and then only events, until it disconnects. One daemon runs per process,
//! so the bus is process-wide; events are built only while someone listens.

use std::cell::RefCell;
use std::sync::{Arc, OnceLock};

use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::broadcast;

use crate::server::Daemon;

/// Who caused an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Actor {
    Client(u64),
    Cli,
    Plugin,
}

impl Actor {
    fn json(self) -> Value {
        match self {
            Actor::Client(id) => json!(id),
            Actor::Cli => json!("cli"),
            Actor::Plugin => json!("plugin"),
        }
    }
}

thread_local! {
    static ACTOR: RefCell<Option<Actor>> = const { RefCell::new(None) };
}

/// Runs `f` with `actor` as the cause of the events it emits on this thread.
pub fn as_actor<T>(actor: Actor, f: impl FnOnce() -> T) -> T {
    let previous = ACTOR.with(|a| a.replace(Some(actor)));
    let out = f();
    ACTOR.with(|a| *a.borrow_mut() = previous);
    out
}

fn bus() -> &'static broadcast::Sender<Arc<str>> {
    static BUS: OnceLock<broadcast::Sender<Arc<str>>> = OnceLock::new();
    BUS.get_or_init(|| broadcast::channel(4096).0)
}

/// How many streams are listening.
pub fn subscribers() -> usize {
    bus().receiver_count()
}

/// Whether anyone listens (events are only built then).
pub fn enabled() -> bool {
    subscribers() > 0
}

/// Emits an event: `fields` (an object) joined to `{"event": kind, "by": …}`. `by` comes from
/// `fields` when it has one, else from the current [`as_actor`] scope.
pub fn emit(kind: &str, fields: Value) {
    if !enabled() {
        return;
    }
    let mut obj = Map::new();
    obj.insert("event".into(), json!(kind));
    if let Value::Object(fields) = fields {
        obj.extend(fields);
    }
    if !obj.contains_key("by") {
        let by = ACTOR.with(|a| *a.borrow());
        obj.insert("by".into(), by.map_or(Value::Null, Actor::json));
    }
    let _ = bus().send(Value::Object(obj).to_string().into());
}

/// Events from what a pane's replica took: titles, directories and command marks.
pub fn pane_events(pane: u64, events: &[auk_replica::Event]) {
    if !enabled() {
        return;
    }
    for event in events {
        match event {
            auk_replica::Event::Title(title) => emit(
                "title_changed",
                json!({"pane": pane, "title": title, "by": null}),
            ),
            auk_replica::Event::Cwd(url) => {
                let cwd = crate::pane::osc7_path(url)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| url.clone());
                emit("cwd_changed", json!({"pane": pane, "cwd": cwd, "by": null}));
            }
            auk_replica::Event::Mark(auk_term::Mark::CommandStart { cmdline, typed }) => emit(
                "command_started",
                json!({"pane": pane, "cmdline": typed.as_ref().or(cmdline.as_ref()), "by": null}),
            ),
            auk_replica::Event::Mark(auk_term::Mark::CommandEnd { status }) => emit(
                "command_finished",
                json!({"pane": pane, "status": status, "by": null}),
            ),
            _ => {}
        }
    }
}

/// Parses `events [--filter k,…]`'s arguments into the kinds to pass (`None`: all).
pub fn parse_filter(args: &[String]) -> Result<Option<Vec<String>>, String> {
    let mut filter: Option<Vec<String>> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let list = match arg.strip_prefix("--filter") {
            Some("") => it.next().ok_or("--filter needs a list")?.as_str(),
            Some(rest) if rest.starts_with('=') => &rest[1..],
            _ => return Err(format!("events: unexpected `{arg}`")),
        };
        filter.get_or_insert_with(Vec::new).extend(
            list.split(',')
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .map(str::to_owned),
        );
    }
    Ok(filter)
}

fn passes(line: &str, filter: &Option<Vec<String>>) -> bool {
    let Some(kinds) = filter else { return true };
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    let kind = v["event"].as_str().unwrap_or("");
    kind == "lagged" || kinds.iter().any(|k| k == kind)
}

/// Serves the daemon's control endpoint: each line goes to [`Daemon::command`], except `events`,
/// which turns the connection into an event stream.
pub async fn serve_control(listener: tokio::net::UnixListener, daemon: Arc<Daemon>) {
    while let Ok((stream, _)) = listener.accept().await {
        let daemon = daemon.clone();
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut lines = tokio::io::BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim().to_owned();
                let words = shlex::split(&line).unwrap_or_default();
                if words.first().map(String::as_str) == Some("events") {
                    let filter = match parse_filter(&words[1..]) {
                        Ok(f) => f,
                        Err(message) => {
                            let reply = format!("{}\n", crate::control::error(message));
                            if write.write_all(reply.as_bytes()).await.is_err() {
                                return;
                            }
                            continue;
                        }
                    };
                    let rx = bus().subscribe();
                    stream_events(rx, filter, lines, write).await;
                    return;
                }
                let mut reply = daemon.command(&line).await.to_string();
                reply.push('\n');
                if write.write_all(reply.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
    }
}

async fn stream_events(
    mut rx: broadcast::Receiver<Arc<str>>,
    filter: Option<Vec<String>>,
    mut lines: tokio::io::Lines<tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>>,
    mut write: tokio::net::unix::OwnedWriteHalf,
) {
    let ack = format!("{}\n", crate::control::ok(json!({"streaming": true})));
    if write.write_all(ack.as_bytes()).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            event = rx.recv() => {
                let line: Arc<str> = match event {
                    Ok(line) => line,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        json!({"event": "lagged", "missed": n, "by": null}).to_string().into()
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                if !passes(&line, &filter) {
                    continue;
                }
                let mut out = line.to_string();
                out.push('\n');
                if write.write_all(out.as_bytes()).await.is_err() {
                    return;
                }
            }
            // The reader hung up (or sent something, which a stream ignores).
            next = lines.next_line() => {
                if !matches!(next, Ok(Some(_))) {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_parse_and_pass() {
        let args = |s: &[&str]| s.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_filter(&args(&[])).unwrap(), None);
        let f = parse_filter(&args(&["--filter", "pane_spawned,layout_changed"])).unwrap();
        assert_eq!(
            f.as_deref(),
            Some(&["pane_spawned".to_owned(), "layout_changed".to_owned()][..])
        );
        assert!(passes(r#"{"event":"layout_changed","by":1}"#, &f));
        assert!(!passes(r#"{"event":"title_changed","by":null}"#, &f));
        assert!(parse_filter(&args(&["--nope"])).is_err());
        let f = parse_filter(&args(&["--filter=a"])).unwrap();
        assert_eq!(f.as_deref(), Some(&["a".to_owned()][..]));
    }

    #[test]
    fn actors_scope_events() {
        let mut rx = bus().subscribe();
        as_actor(Actor::Client(7), || {
            emit("layout_changed", json!({"window": "w"}))
        });
        emit("pane_exited", json!({"pane": 1, "status": 0}));
        let first: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(
            first,
            json!({"event": "layout_changed", "window": "w", "by": 7})
        );
        let second: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(second["by"], Value::Null);
    }
}
