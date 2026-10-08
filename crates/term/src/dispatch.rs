//! What the parser dispatches: C0 controls, CSI and ESC sequences, and SGR (as `vte`'s
//! `ansi` module and `alacritty_terminal` interpret them, with the differences listed in
//! `tests/differential.rs`).

use crate::core::{Core, default_tabs, mode};
use crate::parser::{Params, Perform};
use crate::screen::{Charset, Pen};
use crate::{Color, CursorShape, Event, Flags, Rgb};

/// The deepest title stack (`CSI 22 t`).
const TITLE_STACK_DEPTH: usize = 16;
/// The terminal's version, for XTVERSION.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Reads parameter `p` (the first value of a parameter), with `default` for 0 or missing.
fn param_or(p: Option<&[u16]>, default: u16) -> u16 {
    match p {
        Some(&[v, ..]) if v != 0 => v,
        _ => default,
    }
}

impl Perform for Core {
    fn print(&mut self, text: &str) -> usize {
        self.print_text(text)
    }

    fn print_ascii(&mut self, text: &[u8]) -> usize {
        let charset = self.screen().cursor.charsets[self.active_charset];
        if charset == Charset::Ascii && !self.has(mode::INSERT) {
            self.print_ascii_run(text)
        } else {
            let text = std::str::from_utf8(text).expect("ASCII is UTF-8");
            self.print_text(text)
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x09 => self.put_tab(1),
            0x08 => self.backspace(),
            0x0D => self.carriage_return(),
            0x0A..=0x0C => {
                self.linefeed();
                if self.has(mode::NEWLINE) {
                    self.carriage_return();
                }
            }
            0x07 => self.events.push_back(Event::Bell),
            0x0F => self.active_charset = 0,
            0x0E => self.active_charset = 1,
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: u8) {
        if ignore || intermediates.len() > 2 {
            return;
        }
        let mut it = params.iter();
        let mut next = |default: u16| param_or(it.next(), default);
        match (action, intermediates) {
            (b'@', []) => self.insert_blank(usize::from(next(1))),
            (b'A', []) => self.move_up(usize::from(next(1))),
            (b'B' | b'e', []) => self.move_down(usize::from(next(1))),
            (b'b', []) => {
                if let Some(c) = self.preceding {
                    for _ in 0..next(1) {
                        self.print_char(c);
                    }
                }
            }
            (b'C' | b'a', []) => self.move_forward(usize::from(next(1))),
            (b'c', []) if next(0) == 0 => self.reply(b"\x1b[?62;22c"),
            (b'c', [b'>']) if next(0) == 0 => self.reply(b"\x1b[>1;10;0c"),
            (b'D', []) => self.move_backward(usize::from(next(1))),
            (b'd', []) => self.goto_line(i64::from(next(1)) - 1),
            (b'E', []) => {
                self.move_down(usize::from(next(1)));
                self.carriage_return();
            }
            (b'F', []) => {
                self.move_up(usize::from(next(1)));
                self.carriage_return();
            }
            (b'G' | b'`', []) => self.goto_col(usize::from(next(1)) - 1),
            (b'g', []) => match next(0) {
                0 => {
                    let col = self.screen().cursor.col;
                    self.tabs[col] = false;
                }
                3 => self.tabs.iter_mut().for_each(|t| *t = false),
                _ => {}
            },
            (b'H' | b'f', []) => {
                let y = next(1);
                let x = next(1);
                self.goto(i64::from(y) - 1, usize::from(x) - 1);
            }
            (b'h' | b'l', []) => {
                for p in params.iter() {
                    self.set_ansi_mode(p[0], action == b'h');
                }
            }
            (b'h' | b'l', [b'?']) => {
                for p in params.iter() {
                    self.set_private_mode(p[0], action == b'h');
                }
            }
            (b'I', []) => self.move_forward_tabs(next(1)),
            (b'J', []) => self.clear_screen(next(0)),
            (b'K', []) => self.clear_line(next(0)),
            (b'L', []) => self.insert_lines(usize::from(next(1))),
            (b'M', []) => self.delete_lines(usize::from(next(1))),
            (b'm', []) => self.sgr(params),
            (b'n', []) => self.device_status(next(0), false),
            (b'n', [b'?']) => self.device_status(next(0), true),
            (b'P', []) => self.delete_chars(usize::from(next(1))),
            (b'p', [b'$']) => {
                let m = next(0);
                let state = match m {
                    4 => {
                        if self.has(mode::INSERT) {
                            1
                        } else {
                            2
                        }
                    }
                    20 => {
                        if self.has(mode::NEWLINE) {
                            1
                        } else {
                            2
                        }
                    }
                    _ => 0,
                };
                self.reply(format!("\x1b[{m};{state}$y").as_bytes());
            }
            (b'p', [b'?', b'$']) => {
                let m = next(0);
                let state = self.private_mode_state(m);
                self.reply(format!("\x1b[?{m};{state}$y").as_bytes());
            }
            (b'p', [b'!']) => self.soft_reset(),
            (b'q', [b' ']) => {
                let style = next(0);
                self.cursor_style = match style {
                    0 => None,
                    1 | 2 => Some((CursorShape::Block, style % 2 == 1)),
                    3 | 4 => Some((CursorShape::Underline, style % 2 == 1)),
                    5 | 6 => Some((CursorShape::Bar, style % 2 == 1)),
                    _ => return,
                };
            }
            (b'q', [b'>']) if next(0) == 0 => {
                self.reply(format!("\x1bP>|auk {VERSION}\x1b\\").as_bytes());
            }
            (b'r', []) => {
                let top = usize::from(next(1));
                let bottom = it_bottom(params);
                self.set_scrolling_region(top, bottom);
            }
            (b'S', []) => self.scroll_up_relative(self.top, usize::from(next(1))),
            (b's', []) => self.save_cursor(),
            (b'T', []) => self.scroll_down_relative(self.top, usize::from(next(1))),
            (b't', []) => match next(1) {
                22 => {
                    if self.title_stack.len() >= TITLE_STACK_DEPTH {
                        self.title_stack.remove(0);
                    }
                    self.title_stack.push(self.title.clone());
                }
                23 => {
                    if let Some(title) = self.title_stack.pop() {
                        self.title = title.clone();
                        self.events.push_back(Event::Title(title));
                    }
                }
                op => self.window_report(op),
            },
            (b'u', [b'?']) => {
                let flags = self.screen().kitty_flags();
                self.reply(format!("\x1b[?{flags}u").as_bytes());
            }
            (b'u', [b'=']) => {
                let flags = next(0) as u8;
                let how = next(1);
                self.kitty_set(flags, how);
            }
            (b'u', [b'>']) => self.kitty_push(next(0) as u8),
            (b'u', [b'<']) => self.kitty_pop(usize::from(next(1))),
            (b'u', []) => self.restore_cursor(),
            (b'W', [b'?']) if next(0) == 5 => self.tabs = default_tabs(self.cols),
            (b'X', []) => self.erase_chars(usize::from(next(1))),
            (b'Z', []) => self.move_backward_tabs(next(1)),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        let charset_slot = |i: &[u8]| match i {
            [b'('] => Some(0),
            [b')'] => Some(1),
            [b'*'] => Some(2),
            [b'+'] => Some(3),
            _ => None,
        };
        match (byte, intermediates) {
            (b'B', i) => {
                if let Some(slot) = charset_slot(i) {
                    self.scr().cursor.charsets[slot] = Charset::Ascii;
                }
            }
            (b'0', i) if !i.is_empty() => {
                if let Some(slot) = charset_slot(i) {
                    self.scr().cursor.charsets[slot] = Charset::DecGraphics;
                }
            }
            (b'D', []) => self.linefeed(),
            (b'E', []) => {
                self.linefeed();
                self.carriage_return();
            }
            (b'H', []) => {
                let col = self.screen().cursor.col;
                self.tabs[col] = true;
            }
            (b'M', []) => self.reverse_index(),
            (b'Z', []) => self.reply(b"\x1b[?62;22c"),
            (b'c', []) => self.full_reset(true),
            (b'7', []) => self.save_cursor(),
            (b'8', [b'#']) => self.decaln(),
            (b'8', []) => self.restore_cursor(),
            (b'=', []) => self.modes |= mode::APP_KEYPAD,
            (b'>', []) => self.modes &= !mode::APP_KEYPAD,
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, body: &[u8], bell_terminated: bool) {
        self.osc(body, bell_terminated);
    }

    fn apc_dispatch(&mut self, body: &mut Vec<u8>) {
        self.events.push_back(Event::Tsp(std::mem::take(body)));
    }

    fn terminated(&self) -> bool {
        !self.events.is_empty()
    }
}

/// DECSTBM's bottom margin: the second parameter, `None` when missing or 0.
fn it_bottom(params: &Params) -> Option<usize> {
    params
        .iter()
        .nth(1)
        .map(|p| usize::from(p[0]))
        .filter(|&p| p != 0)
}

impl Core {
    // SGR (as `vte`'s `attrs_from_sgr_parameters`, plus blink, overline and SGR 21).
    pub(crate) fn sgr(&mut self, params: &Params) {
        let pen = &mut self.scr().cursor.pen;
        if params.is_empty() {
            *pen = Pen {
                link: pen.link,
                ..Default::default()
            };
            return;
        }
        const UNDERLINES: u16 = Flags::UNDERLINE
            | Flags::DOUBLE_UNDERLINE
            | Flags::CURLY_UNDERLINE
            | Flags::DOTTED_UNDERLINE
            | Flags::DASHED_UNDERLINE;
        let underline = |pen: &mut Pen, bit: u16| {
            pen.flags = (pen.flags & !UNDERLINES) | bit;
        };
        let mut it = params.iter();
        while let Some(param) = it.next() {
            match param {
                [0] => {
                    pen.fg = Color::Default;
                    pen.bg = Color::Default;
                    pen.underline_color = Color::Default;
                    pen.flags = 0;
                }
                [1] => pen.flags |= Flags::BOLD,
                [2] => pen.flags |= Flags::DIM,
                [3] => pen.flags |= Flags::ITALIC,
                [4, 0] => pen.flags &= !UNDERLINES,
                [4, 2] => underline(pen, Flags::DOUBLE_UNDERLINE),
                [4, 3] => underline(pen, Flags::CURLY_UNDERLINE),
                [4, 4] => underline(pen, Flags::DOTTED_UNDERLINE),
                [4, 5] => underline(pen, Flags::DASHED_UNDERLINE),
                [4, ..] => underline(pen, Flags::UNDERLINE),
                [5] | [6] => pen.flags |= Flags::BLINK,
                [7] => pen.flags |= Flags::INVERSE,
                [8] => pen.flags |= Flags::HIDDEN,
                [9] => pen.flags |= Flags::STRIKE,
                [21] => underline(pen, Flags::DOUBLE_UNDERLINE),
                [22] => pen.flags &= !(Flags::BOLD | Flags::DIM),
                [23] => pen.flags &= !Flags::ITALIC,
                [24] => pen.flags &= !UNDERLINES,
                [25] => pen.flags &= !Flags::BLINK,
                [27] => pen.flags &= !Flags::INVERSE,
                [28] => pen.flags &= !Flags::HIDDEN,
                [29] => pen.flags &= !Flags::STRIKE,
                [n @ 30..=37] => pen.fg = Color::Indexed((n - 30) as u8),
                [38] => {
                    let mut rest = it.by_ref().map(|p| p[0]);
                    if let Some(c) = sgr_color(&mut rest) {
                        pen.fg = c;
                    }
                }
                [38, rest @ ..] => {
                    if let Some(c) = colon_color(rest) {
                        pen.fg = c;
                    }
                }
                [39] => pen.fg = Color::Default,
                [n @ 40..=47] => pen.bg = Color::Indexed((n - 40) as u8),
                [48] => {
                    let mut rest = it.by_ref().map(|p| p[0]);
                    if let Some(c) = sgr_color(&mut rest) {
                        pen.bg = c;
                    }
                }
                [48, rest @ ..] => {
                    if let Some(c) = colon_color(rest) {
                        pen.bg = c;
                    }
                }
                [49] => pen.bg = Color::Default,
                [53] => pen.flags |= Flags::OVERLINE,
                [55] => pen.flags &= !Flags::OVERLINE,
                [58] => {
                    let mut rest = it.by_ref().map(|p| p[0]);
                    if let Some(c) = sgr_color(&mut rest) {
                        pen.underline_color = c;
                    }
                }
                [58, rest @ ..] => {
                    if let Some(c) = colon_color(rest) {
                        pen.underline_color = c;
                    }
                }
                [59] => pen.underline_color = Color::Default,
                [n @ 90..=97] => pen.fg = Color::Indexed((n - 90 + 8) as u8),
                [n @ 100..=107] => pen.bg = Color::Indexed((n - 100 + 8) as u8),
                _ => {}
            }
        }
    }
}

/// `38;5;n` and `38;2;r;g;b` (the semicolon forms), reading the values after the 38.
fn sgr_color(it: &mut dyn Iterator<Item = u16>) -> Option<Color> {
    match it.next() {
        Some(2) => {
            let r = u8::try_from(it.next()?).ok()?;
            let g = u8::try_from(it.next()?).ok()?;
            let b = u8::try_from(it.next()?).ok()?;
            Some(Color::Rgb(Rgb(r, g, b)))
        }
        Some(5) => Some(Color::Indexed(u8::try_from(it.next()?).ok()?)),
        _ => None,
    }
}

/// `38:5:n`, `38:2:r:g:b` and `38:2:cs:r:g:b` (the colon forms): the subparameters after 38.
fn colon_color(rest: &[u16]) -> Option<Color> {
    let start = if rest.len() > 4 { 2 } else { 1 };
    let mut it = std::iter::once(rest[0]).chain(rest[start..].iter().copied());
    sgr_color(&mut it)
}
