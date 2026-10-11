//! The per-pane TSP engine: everything a pane's replica knows about the program's surfaces, and
//! the rules for what goes back to the program.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use crate::blob::{self, BlobStore, MemoryBlobs};
use crate::doc::{Effect, OpContext, truthy};
use crate::events::{Delivery, Outgoing, Refused, ViewEvent};
use crate::grid::{AnchorKey, Grid};
use crate::style::{AcceptAll, MAX_CSS, Palette, SheetChecker, valid_sheet_name};
use crate::surface::{Buffer, Mode, State, Surface, SurfaceChanges, SurfaceKey};
use crate::wire::{self, Joined, Joiner, MAX_APC, Message, ToProgram};

/// The T0 kinds.
pub const T0_KINDS: [&str; 16] = [
    "col", "row", "card", "section", "rule", "spacer", "text", "badge", "kv", "icon", "spinner",
    "elapsed", "progress", "ansi", "rows", "el",
];
/// The T1 kinds.
pub const T1_KINDS: [&str; 22] = [
    "md",
    "code",
    "diff",
    "editor",
    "input",
    "status",
    "seg",
    "overlay",
    "toast",
    "shimmer",
    "rate",
    "meter",
    "list",
    "item",
    "tabs",
    "table",
    "tree",
    "kbd",
    "tool",
    "agent",
    "checklist",
    "image",
];
/// The T2 kinds drawn so far, advertised with T0 and T1 (a kind is advertised once it is
/// drawn).
pub const T2_KINDS: &[&str] = &["picker", "prefs", "effort", "chart", "math", "block"];
/// Every terminal feature name of v1.
pub const ALL_FEATURES: [&str; 10] = [
    "blobs",
    "settle",
    "adopt",
    "dock",
    "program-palette",
    "reduce-motion",
    "aside",
    "scroll",
    "styles",
    "flow",
];
/// The width of the settings sheet a docked `prefs` node takes at the pane's right edge, in
/// columns: Tern's 440px sheet at the default 13px font. Kept in
/// columns because the pane's facts carry the cell size in device pixels, not points.
pub const ASIDE_COLS: u16 = 56;

/// The columns a docked `prefs` sheet takes from a pane `cols` wide: [`ASIDE_COLS`], at most 60%
/// of the pane.
pub fn aside_reserve(cols: u16) -> u16 {
    ASIDE_COLS.min((u32::from(cols) * 3 / 5) as u16)
}

/// The default retention budget per pane.
pub const RETENTION: u64 = 64 << 20;

/// Replicated settings: they change only through sequenced records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// TSP on (`native_surfaces`). Off, no hello is answered and every TSP message is ignored.
    pub enabled: bool,
    /// The kinds advertised, and the only ones `add` accepts.
    pub kinds: Vec<String>,
    /// The terminal features advertised.
    pub features: Vec<String>,
    /// The chunk size advertised (`apc`).
    pub apc: u32,
    /// Frames a program may have unacknowledged.
    pub credits: u32,
    /// Retention budget in bytes.
    pub retention: u64,
    /// `term` in the hello reply.
    pub term: String,
    /// `ver` in the hello reply; equals `TERM_PROGRAM_VERSION`.
    pub version: String,
}

impl Config {
    /// The `apc` actually advertised: at least 1, and small enough that a full chunk, with its
    /// prefix and parameters, fits one APC string.
    pub fn chunk_size(&self) -> u32 {
        self.apc.clamp(1, (MAX_APC - 1024) as u32)
    }
}

impl Default for Config {
    fn default() -> Self {
        let kinds: Vec<String> = T0_KINDS
            .iter()
            .chain(&T1_KINDS)
            .chain(T2_KINDS)
            .map(|k| (*k).to_owned())
            .collect();
        // `aside` is the docking of `prefs`: advertised with it.
        let prefs = kinds.iter().any(|k| k == "prefs");
        Config {
            enabled: true,
            kinds,
            features: ALL_FEATURES
                .iter()
                .filter(|f| **f != "aside" || prefs)
                .map(|f| (*f).to_owned())
                .collect(),
            apc: 65_536,
            credits: 2,
            retention: RETENTION,
            term: "auk".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}

/// Client facts: the primary replica's, or the last primary's. Not replicated.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Facts {
    /// Cell size in px.
    pub cell: (f64, f64),
    pub dark: bool,
    pub reduce_motion: bool,
    pub hour12: bool,
    /// Whether the pane is shown.
    pub visible: bool,
}

impl Default for Facts {
    fn default() -> Self {
        Facts {
            cell: (8.0, 16.0),
            dark: true,
            reduce_motion: false,
            hour12: false,
            visible: true,
        }
    }
}

/// Whether answers reach the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Gate {
    /// Answers go out.
    Open,
    /// The program is known to be gone (a prompt, exit or RIS) until its next `q` or `o`.
    Gone,
    /// A `listen:false` `o`: no answers until the next `q` or listening `o`.
    Muted,
}

impl Gate {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Gate::Open => "open",
            Gate::Gone => "gone",
            Gate::Muted => "muted",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "open" => Gate::Open,
            "gone" => Gate::Gone,
            "muted" => Gate::Muted,
            _ => return None,
        })
    }
}

/// The program's last hello.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hello {
    pub app: Option<String>,
    /// Program features: `edit`, `undo`, `send`; unknown names kept.
    pub features: Vec<String>,
}

impl Hello {
    pub fn has(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }
}

/// Who names the pane.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TitleSource {
    /// The grid's title (OSC 0/2, usually the shell's).
    Grid,
    /// A command's listening surface dropped the shell's title; `o.title`, if any, names the pane
    /// until the program sends OSC 0/2.
    Surface(Option<String>),
}

/// Everything views need to update since the last drain.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Surfaces that changed, in pane order.
    pub surfaces: Vec<SurfaceChanges>,
    /// Surfaces that ended.
    pub removed: Vec<SurfaceKey>,
    /// Blobs that arrived; nodes naming them redraw.
    pub blobs: Vec<String>,
    /// The program hello, gate or title changed.
    pub pane: bool,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.surfaces.is_empty() && self.removed.is_empty() && self.blobs.is_empty() && !self.pane
    }
}

/// A `send` waiting for its field to become sendable.
#[derive(Clone, Debug)]
struct HeldSend {
    surface: SurfaceKey,
    id: String,
    text: String,
}

/// State a snapshot's seeded hello carries, applied at the end of the replay.
#[derive(Clone, Debug, Default)]
pub(crate) struct Seed {
    pub swallow: Option<(String, String)>,
    /// Each rebuilt surface's place in pane order (the reserved `at` parameter). Closed surfaces
    /// are replayed first, so the replay's own order can differ from the pane's.
    pub order: Vec<(SurfaceKey, u64)>,
}

