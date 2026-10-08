//! A pane's surfaces arranged with the real terminal (§6.2, §7.3): the shared compositor every
//! window uses ([`PaneSurfaces`]), fed by `auk-term` and the `auk-tsp` engine in stream order.

use auk_term::{Event, Terminal};
use auk_tsp::{Config, MemoryBlobs, TspPane};
use auk_views::compose::Scroll;
use auk_views::{Arranged, PaneSurfaces, ViewSettings};
use serde_json::{Value, json};

const CELL: f32 = 17.0;

struct Pane {
    term: Terminal,
    tsp: TspPane,
    sf: PaneSurfaces,
    now: f64,
}

impl Pane {
    fn new() -> Pane {
        let term = Terminal::new(80, 12, auk_term::Config::default());
        let mut tsp = TspPane::with(
            Config::default(),
            auk_views::sheet_checker(),
            Box::new(MemoryBlobs::default()),
        );
        tsp.on_resize(&term);
        Pane {
            term,
            tsp,
            sf: PaneSurfaces::new(ViewSettings::default(), 1.0),
            now: 1_000_000.0,
        }
    }

    /// Feeds program output the way a replica does: TSP strings go to the engine in order.
    fn feed(&mut self, bytes: &[u8]) {
        for event in self.term.feed(bytes) {
            if let Event::Tsp(payload) = event {
                self.tsp.on_apc(&mut self.term, &payload);
            }
        }
        let changes = self.tsp.take_changes();
        self.sf.sync(&self.tsp, &changes, self.now);
        let _ = self.tsp.take_output();
    }

    fn apc(&mut self, verb: &str, body: &Value) {
        self.feed(format!("\x1b_tsp;{verb};{body}\x1b\\").as_bytes());
    }

    fn arrange(&mut self) -> Arranged {
        let h = f32::from(self.term.rows()) * CELL;
        self.sf
            .layout(&self.term, &self.tsp, (640.0, h), true, self.now);
        self.sf
            .arrange(&self.term, &self.tsp, CELL, h, 1.0, Scroll::default())
    }
}

/// The height the arranged flow gives the line holding `needle`.
fn height_of(p: &mut Pane, needle: &str) -> f32 {
    let lines = p.term.history_len() + usize::from(p.term.rows());
    let line = (0..lines)
        .find(|&l| p.term.text(l..l + 1).contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle:?}"));
    let arranged = p.arrange();
    arranged.composed.slot_of(line).map_or(0.0, |s| s.height)
}

#[test]
fn stray_output_under_a_live_inline_surface_stays_hidden_until_it_closes_or_suspends() {
    let mut p = Pane::new();
    p.feed(b"$ run\r\n");
    p.apc(
        "q",
        &json!({"q": "hello", "v": [1], "app": "t", "features": []}),
    );
    p.apc("o", &json!({"id": "s1", "mode": "inline"}));
    p.apc(
        "f",
        &json!({"sf": "s1", "s": 1, "ops": [["add", "main", "s1", null, {"id": "main", "k": "col", "c": [{"id": "t", "k": "text", "p": {"text": "surface"}}]}]]}),
    );
    // Output that isn't TSP goes to the grid below the anchor, and stays hidden.
    p.feed(b"stray output\r\n");
    assert_eq!(height_of(&mut p, "$ run"), CELL, "lines above show");
    assert_eq!(height_of(&mut p, "stray output"), 0.0);
    let arranged = p.arrange();
    assert_eq!(arranged.blocks.len(), 1, "the surface block shows");

    // Suspended: the pane goes back to the grid.
    p.apc("f", &json!({"sf": "s1", "s": 2, "ops": [["suspend"]]}));
    assert_eq!(height_of(&mut p, "stray output"), CELL);
    p.apc("f", &json!({"sf": "s1", "s": 3, "ops": [["resume"]]}));
    assert_eq!(height_of(&mut p, "stray output"), 0.0);

    // Closed (kept): the stray output shows below the kept document.
    p.apc("x", &json!({"id": "s1"}));
    assert_eq!(height_of(&mut p, "stray output"), CELL);
    assert_eq!(p.arrange().blocks.len(), 1, "main stays");

    // Flow surfaces never hide what follows them.
    p.apc("o", &json!({"id": "f1", "mode": "flow"}));
    p.apc(
        "f",
        &json!({"sf": "f1", "s": 1, "ops": [["add", "main", "f1", null, {"id": "main", "k": "col", "c": [{"id": "t", "k": "text", "p": {"text": "flow"}}]}]]}),
    );
    p.feed(b"after flow\r\n");
    assert_eq!(height_of(&mut p, "after flow"), CELL);
}
