//! The VT parser: Paul Williams' DEC ANSI state machine (<https://vt100.net/emu/dec_ansi_parser>).
//!
//! Vendored from the `vte` crate, version 0.15.0 (Apache-2.0 OR MIT, Copyright (c) 2016 Joe Wilm
//! and contributors), and changed for Auk:
//!
//! - **APC strings** (§6.1). `vte` swallows them; here `ESC _ tsp;…` strings are dispatched whole
//!   and other APC strings are dropped. A 0x9C byte inside an APC string is payload, the string
//!   ends at ST or BEL, and a string longer than [`APC_LIMIT`] bytes (counted after `ESC _`) is
//!   abandoned, the rest of it showing as text.
//! - **Inspectable state.** The parser records the bytes that brought it into its current
//!   sequence, so that a snapshot can replay them and a state hash can cover them (§15.2, §15.3).
//!   Bytes that don't change the state (executed C0 controls, digits past saturation, ignored
//!   bytes) are not recorded, which keeps the record small.
//! - **Batched printing.** Runs of printable text go to the performer as one `&str`, and the
//!   performer may stop part-way through a run when it raises an event.
//! - **No OSC parameter splitting** in the parser: the performer gets the raw OSC body.
//!
//! Everything else (UTF-8 decoding and its error handling, C1 handling, parameter and
//! intermediate limits, which bytes execute where) behaves as `vte` 0.15.0 does, so that the
//! grid matches `alacritty_terminal`, which is built on it, except for one fix: a UTF-8 sequence
//! split across calls no longer swallows the bytes after it (see `advance_partial_utf8`), so the
//! result never depends on how the input is split (§15.2).

use std::str;

const MAX_INTERMEDIATES: usize = 2;
pub(crate) const MAX_PARAMS: usize = 32;
/// The longest APC string, counted after `ESC _` (§6.1).
pub(crate) const APC_LIMIT: usize = 262_144;
/// The longest OSC body kept; later bytes are dropped (as `vte` does without `std`).
pub(crate) const OSC_LIMIT: usize = 262_144;
/// The prefix that routes an APC string to TSP.
const TSP_PREFIX: &[u8] = b"tsp;";

/// CSI and DCS parameters with optional `:` subparameters (from `vte::Params`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Params {
    /// For each parameter's first slot, how many slots it spans (itself plus its subparameters).
    subparams: [u8; MAX_PARAMS],
    params: [u16; MAX_PARAMS],
    current_subparams: u8,
    len: usize,
}

impl Params {
    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn iter(&self) -> ParamsIter<'_> {
        ParamsIter { params: self, index: 0 }
    }

    fn is_full(&self) -> bool {
        self.len == MAX_PARAMS
    }

    fn clear(&mut self) {
        self.current_subparams = 0;
        self.len = 0;
    }

    fn push(&mut self, item: u16) {
        self.subparams[self.len - self.current_subparams as usize] = self.current_subparams + 1;
        self.params[self.len] = item;
        self.current_subparams = 0;
        self.len += 1;
    }

    fn extend(&mut self, item: u16) {
        self.subparams[self.len - self.current_subparams as usize] = self.current_subparams + 1;
        self.params[self.len] = item;
        self.current_subparams += 1;
        self.len += 1;
    }
}

/// Iterates parameters; each item is a parameter followed by its subparameters.
#[derive(Debug)]
pub(crate) struct ParamsIter<'a> {
    params: &'a Params,
    index: usize,
}

impl<'a> Iterator for ParamsIter<'a> {
    type Item = &'a [u16];

    fn next(&mut self) -> Option<&'a [u16]> {
        if self.index >= self.params.len {
            return None;
        }
        let n = self.params.subparams[self.index] as usize;
        let item = &self.params.params[self.index..self.index + n];
        self.index += n;
        Some(item)
    }
}