/// Document state held outside the TSP engine that counts against the pane's retention budget:
/// a replica's finished command block, by its anchor row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outside {
    pub anchor: AnchorKey,
    /// The replica's id for it, reported back when it is evicted.
    pub id: u64,
    pub cost: u64,
}

/// The TSP side of one pane.
#[derive(Debug)]
pub struct TspPane {
    pub(crate) config: Config,
    pub(crate) kinds: BTreeSet<String>,
    facts: Facts,
    checker: Box<dyn SheetChecker>,
    blobs: Box<dyn BlobStore>,
    pub(crate) surfaces: Vec<Surface>,
    next_surface: u64,
    pub(crate) next_anchor: u64,
    pub(crate) gate: Gate,
    pub(crate) hello: Option<Hello>,
    pub(crate) title: TitleSource,
    /// A listening surface has opened since the last prompt (the title rule).
    pub(crate) command_seen: bool,
    pub(crate) joiner: Joiner,
    pub(crate) replaying: bool,
    replay_target: Option<SurfaceKey>,
    pub(crate) seed: Seed,
    output: Vec<Outgoing>,
    removed: Vec<SurfaceKey>,
    arrived: Vec<String>,
    pane_changed: bool,
    held: Vec<HeldSend>,
    /// Finished command blocks, oldest first: evictable like closed surfaces.
    outside: Vec<Outside>,
    /// Command blocks the budget evicted, for the replica to drop.
    outside_evicted: Vec<u64>,
}

impl TspPane {
    /// An engine with the in-memory blob store and no CSS checks.
    pub fn new(config: Config) -> Self {
        Self::with(
            config,
            Box::new(AcceptAll),
            Box::new(MemoryBlobs::default()),
        )
    }

    /// An engine with the UI engine's CSS checker and the daemon's blob store.
    pub fn with(config: Config, checker: Box<dyn SheetChecker>, blobs: Box<dyn BlobStore>) -> Self {
        let kinds = config.kinds.iter().cloned().collect();
        TspPane {
            config,
            kinds,
            facts: Facts::default(),
            checker,
            blobs,
            surfaces: Vec::new(),
            next_surface: 1,
            next_anchor: 1,
            gate: Gate::Open,
            hello: None,
            title: TitleSource::Grid,
            command_seen: false,
            joiner: Joiner::new(),
            replaying: false,
            replay_target: None,
            seed: Seed::default(),
            output: Vec::new(),
            removed: Vec::new(),
            arrived: Vec::new(),
            pane_changed: false,
            held: Vec::new(),
            outside: Vec::new(),
            outside_evicted: Vec::new(),
        }
    }

    // ---------------------------------------------------------------------------------------
    // Reading

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn facts(&self) -> &Facts {
        &self.facts
    }

    /// Surfaces in pane order (oldest first).
    pub fn surfaces(&self) -> &[Surface] {
        &self.surfaces
    }

    pub fn surface(&self, key: SurfaceKey) -> Option<&Surface> {
        self.surfaces.iter().find(|s| s.key == key)
    }

    /// The first live surface with `id`, the active buffer's first (how `f` and `x` route).
    pub fn find(&self, id: &str, alt: bool) -> Option<&Surface> {
        self.route(id, alt).map(|i| &self.surfaces[i])
    }

    /// Whether something covers this surface's `main` and `dock`: a live screen surface in the
    /// same buffer (over an inline or flow surface), or a sheet in its own `layer`: a `modal`
    /// or `size: "full"` overlay, or a `picker` (`.sf-covered`).
    pub fn covered(&self, key: SurfaceKey) -> bool {
        let Some(s) = self.surface(key) else {
            return false;
        };
        let under_screen = s.mode != Mode::Screen
            && self
                .surfaces
                .iter()
                .any(|o| o.is_live() && o.mode == Mode::Screen && o.buffer == s.buffer);
        under_screen || s.document().region("layer").is_some_and(holds_sheet)
    }

    /// The program's hello, until the program is gone.
    pub fn hello(&self) -> Option<&Hello> {
        self.hello.as_ref()
    }

    /// The pane's program name (`data-app`, tab icon): the hello's `app`, while a listening
    /// surface is open.
    pub fn app(&self) -> Option<&str> {
        let listening = self.surfaces.iter().any(|s| s.is_live() && s.listen);
        self.hello
            .as_ref()
            .and_then(|h| h.app.as_deref())
            .filter(|_| listening)
    }

    pub fn title(&self) -> &TitleSource {
        &self.title
    }

    pub fn gate(&self) -> Gate {
        self.gate
    }

    pub fn blobs(&self) -> &dyn BlobStore {
        self.blobs.as_ref()
    }

    /// A blob that reached this replica outside the stream: the daemon sends a late
    /// window the blobs its documents name, since snapshots don't carry them. Blob bytes are
    /// storage, not replicated state (the hash leaves them out), so this changes nothing a replica
    /// must agree on; nodes naming the blob redraw. Returns false, storing nothing, when `id`
    /// isn't the SHA-256 of `bytes` or the blob is over [`blob::MAX_BLOB`].
    pub fn deliver_blob(&mut self, id: &str, mime: Option<&str>, bytes: Vec<u8>) -> bool {
        if bytes.len() > blob::MAX_BLOB || !blob::blob_id(&bytes).eq_ignore_ascii_case(id) {
            return false;
        }
        let blob = blob::Blob {
            id: id.to_ascii_lowercase(),
            mime: mime.map(str::to_owned),
            bytes,
        };
        let id = blob.id.clone();
        let surfaces = &self.surfaces;
        self.blobs.put(blob, &|id| names_blob(surfaces, id));
        self.arrived.push(id);
        true
    }

    /// Blob ids that `image` nodes name, lowercase: the union of the documents' tracked sets,
    /// so it costs the number of images, not the documents' size.
    pub fn referenced_blobs(&self) -> BTreeSet<String> {
        self.surfaces
            .iter()
            .flat_map(|s| s.doc.blob_refs())
            .map(str::to_owned)
            .collect()
    }

    /// Blob ids that `image` nodes name and this replica doesn't hold: a window asks the daemon
    /// for them.
    pub fn missing_blobs(&self) -> Vec<String> {
        self.referenced_blobs()
            .into_iter()
            .filter(|id| !self.blobs.has(id))
            .collect()
    }

