# Auk

A clean-room terminal with the same core design as Stencil's Tern: a session daemon that owns every pane, replicas that mirror it, native UI described in-band over the Tern Surface Protocol (TSP v1), and Luau plugins. It runs on macOS 27 on Apple M5-series Macs.

- **Spec:** `tern-core-spec-v0.2.md`. Part II is the functional spec; Part III is the architecture; Part IV holds the plan, the questions and the decision log.
- **Evidence:** `research/` holds digests of Tern's public docs and SDKs, plus the source snapshots they cite.
- **Oracles:** `third_party/` holds the pinned TSP SDKs and the omp sources.
- **Recordings:** `fixtures/tsp/` holds TSP sessions recorded from real programs by `tools/tsp_record.py`.

## Building

```sh
source scripts/env.sh   # rustup's toolchain, with the wasm target
cargo test --workspace
```

## Layout

| Path | What |
| --- | --- |
| `crates/` | the Cargo workspace (spec §22) |
| `tools/` | development tools, such as the TSP recorder |
| `fixtures/` | recorded and hand-written test inputs |
| `spikes/` | throwaway prototypes that settle decisions (spec §23); each is its own workspace |
| `scripts/` | shell helpers |