/// Receives what the parser recognizes.
pub(crate) trait Perform {
    /// Printable text: no C0 or C1 controls, though it may hold DEL. Returns how many bytes of
    /// `text` were handled; less than all of it only when an event arose part-way (see
    /// [`Perform::terminated`]).
    fn print(&mut self, text: &str) -> usize;
    /// A run of printable ASCII (0x20–0x7E), the common case. Returns as `print` does.
    fn print_ascii(&mut self, text: &[u8]) -> usize;
    /// A C0 control, or a C1 control that arrived as a raw byte or as UTF-8.
    fn execute(&mut self, byte: u8);
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: u8);
    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8);
    /// An OSC string's body, unsplit.
    fn osc_dispatch(&mut self, body: &[u8], bell_terminated: bool);
    /// A complete `ESC _ tsp;…` string: the bytes after `ESC _`.
    fn apc_dispatch(&mut self, body: &mut Vec<u8>);
    /// Whether the parser must stop after the current byte.
    fn terminated(&self) -> bool;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum State {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsIgnore,
    DcsPassthrough,
    OscString,
    /// SOS and PM strings, which are ignored.
    SosPmString,
    /// An APC string's body.
    Apc,
    /// An ESC inside an APC string: `\` ends it, anything else abandons it.
    ApcEscape,
}

/// What is known about an APC string while it arrives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ApcKind {
    /// Still matching the `tsp;` prefix.
    #[default]
    Undecided,
    Tsp,
    /// Dropped; only its length is kept.
    Other,
}

/// The parser's whole state. `Clone` and comparable, for tests.
#[derive(Clone, Debug, Default)]
pub(crate) struct Parser {
    state: State,
    intermediates: [u8; MAX_INTERMEDIATES],
    intermediate_idx: usize,
    params: Params,
    param: u16,
    ignoring: bool,
    /// The bytes since the sequence began that changed the state, for replay (see module docs).
    /// Used by the escape, CSI, DCS and SOS/PM states.
    seq: Vec<u8>,
    /// The OSC body so far.
    osc: Vec<u8>,
    /// The APC body so far (kept only for `tsp;` strings), and its length.
    apc: Vec<u8>,
    apc_len: usize,
    apc_kind: ApcKind,
    partial_utf8: [u8; 4],
    partial_utf8_len: usize,
}

impl Parser {
    pub(crate) fn new() -> Parser {
        Parser::default()
    }

    /// Parses `bytes` until they run out or the performer reports `terminated`. Returns how many
    /// bytes were consumed.
    pub(crate) fn advance<P: Perform>(&mut self, performer: &mut P, bytes: &[u8]) -> usize {
        let mut i = 0;
        if self.partial_utf8_len != 0 {
            i += self.advance_partial_utf8(performer, bytes);
        }
        while i < bytes.len() && !performer.terminated() {
            if self.state == State::Ground {
                i += self.advance_ground(performer, &bytes[i..]);
            } else {
                let byte = bytes[i];
                i += 1;
                self.change_state(performer, byte);
            }
        }
        i
    }

    /// Writes bytes that bring a fresh parser (in the ground state) to this parser's state.
    pub(crate) fn replay_bytes(&self, out: &mut Vec<u8>) {
        match self.state {
            State::Ground => {}
            State::OscString => {
                out.extend_from_slice(b"\x1b]");
                out.extend_from_slice(&self.osc);
            }
            State::Apc | State::ApcEscape => {
                out.extend_from_slice(b"\x1b_");
                match self.apc_kind {
                    ApcKind::Undecided | ApcKind::Tsp => out.extend_from_slice(&self.apc),
                    // Any first byte other than `t` decides "other"; only the length matters.
                    ApcKind::Other => out.resize(out.len() + self.apc_len, b'x'),
                }
                if self.state == State::ApcEscape {
                    out.push(0x1b);
                }
            }
            _ => out.extend_from_slice(&self.seq),
        }
        out.extend_from_slice(&self.partial_utf8[..self.partial_utf8_len]);
    }

    /// Feeds the replicated parser state into a hash.
    pub(crate) fn hash_into(&self, h: &mut crate::hash::Hasher) {
        h.u8(self.state as u8);
        match self.state {
            State::Ground => {}
            State::OscString => h.bytes(&self.osc),
            State::Apc | State::ApcEscape => {
                h.u8(self.apc_kind as u8);
                h.u64(self.apc_len as u64);
                h.bytes(&self.apc);
            }
            _ => h.bytes(&self.seq),
        }
        h.bytes(&self.partial_utf8[..self.partial_utf8_len]);
    }

