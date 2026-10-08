//! Terminal behavior beyond the reference grid: APC strings for TSP (§6.1), events and their
//! stream order, reports, kitty keyboard stacks, anchors (§7.4), and Unicode details.

use auk_term::{
    AnchorError, AnchorKey, Color, Config, CursorShape, Event, Facts, Flags, Mark, MouseMode, Rgb,
    Terminal,
};

fn term(cols: u16, rows: u16) -> Terminal {
    Terminal::new(cols, rows, Config::default())
}

/// The visible screen as text.
fn screen(t: &Terminal) -> String {
    let top = t.history_len();
    t.text(top..top + usize::from(t.rows()))
}

/// Events from feeding `bytes`, with what each byte position had consumed when it arose.
fn events_at(t: &mut Terminal, bytes: &[u8]) -> Vec<(usize, Event)> {
    let mut at = 0;
    let mut out = Vec::new();
    while at < bytes.len() || t.pending_events() {
        let (n, ev) = t.advance(&bytes[at..]);
        at += n;
        if let Some(ev) = ev {
            out.push((at, ev));
        }
    }
    out
}

// APC (§6.1).

#[test]
fn tsp_apc_is_an_event_in_stream_order() {
    let mut t = term(20, 3);
    let bytes = b"ab\x1b_tsp;{\"x\":1}\x1b\\cd";
    let (n, ev) = t.advance(bytes);
    assert_eq!(ev, Some(Event::Tsp(b"tsp;{\"x\":1}".to_vec())));
    assert_eq!(n, bytes.len() - 2, "stops right after the ST");
    assert_eq!(screen(&t).trim_end(), "ab", "later bytes are not parsed yet");
    let (n, ev) = t.advance(&bytes[n..]);
    assert_eq!((n, ev), (2, None));
    assert_eq!(screen(&t).trim_end(), "abcd");
}

#[test]
fn apc_ends_at_bel_and_keeps_0x9c() {
    let mut t = term(20, 3);
    let events = t.feed(b"\x1b_tsp;a\x9cb\x07");
    assert_eq!(events, vec![Event::Tsp(b"tsp;a\x9cb".to_vec())]);
}

#[test]
fn other_apc_strings_are_dropped() {
    let mut t = term(20, 3);
    let events = t.feed(b"x\x1b_Gi=1;AAAA\x1b\\\x1b_ts;\x07\x1b_\x07y");
    assert!(events.is_empty(), "{events:?}");
    assert_eq!(screen(&t).trim_end(), "xy");
}

#[test]
fn apc_split_across_reads() {
    let mut t = term(20, 3);
    let bytes = b"\x1b_tsp;split payload\x1b\\";
    let mut events = Vec::new();
    for b in bytes {
        events.extend(t.feed(std::slice::from_ref(b)));
    }
    assert_eq!(events, vec![Event::Tsp(b"tsp;split payload".to_vec())]);
}

#[test]
fn apc_at_the_limit_is_kept() {
    let mut t = term(20, 3);
    let mut bytes = b"\x1b_tsp;".to_vec();
    bytes.resize(2 + 262_144, b'x');
    bytes.extend_from_slice(b"\x1b\\");
    let events = t.feed(&bytes);
    assert_eq!(events.len(), 1);
    let Event::Tsp(payload) = &events[0] else { panic!("{events:?}") };
    assert_eq!(payload.len(), 262_144);
}

#[test]
fn apc_over_the_limit_is_abandoned_and_the_rest_shows() {
    let mut t = term(20, 3);
    let mut bytes = b"\x1b_tsp;".to_vec();
    bytes.resize(2 + 262_144, b'x');
    bytes.extend_from_slice(b"abc\x1b\\");
    let events = t.feed(&bytes);
    assert!(events.is_empty(), "{events:?}");
    assert_eq!(screen(&t).trim_end(), "abc");
}

