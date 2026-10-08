#!/usr/bin/env bash
# The completion gate for the implementation loop (GOAL.md). Exits 0 only when every
# automated criterion passes. A missing or empty required suite is a failure, not a skip.
set -uo pipefail
cd "$(dirname "$0")/.."
source scripts/env.sh

SUITES=(persistence terminal replication tsp sdk_examples omp layout perf plugins app cli web remote)
fail=0
step() { printf '\n== %s\n' "$*"; }

step "no placeholders or skipped tests in crates/"
if grep -rnE 'todo!\(|unimplemented!\(|#\[ignore|TODO|FIXME|[Nn]ot implemented' crates --include='*.rs'; then fail=1; fi

step "every required suite exists and has tests"
for suite in "${SUITES[@]}"; do
  if [ ! -f "crates/conformance/tests/$suite.rs" ]; then
    echo "missing suite: $suite"
    fail=1
  elif [ "$(cargo test -q --release -p auk-conformance --test "$suite" -- --list 2>/dev/null | grep -c ': test$')" -eq 0 ]; then
    echo "suite has no tests (or doesn't build): $suite"
    fail=1
  fi
done

step "all tests, release (every crate, every suite)"
cargo test --workspace --release || fail=1

step "web client builds for wasm32-unknown-unknown"
cargo build --release -p auk-web --target wasm32-unknown-unknown || fail=1

echo
if [ "$fail" -eq 0 ]; then echo "verify: PASS"; else echo "verify: FAIL"; fi
exit "$fail"
