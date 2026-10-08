//! Replays every recorded TSP session in `fixtures/tsp` through the engine: the program's messages
//! in order, the recorded acks as the primary's draws. Nothing may draw an error, and a snapshot
//! taken anywhere along the way must rebuild an equal state.

mod common;

use std::path::PathBuf;

use auk_conformance::{Dir, Record, fixtures_dir, load};
use auk_tsp::{Config, Grid, State};
use common::{Harness, payload};
use serde_json::Value;

fn fixtures() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(fixtures_dir())
        .expect("fixtures/tsp")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    paths.sort();
    assert!(paths.len() >= 10, "fixtures missing: {paths:?}");
    paths
}

/// The engine configured as the recording terminal advertised (its hello reply), so kinds the
/// recorded program relied on are accepted; Auk's defaults otherwise. omp's own log of a session
/// has no replies; the recorder's log of the same session does.
fn config(path: &std::path::Path, records: &[Record]) -> Config {
    let mut config = Config::default();
    let is_reply = |r: &&Record| r.dir == Dir::In && r.verb == "r" && r.body["r"] == "hello";
    let companion = path
        .to_string_lossy()
        .strip_suffix("-own.jsonl")
        .map(|p| load(std::path::Path::new(&format!("{p}.jsonl"))).unwrap());
    let reply = records
        .iter()
        .find(is_reply)
        .or_else(|| companion.as_ref().and_then(|c| c.iter().find(is_reply)));
    if let Some(reply) = reply {
        let strings = |key: &str| {
            reply.body[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect()
        };
        config.kinds = strings("kinds");
        config.features = strings("features");
    }
    config
}

/// Feeds one recorded line; returns whether it was a program message.
fn feed(h: &mut Harness, rec: &Record) {
    match (rec.dir, rec.verb.as_str()) {
        (Dir::Out, "osc") => {
            let body = rec.body.as_str().unwrap();
            if body.starts_with("0;") || body.starts_with("2;") {
                h.pane.on_title();
            }
        }
        (Dir::Out, "pty") => h.grid.write(),
        (Dir::Out, verb) => {
            let params: Vec<(&str, &str)> = rec
                .params
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str().unwrap()))
                .collect();
            let body = match &rec.body {
                Value::String(s) if verb == "b" => s.clone(),
                other => other.to_string(),
            };
            h.pane.on_apc(&mut h.grid, &payload(verb, &params, &body));
        }
        (Dir::In, "e") if rec.body["ev"] == "ack" => {
            let (sf, s) = (
                rec.body["sf"].as_str().unwrap(),
                rec.body["s"].as_u64().unwrap(),
            );
            let grid: &dyn Grid = &h.grid;
            h.pane.ack(grid, sf, s);
        }
        (Dir::In, _) => {}
    }
}

#[test]
fn fixtures_replay_without_errors() {
    for path in fixtures() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let records = load(&path).unwrap();
        let mut h = Harness::with(config(&path, &records));
        let mut sent = Vec::new();
        let mut frames = 0;
        for rec in &records {
            feed(&mut h, rec);
            frames += usize::from(rec.dir == Dir::Out && rec.verb == "f");
            sent.extend(h.out());
        }
        let errors: Vec<&Value> = sent
            .iter()
            .filter(|e| e["ev"] == "error" || e["ev"] == "gone")
            .collect();
        assert!(errors.is_empty(), "{name}: {errors:?}");
        // The program saw what Tern's recording shows it saw, by kind: one hello reply per hello,
        // and an ack for every recorded ack.
        let count =
            |v: &[Value], ev: &str| v.iter().filter(|e| e["ev"] == ev || e["r"] == ev).count();
        let recorded: Vec<Value> = records
            .iter()
            .filter(|r| r.dir == Dir::In)
            .map(|r| r.body.clone())
            .collect();
        assert_eq!(count(&sent, "ack"), count(&recorded, "ack"), "{name}");
        assert_eq!(count(&sent, "hello"), count(&recorded, "hello"), "{name}");
        // Every surface the program closed is closed, its `main` kept.
        let closed: Vec<&str> = records
            .iter()
            .filter(|r| r.dir == Dir::Out && r.verb == "x")
            .map(|r| r.body["id"].as_str().unwrap())
            .collect();
        for s in h.pane.surfaces() {
            let want = if closed.contains(&s.id()) {
                State::Closed
            } else {
                State::Live
            };
            assert_eq!(s.state(), want, "{name}: {}", s.id());
            assert!(s.document().region("main").is_some(), "{name}: {}", s.id());
        }
        assert!(frames > 0 || name.contains("docs"), "{name}");
        // Then the shell's prompt takes inline surfaces off the pane, silently.
        h.pane.on_exit(&mut h.grid);
        h.pane.on_prompt(&mut h.grid);
        assert!(h.out().is_empty(), "{name}");
        assert!(
            h.pane
                .surfaces()
                .iter()
                .all(|s| s.mode() == auk_tsp::Mode::Flow),
            "{name}"
        );
    }
}

#[test]
fn fixtures_snapshots_rebuild_equal_state() {
    for path in fixtures() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let records = load(&path).unwrap();
        let mut h = Harness::with(config(&path, &records));
        let every = (records.len() / 40).max(1);
        for (n, rec) in records.iter().enumerate() {
            feed(&mut h, rec);
            h.out();
            if n % every == 0 || n + 1 == records.len() {
                let fresh = h.replay();
                assert_eq!(
                    fresh.pane.state_hash(),
                    h.pane.state_hash(),
                    "{name} after record {n}"
                );
            }
        }
    }
}

#[test]
fn fixtures_are_deterministic() {
    for path in fixtures() {
        let records = load(&path).unwrap();
        let run = || {
            let mut h = Harness::with(config(&path, &records));
            for rec in &records {
                feed(&mut h, rec);
            }
            (h.pane.state_hash(), h.pane.snapshot())
        };
        assert_eq!(run(), run(), "{}", path.display());
    }
}