#[test]
fn esc_inside_apc_starts_a_new_sequence() {
    let mut t = term(20, 3);
    let events = t.feed(b"\x1b_tsp;x\x1b[1mB");
    assert!(events.is_empty());
    let cell = &t.line(0).expect("row").cells[0];
    assert_eq!(cell.text, "B");
    assert!(cell.flags.has(Flags::BOLD));
}

#[test]
fn can_cancels_apc() {
    let mut t = term(20, 3);
    assert!(t.feed(b"\x1b_tsp;x\x18y").is_empty());
    assert_eq!(screen(&t).trim_end(), "y");
}

// Events.

#[test]
fn shell_integration_events() {
    let mut t = term(40, 3);
    let events = t.feed(
        b"\x1b]133;A\x07$ \x1b]133;B\x07\x1b]133;C;cmdline_url=ls%20-la\x07\x1b]133;D;2\x07\
          \x1b]133;D\x07\x1b]7;file://host/tmp\x1b\\\x1b]2;title\x07\x1b]1337;SetUserVar=resume=b21wIC0tcmVzdW1l\x07\x07",
    );
    assert_eq!(
        events,
        vec![
            Event::Mark(Mark::PromptStart),
            Event::Mark(Mark::PromptEnd),
            Event::Mark(Mark::CommandStart { cmdline: Some("ls -la".into()) }),
            Event::Mark(Mark::CommandEnd { status: Some(2) }),
            Event::Mark(Mark::CommandEnd { status: None }),
            Event::Cwd("file://host/tmp".into()),
            Event::Title("title".into()),
            Event::UserVar { name: "resume".into(), value: b"omp --resume".to_vec() },
            Event::Bell,
        ]
    );
    assert_eq!(t.title(), "title");
}

#[test]
fn each_event_stops_the_parser() {
    let mut t = term(40, 3);
    let bytes = b"a\x07b\x1b]133;A\x07c";
    let got = events_at(&mut t, bytes);
    assert_eq!(got, vec![(2, Event::Bell), (11, Event::Mark(Mark::PromptStart))]);
}

#[test]
fn screen_events() {
    let mut t = term(10, 3);
    assert_eq!(t.feed(b"\x1b[?1049h"), vec![Event::AltScreen(true)]);
    assert_eq!(t.feed(b"\x1b[2J"), vec![Event::ScreenCleared]);
    assert_eq!(t.feed(b"\x1b[?1049l"), vec![Event::AltScreen(false)]);
    assert_eq!(t.feed(b"\x1b[3J"), vec![Event::ScreenCleared]);
    assert_eq!(t.feed(b"x\x1bc"), vec![Event::Reset]);
    assert_eq!(screen(&t).trim(), "");
}

// Reports.

fn replies(t: &mut Terminal, bytes: &[u8]) -> String {
    let events = t.feed(bytes);
    assert!(events.contains(&Event::Replies), "{events:?}");
    String::from_utf8(t.take_replies()).expect("ASCII")
}

#[test]
fn device_attributes_have_digits() {
    let mut t = term(10, 3);
    let da1 = replies(&mut t, b"\x1b[c");
    assert!(da1.starts_with("\x1b[?") && da1.ends_with('c'), "{da1:?}");
    assert!(da1.chars().any(|c| c.is_ascii_digit()));
    assert_eq!(replies(&mut t, b"\x1b[0c"), da1);
    assert_eq!(replies(&mut t, b"\x1bZ"), da1);
    let da2 = replies(&mut t, b"\x1b[>c");
    assert!(da2.starts_with("\x1b[>") && da2.ends_with('c'), "{da2:?}");
}

#[test]
fn status_and_cursor_reports() {
    let mut t = term(10, 5);
    assert_eq!(replies(&mut t, b"\x1b[5n"), "\x1b[0n");
    assert_eq!(replies(&mut t, b"\x1b[3;4H\x1b[6n"), "\x1b[3;4R");
    // In origin mode the row is relative to the scroll region.
    assert_eq!(replies(&mut t, b"\x1b[2;5r\x1b[?6h\x1b[2;3H\x1b[6n"), "\x1b[2;3R");
    assert_eq!(replies(&mut t, b"\x1b[?6n"), "\x1b[?2;3;1R");
}

