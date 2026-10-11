//! Lens captures on a worker: `open`, `line`, `finish`, views at most once per 250 ms per
//! block (and once at finish, and after every event), the last 256 captures per plugin, and
//! rehydration from the saved command and raw output when an event arrives for a capture whose
//! state is gone.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use mlua::Value;
use serde_json::{Map, Value as Json};

use crate::api::LensHandlers;
use crate::api::effects::EffectTarget;
use crate::capture::{Capture as Scratch, LensLine};
use crate::claim::{LensRun, normalize};
use crate::events::PaneId;
use crate::vm::CallError;
use crate::worker::{HostOutput, Worker};

/// A lens block's id, chosen by the daemon (unique per host).
pub type LensId = u64;

/// Captures each plugin keeps.
pub const MAX_CAPTURES: usize = 256;
/// The least time between two views of one block while its command runs.
pub const VIEW_INTERVAL: Duration = Duration::from_millis(250);

/// What a lens block keeps, so its state can be rebuilt after the worker lost it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LensRecord {
    /// The command line the claim read.
    pub line: String,
    /// The line as typed.
    pub typed: String,
    /// The shell's directory, when known.
    pub cwd: Option<String>,
    /// The raw output.
    pub raw: Vec<u8>,
    /// The capture width.
    pub cols: u16,
    /// The exit status, once the command finished.
    pub status: Option<i32>,
}

/// Requests about lens captures.
#[derive(Debug, Clone)]
pub(crate) enum LensMsg {
    Open {
        block: LensId,
        pane: PaneId,
        lens: String,
        run: LensRun,
    },
    Lines {
        block: LensId,
        lines: Vec<LensLine>,
    },
    Finish {
        block: LensId,
        status: i32,
    },
    Abort {
        block: LensId,
    },
    Event {
        block: LensId,
        pane: PaneId,
        event: Map<String, Json>,
        /// The claiming lens and the block's record, when the host's current globs still claim
        /// the command (used only when the state is gone).
        rehydrate: Option<(String, LensRecord)>,
    },
}

struct CaptureState {
    pane: PaneId,
    lens: String,
    handlers: LensHandlers,
    /// `None` when `open` raised: the block keeps its raw output.
    state: Option<Value>,
    cx: Value,
    finished: bool,
    dirty: bool,
    last_view: Option<Instant>,
    used: u64,
}

/// A worker's lens captures.
#[derive(Default)]
pub(crate) struct LensCaptures {
    map: HashMap<LensId, CaptureState>,
    clock: u64,
}

impl LensCaptures {
    pub(crate) fn next_view_due(&self) -> Option<Instant> {
        self.map
            .values()
            .filter(|c| c.dirty && !c.finished)
            .filter_map(|c| c.last_view.map(|t| t + VIEW_INTERVAL))
            .min()
    }

    fn touch(&mut self, block: LensId) {
        self.clock += 1;
        if let Some(c) = self.map.get_mut(&block) {
            c.used = self.clock;
        }
    }

    fn insert(&mut self, block: LensId, capture: CaptureState) {
        self.map.insert(block, capture);
        self.touch(block);
        while self.map.len() > MAX_CAPTURES {
            // Finished captures go first, then the least recently used.
            let victim = self
                .map
                .iter()
                .min_by_key(|(_, c)| (!c.finished, c.used))
                .map(|(id, _)| *id);
            match victim {
                Some(id) => {
                    self.map.remove(&id);
                }
                None => break,
            }
        }
    }

    /// Whether the worker still has `block`'s state.
    pub(crate) fn has(&self, block: LensId) -> bool {
        self.map.contains_key(&block)
    }
}

fn line_table(worker: &Worker, line: &LensLine) -> mlua::Result<Value> {
    let json = serde_json::to_value(line).map_err(mlua::Error::external)?;
    worker.vm.to_lua(&json)
}

fn run_table(worker: &Worker, run: &LensRun) -> mlua::Result<Value> {
    let lua = worker.vm.lua();
    let t = lua.create_table()?;
    t.set("line", run.line.as_str())?;
    t.set("typed", run.typed.as_str())?;
    t.set("cwd", run.cwd.as_deref())?;
    t.set("program", run.program.as_str())?;
    t.set(
        "args",
        lua.create_sequence_from(run.args.iter().map(String::as_str))?,
    )?;
    Ok(Value::Table(t))
}

/// Starts a capture; returns false when `open` raised (or the lens is unknown).
fn open(worker: &mut Worker, block: LensId, pane: PaneId, lens: &str, run: &LensRun) -> bool {
    let Some(handlers) = worker.vm.lens(lens) else {
        return false;
    };
    let target = EffectTarget::Pane(pane);
    let Ok(cx) = worker.vm.effect_cx(target) else {
        return false;
    };
    let state = match run_table(worker, run) {
        Ok(run) => {
            worker
                .call::<Value>(
                    &format!("{lens}.open"),
                    target,
                    &handlers.open,
                    (run, cx.clone()),
                )
                .0
        }
        Err(e) => {
            worker.toast_failure(&format!("{lens}.open"), target, &CallError::Lua(e));
            None
        }
    };
    let ok = state.is_some();
    worker.lenses.insert(
        block,
        CaptureState {
            pane,
            lens: lens.to_owned(),
            handlers,
            state,
            cx,
            finished: false,
            dirty: false,
            last_view: None,
            used: 0,
        },
    );
    ok
}

