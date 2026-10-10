//! Command lenses in a replica (§5.3, §9.6, §15.1, §15.2): every replica claims commands on its
//! own, from the catalog the daemon sequences into the stream, and diverts a claimed command's
//! output into a command block (kind `block`) in scrollback instead of the grid.
//!
//! - The claim is made at OSC 133 `C` with [`Catalog::claim`], the runtime-free half of
//!   `auk-plugins`: no Lua and no window half takes part, so every replica decides the same.
//! - The block sits at an anchor row opened at the cursor, like a flow surface (§7.4). It keeps
//!   the line, cwd, raw output, capture width, exit status and the duration the daemon measured
//!   (sequenced as [`Record::LensTook`](crate::Record::LensTook)).
//! - While the command runs, output goes to the capture's scratch screen
//!   ([`auk_plugins::capture::Capture`]); `D` (or a prompt without one) ends it, and the bytes
//!   after the mark go back to the grid. A capture that turns interactive or passes 16 MiB
//!   aborts: the block is dropped and everything captured is replayed into the grid.
//! - The lens view is a side message (§15.1): it is kept with the block and travels in
//!   snapshots, but it isn't stream state and isn't hashed.
//!
//! Only the answering replica (the daemon's) queues [`LensNote`]s for the plugin host.

use auk_plugins::capture::{Capture, LensLine};
use auk_plugins::{Catalog, Claim, LensRun, Preconditions};
use auk_term::AnchorKey;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Anchor keys of command blocks start here, apart from the TSP engine's (which count from 0).
pub const ANCHOR_BASE: u64 = 1 << 48;

/// The prefix of a command block's surface name in view events (`lens:<block>`).
pub const SF_PREFIX: &str = "lens:";

/// A command block in scrollback: a lens-claimed command's output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LensBlock {
    /// The block's number in its pane, the same on every replica.
    pub id: u64,
    /// Its anchor row.
    pub anchor: u64,
    /// The claiming plugin and lens.
    pub plugin: String,
    pub lens: String,
    /// The command line the claim read (alias-expanded when reported), and as typed.
    pub line: String,
    pub typed: String,
    /// The shell's directory (the OSC 7 URL), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The program and arguments the lens sees.
    pub program: String,
    pub args: Vec<String>,
    /// The capture width.
    pub cols: u16,
    /// The raw output (empty in [`LensBlock`]s read while the command runs; see
    /// [`crate::Replica::lens_raw`]).
    #[serde(with = "b64", default)]
    pub raw: Vec<u8>,
    /// The command finished (`D`, a prompt, or the pane's exit).
    pub done: bool,
    /// The exit status from `D`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<i32>,
    /// The duration the daemon measured from `C` to `D`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub took_ms: Option<u64>,
    /// The current lens view (a side message; not stream state). `None` shows the raw output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<Value>,
}

impl LensBlock {
    /// The block's role, `lens.plugin.<plugin>.<lens>` (§9.6, CSS `data-role`).
    pub fn role(&self) -> String {
        format!("lens.plugin.{}.{}", self.plugin, self.lens)
    }

    /// The block's retention cost (§7.10), as the TSP engine estimates a node: a fixed overhead
    /// plus its text. The view, a side message that replicas get at different times, doesn't
    /// count, so every replica evicts the same blocks.
    pub fn cost(&self) -> u64 {
        let text = [
            &self.plugin,
            &self.lens,
            &self.line,
            &self.typed,
            &self.program,
        ]
        .iter()
        .map(|s| s.len())
        .sum::<usize>()
            + self.args.iter().map(String::len).sum::<usize>()
            + self.cwd.as_ref().map_or(0, String::len)
            + self.raw.len();
        auk_tsp::doc::NODE_OVERHEAD + text as u64
    }

    /// The surface name view events use for this block.
    pub fn sf(&self) -> String {
        format!("{SF_PREFIX}{}", self.id)
    }

    /// The block as a `block` node (research/elements.md): its props, then `native` (the lens
    /// view, when there is one) and `raw` (the output as an `ansi` node). Views draw this.
    pub fn node(&self, raw: &[u8]) -> Value {
        let id = format!("lens{}", self.id);
        let mut props = serde_json::json!({
            "cmd": self.program,
            "line": self.line,
            "lens": format!("plugin.{}.{}", self.plugin, self.lens),
            "role": self.role(),
            "state": if self.done { "done" } else { "running" },
            "key": id,
        });
        if let Some(cwd) = &self.cwd {
            props["cwd"] = cwd.as_str().into();
        }
        if let Some(status) = self.status {
            props["exit"] = status.into();
        }
        if let Some(took) = self.took_ms {
            props["took"] = took.into();
        }
        let mut children = Vec::new();
        if let Some(view) = &self.view {
            children.push(serde_json::json!({
                "id": format!("{id}.native"), "k": "col", "p": {}, "c": [view],
            }));
        }
        children.push(serde_json::json!({
            "id": format!("{id}.raw"), "k": "ansi",
            "p": {"text": String::from_utf8_lossy(raw)},
        }));
        serde_json::json!({"id": id, "k": "block", "p": props, "c": children})
    }
}