#[test]
fn mode_reports() {
    let mut t = term(10, 3);
    assert_eq!(replies(&mut t, b"\x1b[?2004$p"), "\x1b[?2004;2$y");
    assert_eq!(replies(&mut t, b"\x1b[?2004h\x1b[?2004$p"), "\x1b[?2004;1$y");
    assert_eq!(replies(&mut t, b"\x1b[?7$p"), "\x1b[?7;1$y");
    assert_eq!(replies(&mut t, b"\x1b[?9999$p"), "\x1b[?9999;0$y");
    assert_eq!(replies(&mut t, b"\x1b[4h\x1b[4$p"), "\x1b[4;1$y");
    assert_eq!(replies(&mut t, b"\x1b[?2026h\x1b[?2026$p"), "\x1b[?2026;1$y");
    assert!(t.synchronized_output());
}

#[test]
fn xtversion_names_auk() {
    let mut t = term(10, 3);
    let reply = replies(&mut t, b"\x1b[>q");
    assert_eq!(reply, format!("\x1bP>|auk {}\x1b\\", env!("CARGO_PKG_VERSION")));
}

#[test]
fn window_reports_use_client_facts() {
    let mut t = term(80, 24);
    t.set_facts(Facts { cell_px: (9, 18), ..Facts::default() });
    assert_eq!(replies(&mut t, b"\x1b[14t"), "\x1b[4;432;720t");
    assert_eq!(replies(&mut t, b"\x1b[16t"), "\x1b[6;18;9t");
    assert_eq!(replies(&mut t, b"\x1b[18t"), "\x1b[8;24;80t");
}

#[test]
fn color_queries_and_overrides() {
    let mut t = term(10, 3);
    let mut facts = Facts::default();
    facts.palette[1] = Rgb(0x12, 0x34, 0x56);
    facts.foreground = Rgb(1, 2, 3);
    t.set_facts(facts);
    assert_eq!(replies(&mut t, b"\x1b]4;1;?\x07"), "\x1b]4;1;rgb:1212/3434/5656\x07");
    assert_eq!(replies(&mut t, b"\x1b]10;?\x1b\\"), "\x1b]10;rgb:0101/0202/0303\x1b\\");
    // xterm's cube for colors past 15.
    assert_eq!(replies(&mut t, b"\x1b]4;196;?\x07"), "\x1b]4;196;rgb:ffff/0000/0000\x07");
    t.feed(b"\x1b]4;1;rgb:ff/00/80\x07\x1b]11;#102030\x07");
    assert_eq!(t.color_overrides(), &[(1, Rgb(255, 0, 128)), (257, Rgb(0x10, 0x20, 0x30))]);
    assert_eq!(replies(&mut t, b"\x1b]4;1;?\x07"), "\x1b]4;1;rgb:ffff/0000/8080\x07");
    t.feed(b"\x1b]104;1\x07\x1b]111\x07");
    assert!(t.color_overrides().is_empty());
}

// Kitty keyboard protocol.

#[test]
fn kitty_stack_push_pop_query() {
    let mut t = term(10, 3);
    assert_eq!(replies(&mut t, b"\x1b[?u"), "\x1b[?0u");
    t.feed(b"\x1b[>1u");
    assert_eq!(t.modes().kitty_flags, 1);
    assert_eq!(replies(&mut t, b"\x1b[?u"), "\x1b[?1u");
    t.feed(b"\x1b[>5u");
    assert_eq!(t.modes().kitty_flags, 5);
    t.feed(b"\x1b[=2;2u");
    assert_eq!(t.modes().kitty_flags, 7);
    t.feed(b"\x1b[=4;3u");
    assert_eq!(t.modes().kitty_flags, 3);
    t.feed(b"\x1b[<u");
    assert_eq!(t.modes().kitty_flags, 1);
    t.feed(b"\x1b[<5u");
    assert_eq!(t.modes().kitty_flags, 0);
}

