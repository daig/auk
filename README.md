# Auk

A clean-room terminal with the same core design as Stencil's Tern: a session daemon that owns every pane, replicas that mirror it, native UI described in-band over the Tern Surface Protocol (TSP v1), and Luau plugins. It runs on macOS 27 on Apple M5-series Macs.

- **Spec:** `tern-core-spec-v1.md`. Part II is the functional spec; Part III is the architecture; Part IV holds the plan, the questions and the decision log.
- **Evidence:** `research/` holds digests of Tern's public docs and SDKs, plus the source snapshots they cite.
- **Oracles:** `third_party/` holds the pinned TSP SDKs and the omp sources.
- **Recordings:** `fixtures/tsp/` holds TSP sessions recorded from real programs by `tools/tsp_record.py`.

## Build and run

```sh
source scripts/env.sh   # rustup's toolchain, with the wasm target
scripts/bundle.sh       # builds and signs target/Auk.app
open target/Auk.app     # or copy it to /Applications first
```

`bundle.sh` builds the release `auk` and the browser client, then signs the app ad hoc with the hardened runtime and the `allow-jit` entitlement (`crates/auk/Auk.entitlements`) that Luau's native code needs. The CLI is the same binary: run `target/Auk.app/Contents/MacOS/auk`, or symlink it onto your `PATH`. `auk doctor` reports whether native code runs.

`scripts/verify.sh` is the completion gate: it runs every suite and writes `target/verify/summary.txt`.

## Layout

| Path | What |
| --- | --- |
| `crates/` | the Cargo workspace (spec §22) |
| `tools/` | development tools, such as the TSP recorder |
| `fixtures/` | recorded and hand-written test inputs |
| `scripts/` | shell helpers |
