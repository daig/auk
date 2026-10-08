//! OSC strings: titles, colors and their queries, OSC 8 links, shell integration marks, user
//! variables, and Auk's private `OSC 6464` sequences.

use crate::core::{Core, Links};
use crate::screen::fix_wide;
use crate::{AnchorKey, Event, Facts, Flags, Mark, Rgb};

impl Core {
    // OSC.

    pub(crate) fn osc(&mut self, body: &[u8], bell: bool) {
        // Split as `vte` does: at most 16 parameters, later text dropped.
        let mut params: Vec<&[u8]> = body.split(|&b| b == b';').collect();
        params.truncate(16);
        if params[0].is_empty() {
            return;
        }
        let terminator: &[u8] = if bell { b"\x07" } else { b"\x1b\\" };
        match params[0] {
            b"0" | b"1" | b"2" if params.len() >= 2 => {
                let text = params[1..]
                    .iter()
                    .filter_map(|p| std::str::from_utf8(p).ok())
                    .collect::<Vec<_>>()
                    .join(";")
                    .trim()
                    .to_owned();
                if params[0] != b"2" {
                    self.icon = text.clone();
                }
                if params[0] != b"1" {
                    self.title = text.clone();
                    self.events.push_back(Event::Title(text));
                }
            }
            b"4" => {
                if params.len() <= 1 || params.len().is_multiple_of(2) {
                    return;
                }
                for pair in params[1..].chunks(2) {
                    let Some(index) = parse_number(pair[0]) else {
                        continue;
                    };
                    if let Some(rgb) = xparse_color(pair[1]) {
                        self.set_override(u16::from(index), rgb);
                    } else if pair[1] == b"?" {
                        let rgb = self.color_value(u16::from(index));
                        self.color_reply(&format!("4;{index}"), rgb, terminator);
                    }
                }
            }
            b"7" if params.len() >= 2 => {
                let cwd = params[1..].join(&b';');
                self.events
                    .push_back(Event::Cwd(String::from_utf8_lossy(&cwd).into_owned()));
            }
            b"8" if params.len() > 2 => {
                let mut uri = String::from_utf8_lossy(params[2]).into_owned();
                for p in &params[3..] {
                    uri.push(';');
                    uri.push_str(&String::from_utf8_lossy(p));
                }
                let link = if uri.is_empty() {
                    0
                } else {
                    let id = params[1]
                        .split(|&b| b == b':')
                        .find_map(|kv| kv.strip_prefix(b"id="))
                        .and_then(|id| std::str::from_utf8(id).ok())
                        .map(str::to_owned);
                    self.intern_link(id, uri)
                };
                self.scr().cursor.pen.link = link;
            }
            b"10" | b"11" | b"12" if params.len() >= 2 => {
                let first = u16::from(parse_number(params[0]).expect("digits"));
                for (code, p) in (first..).zip(&params[1..]) {
                    if code > 12 {
                        break;
                    }
                    let index = 256 + code - 10;
                    if let Some(rgb) = xparse_color(p) {
                        self.set_override(index, rgb);
                    } else if *p == b"?" {
                        let rgb = self.color_value(index);
                        self.color_reply(&code.to_string(), rgb, terminator);
                    }
                }
            }
            b"104" => {
                if params.len() == 1 || params[1].is_empty() {
                    self.overrides.retain(|&(i, _)| i > 255);
                    return;
                }
                for p in &params[1..] {
                    if let Some(index) = parse_number(p) {
                        self.overrides.retain(|&(i, _)| i != u16::from(index));
                    }
                }
            }
            b"110" => self.overrides.retain(|&(i, _)| i != 256),
            b"111" => self.overrides.retain(|&(i, _)| i != 257),
            b"112" => self.overrides.retain(|&(i, _)| i != 258),
            b"133" if params.len() >= 2 => {
                if let Some(mark) = parse_mark(&params[1..]) {
                    if mark == Mark::PromptStart && !self.alt_active {
                        self.scr().cursor_row_mut().prompt = true;
                    }
                    self.events.push_back(Event::Mark(mark));
                }
            }
            b"1337" if params.len() >= 2 => {
                // `SetUserVar=<name>` with no value unsets it (omp does this when its session is
                // cleared); it arrives as an empty value.
                if let Some(var) = params[1].strip_prefix(b"SetUserVar=") {
                    let (name, value) = match var.iter().position(|&b| b == b'=') {
                        Some(eq) => (&var[..eq], decode_base64(&var[eq + 1..])),
                        None => (var, Some(Vec::new())),
                    };
                    if let Some(value) = value
                        && !name.is_empty()
                    {
                        let name = String::from_utf8_lossy(name).into_owned();
                        self.events.push_back(Event::UserVar { name, value });
                    }
                }
            }
            b"6464" if params.len() >= 2 => self.private_osc(&params[1..]),
            _ => {}
        }
    }