#[test]
fn kitty_stacks_are_per_screen() {
    let mut t = term(10, 3);
    t.feed(b"\x1b[>1u\x1b[?1049h");
    assert_eq!(t.modes().kitty_flags, 0);
    t.feed(b"\x1b[>3u");
    assert_eq!(t.modes().kitty_flags, 3);
    t.feed(b"\x1b[?1049l");
    assert_eq!(t.modes().kitty_flags, 1);
    t.feed(b"\x1bc");
    assert_eq!(t.modes().kitty_flags, 0);
}

#[test]
fn kitty_stack_is_bounded() {
    let mut t = term(10, 3);
    for i in 1..=20u8 {
        t.feed(format!("\x1b[>{}u", i % 32).as_bytes());
    }
    t.feed(b"\x1b[<7u");
    assert_eq!(t.modes().kitty_flags, 13, "eight entries kept, the oldest dropped");
    t.feed(b"\x1b[<u");
    assert_eq!(t.modes().kitty_flags, 0);
}

// Modes.

#[test]
fn modes_follow_sequences() {
    let mut t = term(10, 3);
    let m = t.modes();
    assert!(m.cursor_visible && m.autowrap && !m.alt_screen);
    t.feed(b"\x1b[?1h\x1b=\x1b[?2004h\x1b[?1002h\x1b[?1006h\x1b[?1004h\x1b[?25l\x1b[?7l\x1b[4h\x1b[20h");
    let m = t.modes();
    assert!(m.app_cursor && m.app_keypad && m.bracketed_paste && m.mouse_sgr && m.focus_reports);
    assert!(!m.cursor_visible && !m.autowrap && m.insert && m.newline);
    assert_eq!(m.mouse, MouseMode::ButtonMotion);
    t.feed(b"\x1b[?1003h");
    assert_eq!(t.modes().mouse, MouseMode::AnyMotion);
    t.feed(b"\x1b[?1002l");
    assert_eq!(t.modes().mouse, MouseMode::AnyMotion, "resetting another mode leaves it");
    t.feed(b"\x1b[?1003l\x1b[?9h");
    assert_eq!(t.modes().mouse, MouseMode::Press);
    t.feed(b"\x1b[5 q");
    assert_eq!(t.cursor().shape, CursorShape::Bar);
    assert_eq!(t.cursor_blinks(), Some(true));
}

#[test]
fn newline_mode_returns_the_carriage() {
    let mut t = term(10, 3);
    t.feed(b"\x1b[20hab\ncd");
    assert_eq!(screen(&t).trim_end(), "ab\ncd");
}

#[test]
fn alternate_screen_variants() {
    let mut t = term(10, 3);
    t.feed(b"main");
    t.feed(b"\x1b[?47h\x1b[Halt");
    assert_eq!(screen(&t).trim_end(), "alt");
    t.feed(b"\x1b[?47l");
    assert_eq!(screen(&t).trim_end(), "main");
    // 47 keeps the alternate screen's contents; 1047 clears them on the way out.
    t.feed(b"\x1b[?47h");
    assert_eq!(screen(&t).trim_end(), "alt");
    t.feed(b"\x1b[?1047l\x1b[?47h");
    assert_eq!(screen(&t).trim_end(), "");
    t.feed(b"\x1b[?47l\x1b[?1049h");
    assert_eq!(t.history_len(), 0);
    t.feed(b"\x1b[?1049l");
    assert_eq!(screen(&t).trim_end(), "main");
}

