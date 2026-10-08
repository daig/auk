# Source this before building: `source scripts/env.sh`.
#
# The project builds with rustup's toolchain (pinned by rust-toolchain.toml, with the
# wasm32 target), not Homebrew's `rust`, which has no wasm target. Homebrew installs
# rustup keg-only, so its proxies have to come first on PATH.
if command -v brew >/dev/null 2>&1; then
  export PATH="$(brew --prefix rustup)/bin:$PATH"
fi