    fn change_state<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match self.state {
            State::Ground => unreachable!("ground bytes go through advance_ground"),
            State::Escape => self.advance_esc(performer, byte),
            State::EscapeIntermediate => self.advance_esc_intermediate(performer, byte),
            State::CsiEntry => self.advance_csi_entry(performer, byte),
            State::CsiParam => self.advance_csi_param(performer, byte),
            State::CsiIntermediate => self.advance_csi_intermediate(performer, byte),
            State::CsiIgnore => self.advance_csi_ignore(performer, byte),
            State::DcsEntry => self.advance_dcs_entry(performer, byte),
            State::DcsParam => self.advance_dcs_param(performer, byte),
            State::DcsIntermediate => self.advance_dcs_intermediate(performer, byte),
            State::DcsIgnore | State::SosPmString => self.anywhere(performer, byte),
            State::DcsPassthrough => self.advance_dcs_passthrough(performer, byte),
            State::OscString => self.advance_osc_string(performer, byte),
            State::Apc => self.advance_apc(performer, byte),
            State::ApcEscape => self.advance_apc_escape(performer, byte),
        }
    }

    /// Moves to `state`, recording `byte` as part of the sequence.
    fn enter(&mut self, state: State, byte: u8) {
        self.seq.push(byte);
        self.state = state;
    }

    fn enter_ground(&mut self) {
        self.state = State::Ground;
        self.seq.clear();
    }

    fn enter_escape(&mut self) {
        self.reset_params();
        self.seq.clear();
        self.seq.push(0x1b);
        self.state = State::Escape;
    }

    fn advance_esc<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => performer.execute(byte),
            0x20..=0x2F => {
                self.action_collect(byte);
                self.state = State::EscapeIntermediate;
            }
            0x50 => {
                self.reset_params();
                self.enter(State::DcsEntry, byte);
            }
            0x58 | 0x5E => self.enter(State::SosPmString, byte),
            0x5B => {
                self.reset_params();
                self.enter(State::CsiEntry, byte);
            }
            0x5D => {
                self.osc.clear();
                self.seq.clear();
                self.state = State::OscString;
            }
            0x5F => {
                self.apc.clear();
                self.apc_len = 0;
                self.apc_kind = ApcKind::Undecided;
                self.seq.clear();
                self.state = State::Apc;
            }
            0x30..=0x7E => {
                self.enter_ground();
                performer.esc_dispatch(self.intermediates(), self.ignoring, byte);
            }
            0x18 | 0x1A => {
                self.enter_ground();
                performer.execute(byte);
            }
            _ => {}
        }
    }

    fn advance_esc_intermediate<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => performer.execute(byte),
            0x20..=0x2F => self.action_collect(byte),
            0x30..=0x7E => {
                self.enter_ground();
                performer.esc_dispatch(self.intermediates(), self.ignoring, byte);
            }
            0x7F => {}
            _ => self.anywhere(performer, byte),
        }
    }

    fn advance_csi_entry<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => performer.execute(byte),
            0x20..=0x2F => {
                self.action_collect(byte);
                self.state = State::CsiIntermediate;
            }
            0x30..=0x39 => {
                self.enter(State::CsiParam, byte);
                self.action_paramnext(byte, false);
            }
            0x3A => {
                self.enter(State::CsiParam, byte);
                self.action_subparam(false);
            }
            0x3B => {
                self.enter(State::CsiParam, byte);
                self.action_param(false);
            }
            0x3C..=0x3F => {
                self.action_collect(byte);
                self.state = State::CsiParam;
            }
            0x40..=0x7E => self.action_csi_dispatch(performer, byte),
            _ => self.anywhere(performer, byte),
        }
    }

    fn advance_csi_param<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => performer.execute(byte),
            0x20..=0x2F => {
                self.action_collect(byte);
                self.state = State::CsiIntermediate;
            }
            0x30..=0x39 => self.action_paramnext(byte, true),
            0x3A => self.action_subparam(true),
            0x3B => self.action_param(true),
            0x3C..=0x3F => self.enter(State::CsiIgnore, byte),
            0x40..=0x7E => self.action_csi_dispatch(performer, byte),
            0x7F => {}
            _ => self.anywhere(performer, byte),
        }
    }

    fn advance_csi_intermediate<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => performer.execute(byte),
            0x20..=0x2F => self.action_collect(byte),
            0x30..=0x3F => self.enter(State::CsiIgnore, byte),
            0x40..=0x7E => self.action_csi_dispatch(performer, byte),
            _ => self.anywhere(performer, byte),
        }
    }

    fn advance_csi_ignore<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => performer.execute(byte),
            0x20..=0x3F | 0x7F => {}
            0x40..=0x7E => self.enter_ground(),
            _ => self.anywhere(performer, byte),
        }
    }

    fn advance_dcs_entry<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F | 0x7F => {}
            0x20..=0x2F => {
                self.action_collect(byte);
                self.state = State::DcsIntermediate;
            }
            0x30..=0x39 => {
                self.enter(State::DcsParam, byte);
                self.action_paramnext(byte, false);
            }
            0x3A => {
                self.enter(State::DcsParam, byte);
                self.action_subparam(false);
            }
            0x3B => {
                self.enter(State::DcsParam, byte);
                self.action_param(false);
            }
            0x3C..=0x3F => {
                self.action_collect(byte);
                self.state = State::DcsParam;
            }
            0x40..=0x7E => self.action_hook(byte),
            _ => self.anywhere(performer, byte),
        }
    }

    fn advance_dcs_param<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F | 0x7F => {}
            0x20..=0x2F => {
                self.action_collect(byte);
                self.state = State::DcsIntermediate;
            }
            0x30..=0x39 => self.action_paramnext(byte, true),
            0x3A => self.action_subparam(true),
            0x3B => self.action_param(true),
            0x3C..=0x3F => self.enter(State::DcsIgnore, byte),
            0x40..=0x7E => self.action_hook(byte),
            _ => self.anywhere(performer, byte),
        }
    }

    fn advance_dcs_intermediate<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F | 0x7F => {}
            0x20..=0x2F => self.action_collect(byte),
            0x30..=0x3F => self.enter(State::DcsIgnore, byte),
            0x40..=0x7E => self.action_hook(byte),
            _ => self.anywhere(performer, byte),
        }
    }

    fn advance_dcs_passthrough<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x18 | 0x1A => {
                self.enter_ground();
                performer.execute(byte);
            }
            0x1B => self.enter_escape(),
            0x9C => self.enter_ground(),
            // The string's data, which nothing uses.
            _ => {}
        }
    }

    fn advance_osc_string<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x06 | 0x08..=0x17 | 0x19 | 0x1C..=0x1F => {}
            0x07 => {
                self.state = State::Ground;
                self.osc_end(performer, true);
            }
            0x18 | 0x1A => {
                self.state = State::Ground;
                self.osc_end(performer, false);
                performer.execute(byte);
            }
            0x1B => {
                self.enter_escape();
                self.osc_end(performer, false);
            }
            _ => {
                if self.osc.len() < OSC_LIMIT {
                    self.osc.push(byte);
                }
            }
        }
    }

    fn osc_end<P: Perform>(&mut self, performer: &mut P, bell: bool) {
        let body = std::mem::take(&mut self.osc);
        performer.osc_dispatch(&body, bell);
        self.osc = body;
        self.osc.clear();
    }

    fn advance_apc<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x07 => {
                self.state = State::Ground;
                self.apc_end(performer);
            }
            0x18 | 0x1A => {
                self.state = State::Ground;
                self.apc.clear();
                performer.execute(byte);
            }
            0x1B => self.state = State::ApcEscape,
            _ => self.apc_put(performer, byte),
        }
    }

    fn advance_apc_escape<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        if byte == b'\\' {
            self.state = State::Ground;
            self.apc_end(performer);
        } else {
            // An ESC that doesn't start ST abandons the string and starts a new sequence.
            self.apc.clear();
            self.enter_escape();
            self.advance_esc(performer, byte);
        }
    }

    fn apc_put<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        if self.apc_len == APC_LIMIT {
            // Too long: abandon the string. This byte and the rest show as text.
            self.apc.clear();
            self.state = State::Ground;
            self.advance_ground(performer, &[byte]);
            return;
        }
        self.apc_len += 1;
        match self.apc_kind {
            ApcKind::Other => {}
            ApcKind::Tsp => self.apc.push(byte),
            ApcKind::Undecided => {
                self.apc.push(byte);
                let n = self.apc.len();
                if self.apc[n - 1] != TSP_PREFIX[n - 1] {
                    self.apc_kind = ApcKind::Other;
                    self.apc.clear();
                } else if n == TSP_PREFIX.len() {
                    self.apc_kind = ApcKind::Tsp;
                }
            }
        }
    }

    fn apc_end<P: Perform>(&mut self, performer: &mut P) {
        if self.apc_kind == ApcKind::Tsp {
            performer.apc_dispatch(&mut self.apc);
        }
        self.apc.clear();
    }

    fn anywhere<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x18 | 0x1A => {
                self.enter_ground();
                performer.execute(byte);
            }
            0x1B => self.enter_escape(),
            _ => {}
        }
    }

    fn intermediates(&self) -> &[u8] {
        &self.intermediates[..self.intermediate_idx]
    }

    fn reset_params(&mut self) {
        self.intermediate_idx = 0;
        self.ignoring = false;
        self.param = 0;
        self.params.clear();
    }

    fn action_collect(&mut self, byte: u8) {
        if self.intermediate_idx == MAX_INTERMEDIATES {
            if !self.ignoring {
                self.ignoring = true;
                self.seq.push(byte);
            }
        } else {
            self.intermediates[self.intermediate_idx] = byte;
            self.intermediate_idx += 1;
            self.seq.push(byte);
        }
    }

    /// `record` is false when the caller already recorded the byte.
    fn action_subparam(&mut self, record: bool) {
        if self.params.is_full() {
            self.ignore(record, b':');
        } else {
            self.params.extend(self.param);
            self.param = 0;
            if record {
                self.seq.push(b':');
            }
        }
    }

    fn action_param(&mut self, record: bool) {
        if self.params.is_full() {
            self.ignore(record, b';');
        } else {
            self.params.push(self.param);
            self.param = 0;
            if record {
                self.seq.push(b';');
            }
        }
    }

    fn action_paramnext(&mut self, byte: u8, record: bool) {
        if self.params.is_full() {
            self.ignore(record, byte);
        } else {
            let old = self.param;
            self.param = self.param.saturating_mul(10).saturating_add(u16::from(byte - b'0'));
            // A digit that leaves the value as it was (a leading zero, or past saturation)
            // changes nothing once the parameter has begun.
            let begun = matches!(self.seq.last(), Some(b'0'..=b'9'));
            if record && (self.param != old || !begun) {
                self.seq.push(byte);
            }
        }
    }

    fn ignore(&mut self, record: bool, byte: u8) {
        if !self.ignoring {
            self.ignoring = true;
            if record {
                self.seq.push(byte);
            }
        }
    }

    fn action_csi_dispatch<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        if self.params.is_full() {
            self.ignoring = true;
        } else {
            self.params.push(self.param);
        }
        self.enter_ground();
        performer.csi_dispatch(&self.params, &self.intermediates[..self.intermediate_idx], self.ignoring, byte);
    }

    /// The start of a DCS string's data. No DCS function is supported, so the data is dropped.
    fn action_hook(&mut self, byte: u8) {
        self.enter(State::DcsPassthrough, byte);
    }

    /// The ground state: printable runs, C0 controls and UTF-8.
    fn advance_ground<P: Perform>(&mut self, performer: &mut P, bytes: &[u8]) -> usize {
        let mut i = 0;
        while i < bytes.len() {
            let byte = bytes[i];
            match byte {
                0x1B => {
                    self.enter_escape();
                    return i + 1;
                }
                0x00..=0x1A | 0x1C..=0x1F => {
                    performer.execute(byte);
                    i += 1;
                }
                0x20..=0x7E => {
                    let start = i;
                    let end = start
                        + bytes[start..]
                            .iter()
                            .position(|&b| !(0x20..=0x7E).contains(&b))
                            .unwrap_or(bytes.len() - start);
                    i = start + performer.print_ascii(&bytes[start..end]);
                    if i < end {
                        return i;
                    }
                }
                0x7F => {
                    performer.print("\x7f");
                    i += 1;
                }
                _ => {
                    let used = self.advance_utf8(performer, &bytes[i..]);
                    i += used;
                }
            }
            if performer.terminated() {
                return i;
            }
        }
        i
    }

    /// Non-ASCII text starting at `bytes[0]`, up to the next C0 control or ESC. Returns the bytes
    /// consumed. Mirrors `vte`'s handling of invalid and partial UTF-8.
    fn advance_utf8<P: Perform>(&mut self, performer: &mut P, bytes: &[u8]) -> usize {
        let end = bytes.iter().position(|&b| b < 0x20).unwrap_or(bytes.len());
        match str::from_utf8(&bytes[..end]) {
            Ok(text) => print_text(performer, text),
            Err(err) => {
                let valid = err.valid_up_to();
                if valid > 0 {
                    let text = str::from_utf8(&bytes[..valid]).expect("valid prefix");
                    let done = print_text(performer, text);
                    if done < valid {
                        return done;
                    }
                    if performer.terminated() {
                        return valid;
                    }
                }
                match err.error_len() {
                    Some(len) => {
                        let first = bytes[valid];
                        if len == 1 && first <= 0x9F {
                            performer.execute(first);
                        } else {
                            performer.print("\u{FFFD}");
                        }
                        valid + len
                    }
                    None if end < bytes.len() => {
                        // A sequence cut short by a control or ESC.
                        performer.print("\u{FFFD}");
                        end
                    }
                    None => {
                        // Cut short by the end of the input: keep it for the next call.
                        let rest = &bytes[valid..];
                        self.partial_utf8[..rest.len()].copy_from_slice(rest);
                        self.partial_utf8_len = rest.len();
                        bytes.len()
                    }
                }
            }
        }
    }

    /// Completes a UTF-8 sequence left over from the previous call. Returns the bytes consumed.
    fn advance_partial_utf8<P: Perform>(&mut self, performer: &mut P, bytes: &[u8]) -> usize {
        let old = self.partial_utf8_len;
        let to_copy = bytes.len().min(4 - old);
        self.partial_utf8[old..old + to_copy].copy_from_slice(&bytes[..to_copy]);
        self.partial_utf8_len += to_copy;
        match str::from_utf8(&self.partial_utf8[..self.partial_utf8_len]) {
            Ok(text) => {
                let c = text.chars().next().expect("non-empty");
                self.partial_utf8_len = 0;
                let mut buf = [0; 4];
                print_text(performer, c.encode_utf8(&mut buf));
                c.len_utf8() - old
            }
            Err(err) => {
                let valid = err.valid_up_to();
                if valid > 0 {
                    let text = str::from_utf8(&self.partial_utf8[..valid]).expect("valid prefix");
                    let c = text.chars().next().expect("non-empty");
                    self.partial_utf8_len = 0;
                    let mut buf = [0; 4];
                    print_text(performer, c.encode_utf8(&mut buf));
                    // `vte` returns `valid - old` here, skipping whatever valid bytes followed the
                    // character, so its result depended on where the input was split. Only the
                    // character is consumed.
                    return c.len_utf8() - old;
                }
                match err.error_len() {
                    Some(len) => {
                        self.partial_utf8_len = 0;
                        performer.print("\u{FFFD}");
                        len - old
                    }
                    None => to_copy,
                }
            }
        }
    }
}

