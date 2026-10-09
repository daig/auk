// A build script is required for cargo to consider build dependencies.
//
// On macOS it also refuses Homebrew's `cargo`/`rustc`: they ignore rust-toolchain.toml, lack the
// wasm32 target, and build every artifact under different hashes than rustup's toolchain, so a
// mixed workflow keeps two copies of everything in target/. `source scripts/env.sh` first.
fn main() {
    println!("cargo:rerun-if-env-changed=RUSTUP_TOOLCHAIN");
    if cfg!(target_os = "macos") && std::env::var_os("RUSTUP_TOOLCHAIN").is_none() {
        panic!("build with rustup's toolchain: run `source scripts/env.sh` first (Homebrew's cargo is on PATH)");
    }
}