fn feed_lines(worker: &mut Worker, block: LensId, lines: &[LensLine]) {
    let Some(c) = worker.lenses.map.get(&block) else {
        return;
    };
    let Some(state) = c.state.clone() else { return };
    let (pane, lens, f) = (c.pane, c.lens.clone(), c.handlers.line.clone());
    let hook = format!("{lens}.line");
    for line in lines {
        let Ok(table) = line_table(worker, line) else {
            continue;
        };
        let (_, go_on) =
            worker.call::<()>(&hook, EffectTarget::Pane(pane), &f, (state.clone(), table));
        if !go_on {
            break;
        }
    }
    if let Some(c) = worker.lenses.map.get_mut(&block) {
        c.dirty |= !lines.is_empty();
    }
}

fn finish(worker: &mut Worker, block: LensId, status: i32) {
    let Some(c) = worker.lenses.map.get(&block) else {
        return;
    };
    let Some(state) = c.state.clone() else { return };
    let hook = format!("{}.finish", c.lens);
    let (pane, f) = (c.pane, c.handlers.finish.clone());
    worker.call::<()>(
        &hook,
        EffectTarget::Pane(pane),
        &f,
        (state, f64::from(status)),
    );
    if let Some(c) = worker.lenses.map.get_mut(&block) {
        c.finished = true;
    }
}

/// Runs `view` and sends the result out of band.
fn view(worker: &mut Worker, block: LensId) {
    let Some(c) = worker.lenses.map.get_mut(&block) else {
        return;
    };
    c.dirty = false;
    c.last_view = Some(Instant::now());
    let Some(state) = c.state.clone() else { return };
    let (pane, hook, f) = (c.pane, format!("{}.view", c.lens), c.handlers.view.clone());
    let target = EffectTarget::Pane(pane);
    let (value, _) = worker.call::<Value>(&hook, target, &f, state);
    let Some(value) = value else { return };
    let view = match value {
        Value::Nil => None,
        other => match worker.vm.to_json(&other) {
            Ok(json) if json.get("k").and_then(Json::as_str).is_some() => Some(json),
            Ok(_) => {
                let e = mlua::Error::runtime("view: a node needs a string `k`");
                worker.toast_failure(&hook, target, &CallError::Lua(e));
                return;
            }
            Err(e) => {
                worker.toast_failure(&hook, target, &CallError::Lua(mlua::Error::runtime(e)));
                return;
            }
        },
    };
    // Effects of the handler that led here go out first.
    worker.after_handler();
    worker.send(HostOutput::LensView { block, pane, view });
}

fn event(worker: &mut Worker, block: LensId, ev: Map<String, Json>) {
    let Some(c) = worker.lenses.map.get(&block) else {
        return;
    };
    let (Some(state), Some(f)) = (c.state.clone(), c.handlers.event.clone()) else {
        view(worker, block);
        return;
    };
    let (pane, hook, cx) = (c.pane, format!("{}.event", c.lens), c.cx.clone());
    if let Ok(table) = worker.vm.to_lua(&Json::Object(ev)) {
        worker.call::<()>(&hook, EffectTarget::Pane(pane), &f, (state, table, cx));
    }
    view(worker, block);
}

/// Replays a record through `open`, `line` and `finish`.
fn rehydrate(worker: &mut Worker, block: LensId, pane: PaneId, lens: &str, record: &LensRecord) {
    let Some(command) = normalize(&record.line) else {
        return;
    };
    let run = LensRun {
        line: record.line.clone(),
        typed: record.typed.clone(),
        cwd: record.cwd.clone(),
        program: command.program,
        args: command.args,
    };
    if !open(worker, block, pane, lens, &run) {
        return;
    }
    let mut scratch = Scratch::new(record.cols);
    let mut lines = scratch.feed(&record.raw).unwrap_or_default();
    if let Some(status) = record.status {
        lines.extend(scratch.finish());
        feed_lines(worker, block, &lines);
        finish(worker, block, status);
    } else {
        feed_lines(worker, block, &lines);
    }
}

pub(crate) fn handle(worker: &mut Worker, msg: LensMsg) {
    match msg {
        LensMsg::Open {
            block,
            pane,
            lens,
            run,
        } => {
            open(worker, block, pane, &lens, &run);
        }
        LensMsg::Lines { block, lines } => {
            worker.lenses.touch(block);
            feed_lines(worker, block, &lines);
            let due = worker.lenses.map.get(&block).is_some_and(|c| {
                c.dirty && c.last_view.is_none_or(|t| t.elapsed() >= VIEW_INTERVAL)
            });
            if due {
                view(worker, block);
            }
        }
        LensMsg::Finish { block, status } => {
            worker.lenses.touch(block);
            finish(worker, block, status);
            view(worker, block);
        }
        LensMsg::Abort { block } => {
            worker.lenses.map.remove(&block);
        }
        LensMsg::Event {
            block,
            pane,
            event: ev,
            rehydrate: record,
        } => {
            if !worker.lenses.has(block) {
                match &record {
                    Some((lens, record)) => rehydrate(worker, block, pane, lens, record),
                    None => return, // the globs no longer claim it: dropped
                }
            }
            worker.lenses.touch(block);
            event(worker, block, ev);
        }
    }
}

/// Runs views that came due; true when any ran.
pub(crate) fn run_due_views(worker: &mut Worker, now: Instant) -> bool {
    let due: Vec<LensId> = worker
        .lenses
        .map
        .iter()
        .filter(|(_, c)| {
            c.dirty && !c.finished && c.last_view.is_some_and(|t| t + VIEW_INTERVAL <= now)
        })
        .map(|(id, _)| *id)
        .collect();
    for block in &due {
        view(worker, *block);
    }
    !due.is_empty()
}
