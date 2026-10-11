//! Hostile and random input: mangled recordings and random op streams never
//! panic, and the state stays replicable (a snapshot rebuilds an equal state).

mod common;

use auk_conformance::{Dir, fixtures_dir, load};
use auk_tsp::Config;
use common::{Harness, payload};
use serde_json::{Value, json};

/// xorshift64*: deterministic, no dependencies.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

fn all_kinds() -> Config {
    let kinds = tern_sdk::wire::kind::ALL
        .iter()
        .map(|k| (*k).to_owned())
        .collect();
    Config {
        kinds,
        ..Config::default()
    }
}

#[test]
fn mangled_recordings_never_panic() {
    let records = load(&fixtures_dir().join("omp-session.jsonl")).unwrap();
    let messages: Vec<Vec<u8>> = records
        .iter()
        .filter(|r| r.dir == Dir::Out && r.verb.len() == 1)
        .map(|r| {
            let body = match &r.body {
                Value::String(s) => s.clone(),
                v => v.to_string(),
            };
            payload(&r.verb, &[], &body)
        })
        .collect();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut h = Harness::with(all_kinds());
    for round in 0..3 {
        for msg in &messages {
            let mut m = msg.clone();
            match rng.below(6) {
                0 => {}
                1 => {
                    let at = rng.below(m.len());
                    m[at] = rng.next() as u8;
                }
                2 => m.truncate(rng.below(m.len()).max(5)),
                3 => {
                    let at = 4 + rng.below(m.len() - 4);
                    m.splice(
                        at..at,
                        rng.pick(&[&b";c=1;m=1;"[..], b"\xc2\x9c", b"\x00", b"]]", b"{"])
                            .iter()
                            .copied(),
                    );
                }
                4 => m.extend_from_slice(b";m=1"),
                _ => {
                    m = payload(
                        rng.pick(&["f", "o", "x", "t", "s", "q", "b"]),
                        &[("c", "z")],
                        "{}",
                    )
                }
            }
            h.pane.on_apc(&mut h.grid, &m);
            h.out();
            if rng.below(200) == 0 {
                h.pane.expire_chunks();
            }
        }
        h.pane.on_prompt(&mut h.grid);
        let fresh = h.replay();
        assert_eq!(
            fresh.pane.state_hash(),
            h.pane.state_hash(),
            "round {round}"
        );
    }
}

#[test]
fn random_op_streams_keep_the_document_consistent() {
    let mut rng = Rng(42);
    let ids = [
        "main", "dock", "layer", "a", "b", "c", "d", "e", "s1", "ed", "",
    ];
    let kinds = ["col", "text", "editor", "md", "nope", "el"];
    let mut h = Harness::new();
    h.hello(&["edit"]);
    for frame in 1..=2000u64 {
        if frame % 300 == 1 {
            let mode = *rng.pick(&["inline", "flow", "screen"]);
            h.send_json(
                "o",
                json!({"id": "s1", "mode": mode, "listen": rng.below(4) != 0}),
            );
        }
        let mut ops = Vec::new();
        for _ in 0..rng.below(6) {
            let id = *rng.pick(&ids);
            let other = *rng.pick(&ids);
            let before = if rng.below(2) == 0 {
                Value::Null
            } else {
                json!(rng.pick(&ids))
            };
            let node = json!({"id": id, "k": rng.pick(&kinds), "p": {"text": "x😀y", "tone": null},
                "c": [{"id": format!("{id}.{}", rng.below(5)), "k": "text"}]});
            ops.push(match rng.below(14) {
                0 | 1 => json!(["add", id, other, before, node]),
                2 => json!(["set", id, {"k": rng.below(9), "text": null}]),
                3 => json!(["text", id, rng.pick(&["append", "replace", "x"]), "é"]),
                4 => json!(["splice", id, rng.below(5), rng.below(3), "😀"]),
                5 => json!(["move", id, other, before]),
                6 => json!(["del", id]),
                7 => json!(["settle", id]),
                8 => json!([
                    "focus",
                    if rng.below(3) == 0 {
                        Value::Null
                    } else {
                        json!(id)
                    }
                ]),
                9 => json!(["reveal", id, "end"]),
                10 => json!(["scroll", id, "page-up"]),
                11 => json!([rng.pick(&["suspend", "resume"])]),
                12 => json!([rng.next() % 7, id]),
                _ => json!(["add", id, "s1", null, node]),
            });
        }
        h.frame("s1", frame, Value::Array(ops));
        h.pane.take_changes();
        h.out();
        if rng.below(50) == 0 {
            h.pane.ack(&h.grid, "s1", frame);
        }
        if frame % 250 == 0 {
            let fresh = h.replay();
            assert_eq!(
                fresh.pane.state_hash(),
                h.pane.state_hash(),
                "frame {frame}"
            );
            for s in h.pane.surfaces() {
                check_tree(s.document().root(), None, &mut Default::default());
                if let Some(focus) = s.document().focus() {
                    assert!(s.document().get(focus).is_some(), "focus on a removed node");
                }
            }
        }
    }
}

/// Parent links, unique ids and the region rule hold everywhere.
fn check_tree(
    n: auk_tsp::NodeRef<'_>,
    parent: Option<&str>,
    seen: &mut std::collections::BTreeSet<String>,
) {
    assert!(seen.insert(n.id().to_owned()), "duplicate {}", n.id());
    assert_eq!(n.parent().map(|p| p.id()), parent);
    if let Some(p) = n.parent() {
        assert_eq!(
            p.parent().is_none(),
            ["main", "dock", "layer"].contains(&n.id()),
            "{}",
            n.id()
        );
    }
    for c in n.children() {
        check_tree(c, Some(n.id()), seen);
    }
}