    /// The pane's retention estimate in bytes: its documents, plus the finished command blocks
    /// the replica reported ([`set_outside`](Self::set_outside)).
    pub fn retention_cost(&self) -> u64 {
        self.surfaces.iter().map(Surface::cost).sum::<u64>()
            + self.outside.iter().map(|o| o.cost).sum::<u64>()
    }

    /// The replica's finished command blocks, which share the retention budget. Over
    /// budget, they are evicted whole with the closed surfaces, oldest (highest anchor row)
    /// first; [`take_outside_evicted`](Self::take_outside_evicted) says which went.
    pub fn set_outside(&mut self, grid: &mut dyn Grid, outside: Vec<Outside>) {
        self.outside = outside;
        self.enforce_budget(grid);
    }

    /// The command blocks the budget evicted since the last call.
    pub fn take_outside_evicted(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.outside_evicted)
    }

    /// Takes the messages for the program, in order. The daemon writes them to the PTY subject
    /// to the mode gate; other replicas drop them.
    pub fn take_output(&mut self) -> Vec<Outgoing> {
        std::mem::take(&mut self.output)
    }

    /// Takes what changed since the last call.
    pub fn take_changes(&mut self) -> Changes {
        Changes {
            surfaces: self
                .surfaces
                .iter_mut()
                .filter_map(Surface::take_changes)
                .collect(),
            removed: std::mem::take(&mut self.removed),
            blobs: std::mem::take(&mut self.arrived),
            pane: std::mem::take(&mut self.pane_changed),
        }
    }

    // ---------------------------------------------------------------------------------------
    // Records and client input

    /// A sequenced settings record. A lower budget evicts at once.
    pub fn set_config(&mut self, grid: &mut dyn Grid, config: Config) {
        self.kinds = config.kinds.iter().cloned().collect();
        self.config = config;
        self.enforce_budget(grid);
    }

    /// New client facts: reports `theme`, `motion` (feature `reduce-motion`), `resize`
    /// and `visible` to the program as they change.
    pub fn set_facts(&mut self, grid: &dyn Grid, facts: Facts) {
        let old = std::mem::replace(&mut self.facts, facts);
        if old.dark != facts.dark {
            self.originate(json!({"ev": "theme", "dark": facts.dark}), None);
        }
        if old.reduce_motion != facts.reduce_motion && self.has_feature("reduce-motion") {
            self.originate(json!({"ev": "motion", "reduce": facts.reduce_motion}), None);
        }
        if old.cell != facts.cell {
            self.report_size(grid);
        }
        if old.visible != facts.visible {
            for i in self.view_targets(grid.alt_screen()) {
                let body =
                    json!({"ev": "visible", "sf": self.surfaces[i].id, "visible": facts.visible});
                self.originate(body, Some(i));
            }
        }
    }

    /// The primary drew surface `sf` up to frame `s`. Sends the first-draw `resize`, then
    /// one `ack` per displayed frame.
    pub fn ack(&mut self, grid: &dyn Grid, sf: &str, s: u64) {
        let Some(i) = self.route(sf, grid.alt_screen()) else {
            return;
        };
        if !self.surfaces[i].listen {
            return;
        }
        if self.surfaces[i].delivery.reported.is_none() {
            let cols = self.program_cols(grid);
            self.send_resize(i, cols);
        }
        let surface = &mut self.surfaces[i];
        let mark = (s, surface.delivery.applied);
        if surface.delivery.acked == Some(mark) {
            return;
        }
        surface.delivery.acked = Some(mark);
        let body = json!({"ev": "ack", "sf": surface.id, "s": s});
        self.originate(body, Some(i));
    }

    /// The pane was resized (a record): reports the new width to drawn surfaces.
    pub fn on_resize(&mut self, grid: &dyn Grid) {
        self.report_size(grid);
    }

    /// An event a client built from the view.
    pub fn view_event(
        &mut self,
        grid: &dyn Grid,
        sf: &str,
        event: ViewEvent,
    ) -> Result<Delivery, Refused> {
        let i = self
            .route(sf, grid.alt_screen())
            .ok_or(Refused::NotListening)?;
        let surface = &self.surfaces[i];
        if !surface.listen {
            return Err(Refused::NotListening);
        }
        let node = surface.doc.get(event.id()).ok_or(Refused::UnknownNode)?;
        let hello = self.hello.as_ref();
        if !event
            .feature()
            .iter()
            .all(|f| hello.is_some_and(|h| h.has(f)))
        {
            return Err(Refused::Feature);
        }
        let field = matches!(node.kind(), "editor" | "input");
        let event = match event {
            ViewEvent::Edit { .. } | ViewEvent::Undo { .. } => {
                if !field || node.prop("mode").is_some() || truthy(node.prop("readonly")) {
                    return Err(Refused::Field);
                }
                event
            }
            ViewEvent::Send { id, text } => {
                if !field {
                    return Err(Refused::Field);
                }
                if !sendable(surface, &id) {
                    let surface = surface.key;
                    self.held.push(HeldSend { surface, id, text });
                    return Ok(Delivery::Held);
                }
                ViewEvent::Send { id, text }
            }
            ViewEvent::Toggle {
                id,
                collapsed,
                key: None,
            } => {
                let key = node.prop("key").cloned();
                ViewEvent::Toggle { id, collapsed, key }
            }
            other => other,
        };
        let body = event.body(sf);
        self.originate(body, Some(i));
        Ok(Delivery::Sent)
    }

    /// The bytes joined so far when a chunk sequence is being joined. The daemon arms its 1 s
    /// timeout while this is `Some`, and restarts it whenever a chunk arrives.
    pub fn chunk_pending(&self) -> Option<usize> {
        self.joiner.pending().map(|seq| seq.body.len())
    }

    /// The daemon's chunk timeout fired (a record): drops the sequence being joined.
    pub fn expire_chunks(&mut self) {
        if let Some(msg) = self.joiner.expire() {
            self.answer(json!({"ev": "error", "msg": msg}), None);
        }
    }

    // ---------------------------------------------------------------------------------------
    // Hooks from the grid

    /// OSC 133 `A`: discards inline and screen surfaces, kept ones included, undoing an
    /// inline anchor row with nothing below it; closes live flow surfaces. The program is gone.
    ///
    /// Inline surfaces restored after a daemon restart stay (our rule, not Tern's): the rule
    /// discards only inline surfaces opened since the previous prompt.
    pub fn on_prompt(&mut self, grid: &mut dyn Grid) {
        for key in self.keys() {
            let Some(i) = self.index(key) else { continue };
            match self.surfaces[i].mode {
                Mode::Inline if self.surfaces[i].restored => {}
                Mode::Inline => {
                    if let Some(anchor) = self.surfaces[i].anchor
                        && grid.undo_anchor(anchor)
                    {
                        self.surfaces[i].anchor = None;
                    }
                    self.discard(grid, i);
                }
                Mode::Screen => self.discard(grid, i),
                Mode::Flow if self.surfaces[i].is_live() => self.close_kept(grid, i),
                Mode::Flow => {}
            }
        }
        self.program_gone();
        self.title = TitleSource::Grid;
        self.command_seen = false;
    }

    /// The pane's process exited: inline and flow surfaces close as if kept, screen surfaces go.
    pub fn on_exit(&mut self, grid: &mut dyn Grid) {
        for key in self.keys() {
            let Some(i) = self.index(key) else { continue };
            if !self.surfaces[i].is_live() {
                continue;
            }
            match self.surfaces[i].mode {
                Mode::Screen => self.discard(grid, i),
                Mode::Inline | Mode::Flow => self.close_kept(grid, i),
            }
        }
        self.program_gone();
    }

    /// The pane came back from the state file after a daemon restart: inline and flow
    /// surfaces close as if kept and become restored (read-only, and past the prompt rule); screen
    /// surfaces go; a chunk sequence being joined is dropped; the program is gone.
    pub fn on_restore(&mut self, grid: &mut dyn Grid) {
        for key in self.keys() {
            let Some(i) = self.index(key) else { continue };
            match self.surfaces[i].mode {
                Mode::Screen => {
                    self.discard(grid, i);
                    continue;
                }
                Mode::Inline | Mode::Flow if self.surfaces[i].is_live() => self.close_kept(grid, i),
                Mode::Inline | Mode::Flow => {}
            }
            let s = &mut self.surfaces[i];
            s.restored = true;
            s.flags.meta = true;
        }
        self.joiner.reset();
        self.program_gone();
        self.title = TitleSource::Grid;
        self.command_seen = false;
    }

    /// RIS: clears every surface and drops a chunk sequence silently.
    pub fn on_reset(&mut self, grid: &mut dyn Grid) {
        while !self.surfaces.is_empty() {
            self.discard(grid, 0);
        }
        self.joiner.reset();
        self.program_gone();
        self.title = TitleSource::Grid;
        self.command_seen = false;
    }

    /// The alternate screen was entered or left. Leaving ends the screen surfaces opened on it.
    pub fn on_alt_screen(&mut self, grid: &mut dyn Grid, entered: bool) {
        if !entered {
            self.end_alt_surfaces(grid);
        }
    }

    /// The active screen was cleared (`ED 2`/`ED 3`). On the alternate screen this ends the screen
    /// surfaces opened there.
    pub fn on_screen_cleared(&mut self, grid: &mut dyn Grid) {
        if grid.alt_screen() {
            self.end_alt_surfaces(grid);
        }
    }

    /// An anchor row ended: its surface ends, and a listening program hears `gone`.
    pub fn on_anchor_gone(&mut self, grid: &mut dyn Grid, key: AnchorKey) {
        if let Some(i) = self.surfaces.iter().position(|s| s.anchor == Some(key)) {
            self.surfaces[i].anchor = None;
            self.end_with_gone(grid, i);
        }
    }

    /// The program set the title with OSC 0/2; it names the pane again.
    pub fn on_title(&mut self) {
        if self.title != TitleSource::Grid {
            self.title = TitleSource::Grid;
            self.pane_changed = true;
        }
    }

    // ---------------------------------------------------------------------------------------
    // Program output

    /// One APC string from the program's output: the bytes after `ESC _`, as
    /// `auk_term::Event::Tsp` delivers them. Non-TSP strings are ignored.
    pub fn on_apc(&mut self, grid: &mut dyn Grid, payload: &[u8]) {
        if (!self.config.enabled && !self.replaying) || payload.len() > MAX_APC {
            return;
        }
        let Some(msg) = wire::parse(payload) else {
            return;
        };
        for joined in self.joiner.push(msg) {
            match joined {
                Joined::Dropped(msg) => self.answer(json!({"ev": "error", "msg": msg}), None),
                Joined::Message(msg) => self.handle(grid, msg),
            }
        }
    }

    fn handle(&mut self, grid: &mut dyn Grid, msg: Message) {
        let verb = msg.verb.as_str();
        if !matches!(verb, "q" | "o" | "f" | "b" | "t" | "s" | "x") {
            return;
        }
        if verb == "b" {
            self.blob(&msg);
            return;
        }
        if verb == "q" && !self.seeded(&msg) {
            self.set_gate(Gate::Open);
        }
        let body = match serde_json::from_slice::<Value>(&msg.body) {
            Ok(Value::Object(body)) => body,
            Ok(_) => return self.malformed(verb, "expected a JSON object"),
            Err(e) => return self.malformed(verb, &e.to_string()),
        };
        match verb {
            "q" => self.query(grid.cols(), &msg, &body),
            "o" => self.open(grid, &msg, &body),
            "f" => self.frame(grid, &body),
            "t" => self.set_palette(grid, &body),
            "s" => self.set_sheet(grid, &body),
            "x" => self.close(grid, &body),
            _ => unreachable!("verbs filtered above"),
        }
    }

    fn malformed(&mut self, verb: &str, why: &str) {
        if verb == "o" && !self.replaying {
            // An `o` is the program speaking, even a broken one.
            self.set_gate(Gate::Open);
        }
        self.answer(
            json!({"ev": "error", "msg": format!("malformed {verb} body: {why}")}),
            None,
        );
    }

    /// Whether `msg` is a snapshot's synthetic message (only honored during replay).
    fn seeded(&self, msg: &Message) -> bool {
        self.replaying && msg.param("seeded") == Some("1")
    }

    fn query(&mut self, cols: u16, msg: &Message, body: &Map<String, Value>) {
        match body.get("q").and_then(Value::as_str) {
            Some("hello") => {
                let v1 = body
                    .get("v")
                    .and_then(Value::as_array)
                    .is_some_and(|v| v.iter().any(|x| x.as_u64() == Some(1)));
                if !v1 {
                    return;
                }
                let hello = Hello {
                    app: body.get("app").and_then(Value::as_str).map(str::to_owned),
                    features: body
                        .get("features")
                        .and_then(Value::as_array)
                        .map(|f| {
                            f.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default(),
                };
                if self.seeded(msg) {
                    self.apply_seed(body, hello);
                    return;
                }
                self.hello = Some(hello);
                self.pane_changed = true;
                let reply = self.hello_reply(cols);
                self.reply(reply);
            }
            Some("blobs") if self.has_feature("blobs") => {
                let ids: Vec<&str> = body
                    .get("ids")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                let mut have = Vec::new();
                for id in ids {
                    if self.blobs.has(id) {
                        self.blobs.touch(id);
                        have.push(Value::String(id.to_owned()));
                    }
                }
                self.reply(json!({"r": "blobs", "have": have}));
            }
            _ => {}
        }
    }

    /// The hello reply: every field, always, with exact JSON types.
    pub fn hello_reply(&self, cols: u16) -> Value {
        json!({
            "r": "hello",
            "v": 1,
            "term": self.config.term,
            "ver": self.config.version,
            "kinds": self.config.kinds,
            "features": self.config.features,
            "apc": self.config.chunk_size(),
            "credits": self.config.credits.max(1),
            "cols": cols,
            "cell": {"w": px(self.facts.cell.0), "h": px(self.facts.cell.1)},
            "dark": self.facts.dark,
            "reduceMotion": self.facts.reduce_motion,
            "hour12": self.facts.hour12,
        })
    }

    fn open(&mut self, grid: &mut dyn Grid, msg: &Message, body: &Map<String, Value>) {
        let seeded = self.seeded(msg);
        let listen = body.get("listen") != Some(&Value::Bool(false));
        if !seeded {
            self.set_gate(if listen { Gate::Open } else { Gate::Muted });
        }
        let id = match body.get("id") {
            Some(Value::String(id)) if !id.is_empty() => id.clone(),
            _ => return self.answer(json!({"ev": "error", "msg": "o without an id"}), None),
        };
        // `adopt` reopens an inline surface; its `mode` is ignored.
        let adopt =
            !seeded && body.get("adopt") == Some(&Value::Bool(true)) && self.has_feature("adopt");
        let mode = match body.get("mode") {
            _ if adopt => Some(Mode::Inline),
            None | Some(Value::Null) => Some(Mode::Inline),
            Some(Value::String(m)) => Mode::parse(m),
            Some(_) => None,
        };
        let Some(mode) = mode.filter(|m| seeded || *m != Mode::Flow || self.has_feature("flow"))
        else {
            let mode = body.get("mode").map(Value::to_string).unwrap_or_default();
            return self.answer(
                json!({"ev": "error", "sf": id, "msg": format!("unknown mode {mode}")}),
                None,
            );
        };
        let title = body.get("title").and_then(Value::as_str).map(str::to_owned);
        let role = body.get("role").and_then(Value::as_str).map(str::to_owned);
        let alt = if seeded {
            msg.param("buf") == Some("alt")
        } else {
            grid.alt_screen()
        };
        if mode != Mode::Screen && alt {
            let msg = format!(
                "{} surfaces can't open on the alternate screen",
                mode.as_str()
            );
            return self.answer(json!({"ev": "error", "sf": id, "msg": msg}), None);
        }
        if adopt {
            return self.adopt(grid, id, title, role, listen);
        }
        let buffer = Buffer::from_alt(alt);
        if !seeded {
            self.replace(grid, mode, buffer);
        }
        let anchor = if mode == Mode::Screen {
            None
        } else if seeded {
            msg.param("key").and_then(|k| k.parse().ok()).map(AnchorKey)
        } else {
            let key = AnchorKey(self.next_anchor);
            self.next_anchor += 1;
            if let Err(e) = grid.open_anchor(key) {
                return self.answer(
                    json!({"ev": "error", "sf": id, "msg": format!("can't anchor {id}: {e:?}")}),
                    None,
                );
            }
            Some(key)
        };
        if !seeded && listen {
            self.name_pane(title.clone());
        }
        let key = SurfaceKey(self.next_surface);
        self.next_surface += 1;
        let mut surface = Surface::new(key, id, mode, buffer, listen, title, role, anchor);
        surface.restored = seeded && msg.param("restored") == Some("1");
        self.surfaces.push(surface);
        if seeded {
            self.replay_target = Some(key);
            if let Some(at) = msg.param("at").and_then(|a| a.parse().ok()) {
                self.seed.order.push((key, at));
            }
        }
    }

    /// Replacement and stacking: a new `o` replaces the live surface of its mode in its
    /// buffer, and a new inline `o` also discards a screen surface covering the buffer.
    fn replace(&mut self, grid: &mut dyn Grid, mode: Mode, buffer: Buffer) {
        for key in self.keys() {
            let Some(i) = self.index(key) else { continue };
            let s = &self.surfaces[i];
            if !s.is_live() || s.buffer != buffer {
                continue;
            }
            if s.mode == mode {
                match mode {
                    Mode::Inline | Mode::Flow => self.close_kept(grid, i),
                    Mode::Screen => self.discard(grid, i),
                }
            } else if mode == Mode::Inline && s.mode == Mode::Screen {
                self.discard(grid, i);
            }
        }
    }

    /// The title rule: a command's first listening surface drops the shell's title, and
    /// `o.title` names the pane until the program sends OSC 0/2.
    fn name_pane(&mut self, title: Option<String>) {
        let next = match (title, self.command_seen) {
            (Some(t), _) => TitleSource::Surface(Some(t)),
            (None, false) => TitleSource::Surface(None),
            (None, true) => return,
        };
        self.command_seen = true;
        if self.title != next {
            self.title = next;
            self.pane_changed = true;
        }
    }

    fn adopt(
        &mut self,
        grid: &mut dyn Grid,
        id: String,
        title: Option<String>,
        role: Option<String>,
        listen: bool,
    ) {
        if self
            .surfaces
            .iter()
            .any(|s| s.is_live() && s.mode == Mode::Inline && s.id == id)
        {
            return;
        }
        let Some(target) = self
            .surfaces
            .iter()
            .rev()
            .find(|s| s.state == State::Closed && s.mode == Mode::Inline && s.id == id)
            .map(|s| s.key)
        else {
            return self.answer(json!({"ev": "gone", "ids": [id]}), None);
        };
        self.replace(grid, Mode::Inline, Buffer::Main);
        let i = self
            .index(target)
            .expect("closed surfaces survive replacement");
        let s = &mut self.surfaces[i];
        s.state = State::Live;
        s.listen = listen;
        if title.is_some() {
            s.title = title.clone();
        }
        if role.is_some() {
            s.role = role;
        }
        s.delivery = Default::default();
        s.flags.meta = true;
        if listen {
            self.name_pane(title);
        }
    }

    fn frame(&mut self, grid: &mut dyn Grid, body: &Map<String, Value>) {
        let s = body.get("s").and_then(Value::as_u64);
        let with_s = |mut v: Value| {
            if let Some(s) = s {
                v["s"] = s.into();
            }
            v
        };
        let Some(sf) = body.get("sf").and_then(Value::as_str) else {
            return self.answer(
                with_s(json!({"ev": "error", "msg": "frame without sf"})),
                None,
            );
        };
        let Some(i) = self.target(sf, grid.alt_screen()) else {
            let closed = self.surfaces.iter().rev().find(|x| x.id == sf);
            if closed.is_some_and(|c| !c.listen) {
                return;
            }
            let msg = if closed.is_some() {
                format!("surface {sf} is closed")
            } else {
                format!("unknown surface {sf}")
            };
            return self.answer(with_s(json!({"ev": "error", "sf": sf, "msg": msg})), None);
        };
        let Some(ops) = body.get("ops").and_then(Value::as_array) else {
            return self.answer(
                with_s(json!({"ev": "error", "sf": sf, "msg": "ops must be an array"})),
                Some(i),
            );
        };
        let kinds = (!self.replaying).then_some(&self.kinds);
        let ctx = OpContext { kinds };
        let mut errors = Vec::new();
        let surface = &mut self.surfaces[i];
        for (n, op) in ops.iter().enumerate() {
            match surface.doc.apply(op, &ctx) {
                Ok(Effect::None) => {}
                Ok(Effect::Suspend) | Ok(Effect::Resume) => {
                    let suspend = op[0] == "suspend";
                    if surface.suspended != suspend {
                        surface.suspended = suspend;
                        surface.flags.meta = true;
                    }
                }
                Err(msg) => errors.push(with_s(
                    json!({"ev": "error", "sf": sf, "op": n, "msg": msg}),
                )),
            }
            // An `el` whose tag isn't built draws as a `div`, and the program hears once per
            // node version.
            for (id, msg) in surface.doc.take_tag_errors() {
                errors.push(json!({"ev": "error", "sf": sf, "id": id, "msg": msg}));
            }
        }
        if s.is_some() {
            surface.last_s = s;
        }
        surface.delivery.applied += 1;
        for e in errors {
            self.answer(e, Some(i));
        }
        self.release_held(i);
        self.enforce_budget(grid);
        // A `prefs` sheet docking or leaving narrows or widens the program's columns (aside).
        self.report_size(grid);
    }

    fn close(&mut self, grid: &mut dyn Grid, body: &Map<String, Value>) {
        let Some(id) = body
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            return self.answer(json!({"ev": "error", "msg": "x without an id"}), None);
        };
        let keep = body.get("keep") != Some(&Value::Bool(false));
        match self.target(id, grid.alt_screen()) {
            Some(i) if keep && self.surfaces[i].mode != Mode::Screen => self.close_kept(grid, i),
            Some(i) => self.discard(grid, i),
            None => match self.surfaces.iter().rposition(|s| s.id == id) {
                Some(i) if !keep => self.discard(grid, i),
                Some(_) => {}
                None => self.answer(
                    json!({"ev": "error", "sf": id, "msg": format!("unknown surface {id}")}),
                    None,
                ),
            },
        }
        self.enforce_budget(grid);
        self.report_size(grid);
    }

    fn blob(&mut self, msg: &Message) {
        match blob::decode(msg.param("id"), msg.param("mime"), &msg.body) {
            Ok(blob) => {
                let id = blob.id.clone();
                let surfaces = &self.surfaces;
                self.blobs.put(blob, &|id| names_blob(surfaces, id));
                self.arrived.push(id);
            }
            Err(e) => self.answer(json!({"ev": "error", "msg": e}), None),
        }
    }

    /// The surface a `t` or `s` names (`sf`), or the live surface.
    fn style_target(&mut self, alt: bool, body: &Map<String, Value>) -> Option<usize> {
        match body.get("sf").and_then(Value::as_str) {
            Some(sf) => {
                let found = self.target(sf, alt);
                if found.is_none() {
                    self.answer(
                        json!({"ev": "error", "sf": sf, "msg": format!("unknown surface {sf}")}),
                        None,
                    );
                }
                found
            }
            None => {
                let found = self.front(alt);
                if found.is_none() {
                    self.answer(json!({"ev": "error", "msg": "no live surface"}), None);
                }
                found
            }
        }
    }

    fn set_palette(&mut self, grid: &mut dyn Grid, body: &Map<String, Value>) {
        let Some(i) = self.style_target(grid.alt_screen(), body) else {
            return;
        };
        let surface = &mut self.surfaces[i];
        surface.palette = Some(Palette::parse(body));
        surface.flags.palette = true;
        self.enforce_budget(grid);
    }

    fn set_sheet(&mut self, grid: &mut dyn Grid, body: &Map<String, Value>) {
        let Some(i) = self.style_target(grid.alt_screen(), body) else {
            return;
        };
        let sf = self.surfaces[i].id.clone();
        let name = body.get("name").and_then(Value::as_str).unwrap_or_default();
        if !valid_sheet_name(name) {
            let name = body.get("name").cloned().unwrap_or(Value::Null);
            let msg = "a sheet name is 1-64 of [A-Za-z0-9_-]";
            return self.answer(
                json!({"ev": "error", "sf": sf, "sheet": name, "msg": msg}),
                Some(i),
            );
        }
        let css = match body.get("css") {
            None | Some(Value::Null) => "",
            Some(Value::String(css)) => css.as_str(),
            Some(_) => {
                return self.answer(
                    json!({"ev": "error", "sf": sf, "sheet": name, "msg": "css must be a string"}),
                    Some(i),
                );
            }
        };
        let sheets = &self.surfaces[i].sheets;
        if css.is_empty() {
            if self.surfaces[i].sheets.remove(name) {
                self.surfaces[i].flags.sheets = true;
            }
            return self.enforce_budget(grid);
        }
        if sheets.bytes_with(name, css) > MAX_CSS {
            let msg = format!("over {MAX_CSS} bytes of CSS");
            return self.answer(
                json!({"ev": "error", "sf": sf, "sheet": name, "msg": msg}),
                Some(i),
            );
        }
        let messages = if self.replaying {
            Vec::new()
        } else {
            self.checker.check(css)
        };
        let surface = &mut self.surfaces[i];
        surface.sheets.set(name, css.to_owned());
        surface.flags.sheets = true;
        if !messages.is_empty() {
            self.answer(
                json!({"ev": "error", "sf": sf, "sheet": name, "msg": messages.join("; ")}),
                Some(i),
            );
        }
        self.enforce_budget(grid);
    }

    // ---------------------------------------------------------------------------------------
    // Surface lifetimes

    fn keys(&self) -> Vec<SurfaceKey> {
        self.surfaces.iter().map(|s| s.key).collect()
    }

    fn index(&self, key: SurfaceKey) -> Option<usize> {
        self.surfaces.iter().position(|s| s.key == key)
    }

    /// The first live surface with `id`, the active buffer's first.
    fn route(&self, id: &str, alt: bool) -> Option<usize> {
        let active = Buffer::from_alt(alt);
        let live = |s: &Surface| s.is_live() && s.id == id;
        self.surfaces
            .iter()
            .position(|s| live(s) && s.buffer == active)
            .or_else(|| self.surfaces.iter().position(live))
    }

    /// Routing for `f`, `t`, `s` and `x`: during replay, the surface just rebuilt comes first.
    fn target(&self, id: &str, alt: bool) -> Option<usize> {
        if self.replaying
            && let Some(key) = self.replay_target
            && let Some(i) = self.index(key)
            && self.surfaces[i].id == id
            && self.surfaces[i].is_live()
        {
            return Some(i);
        }
        self.route(id, alt)
    }

    /// The live surface on the active buffer: the newest.
    fn front(&self, alt: bool) -> Option<usize> {
        if self.replaying
            && let Some(i) = self.replay_target.and_then(|k| self.index(k))
        {
            return Some(i);
        }
        let active = Buffer::from_alt(alt);
        self.surfaces
            .iter()
            .rposition(|s| s.is_live() && s.buffer == active)
    }

    /// Live, listening surfaces the pane shows: a screen surface on the active buffer hides the
    /// others there.
    fn view_targets(&self, alt: bool) -> Vec<usize> {
        let active = Buffer::from_alt(alt);
        let shown = |s: &Surface| s.is_live() && s.listen && s.buffer == active;
        if let Some(i) = self
            .surfaces
            .iter()
            .rposition(|s| shown(s) && s.mode == Mode::Screen)
        {
            return vec![i];
        }
        (0..self.surfaces.len())
            .filter(|&i| shown(&self.surfaces[i]))
            .collect()
    }

    /// Closes as if `keep:true`: `main` stays; `dock`, `layer` and focus go. Screen surfaces are
    /// discarded instead.
    fn close_kept(&mut self, grid: &mut dyn Grid, i: usize) {
        if self.surfaces[i].mode == Mode::Screen {
            return self.discard(grid, i);
        }
        let s = &mut self.surfaces[i];
        s.state = State::Closed;
        s.doc.remove_id("dock");
        s.doc.remove_id("layer");
        s.doc.set_focus(None);
        s.flags.meta = true;
        let key = s.key;
        self.held.retain(|h| h.surface != key);
    }

    /// Removes a surface and ends its anchor.
    fn discard(&mut self, grid: &mut dyn Grid, i: usize) {
        let s = self.surfaces.remove(i);
        if let Some(anchor) = s.anchor {
            grid.drop_anchor(anchor);
        }
        self.held.retain(|h| h.surface != s.key);
        if self.replay_target == Some(s.key) {
            self.replay_target = None;
        }
        self.removed.push(s.key);
    }

    /// Ends a surface, telling a listening program (`gone` with `sf`).
    fn end_with_gone(&mut self, grid: &mut dyn Grid, i: usize) {
        let id = self.surfaces[i].id.clone();
        self.answer(json!({"ev": "gone", "sf": id, "ids": [id]}), Some(i));
        self.discard(grid, i);
    }

    fn end_alt_surfaces(&mut self, grid: &mut dyn Grid) {
        while let Some(i) = self.surfaces.iter().position(|s| s.buffer == Buffer::Alt) {
            self.end_with_gone(grid, i);
        }
    }

    fn program_gone(&mut self) {
        self.set_gate(Gate::Gone);
        if self.hello.take().is_some() {
            self.pane_changed = true;
        }
    }

    fn set_gate(&mut self, gate: Gate) {
        if self.gate != gate {
            self.gate = gate;
            self.pane_changed = true;
        }
    }

    /// Retention: evicts closed surfaces oldest first, then the oldest settled top-level
    /// children of `main` in open surfaces, until the pane fits its budget or nothing is left to
    /// evict.
    fn enforce_budget(&mut self, grid: &mut dyn Grid) {
        if self.replaying {
            return;
        }
        while self.retention_cost() > self.config.retention {
            let closed = self.surfaces.iter().position(|s| s.state == State::Closed);
            // A command block older than the oldest closed surface goes first: rows higher up
            // are older; a surface without an anchor row counts as oldest.
            let line = |key: Option<AnchorKey>| key.and_then(|k| grid.anchor_line(k));
            let block_first = match (closed, self.outside.first()) {
                (_, None) => false,
                (None, Some(_)) => true,
                (Some(i), Some(o)) => match (line(self.surfaces[i].anchor), line(Some(o.anchor))) {
                    (Some(s), Some(b)) => b < s,
                    (None, _) => false,
                    (Some(_), None) => true,
                },
            };
            if block_first {
                let o = self.outside.remove(0);
                self.outside_evicted.push(o.id);
                continue;
            }
            if let Some(i) = closed {
                self.discard(grid, i);
                continue;
            }
            let evicted = (0..self.surfaces.len())
                .find_map(|i| self.surfaces[i].doc.evict_settled().map(|id| (i, id)));
            let Some((i, id)) = evicted else { break };
            let sf = self.surfaces[i].id.clone();
            self.answer(json!({"ev": "gone", "sf": sf, "ids": [id]}), Some(i));
        }
    }

    // ---------------------------------------------------------------------------------------
    // Delivery

    fn has_feature(&self, feature: &str) -> bool {
        self.config.features.iter().any(|f| f == feature)
    }

    fn report_size(&mut self, grid: &dyn Grid) {
        let cols = self.program_cols(grid);
        for i in self.view_targets(grid.alt_screen()) {
            let reported = self.surfaces[i].delivery.reported;
            if reported.is_some_and(|r| r != (cols, self.facts.cell.0, self.facts.cell.1)) {
                self.send_resize(i, cols);
            }
        }
    }

    /// The `prefs` node docked beside the pane (feature `aside`): a `prefs` node in
    /// the `layer` of a live inline surface on the active buffer, with no screen surface over it.
    /// Returns the surface and the node's id.
    pub fn aside(&self, alt: bool) -> Option<(SurfaceKey, String)> {
        if !self.has_feature("aside") {
            return None;
        }
        let active = Buffer::from_alt(alt);
        if self
            .surfaces
            .iter()
            .any(|s| s.is_live() && s.mode == Mode::Screen && s.buffer == active)
        {
            return None;
        }
        self.surfaces.iter().rev().find_map(|s| {
            if !(s.is_live() && s.mode == Mode::Inline && s.buffer == active) {
                return None;
            }
            let layer = s.doc.region("layer")?;
            let node = layer.children().find(|c| c.kind() == "prefs")?;
            Some((s.key, node.id().to_owned()))
        })
    }

    /// The width the program lays out at: the grid's, less the docked `prefs` sheet's.
    fn program_cols(&self, grid: &dyn Grid) -> u16 {
        let cols = grid.cols();
        match self.aside(grid.alt_screen()) {
            Some(_) => cols - aside_reserve(cols),
            None => cols,
        }
    }

    fn send_resize(&mut self, i: usize, cols: u16) {
        let (w, h) = self.facts.cell;
        self.surfaces[i].delivery.reported = Some((cols, w, h));
        let body = json!({"ev": "resize", "sf": self.surfaces[i].id, "cols": cols, "cell": {"w": px(w), "h": px(h)}, "visible": self.facts.visible});
        self.originate(body, Some(i));
    }

    /// Sends `send`s whose fields became sendable; drops those whose field or surface went.
    fn release_held(&mut self, i: usize) {
        let key = self.surfaces[i].key;
        let mut ready = Vec::new();
        self.held.retain(|h| {
            if h.surface != key {
                return true;
            }
            let surface = &self.surfaces[i];
            match surface.doc.get(&h.id) {
                None => false,
                Some(_) if sendable(surface, &h.id) => {
                    ready.push(ViewEvent::Send {
                        id: h.id.clone(),
                        text: h.text.clone(),
                    });
                    false
                }
                Some(_) => true,
            }
        });
        let sf = self.surfaces[i].id.clone();
        for event in ready {
            self.originate(event.body(&sf), Some(i));
        }
    }

    /// An answer (`r`, `error`, `gone`): sent unless the program is gone or muted, and never about
    /// a `listen:false` surface.
    fn answer(&mut self, body: Value, about: Option<usize>) {
        if self.replaying
            || self.gate != Gate::Open
            || about.is_some_and(|i| !self.surfaces[i].listen)
        {
            return;
        }
        self.output.push(Outgoing {
            verb: ToProgram::Event,
            body,
        });
    }

    fn reply(&mut self, body: Value) {
        if self.replaying || self.gate != Gate::Open {
            return;
        }
        self.output.push(Outgoing {
            verb: ToProgram::Reply,
            body,
        });
    }

    /// An event the terminal originates: sent only while the surface it names is open and
    /// listening, or, naming none, while some listening surface is open.
    fn originate(&mut self, body: Value, about: Option<usize>) {
        if self.replaying {
            return;
        }
        let ok = match about {
            Some(i) => self.surfaces[i].is_live() && self.surfaces[i].listen,
            None => self.surfaces.iter().any(|s| s.is_live() && s.listen),
        };
        if ok {
            self.output.push(Outgoing {
                verb: ToProgram::Event,
                body,
            });
        }
    }

    // ---------------------------------------------------------------------------------------
    // Snapshot replay

    /// Starts replaying a snapshot: reserved parameters are honored and nothing is
    /// answered until [`end_replay`](Self::end_replay).
    pub fn begin_replay(&mut self) {
        self.replaying = true;
        self.replay_target = None;
        self.seed = Seed::default();
    }

    pub fn end_replay(&mut self) {
        self.replaying = false;
        self.replay_target = None;
        if let Some(swallow) = self.seed.swallow.take() {
            self.joiner.set_swallowing(swallow);
        }
        // Back to pane order: a live surface may precede a closed one (it closed later than
        // one opened after it), and which comes first matters (adoption, retention, the hash).
        let order = std::mem::take(&mut self.seed.order);
        if !order.is_empty() {
            let place = |key: SurfaceKey| {
                order
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map_or(u64::MAX, |&(_, at)| at)
            };
            self.surfaces.sort_by_key(|s| place(s.key));
        }
        for s in &mut self.surfaces {
            s.delivery = Default::default();
        }
    }

    fn apply_seed(&mut self, body: &Map<String, Value>, hello: Hello) {
        let Some(seed) = body.get("auk").and_then(Value::as_object) else {
            return;
        };
        self.hello = seed
            .get("program")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            .then_some(hello);
        if let Some(gate) = seed
            .get("gate")
            .and_then(Value::as_str)
            .and_then(Gate::parse)
        {
            self.gate = gate;
        }
        if let Some(n) = seed.get("anchor").and_then(Value::as_u64) {
            self.next_anchor = n;
        }
        self.command_seen = seed
            .get("command")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.title = match seed.get("title") {
            Some(Value::Object(t)) => {
                TitleSource::Surface(t.get("surface").and_then(Value::as_str).map(str::to_owned))
            }
            _ => TitleSource::Grid,
        };
        self.seed.swallow =
            seed.get("swallow")
                .and_then(Value::as_array)
                .and_then(|a| match a.as_slice() {
                    [Value::String(v), Value::String(c)] => Some((v.clone(), c.clone())),
                    _ => None,
                });
        self.pane_changed = true;
    }
}

