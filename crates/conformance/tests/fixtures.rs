//! Every recording in `fixtures/tsp` parses and has well-formed messages.

use auk_conformance::{fixtures_dir, load, validate};

#[test]
fn every_fixture_is_well_formed() {
    let mut paths: Vec<_> = std::fs::read_dir(fixtures_dir())
        .expect("fixtures/tsp exists")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "no recordings in {}",
        fixtures_dir().display()
    );

    let mut failures = Vec::new();
    for path in &paths {
        match load(path) {
            Ok(records) => {
                for problem in validate(&records) {
                    failures.push(format!("{}: {problem}", path.display()));
                }
            }
            Err(err) => failures.push(err),
        }
    }
    assert!(
        failures.is_empty(),
        "malformed fixtures:\n{}",
        failures.join("\n")
    );
}
