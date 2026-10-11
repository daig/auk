#!/usr/bin/env bash
# Runs every suite and check, and exits 0 only when all of them pass. A missing or empty suite and
# an ignored test are failures, not skips.
#
# Once the suites exist this takes a while: run it in the background, then read
# target/verify/summary.txt. Each step's output is beside it, in target/verify/<step>.log.
# Written for the bash 3.2 that macOS ships.
set -uo pipefail
cd "$(dirname "$0")/.."
source scripts/env.sh

SUITES="persistence terminal replication tsp sdk_examples omp layout perf plugins app cli web remote"
TOOLS="cargo-nextest cargo-hakari cargo-sweep node bun go wasmtime limactl omp htop tmux python3 swiftc"

out=target/verify
mkdir -p "$out"
rm -f "$out"/*.log
summary=$out/summary.txt
: >"$summary"
fail=0

# record <check> <PASS|FAIL> [detail]
record() {
  if [ -n "${3:-}" ]; then printf '%-14s %-4s %s\n' "$1" "$2" "$3"; else printf '%-14s %s\n' "$1" "$2"; fi >>"$summary"
  [ "$2" = PASS ] || fail=1
}

# The tests a nextest log reports as failed, from its final summary.
failed_tests() {
  sed -n '/^ *Summary \[/,$p' "$1" | grep -E '^ +[A-Z-]+ +\[' | grep -vE '^ +(PASS|SKIP|SLOW)' |
    sed -E 's/^ +[A-Z-]+ +\[[^]]*\] +(\([0-9]+\/[0-9]+\) +)?//' | sort -u
}

# step <name> <command...>: run a command, keeping its output in target/verify/<name>.log.
step() {
  local name=$1
  shift
  echo "== $name"
  if "$@" >"$out/$name.log" 2>&1; then
    record "$name" PASS
  else
    record "$name" FAIL "see $out/$name.log"
    failed_tests "$out/$name.log" | head -40 | sed 's/^/    /' >>"$summary"
  fi
}

echo "== prerequisites"
missing=
for tool in $TOOLS; do
  command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"
done
[ -d /Applications/Ghostty.app ] || missing="$missing Ghostty.app"
if [ -z "$missing" ]; then record prerequisites PASS; else record prerequisites FAIL "missing:$missing"; fi

# Every crate depends on workspace-hack, and its feature list is current (see crates/workspace-hack).
step hakari sh -c 'cargo hakari manage-deps --dry-run && cargo hakari generate --diff'

echo "== placeholders"
grep -rnE 'todo!\(|unimplemented!\(|TODO|FIXME|[Nn]ot implemented' crates --include='*.rs' >"$out/placeholders.log"
hits=$(wc -l <"$out/placeholders.log" | tr -d ' ')
if [ "$hits" -eq 0 ]; then record placeholders PASS; else record placeholders FAIL "$hits in crates/ (see $out/placeholders.log)"; fi

echo "== coverage"
if cargo nextest list --workspace --cargo-profile release --message-format json >"$out/tests.json" 2>"$out/build.log"; then
  python3 -I - "$out/tests.json" "$SUITES" >>"$summary" 2>"$out/coverage.log" <<'EOF' || fail=1
import json, sys

tests_json, suites = sys.argv[1:]
failed = False

def record(check, ok, detail=""):
    global failed
    failed = failed or not ok
    print(f"{check:<14} {'PASS' if ok else 'FAIL'} {detail}".rstrip())

# binary id (auk::tsp) -> {test name: ignored}
binaries = {
    bid: {name: case["ignored"] for name, case in suite["testcases"].items()}
    for bid, suite in json.load(open(tests_json))["rust-suites"].items()
}

empty = [s for s in suites.split() if not binaries.get(f"auk::{s}")]
record("suites", not empty, "missing or empty: " + " ".join(empty) if empty else "")

ignored = [f"{bid} {name}" for bid, cases in binaries.items() for name, ig in cases.items() if ig]
record("ignored", not ignored, " ".join(ignored))

sys.exit(1 if failed else 0)
EOF
else
  record coverage FAIL "the workspace doesn't build (see $out/build.log)"
fi

# perf runs alone, after everything else, so other tests can't skew its numbers. nextest rejects a
# filter that names a missing test binary, so the split applies only once the suite exists.
if [ -f crates/auk/tests/perf.rs ]; then
  step tests cargo nextest run --workspace --cargo-profile release --no-fail-fast -E 'not binary_id(auk::perf)'
  step perf cargo nextest run --workspace --cargo-profile release --no-fail-fast --test-threads 1 -E 'binary_id(auk::perf)'
else
  step tests cargo nextest run --workspace --cargo-profile release --no-fail-fast
  record perf FAIL "suite missing"
fi

step wasm-build cargo build --release -p auk-web --target wasm32-unknown-unknown

# Drop build output untouched for a day, and output from toolchains rustup doesn't have (Homebrew's).
cargo sweep --time 1 >"$out/sweep.log" 2>&1 || true
cargo sweep --installed >>"$out/sweep.log" 2>&1 || true

if [ "$fail" -eq 0 ]; then echo "verify: PASS" >>"$summary"; else echo "verify: FAIL" >>"$summary"; fi
echo
cat "$summary"
exit "$fail"