/// A number of pixels as JSON: an integer when whole.
fn px(v: f64) -> Value {
    if v.fract() == 0.0 && (0.0..9.0e15).contains(&v) {
        Value::from(v as u64)
    } else {
        Value::from(v)
    }
}

/// A live, editable field reporting `sendable:true`.
fn sendable(surface: &Surface, id: &str) -> bool {
    let Some(node) = surface.doc.get(id) else {
        return false;
    };
    surface.is_live()
        && matches!(node.kind(), "editor" | "input")
        && truthy(node.prop("sendable"))
        && !truthy(node.prop("readonly"))
        && !truthy(node.prop("disabled"))
        && !truthy(node.prop("hidden"))
}

/// Whether an `image` node in any of `surfaces` names `blob` (lowercase).
fn names_blob(surfaces: &[Surface], blob: &str) -> bool {
    surfaces.iter().any(|s| s.doc.names_blob(blob))
}

/// Whether a node holds a sheet that covers the view: a `modal` or full overlay, or a picker.
fn holds_sheet(node: crate::doc::NodeRef) -> bool {
    node.children().any(|c| {
        let sheet = match c.kind() {
            "picker" => true,
            "overlay" => {
                // As the views read `modal`: true, or a nonzero number.
                let modal = match c.prop("modal") {
                    Some(Value::Bool(b)) => *b,
                    Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
                    _ => false,
                };
                modal || c.prop("size").and_then(Value::as_str) == Some("full")
            }
            _ => false,
        };
        sheet || holds_sheet(c)
    })
}