    fn intern_link(&mut self, id: Option<String>, uri: String) -> u32 {
        if self.links.len() >= 4096 {
            self.compact_links();
        }
        self.links.intern(id, uri)
    }

    /// Drops links no cell or pen uses, renumbering the rest in order.
    fn compact_links(&mut self) {
        let mut used = vec![false; self.links.len()];
        used[0] = true;
        for screen in [&self.main, &self.alt] {
            for row in &screen.buf {
                for cell in &row.cells {
                    used[cell.link as usize] = true;
                }
            }
            used[screen.cursor.pen.link as usize] = true;
            used[screen.saved.pen.link as usize] = true;
        }
        if used.iter().all(|&u| u) {
            return;
        }
        let mut map = vec![0u32; used.len()];
        let mut links = Links::new();
        for (old, &u) in used.iter().enumerate().skip(1) {
            if u {
                map[old] = links.intern(self.links.ids[old].clone(), self.links.uris[old].clone());
            }
        }
        for screen in [&mut self.main, &mut self.alt] {
            for row in &mut screen.buf {
                for cell in &mut row.cells {
                    cell.link = map[cell.link as usize];
                }
            }
            screen.cursor.pen.link = map[screen.cursor.pen.link as usize];
            screen.saved.pen.link = map[screen.saved.pen.link as usize];
        }
        self.links = links;
    }

    fn set_override(&mut self, index: u16, rgb: Rgb) {
        match self.overrides.binary_search_by_key(&index, |&(i, _)| i) {
            Ok(at) => self.overrides[at].1 = rgb,
            Err(at) => self.overrides.insert(at, (index, rgb)),
        }
    }

    /// The color an OSC query reports: the override, else the client's theme.
    fn color_value(&self, index: u16) -> Rgb {
        if let Ok(at) = self.overrides.binary_search_by_key(&index, |&(i, _)| i) {
            return self.overrides[at].1;
        }
        match index {
            0..=15 => self.facts.palette[usize::from(index)],
            16..=255 => xterm_color(index as u8),
            256 => self.facts.foreground,
            257 => self.facts.background,
            _ => self.facts.cursor,
        }
    }

    fn color_reply(&mut self, prefix: &str, rgb: Rgb, terminator: &[u8]) {
        let Rgb(r, g, b) = rgb;
        let mut text =
            format!("\x1b]{prefix};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}").into_bytes();
        text.extend_from_slice(terminator);
        self.reply(&text);
    }

    /// Auk's private `OSC 6464` sequences, which snapshots use for state plain VT can't set.
    fn private_osc(&mut self, params: &[&[u8]]) {
        let number =
            |p: Option<&&[u8]>| -> Option<u64> { std::str::from_utf8(p?).ok()?.parse().ok() };
        match params[0] {
            b"anchor" => {
                if let Some(key) = number(params.get(1)) {
                    self.mark_anchor(AnchorKey(key));
                }
            }
            // The cursor row's flags: bit 0 wrapped, bit 1 a leading spacer in the last column,
            // bit 2 a prompt started there.
            b"row" => {
                let bits = number(params.get(1)).unwrap_or(0);
                let cols = self.cols;
                let row = self.scr().cursor_row_mut();
                row.touch_all();
                row.wrapped = bits & 1 != 0;
                row.prompt = bits & 4 != 0;
                if bits & 2 != 0 {
                    row.cells[cols - 1].flags.0 |= Flags::LEADING_SPACER;
                }
                fix_wide(row);
            }
            // The cursor, placed absolutely: row, column, pending wrap.
            b"cursor" => {
                let row = number(params.get(1)).unwrap_or(0) as usize;
                let col = number(params.get(2)).unwrap_or(0) as usize;
                let pending = number(params.get(3)).unwrap_or(0) != 0;
                let (rows, cols) = (self.rows, self.cols);
                let s = self.scr();
                s.cursor.row = row.min(rows - 1);
                s.cursor.col = col.min(cols - 1);
                s.cursor.pending_wrap = pending;
            }
            // The character REP repeats, as a code point; none when absent.
            b"rep" => self.preceding = number(params.get(1)).and_then(|c| char::from_u32(c as u32)),
            // Zero-width characters appended to the cell before the cursor, as code points.
            b"zw" => {
                let s = self.scr();
                let mut col = s.cursor.col;
                if !s.cursor.pending_wrap {
                    col = col.saturating_sub(1);
                }
                let slot = s.cursor_row_mut();
                slot.touch(col + 1);
                let cell = &mut slot.cells[col];
                for p in &params[1..] {
                    if let Some(c) = std::str::from_utf8(p)
                        .ok()
                        .and_then(|p| p.parse().ok())
                        .and_then(char::from_u32)
                    {
                        if cell.text.is_empty() {
                            cell.text.push(' ');
                        }
                        cell.text.push(c);
                    }
                }
            }
            _ => {}
        }
    }
}

