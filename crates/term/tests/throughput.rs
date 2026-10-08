//! Throughput: a 100 MB mixed stream (plain ASCII lines, colored output, CJK text, long wrapped
//! lines) through an 80×24 terminal with 10,000 rows of scrollback, read in 64 KiB chunks as a
//! PTY reader would. Prints MB/s. Release builds must reach 200 MB/s; debug builds run a smaller
//! stream and only report.

// Uses dev-dependencies that don't build for wasm32 (see Cargo.toml).
#![cfg(not(target_family = "wasm"))]

use std::time::Instant;

use auk_term::{Config, Terminal};

fn stream(size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size + 4096);
    let mut i = 0u64;
    while out.len() < size {
        i += 1;
        match i % 10 {
            // `ls --color`-like output.
            0 | 1 => out.extend_from_slice(
                format!("\x1b[0m\x1b[01;34mdirectory-{i}\x1b[0m  \x1b[01;32mexecutable\x1b[0m  plain-file-{i}.txt  notes.md\r\n").as_bytes(),
            ),
            // CJK and emoji.
            2 => out.extend_from_slice(format!("{i}: 中文字符的输出 — ünïcödé text ✓ 😀 mixed with ASCII words\r\n").as_bytes()),
            // A long line that wraps several times.
            3 => {
                for _ in 0..4 {
                    out.extend_from_slice(b"the quick brown fox jumps over the lazy dog; ");
                }
                out.extend_from_slice(b"\r\n");
            }
            // Plain log lines.
            _ => out.extend_from_slice(
                format!("2026-10-08T12:00:00Z INFO request id={i} path=/api/v1/items status=200 dur=1.3ms\r\n").as_bytes(),
            ),
        }
    }
    out
}

#[test]
fn throughput_100mb() {
    let size = if cfg!(debug_assertions) {
        4 << 20
    } else {
        100 << 20
    };
    let data = stream(size);
    let mut t = Terminal::new(80, 24, Config { scrollback: 10_000 });
    let start = Instant::now();
    for chunk in data.chunks(64 << 10) {
        let mut rest = chunk;
        while !rest.is_empty() {
            let (n, _) = t.advance(rest);
            rest = &rest[n..];
        }
    }
    let secs = start.elapsed().as_secs_f64();
    let mbps = data.len() as f64 / secs / 1e6;
    println!(
        "throughput: {:.1} MB in {secs:.3} s = {mbps:.0} MB/s",
        data.len() as f64 / 1e6
    );
    assert_eq!(t.history_len(), 10_000);
    assert!(
        t.text(t.history_len()..t.history_len() + 24)
            .contains("status=200")
    );
    if !cfg!(debug_assertions) {
        assert!(mbps >= 200.0, "{mbps:.0} MB/s is below 200 MB/s");
    }
}
