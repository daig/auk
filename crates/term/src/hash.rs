//! The state hash (§15.2): BLAKE3 over a canonical serialization of all replicated state.
//!
//! Everything is written as fixed-width little-endian integers and length-prefixed bytes, in a
//! fixed order, so the hash is the same on every platform (wasm32 included). Cells name their
//! OSC 8 link by content (id and URI), not by table index, so the link table's layout doesn't
//! matter.

use crate::core::Core;
use crate::parser::Parser;
use crate::screen::{CursorState, Screen};
use crate::{Cell, Color, CursorShape, MouseMode, Row};

/// A BLAKE3 hasher with canonical encoders.
pub(crate) struct Hasher {
    inner: blake3::Hasher,
    buf: Vec<u8>,
}

impl Hasher {
    fn new() -> Hasher {
        Hasher { inner: blake3::Hasher::new(), buf: Vec::with_capacity(1 << 16) }
    }

    fn flush(&mut self) {
        self.inner.update(&self.buf);
        self.buf.clear();
    }

    pub(crate) fn u8(&mut self, v: u8) {
        self.buf.push(v);
        if self.buf.len() >= 1 << 16 {
            self.flush();
        }
    }

    pub(crate) fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
        if self.buf.len() >= 1 << 16 {
            self.flush();
        }
    }

    pub(crate) fn bool(&mut self, v: bool) {
        self.u8(u8::from(v));
    }

    pub(crate) fn bytes(&mut self, v: &[u8]) {
        self.u64(v.len() as u64);
        self.buf.extend_from_slice(v);
        if self.buf.len() >= 1 << 16 {
            self.flush();
        }
    }

    fn color(&mut self, c: Color) {
        match c {
            Color::Default => self.u8(0),
            Color::Indexed(i) => {
                self.u8(1);
                self.u8(i);
            }
            Color::Rgb(rgb) => {
                self.u8(2);
                self.u8(rgb.0);
                self.u8(rgb.1);
                self.u8(rgb.2);
            }
        }
    }

    fn finish(mut self) -> [u8; 32] {
        self.flush();
        *self.inner.finalize().as_bytes()
    }
}

/// Hashes a link by content: absent, or its id and URI.
fn link(h: &mut Hasher, core: &Core, index: u32) {
    if index == 0 {
        h.u8(0);
        return;
    }
    let i = index as usize;
    h.u8(1);
    match &core.links.ids[i] {
        None => h.u8(0),
        Some(id) => {
            h.u8(1);
            h.bytes(id.as_bytes());
        }
    }
    h.bytes(core.links.uris[i].as_bytes());
}

fn cell(h: &mut Hasher, core: &Core, c: &Cell) {
    h.bytes(c.text.as_bytes());
    h.color(c.fg);
    h.color(c.bg);
    h.color(c.underline_color);
    h.u64(u64::from(c.flags.0));
    link(h, core, c.link);
}

fn row(h: &mut Hasher, core: &Core, r: &Row) {
    h.bool(r.wrapped);
    match r.anchor {
        None => h.u8(0),
        Some(key) => {
            h.u8(1);
            h.u64(key.0);
        }
    }
    h.u64(r.cells.len() as u64);
    for c in &r.cells {
        cell(h, core, c);
    }
}

fn cursor(h: &mut Hasher, core: &Core, c: &CursorState) {
    h.u64(c.row as u64);
    h.u64(c.col as u64);
    h.bool(c.pending_wrap);
    h.color(c.pen.fg);
    h.color(c.pen.bg);
    h.color(c.pen.underline_color);
    h.u64(u64::from(c.pen.flags));
    link(h, core, c.pen.link);
    for cs in c.charsets {
        h.u8(cs as u8);
    }
}

fn screen(h: &mut Hasher, core: &Core, s: &Screen) {
    h.u64(s.history as u64);
    h.u64(s.buf.len() as u64);
    for r in &s.buf {
        row(h, core, r);
    }
    cursor(h, core, &s.cursor);
    cursor(h, core, &s.saved);
    h.bytes(&s.kitty);
}

pub(crate) fn state_hash(core: &Core, parser: &Parser) -> [u8; 32] {
    let mut h = Hasher::new();
    h.bytes(b"auk-term state v1");
    h.u64(core.cols as u64);
    h.u64(core.rows as u64);
    h.u64(core.config.scrollback as u64);
    screen(&mut h, core, &core.main);
    screen(&mut h, core, &core.alt);
    h.bool(core.alt_active);
    h.u64(u64::from(core.modes));
    h.u8(match core.mouse {
        MouseMode::Off => 0,
        MouseMode::Press => 1,
        MouseMode::PressRelease => 2,
        MouseMode::ButtonMotion => 3,
        MouseMode::AnyMotion => 4,
    });
    h.u8(core.mouse_encoding as u8);
    h.u64(core.top as u64);
    h.u64(core.bot as u64);
    for &t in &core.tabs {
        h.bool(t);
    }
    h.u64(core.active_charset as u64);
    match core.cursor_style {
        None => h.u8(0),
        Some((shape, blink)) => {
            h.u8(match shape {
                CursorShape::Block => 1,
                CursorShape::Underline => 2,
                CursorShape::Bar => 3,
            });
            h.bool(blink);
        }
    }
    h.bytes(core.title.as_bytes());
    h.bytes(core.icon.as_bytes());
    h.u64(core.title_stack.len() as u64);
    for t in &core.title_stack {
        h.bytes(t.as_bytes());
    }
    h.u64(core.overrides.len() as u64);
    for &(i, rgb) in &core.overrides {
        h.u64(u64::from(i));
        h.color(Color::Rgb(rgb));
    }
    match core.preceding {
        None => h.u8(0),
        Some(c) => {
            h.u8(1);
            h.u64(u64::from(u32::from(c)));
        }
    }
    parser.hash_into(&mut h);
    h.finish()
}