fn parse_mark(params: &[&[u8]]) -> Option<Mark> {
    let kind = params[0].first()?;
    match kind {
        b'A' => Some(Mark::PromptStart),
        b'B' => Some(Mark::PromptEnd),
        b'C' => {
            let param = |name: &[u8]| {
                params[1..]
                    .iter()
                    .find_map(|p| p.strip_prefix(name))
                    .map(|url| String::from_utf8_lossy(&percent_decode(url)).into_owned())
            };
            Some(Mark::CommandStart {
                cmdline: param(b"cmdline_url="),
                typed: param(b"typed_url="),
            })
        }
        b'D' => {
            let status = params
                .get(1)
                .and_then(|p| std::str::from_utf8(p).ok()?.parse().ok());
            Some(Mark::CommandEnd { status })
        }
        _ => None,
    }
}

fn percent_decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' && i + 2 < s.len() {
            let hex = std::str::from_utf8(&s[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(b) = hex {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// Standard base64, padding optional.
fn decode_base64(s: &[u8]) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    );
    engine.decode(s).ok()
}

/// An OSC 4 index (`vte`'s `parse_number`).
fn parse_number(input: &[u8]) -> Option<u8> {
    if input.is_empty() {
        return None;
    }
    let mut n: u8 = 0;
    for &c in input {
        let d = (c as char).to_digit(10)?;
        n = n.checked_mul(10)?.checked_add(d as u8)?;
    }
    Some(n)
}

/// XParseColor's `rgb:r/g/b` and `#rgb` forms (`vte`'s `xparse_color`).
pub(crate) fn xparse_color(color: &[u8]) -> Option<Rgb> {
    if let Some(rest) = color.strip_prefix(b"#") {
        let n = rest.len() / 3;
        let part = |s: &[u8]| -> Option<u8> {
            let v = usize::from_str_radix(std::str::from_utf8(s).ok()?, 16).ok()? << 4;
            Some((v >> (4 * s.len().saturating_sub(1))) as u8)
        };
        Some(Rgb(
            part(&rest[..n])?,
            part(&rest[n..2 * n])?,
            part(&rest[2 * n..])?,
        ))
    } else if let Some(rest) = color.strip_prefix(b"rgb:") {
        let parts: Vec<&str> = std::str::from_utf8(rest).ok()?.split('/').collect();
        if parts.len() != 3 {
            return None;
        }
        let scale = |s: &str| -> Option<u8> {
            if s.len() > 4 {
                return None;
            }
            let max = 16u32.pow(s.len() as u32) - 1;
            let v = u32::from_str_radix(s, 16).ok()?;
            Some((255 * v / max) as u8)
        };
        Some(Rgb(scale(parts[0])?, scale(parts[1])?, scale(parts[2])?))
    } else {
        None
    }
}

/// xterm's colors 16–255: the 6×6×6 cube, then the gray ramp.
pub(crate) fn xterm_color(index: u8) -> Rgb {
    if index >= 232 {
        let v = 8 + (index - 232) * 10;
        return Rgb(v, v, v);
    }
    let i = index - 16;
    let level = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
    Rgb(level(i / 36), level(i / 6 % 6), level(i % 6))
}

impl Default for Facts {
    fn default() -> Facts {
        const ANSI: [u32; 16] = [
            0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5,
            0x7f7f7f, 0xff0000, 0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
        ];
        let rgb = |v: u32| Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8);
        Facts {
            cell_px: (8, 16),
            foreground: rgb(0xe5e5e5),
            background: rgb(0x000000),
            cursor: rgb(0xe5e5e5),
            palette: ANSI.map(rgb),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_parse_as_xparsecolor() {
        assert_eq!(xparse_color(b"rgb:ff/80/00"), Some(Rgb(255, 128, 0)));
        assert_eq!(xparse_color(b"rgb:f/8/0"), Some(Rgb(255, 136, 0)));
        assert_eq!(xparse_color(b"rgb:ffff/0000/8080"), Some(Rgb(255, 0, 128)));
        assert_eq!(xparse_color(b"#ff8000"), Some(Rgb(255, 128, 0)));
        assert_eq!(xparse_color(b"#f80"), Some(Rgb(240, 128, 0)));
        assert_eq!(xparse_color(b"#fff000000"), Some(Rgb(255, 0, 0)));
        assert_eq!(xparse_color(b"rgb:ff/80"), None);
        assert_eq!(xparse_color(b"red"), None);
    }

    #[test]
    fn xterm_palette_beyond_16() {
        assert_eq!(xterm_color(16), Rgb(0, 0, 0));
        assert_eq!(xterm_color(21), Rgb(0, 0, 255));
        assert_eq!(xterm_color(231), Rgb(255, 255, 255));
        assert_eq!(xterm_color(232), Rgb(8, 8, 8));
        assert_eq!(xterm_color(255), Rgb(238, 238, 238));
    }

    #[test]
    fn marks_and_decoding() {
        assert_eq!(parse_mark(&[b"A"]), Some(Mark::PromptStart));
        assert_eq!(parse_mark(&[b"A", b"k=s"]), Some(Mark::PromptStart));
        assert_eq!(
            parse_mark(&[b"C", b"cmdline_url=echo%20%22hi%22%3B%20ls"]),
            Some(Mark::CommandStart {
                cmdline: Some("echo \"hi\"; ls".into()),
                typed: None
            })
        );
        assert_eq!(
            parse_mark(&[b"C"]),
            Some(Mark::CommandStart {
                cmdline: None,
                typed: None
            })
        );
        assert_eq!(
            parse_mark(&[b"D", b"130"]),
            Some(Mark::CommandEnd { status: Some(130) })
        );
        assert_eq!(
            parse_mark(&[b"C", b"cmdline_url=ls%20-la", b"typed_url=ll"]),
            Some(Mark::CommandStart {
                cmdline: Some("ls -la".into()),
                typed: Some("ll".into())
            })
        );
        assert_eq!(parse_mark(&[b"Z"]), None);
        assert_eq!(percent_decode(b"100%"), b"100%");
        assert_eq!(percent_decode(b"%zz%41"), b"%zzA");
        assert_eq!(decode_base64(b"aGk="), Some(b"hi".to_vec()));
        assert_eq!(decode_base64(b"aGk"), Some(b"hi".to_vec()));
        assert_eq!(decode_base64(b"!!"), None);
    }

    #[test]
    fn links_are_interned_and_compacted() {
        let mut links = Links::new();
        let a = links.intern(None, "https://a".into());
        let b = links.intern(Some("x".into()), "https://a".into());
        assert_ne!(a, b);
        assert_eq!(links.intern(None, "https://a".into()), a);

        // A program that opens many links: the table stays bounded, and links in use survive
        // compaction with their targets.
        let mut core = Core::new(4, 2, crate::Config::default());
        core.osc(b"8;;https://example/kept", true);
        let kept = core.scr().cursor.pen.link;
        core.main.row_mut(0).cells[0].link = kept;
        for i in 0..10_000 {
            core.osc(format!("8;;https://example/{i}").as_bytes(), true);
            assert!(core.links.len() <= 4097);
        }
        let cell_link = core.main.row(0).cells[0].link as usize;
        assert_eq!(core.links.uris[cell_link], "https://example/kept");
        let pen_link = core.scr().cursor.pen.link as usize;
        assert_eq!(core.links.uris[pen_link], "https://example/9999");
    }
}