/// What the answering replica tells the plugin host about command blocks, in stream order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LensNote {
    /// A command was claimed: the lens `open`s a capture.
    Open {
        block: u64,
        claim: Claim,
        run: LensRun,
        cols: u16,
    },
    /// Lines that scrolled off the scratch screen.
    Lines { block: u64, lines: Vec<LensLine> },
    /// The command finished (after its last lines). `status` is `D`'s, when it had one.
    Finish { block: u64, status: Option<i32> },
    /// The capture ended without finishing (it aborted, the pane exited, or its row went): the
    /// lens state is dropped and no `finish` runs.
    Drop { block: u64 },
}

/// The lens part of a snapshot (§15.3): the claim catalog, the blocks with their views, and the
/// running capture's block (whose `raw` then holds the output so far).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LensSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<Catalog>,
    #[serde(default)]
    pub next: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<LensBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running: Option<u64>,
}

impl LensSnapshot {
    pub fn is_empty(&self) -> bool {
        self.catalog.is_none() && self.next == 0 && self.blocks.is_empty()
    }
}

/// A replica's lens state.
#[derive(Debug, Default)]
pub(crate) struct Lenses {
    pub catalog: Option<Catalog>,
    pub next: u64,
    pub blocks: Vec<LensBlock>,
    /// The block whose command runs, and its capture.
    pub running: Option<(u64, Capture)>,
    pub notes: Vec<LensNote>,
    /// The finished blocks last handed to the TSP engine's retention budget.
    pub outside: Vec<auk_tsp::Outside>,
}

impl Lenses {
    /// The block a claim at `C` opens, or `None` when nothing claims the command.
    pub fn claim(
        &self,
        cmdline: Option<&str>,
        typed: Option<&str>,
        pre: Preconditions,
    ) -> Option<(Claim, String, String)> {
        let catalog = self.catalog.as_ref()?;
        let line = cmdline?;
        let claim = catalog.claim(line, pre)?;
        Some((claim, line.to_owned(), typed.unwrap_or(line).to_owned()))
    }

    pub fn index(&self, id: u64) -> Option<usize> {
        self.blocks.iter().position(|b| b.id == id)
    }

    pub fn by_anchor(&self, key: AnchorKey) -> Option<usize> {
        self.blocks.iter().position(|b| b.anchor == key.0)
    }

    pub fn snapshot(&self) -> LensSnapshot {
        let mut blocks = self.blocks.clone();
        if let Some((id, capture)) = &self.running
            && let Some(b) = blocks.iter_mut().find(|b| b.id == *id)
        {
            b.raw = capture.raw().to_vec();
        }
        LensSnapshot {
            catalog: self.catalog.clone(),
            next: self.next,
            blocks,
            running: self.running.as_ref().map(|(id, _)| *id),
        }
    }

    pub fn restore(snapshot: &LensSnapshot) -> Self {
        let mut blocks = snapshot.blocks.clone();
        let running = snapshot.running.and_then(|id| {
            let b = blocks.iter_mut().find(|b| b.id == id)?;
            let raw = std::mem::take(&mut b.raw);
            Some((id, Capture::resume(b.cols, &raw)))
        });
        Lenses {
            catalog: snapshot.catalog.clone(),
            next: snapshot.next,
            blocks,
            running,
            notes: Vec::new(),
            outside: Vec::new(),
        }
    }

    /// Everything but the views, which are side messages.
    pub fn hash(&self, h: &mut Sha256) {
        h.update(b"lenses\0");
        let catalog = self
            .catalog
            .as_ref()
            .map(|c| serde_json::to_vec(c).unwrap_or_default());
        hash_bytes(h, catalog.as_deref());
        h.update(self.next.to_le_bytes());
        h.update((self.blocks.len() as u64).to_le_bytes());
        for b in &self.blocks {
            let running = self.running.as_ref().filter(|(id, _)| *id == b.id);
            let raw = running.map_or(b.raw.as_slice(), |(_, c)| c.raw());
            h.update(b.id.to_le_bytes());
            h.update(b.anchor.to_le_bytes());
            for s in [&b.plugin, &b.lens, &b.line, &b.typed, &b.program] {
                hash_bytes(h, Some(s.as_bytes()));
            }
            hash_bytes(h, b.cwd.as_deref().map(str::as_bytes));
            h.update((b.args.len() as u64).to_le_bytes());
            for a in &b.args {
                hash_bytes(h, Some(a.as_bytes()));
            }
            h.update(b.cols.to_le_bytes());
            hash_bytes(h, Some(raw));
            h.update([u8::from(b.done), u8::from(running.is_some())]);
            hash_bytes(h, b.status.map(i32::to_le_bytes).as_ref().map(|s| &s[..]));
            hash_bytes(h, b.took_ms.map(u64::to_le_bytes).as_ref().map(|s| &s[..]));
        }
    }
}