/// Prints valid text, sending C1 controls (U+0080–U+009F) to `execute` as `vte` does. Returns
/// the bytes handled.
fn print_text<P: Perform>(performer: &mut P, text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        // C1 controls are encoded as C2 80..C2 9F.
        if bytes[i] == 0xC2 && i + 1 < bytes.len() && (0x80..=0x9F).contains(&bytes[i + 1]) {
            if start < i {
                let done = performer.print(&text[start..i]);
                if done < i - start {
                    return start + done;
                }
            }
            performer.execute(bytes[i + 1]);
            i += 2;
            start = i;
            if performer.terminated() {
                return i;
            }
        } else {
            i += 1;
        }
    }
    if start < bytes.len() {
        start + performer.print(&text[start..])
    } else {
        bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records what the parser dispatches.
    #[derive(Default)]
    struct Log(Vec<String>);

    impl Perform for Log {
        fn print(&mut self, text: &str) -> usize {
            self.0.push(format!("print {text:?}"));
            text.len()
        }
        fn print_ascii(&mut self, text: &[u8]) -> usize {
            self.0.push(format!("print {:?}", String::from_utf8_lossy(text)));
            text.len()
        }
        fn execute(&mut self, byte: u8) {
            self.0.push(format!("execute {byte:#x}"));
        }
        fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: u8) {
            let params: Vec<&[u16]> = params.iter().collect();
            self.0.push(format!("csi {params:?} {intermediates:?} {ignore} {}", action as char));
        }
        fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
            self.0.push(format!("esc {intermediates:?} {ignore} {}", byte as char));
        }
        fn osc_dispatch(&mut self, body: &[u8], bell: bool) {
            self.0.push(format!("osc {:?} {bell}", String::from_utf8_lossy(body)));
        }
        fn apc_dispatch(&mut self, body: &mut Vec<u8>) {
            self.0.push(format!("apc {:?}", String::from_utf8_lossy(body)));
        }
        fn terminated(&self) -> bool {
            false
        }
    }

    fn run(parser: &mut Parser, bytes: &[u8]) -> Vec<String> {
        let mut log = Log::default();
        let mut rest = bytes;
        while !rest.is_empty() {
            let n = parser.advance(&mut log, rest);
            rest = &rest[n..];
        }
        log.0
    }

    const SAMPLES: &[&[u8]] = &[
        b"\x1b[38;2;1;2;3m\x1b[?1049h\x1b[>1u\x1b[4:3m",
        b"\x1b[1\n2;3\x7fH\x1b[!p\x1b[1 q\x1b[?$p",
        b"\x1b]0;title\x07\x1b]8;id=1;http://x\x1b\\",
        b"\x1bP1;2$qm\x1b\\\x1bXsos\x1b\\\x1b^pm\x1b\\",
        b"\x1b_tsp;{\"a\":1}\x1b\\\x1b_other\x07\x1b_ts\x1bx",
        b"\x1b(0\x1b#8\x1b \x21\x22F\x1b%G",
        "中e\u{301}😀\u{9c}\u{85}".as_bytes(),
        b"\xe4\xb8\x1b[m\xf0\x9f\x98\xff\xc2",
        b"\x1b[;;;:1:2;99999999;?h\x1b[1;2;3;4;5;6;7;8;9;10;11;12;13;14;15;16;17;18;19;20;21;22;23;24;25;26;27;28;29;30;31;32;33;34m",
    ];

    /// Replaying the record of any prefix leaves a parser that treats the rest identically.
    #[test]
    fn replay_reproduces_every_prefix() {
        for sample in SAMPLES {
            for cut in 0..=sample.len() {
                let mut original = Parser::new();
                run(&mut original, &sample[..cut]);
                let mut replay = Vec::new();
                original.replay_bytes(&mut replay);
                let mut rebuilt = Parser::new();
                run(&mut rebuilt, &replay);
                let rest = &sample[cut..];
                assert_eq!(
                    run(&mut original, rest),
                    run(&mut rebuilt, rest),
                    "{sample:?} cut at {cut}, replay {replay:?}"
                );
            }
        }
    }

    /// The result doesn't depend on how the input is split.
    #[test]
    fn splitting_does_not_matter() {
        for sample in SAMPLES {
            let whole = run(&mut Parser::new(), sample);
            for size in 1..5 {
                let mut parser = Parser::new();
                let mut log = Vec::new();
                for chunk in sample.chunks(size) {
                    log.extend(run(&mut parser, chunk));
                }
                // Text may arrive in more pieces; compare it joined.
                let join = |l: Vec<String>| -> Vec<String> {
                    let mut out: Vec<String> = Vec::new();
                    for e in l {
                        match (out.last_mut(), e.strip_prefix("print ")) {
                            (Some(last), Some(t)) if last.starts_with("print ") => {
                                last.truncate(last.len() - 1);
                                last.push_str(&t[1..]);
                            }
                            _ => out.push(e),
                        }
                    }
                    out
                };
                assert_eq!(join(whole.clone()), join(log), "{sample:?} in chunks of {size}");
            }
        }
    }

    #[test]
    fn the_record_stays_small() {
        let mut parser = Parser::new();
        let mut bytes = b"\x1b[".to_vec();
        for _ in 0..100 {
            bytes.extend_from_slice(b"0000123456789;");
        }
        bytes.extend_from_slice(&[b'1'; 10_000]);
        run(&mut parser, &bytes);
        assert!(parser.seq.len() < 300, "{}", parser.seq.len());
        let mut ignored = b"\x1b[?".to_vec();
        ignored.extend_from_slice(&[b'<'; 10_000]);
        let mut parser = Parser::new();
        run(&mut parser, &ignored);
        assert!(parser.seq.len() < 8);
    }

    #[test]
    fn apc_limit_counts_after_esc_underscore() {
        let mut parser = Parser::new();
        let mut bytes = b"\x1b_tsp;".to_vec();
        bytes.resize(2 + APC_LIMIT, b'x');
        bytes.extend_from_slice(b"yz");
        let log = run(&mut parser, &bytes);
        // The byte past the limit is printed on its own, then the rest.
        assert_eq!(log, vec!["print \"y\"".to_owned(), "print \"z\"".to_owned()]);
    }
}
