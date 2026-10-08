//! Runs [`auk_web::replay`] over the encoded cases on stdin and prints one line per case. The
//! web suite builds it for `wasm32-wasip1` and runs it under wasmtime, to compare wasm replicas
//! with native ones (AC2).

use std::io::{Read, Write};

fn main() {
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .expect("read the cases from stdin");
    let cases = match auk_web::replay::decode(&input) {
        Ok(cases) => cases,
        Err(err) => {
            eprintln!("replay: {err}");
            std::process::exit(2);
        }
    };
    let mut out = std::io::stdout().lock();
    for line in auk_web::replay::run(&cases) {
        writeln!(out, "{line}").expect("write a result");
    }
}