#[test]
fn title_stack() {
    let mut t = term(10, 3);
    t.feed(b"\x1b]2;one\x07\x1b[22t\x1b]2;two\x07");
    assert_eq!(t.title(), "two");
    assert_eq!(t.feed(b"\x1b[23t"), vec![Event::Title("one".into())]);
    assert_eq!(t.title(), "one");
    t.feed(b"\x1b]1;icon\x07");
    assert_eq!((t.title(), t.icon_title()), ("one", "icon"));
}

#[test]
fn hyperlinks() {
    let mut t = term(20, 3);
    t.feed(b"\x1b]8;id=a;https://a.example\x1b\\link\x1b]8;;\x1b\\ plain");
    let row = t.line(0).expect("row");
    let link = row.cells[0].link;
    assert_ne!(link, 0);
    assert_eq!(t.links()[link as usize], "https://a.example");
    assert!(row.cells[..4].iter().all(|c| c.link == link));
    assert!(row.cells[4..].iter().all(|c| c.link == 0));
}

// Unicode.

#[test]
fn wide_character_at_the_last_column_wraps() {
    let mut t = term(5, 3);
    t.feed("abcd中".as_bytes());
    let row = t.line(0).expect("row");
    assert!(row.wrapped);
    assert!(row.cells[4].flags.has(Flags::LEADING_SPACER));
    let next = t.line(1).expect("row");
    assert_eq!(next.cells[0].text, "中");
    assert!(next.cells[0].flags.has(Flags::WIDE) && next.cells[1].flags.has(Flags::SPACER));
    assert_eq!(t.text(0..2), "abcd中");
}

#[test]
fn combining_marks_join_the_previous_cell() {
    let mut t = term(10, 3);
    t.feed("e\u{301}x中\u{302}".as_bytes());
    let row = t.line(0).expect("row");
    assert_eq!(row.cells[0].text, "e\u{301}");
    assert_eq!(row.cells[1].text, "x");
    assert_eq!(row.cells[2].text, "中\u{302}");
    assert_eq!(t.cursor().col, 4);
}

#[test]
fn vs16_widens_emoji() {
    let mut t = term(10, 3);
    t.feed("\u{2764}\u{fe0f}!".as_bytes());
    let row = t.line(0).expect("row");
    assert_eq!(row.cells[0].text, "\u{2764}\u{fe0f}");
    assert!(row.cells[0].flags.has(Flags::WIDE) && row.cells[1].flags.has(Flags::SPACER));
    assert_eq!(row.cells[2].text, "!");
    // A character without an emoji form stays narrow.
    t.feed("\r\nA\u{fe0f}!".as_bytes());
    let row = t.line(1).expect("row");
    assert_eq!(row.cells[0].text, "A\u{fe0f}");
    assert_eq!(row.cells[1].text, "!");
}

#[test]
fn dec_special_graphics() {
    let mut t = term(10, 3);
    t.feed(b"\x1b(0lqk\x1b(Bq\x0e");
    t.feed(b"\x1b)0q\x0fq");
    assert_eq!(screen(&t).trim_end(), "┌─┐q─q");
}

#[test]
fn sgr_attributes() {
    let mut t = term(20, 3);
    t.feed(b"\x1b[1;3;4:3;38;5;200;48;2;1;2;3;58:2::9:8:7mA\x1b[21;53;5;7;9;2mB\x1b[0;91;102mC");
    let cells = &t.line(0).expect("row").cells;
    assert_eq!(cells[0].fg, Color::Indexed(200));
    assert_eq!(cells[0].bg, Color::Rgb(Rgb(1, 2, 3)));
    assert_eq!(cells[0].underline_color, Color::Rgb(Rgb(9, 8, 7)));
    assert_eq!(cells[0].flags.0, Flags::BOLD | Flags::ITALIC | Flags::CURLY_UNDERLINE);
    let b = cells[1].flags.0;
    assert_eq!(
        b,
        Flags::BOLD | Flags::ITALIC | Flags::DOUBLE_UNDERLINE | Flags::OVERLINE | Flags::BLINK | Flags::INVERSE | Flags::STRIKE | Flags::DIM
    );
    assert_eq!((cells[2].fg, cells[2].bg, cells[2].flags.0), (Color::Indexed(9), Color::Indexed(10), 0));
}