fn hash_bytes(h: &mut Sha256, bytes: Option<&[u8]>) {
    match bytes {
        None => h.update([0]),
        Some(bytes) => {
            h.update([1]);
            h.update((bytes.len() as u64).to_le_bytes());
            h.update(bytes);
        }
    }
}

/// The run table and the block a claim opens.
pub(crate) fn new_block(
    id: u64,
    claim: &Claim,
    line: String,
    typed: String,
    cwd: Option<String>,
    cols: u16,
) -> (LensBlock, LensRun) {
    let run = claim.run(&line, &typed, cwd.as_deref());
    let block = LensBlock {
        id,
        anchor: ANCHOR_BASE + id,
        plugin: claim.plugin.clone(),
        lens: claim.lens.clone(),
        line,
        typed,
        cwd,
        program: claim.command.program.clone(),
        args: claim.command.args.clone(),
        cols,
        raw: Vec::new(),
        done: false,
        status: None,
        took_ms: None,
        view: None,
    };
    (block, run)
}

/// The part of a catalog a replica claims from (§9.4, §15.2): Ready plugins that declare lenses,
/// with their lens rules. Block types, styles and problems don't change parsing.
pub fn claim_catalog(catalog: &Catalog) -> Catalog {
    Catalog {
        plugins: catalog
            .ready()
            .filter(|p| !p.lenses.is_empty())
            .map(|p| auk_plugins::catalog::PluginEntry {
                css: String::new(),
                blocks: Vec::new(),
                description: String::new(),
                ..p.clone()
            })
            .collect(),
        problems: Vec::new(),
        jit: false,
    }
}

mod b64 {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .map_err(serde::de::Error::custom)
    }
}

impl crate::Replica {
    /// OSC 133 `C`: claims the command from the catalog (§9.6) and, when a lens claims it, opens
    /// a command block at the cursor and starts capturing.
    pub(crate) fn claim(&mut self, cmdline: Option<&str>, typed: Option<&str>) {
        if self.lens.running.is_some() {
            return;
        }
        let pre = Preconditions {
            shell_integration: cmdline.is_some(),
            lenses_on: self.settings.command_lenses,
            main_screen: !self.term.modes().alt_screen,
            surface_open: self.tsp.surfaces().iter().any(|s| s.is_live()),
        };
        let Some((claim, line, typed)) = self.lens.claim(cmdline, typed, pre) else {
            return;
        };
        let id = self.lens.next;
        let cols = self.term.cols();
        let (block, run) = new_block(id, &claim, line, typed, self.cwd.clone(), cols);
        if self.term.open_anchor(AnchorKey(block.anchor)).is_err() {
            return;
        }
        self.lens.next += 1;
        self.lens.blocks.push(block);
        self.lens.running = Some((id, Capture::new(cols)));
        if self.role == crate::Role::Authoritative {
            self.lens.notes.push(LensNote::Open {
                block: id,
                claim,
                run,
                cols,
            });
        }
    }

    /// Feeds output to the running capture. Returns how many bytes it took; the rest belong to
    /// the grid.
    pub(crate) fn feed_capture(
        &mut self,
        bytes: &[u8],
        events: &mut Vec<auk_term::Event>,
    ) -> usize {
        let authoritative = self.role == crate::Role::Authoritative;
        let Some((id, capture)) = self.lens.running.as_mut() else {
            return 0;
        };
        let id = *id;
        let step = capture.feed_stream(bytes);
        if authoritative {
            if !step.replies.is_empty() {
                self.replies.push(crate::Answer::Term(step.replies));
            }
            if !step.lines.is_empty() {
                self.lens.notes.push(LensNote::Lines {
                    block: id,
                    lines: step.lines,
                });
            }
        }
        if step.abort.is_some() {
            // Everything captured goes to the grid instead (§9.6 "Aborting").
            let raw = capture.raw().to_vec();
            self.lens.running = None;
            if let Some(i) = self.lens.index(id) {
                let b = self.lens.blocks.remove(i);
                let key = AnchorKey(b.anchor);
                if !self.term.undo_anchor(key) {
                    self.term.drop_anchor(key);
                }
            }
            if authoritative {
                self.lens.notes.push(LensNote::Drop { block: id });
            }
            self.feed(&raw, false, events);
            return step.consumed;
        }
        if let Some(mark) = step.end {
            let rest = capture.finish();
            let raw = capture.raw().to_vec();
            self.lens.running = None;
            let status = match mark {
                auk_term::Mark::CommandEnd { status } => status,
                _ => None,
            };
            if let Some(i) = self.lens.index(id) {
                let b = &mut self.lens.blocks[i];
                b.raw = raw;
                b.done = true;
                b.status = status;
            }
            if authoritative {
                if !rest.is_empty() {
                    self.lens.notes.push(LensNote::Lines {
                        block: id,
                        lines: rest,
                    });
                }
                self.lens.notes.push(LensNote::Finish { block: id, status });
            }
            // The grid's parser didn't see the mark; it reaches the TSP rules and the caller as
            // if it had.
            self.observe(auk_term::Event::Mark(mark), false, events);
        }
        step.consumed
    }

