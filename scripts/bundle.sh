#!/usr/bin/env bash
# Builds Auk.app (spec §19): the release `auk` binary, with the browser client's files in
# Contents/Resources/web for `auk web serve`, signed ad hoc with the hardened runtime and the
# allow-jit entitlement Luau's native codegen needs (§20).
#
# The app goes in the target dir: target/Auk.app, or $CARGO_TARGET_DIR/Auk.app when that's set.
# Written for the bash 3.2 that macOS ships.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/env.sh

target=${CARGO_TARGET_DIR:-target}
mkdir -p "$target"
target=$(cd "$target" && pwd)
app=$target/Auk.app
entitlements=crates/auk/Auk.entitlements
bundle_id=dev.auk.Auk

echo "== auk (release)"
cargo build --locked --release -p auk
exe=$target/release/auk
version=$("$exe" --version | awk '{print $2}')

echo "== web client (wasm32)"
cargo build --locked --release -p auk-web --target wasm32-unknown-unknown
wasm=$target/wasm32-unknown-unknown/release/auk_web.wasm

# The wasm-bindgen CLI must be the version Cargo.lock pins for the wasm-bindgen crate. cargo keeps
# the install while that version matches.
bindgen_version=$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/version = |"/, ""); print; exit }' Cargo.lock)
[ -n "$bindgen_version" ] || { echo "bundle: wasm-bindgen isn't in Cargo.lock" >&2; exit 1; }
tools=$target/bundle-tools
echo "== wasm-bindgen $bindgen_version"
CARGO_TARGET_DIR=$tools/build cargo install wasm-bindgen-cli --locked --quiet \
  --version "=$bindgen_version" --root "$tools"
bindgen=$tools/bin/wasm-bindgen
"$bindgen" --version | grep -qw "$bindgen_version" || {
  echo "bundle: $bindgen reports $("$bindgen" --version), not $bindgen_version" >&2
  exit 1
}

echo "== $app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/web"
cp "$exe" "$app/Contents/MacOS/auk"
"$bindgen" --target web --no-typescript --out-dir "$app/Contents/Resources/web" "$wasm"
cat >"$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>$bundle_id</string>
	<key>CFBundleName</key>
	<string>Auk</string>
	<key>CFBundleDisplayName</key>
	<string>Auk</string>
	<key>CFBundleExecutable</key>
	<string>auk</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleShortVersionString</key>
	<string>$version</string>
	<key>CFBundleVersion</key>
	<string>$version</string>
	<key>LSMinimumSystemVersion</key>
	<string>27.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSPrincipalClass</key>
	<string>NSApplication</string>
</dict>
</plist>
EOF
plutil -lint "$app/Contents/Info.plist" >/dev/null

echo "== signing (ad hoc, hardened runtime, allow-jit)"
codesign --force --sign - --options runtime --entitlements "$entitlements" --timestamp=none "$app"
codesign --verify --strict "$app"
echo "bundle: $app"