#[test]
fn scrollback_limit() {
    let mut t = Terminal::new(10, 3, Config { scrollback: 5 });
    for i in 0..20 {
        t.feed(format!("{i}\r\n").as_bytes());
    }
    assert_eq!(t.history_len(), 5);
    assert_eq!(t.text(0..1), "13");
    t.set_config(Config { scrollback: 2 });
    assert_eq!(t.history_len(), 2);
    t.clear_scrollback();
    assert_eq!(t.history_len(), 0);
}

#[test]
fn text_joins_wrapped_rows() {
    let mut t = term(4, 3);
    t.feed(b"abcdefg\r\nhi  ");
    assert_eq!(t.text(0..3), "abcdefg\nhi");
}

// Anchors (§7.4).

const A: AnchorKey = AnchorKey(1);
const B: AnchorKey = AnchorKey(2);

#[test]
fn open_anchor_ends_a_used_row() {
    let mut t = term(10, 5);
    t.feed(b"$ prompt");
    let line = t.open_anchor(A).expect("opens");
    assert_eq!(line, 1, "the prompt row ended first");
    assert_eq!(t.anchor_line(A), Some(1));
    let c = t.cursor();
    assert_eq!((c.row, c.col), (2, 0));
    // On an empty row at column 0 the row itself becomes the anchor.
    let line = t.open_anchor(B).expect("opens");
    assert_eq!(line, 2);
    assert_eq!(t.cursor().row, 3);
    assert_eq!(t.open_anchor(B), Err(AnchorError::DuplicateKey));
    t.feed(b"\x1b[?1049h");
    assert_eq!(t.open_anchor(AnchorKey(3)), Err(AnchorError::AltScreen));
}

#[test]
fn an_anchor_row_is_its_own_row() {
    let mut t = term(10, 5);
    t.open_anchor(A).expect("opens");
    // Even right after an anchor, the next anchor takes the next row.
    t.feed(b"\x1b[1A");
    let line = t.open_anchor(B).expect("opens");
    assert_eq!(line, 1);
}

#[test]
fn undo_anchor_when_nothing_followed() {
    let mut t = term(10, 5);
    t.feed(b"$ run\r\n");
    t.open_anchor(A).expect("opens");
    assert!(t.undo_anchor(A));
    assert_eq!(t.anchor_line(A), None);
    let c = t.cursor();
    assert_eq!((c.row, c.col), (1, 0));
    t.open_anchor(B).expect("opens");
    t.feed(b"output");
    assert!(!t.undo_anchor(B), "something was written below");
    assert_eq!(t.anchor_line(B), Some(1));
}

#[test]
fn anchors_scroll_into_history_and_leave_off_the_end() {
    let mut t = Terminal::new(10, 3, Config { scrollback: 4 });
    t.open_anchor(A).expect("opens");
    let mut gone = Vec::new();
    for i in 0..4 {
        gone.extend(t.feed(format!("{i}\r\n").as_bytes()));
    }
    gone.extend(t.feed(b"x\r\n"));
    assert!(gone.is_empty());
    assert_eq!(t.history_len(), 4);
    assert_eq!(t.anchor_line(A), Some(0), "in the scrollback");
    let events = t.feed(b"y\r\n");
    assert_eq!(events, vec![Event::AnchorGone(A)]);
    assert_eq!(t.anchor_line(A), None);
}

#[test]
fn erasing_an_anchor_ends_it() {
    let mut t = term(10, 5);
    t.feed(b"a\r\n");
    t.open_anchor(A).expect("opens");
    t.feed(b"b");
    let bytes = b"c\x1b[2Jd";
    let got = events_at(&mut t, bytes);
    assert_eq!(got, vec![(5, Event::AnchorGone(A)), (5, Event::ScreenCleared)]);
}

