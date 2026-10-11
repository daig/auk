//! Fuzzed pane streams for the determinism checks: byte soup biased toward what
//! terminals parse, TSP messages among it, and records at random positions. Deterministic per
//! seed, so a failure reproduces.

use auk_replica::{Record, Settings};

/// A small xorshift generator.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

fn tsp(verb: &str, body: &str) -> Vec<u8> {
    format!("\x1b_tsp;{verb};{body}\x1b\\").into_bytes()
}

/// Byte soup: CSI, OSC, SGR, wide text, controls, broken UTF-8, and pieces of TSP traffic
/// (hellos, surfaces, frames, closes) whole and cut.
pub fn bytes(rng: &mut Rng, len: usize) -> Vec<u8> {
    const PIECES: &[&[u8]] = &[
        b"\x1b[",
        b"\x1b]",
        b"\x07",
        b"\x1b\\",
        b"\r\n",
        b"\n",
        b"\r",
        b"\t",
        b"\x08",
        b";",
        b"m",
        b"H",
        b"J",
        b"K",
        b"?1049h",
        b"?1049l",
        b"?2004h",
        b"r",
        b"S",
        b"T",
        b"L",
        b"M",
        b"@",
        b"P",
        b"X",
        b"\x1bc",
        b"\x1b7",
        b"\x1b8",
        b"\x1bD",
        b"\x1bM",
        b"133;A",
        b"7;file:///x",
        b"\xff",
        b"\xc3",
        b"0",
        b"1",
        b"2",
        b"38;5;",
        b"48;2;1;2;3",
        b"hello",
        b"x",
        b"\x1b_",
        b"tsp;",
    ];
    let texts: [&[u8]; 3] = [
        "\u{4e2d}".as_bytes(),
        "\u{1f600}".as_bytes(),
        "e\u{301}".as_bytes(),
    ];
    let tsp_pieces = [
        tsp("q", r#"{"q":"hello","v":[1],"app":"fuzz"}"#),
        tsp("o", r#"{"id":"s1","mode":"flow","title":"T"}"#),
        tsp("o", r#"{"id":"s2","mode":"inline"}"#),
        tsp(
            "f",
            r#"{"sf":"s1","s":1,"ops":[["add","main","s1",null,{"id":"main","k":"col","c":[{"id":"t","k":"text","p":{"text":"hi"}}]}]]}"#,
        ),
        tsp(
            "f",
            r#"{"sf":"s2","s":2,"ops":[["add","main","s2",null,{"id":"main","k":"col"}]]}"#,
        ),
        tsp(
            "f",
            r#"{"sf":"s2","s":3,"ops":[["add","p","main",null,{"id":"p","k":"progress","p":{"value":0.5}}]]}"#,
        ),
        tsp("x", r#"{"id":"s1","keep":true}"#),
        tsp("x", r#"{"id":"s2"}"#),
        b"\x1b]133;A\x07$ ".to_vec(),
    ];
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        match rng.below(12) {
            0..=3 => out.push(rng.below(256) as u8),
            4 => out.extend_from_slice(texts[rng.below(3) as usize]),
            5 => {
                let piece = &tsp_pieces[rng.below(tsp_pieces.len() as u64) as usize];
                // Now and then only part of it, so frames are cut mid-message.
                let end = if rng.below(4) == 0 {
                    rng.below(piece.len() as u64) as usize
                } else {
                    piece.len()
                };
                out.extend_from_slice(&piece[..end]);
            }
            _ => out.extend_from_slice(PIECES[rng.below(PIECES.len() as u64) as usize]),
        }
    }
    out
}

/// A record now and then: resizes, clears, resets, settings changes, exits, chunk timeouts.
pub fn record(rng: &mut Rng) -> Option<Record> {
    Some(match rng.below(14) {
        0 => Record::Resize {
            cols: 10 + rng.below(120) as u16,
            rows: 3 + rng.below(50) as u16,
        },
        1 => Record::ClearScrollback,
        2 if rng.below(4) == 0 => Record::Reset,
        3 => Record::Settings {
            settings: Settings {
                scrollback: rng.below(200) as usize,
                native_surfaces: rng.below(4) != 0,
                ..Settings::default()
            },
        },
        4 if rng.below(8) == 0 => Record::Exit {
            status: rng.below(3) as i32,
        },
        5 => Record::ChunkTimeout,
        _ => return None,
    })
}