    /// The running capture ends unfinished (the process exited): the block keeps what it has.
    pub(crate) fn end_capture(&mut self) {
        let Some((id, capture)) = self.lens.running.take() else {
            return;
        };
        if let Some(i) = self.lens.index(id) {
            let b = &mut self.lens.blocks[i];
            b.raw = capture.raw().to_vec();
            b.done = true;
        }
        if self.role == crate::Role::Authoritative {
            self.lens.notes.push(LensNote::Drop { block: id });
        }
    }

    /// A command block's anchor row ended (it left the scrollback, or was cleared): the block
    /// goes.
    pub(crate) fn lens_anchor_gone(&mut self, key: AnchorKey) {
        let Some(i) = self.lens.by_anchor(key) else {
            return;
        };
        let id = self.lens.blocks[i].id;
        if self.lens.running.as_ref().is_some_and(|(r, _)| *r == id) {
            self.lens.running = None;
            if self.role == crate::Role::Authoritative {
                self.lens.notes.push(LensNote::Drop { block: id });
            }
        }
        self.lens.blocks.remove(i);
    }

    /// Retention (§7.10): finished command blocks share the pane's budget with its surface
    /// documents, and go whole, oldest first, with the closed surfaces. Runs after every item.
    pub(crate) fn enforce_retention(&mut self) {
        let running = self.lens.running.as_ref().map(|(id, _)| *id);
        let outside: Vec<auk_tsp::Outside> = self
            .lens
            .blocks
            .iter()
            .filter(|b| b.done && Some(b.id) != running)
            .map(|b| auk_tsp::Outside {
                anchor: AnchorKey(b.anchor),
                id: b.id,
                cost: b.cost(),
            })
            .collect();
        // The engine weighs them after its own frames, palettes, sheets and closes; here only a
        // change in the finished blocks needs a pass.
        if outside != self.lens.outside {
            self.lens.outside = outside.clone();
            self.tsp.set_outside(&mut self.term, outside);
        }
        for id in self.tsp.take_outside_evicted() {
            if let Some(i) = self.lens.index(id) {
                let b = self.lens.blocks.remove(i);
                self.term.drop_anchor(AnchorKey(b.anchor));
            }
        }
    }

    // Public reading and the side messages.

    /// The command blocks in this pane, oldest first. A running block's `raw` is empty; read it
    /// with [`lens_raw`](Self::lens_raw).
    pub fn lens_blocks(&self) -> &[LensBlock] {
        &self.lens.blocks
    }

    /// A command block's raw output so far.
    pub fn lens_raw(&self, block: u64) -> Option<&[u8]> {
        if let Some((id, capture)) = &self.lens.running
            && *id == block
        {
            return Some(capture.raw());
        }
        self.lens
            .index(block)
            .map(|i| self.lens.blocks[i].raw.as_slice())
    }

    /// The block whose command is running, if any.
    pub fn lens_running(&self) -> Option<u64> {
        self.lens.running.as_ref().map(|(id, _)| *id)
    }

    /// The claim catalog this replica last received.
    pub fn lens_catalog(&self) -> Option<&Catalog> {
        self.lens.catalog.as_ref()
    }

    /// A lens view arrived (§15.1 side message): it replaces the block's view whole. Returns
    /// false when this replica has no such block (yet).
    pub fn set_lens_view(&mut self, block: u64, view: Option<Value>) -> bool {
        match self.lens.index(block) {
            Some(i) => {
                self.lens.blocks[i].view = view;
                true
            }
            None => false,
        }
    }

    /// What the plugin host should hear about command blocks, in stream order (the answering
    /// replica only).
    pub fn take_lens_notes(&mut self) -> Vec<LensNote> {
        std::mem::take(&mut self.lens.notes)
    }
}