#[test]
fn scrolling_an_anchor_out_of_a_region_ends_it() {
    let mut t = term(10, 5);
    t.feed(b"\x1b[2;4r\x1b[2;1H");
    t.open_anchor(A).expect("opens");
    t.feed(b"\x1b[4;1H");
    assert_eq!(t.feed(b"\n\n"), vec![Event::AnchorGone(A)]);
}

#[test]
fn clear_scrollback_and_reset_end_anchors() {
    let mut t = term(10, 2);
    t.open_anchor(A).expect("opens");
    t.feed(b"1\r\n2\r\n3");
    t.open_anchor(B).expect("opens");
    t.clear_scrollback();
    assert_eq!(t.take_events(), vec![Event::AnchorGone(A)]);
    let got = events_at(&mut t, b"\x1bc");
    assert_eq!(got, vec![(2, Event::AnchorGone(B)), (2, Event::Reset)]);
}

#[test]
fn drop_anchor_is_silent() {
    let mut t = term(10, 5);
    t.open_anchor(A).expect("opens");
    t.drop_anchor(A);
    assert_eq!(t.anchor_line(A), None);
    assert!(!t.pending_events());
}

#[test]
fn anchors_never_join_wrapped_lines() {
    let mut t = term(10, 6);
    t.feed(b"0123456789abcdef");
    t.open_anchor(A).expect("opens");
    t.feed(b"after");
    for (cols, rows) in [(5u16, 6u16), (20, 6), (3, 4), (40, 6), (10, 6)] {
        t.resize(cols, rows);
        let line = t.anchor_line(A).expect("survives");
        let anchor = t.line(line).expect("row");
        assert!(anchor.cells.iter().all(|c| c.text.is_empty()), "{cols}x{rows}");
        assert!(!anchor.wrapped);
        assert!(!t.line(line - 1).expect("row").wrapped, "{cols}x{rows}");
        assert_eq!(t.text(0..line), "0123456789abcdef", "{cols}x{rows}");
        let end = t.history_len() + usize::from(t.rows());
        assert_eq!(t.text(line + 1..end).trim_end(), "after", "{cols}x{rows}");
    }
}

#[test]
fn anchor_rows_are_not_trimmed_as_blank() {
    let mut t = term(10, 4);
    t.open_anchor(A).expect("opens");
    t.open_anchor(B).expect("opens");
    t.resize(30, 4);
    t.resize(4, 4);
    t.resize(30, 2);
    assert!(t.anchor_line(A).is_some() && t.anchor_line(B).is_some());
    assert_eq!(t.anchor_line(B), t.anchor_line(A).map(|l| l + 1));
}

#[test]
fn snapshot_anchor_sequences_can_be_refused() {
    let mut t = term(10, 3);
    t.set_accept_anchor_sequences(false);
    t.feed(b"\x1b]6464;anchor;7\x1b\\");
    assert_eq!(t.anchor_line(AnchorKey(7)), None);
    t.set_accept_anchor_sequences(true);
    t.feed(b"\x1b]6464;anchor;7\x1b\\");
    assert_eq!(t.anchor_line(AnchorKey(7)), Some(0));
}

#[test]
fn resize_reflows_the_main_screen() {
    let mut t = term(10, 4);
    t.feed(b"hello world, wrapped\r\n$ ");
    t.resize(5, 4);
    assert_eq!(t.history_len(), 2);
    assert_eq!(t.text(0..t.history_len() + 4).trim_end(), "hello world, wrapped\n$");
    t.resize(30, 4);
    assert_eq!(t.history_len(), 0);
    assert_eq!(screen(&t).trim_end(), "hello world, wrapped\n$");
    let c = t.cursor();
    assert_eq!((c.row, c.col), (1, 2));
}
