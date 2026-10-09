//! The Rust tern-sdk's real `Session` talking to the engine: its handshake, flow control, adopt,
//! blobs and event decoding, with the engine's output fed back as the program's input. The
//! harness plays the primary client: it draws (acks) each frame as it arrives, or doesn't.

mod common;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::rc::Rc;
use std::time::Duration;

use auk_tsp::{Mode, State, T0_KINDS, T1_KINDS, T2_KINDS, ViewEvent};
use common::{Harness, apc_payloads};
use serde_json::Value;
use tern_sdk::term::Terminal;
use tern_sdk::ui::{self, View};
use tern_sdk::wire::Event;
use tern_sdk::{Input, Options, Session, SurfaceOptions};

#[derive(Debug)]
struct Pane {
    h: Harness,
    /// The primary draws (and acks) every frame.
    draw: bool,
    input: VecDeque<Vec<u8>>,
    frames: usize,
}

/// The program's tty: writes go to the pane's parser, the pane's answers come back as input.
struct Tty(Rc<RefCell<Pane>>);

impl Terminal for Tty {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut p = self.0.borrow_mut();
        for payload in apc_payloads(bytes) {
            let Pane { h, .. } = &mut *p;
            h.pane.on_apc(&mut h.grid, &payload);
            if payload.starts_with(b"tsp;f;") {
                p.frames += 1;
                if p.draw {
                    let frame: Value = serde_json::from_slice(&payload[6..]).unwrap();
                    let h = &mut p.h;
                    h.pane.ack(
                        &h.grid,
                        frame["sf"].as_str().unwrap(),
                        frame["s"].as_u64().unwrap(),
                    );
                }
            }
        }
        let out: Vec<u8> =
            p.h.pane
                .take_output()
                .iter()
                .flat_map(|o| o.encode())
                .collect();
        if !out.is_empty() {
            p.input.push_back(out);
        }
        if bytes.ends_with(b"\x1b[c") {
            p.input.push_back(b"\x1b[?62;22c".to_vec());
        }
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8], _timeout: Option<Duration>) -> io::Result<Option<usize>> {
        let mut p = self.0.borrow_mut();
        let Some(mut chunk) = p.input.pop_front() else {
            return Ok(None);
        };
        let n = chunk.len().min(buf.len());
        buf[..n].copy_from_slice(&chunk[..n]);
        if n < chunk.len() {
            p.input.push_front(chunk.split_off(n));
        }
        Ok(Some(n))
    }
}

fn pane(draw: bool) -> Rc<RefCell<Pane>> {
    Rc::new(RefCell::new(Pane {
        h: Harness::new(),
        draw,
        input: VecDeque::new(),
        frames: 0,
    }))
}

fn connect(p: &Rc<RefCell<Pane>>, options: Options) -> Session {
    Session::with_terminal(
        Tty(Rc::clone(p)),
        options.record(None).timeout(Duration::from_millis(200)),
    )
    .expect("handshake runs")
    .expect("the engine answers the hello before DA1")
}

fn text_view(text: &str) -> View {
    View::new().main(ui::col().child(ui::text(text).key("t")))
}

#[test]
fn e2_sdk_sessions_flow_while_the_primary_draws() {
    let p = pane(true);
    let mut session = connect(&p, Options::new().app("demo").feature("edit"));
    let caps = session.caps().clone();
    let mut kinds = caps.kinds.clone();
    kinds.sort();
    let mut want: Vec<String> = T0_KINDS
        .iter()
        .chain(&T1_KINDS)
        .chain(T2_KINDS)
        .map(|k| (*k).to_owned())
        .collect();
    want.sort();
    assert_eq!(
        (kinds, caps.credits, caps.apc, caps.term.as_str()),
        (want, 2, 65536, "auk")
    );

    let sf = session.open(SurfaceOptions::inline().id("s1")).unwrap();
    let mut text = String::new();
    for _ in 0..40 {
        text.push('x');
        session.render(sf, text_view(&text)).unwrap();
    }
    session.close().unwrap();
    let p = p.borrow();
    // Every render went out as its own frame: acks kept the credits free.
    assert_eq!(p.frames, 40);
    let s = &p.h.pane.surfaces()[0];
    assert_eq!(s.state(), State::Closed);
    assert_eq!(
        s.document().get("main.t").unwrap().text(),
        Some(text.as_str())
    );
    assert_eq!(s.last_s(), Some(40));
}

#[test]
fn e4_without_a_drawing_client_the_program_holds_its_frames() {
    let p = pane(false);
    let mut session = connect(&p, Options::new());
    let sf = session.open(SurfaceOptions::flow()).unwrap();
    for n in 1..=10 {
        session.render(sf, text_view(&"y".repeat(n))).unwrap();
    }
    assert_eq!(
        p.borrow().frames,
        2,
        "credits: 2 frames unacknowledged, then the SDK waits"
    );
    // A window attaches and draws the current document: the SDK sends one coalesced frame.
    {
        let mut guard = p.borrow_mut();
        let Pane { h, input, .. } = &mut *guard;
        h.pane.ack(&h.grid, "s1", 2);
        let out: Vec<u8> = h
            .pane
            .take_output()
            .iter()
            .flat_map(|o| o.encode())
            .collect();
        input.push_back(out);
    }
    session.pump(Duration::from_millis(20)).unwrap();
    assert_eq!(p.borrow().frames, 3);
    let text = p.borrow().h.pane.surfaces()[0]
        .document()
        .get("main.t")
        .unwrap()
        .text()
        .map(str::to_owned);
    assert_eq!(text.as_deref(), Some("yyyyyyyyyy"));
}

#[test]
fn c8_sdk_listen_false_runs_without_answers() {
    let p = pane(true);
    let mut session = connect(&p, Options::new());
    let sf = session.open(SurfaceOptions::flow().listen(false)).unwrap();
    session
        .stylesheet(sf, "main", Some(".a{color:red}"))
        .unwrap();
    for n in 1..=5 {
        session.render(sf, text_view(&"z".repeat(n))).unwrap();
    }
    session.close().unwrap();
    let p = p.borrow();
    assert_eq!(p.frames, 5, "never waits for credits");
    assert!(p.input.is_empty(), "nothing answered");
    assert_eq!(p.h.pane.surfaces()[0].mode(), Mode::Flow);
}

#[test]
fn d13_sdk_adopt_after_a_restart() {
    let p = pane(true);
    let mut first = connect(&p, Options::new());
    let sf = first.open(SurfaceOptions::inline().id("saved")).unwrap();
    first
        .render(
            sf,
            text_view("before").dock(ui::col().child(ui::text("dock"))),
        )
        .unwrap();
    first.close().unwrap();
    // A new process adopts a surface it doesn't remember: `["del","main"]`, then a fresh `add`.
    let mut second = connect(&p, Options::new());
    let sf = second
        .open(SurfaceOptions::inline().id("saved").adopt(true))
        .unwrap();
    second.render(sf, text_view("after")).unwrap();
    second.pump(Duration::from_millis(5)).unwrap();
    let p = p.borrow();
    let s = &p.h.pane.surfaces()[0];
    assert_eq!((s.state(), p.h.pane.surfaces().len()), (State::Live, 1));
    assert_eq!(s.document().get("main.t").unwrap().text(), Some("after"));
    assert!(s.document().region("dock").is_none());
}

#[test]
fn b7_sdk_blob_round_trip() {
    let p = pane(true);
    let mut session = connect(&p, Options::new());
    let id = session
        .blob(b"\x89PNG fake".to_vec(), Some("image/png"))
        .unwrap();
    assert_eq!(
        session.blobs(vec!["missing".into(), id.clone()]).unwrap(),
        std::slice::from_ref(&id)
    );
    assert_eq!(
        p.borrow().h.pane.blobs().get(&id).unwrap().mime.as_deref(),
        Some("image/png")
    );
}

#[test]
fn f13_sdk_receives_view_events() {
    let p = pane(true);
    let mut session = connect(&p, Options::new().feature("edit"));
    let sf = session.open(SurfaceOptions::inline().id("s1")).unwrap();
    session
        .render(
            sf,
            View::new().main(ui::col().child(ui::text("x").key("t"))),
        )
        .unwrap();
    {
        let mut guard = p.borrow_mut();
        let Pane { h, input, .. } = &mut *guard;
        h.pane
            .view_event(
                &h.grid,
                "s1",
                ViewEvent::Focus {
                    id: "main.t".into(),
                },
            )
            .unwrap();
        let out: Vec<u8> = h
            .pane
            .take_output()
            .iter()
            .flat_map(|o| o.encode())
            .collect();
        input.push_back(out);
    }
    let mut seen = Vec::new();
    while let Some(input) = session.next(Some(Duration::from_millis(20))).unwrap() {
        if let Input::Event(e) = input {
            seen.push(e);
        }
    }
    assert!(
        seen.iter().any(
            |e| matches!(e, Event::Focus(f) if f.id == "main.t" && f.sf.as_deref() == Some("s1"))
        ),
        "{seen:?}"
    );
    assert!(
        seen.iter()
            .any(|e| matches!(e, Event::Resize(r) if r.cols == 80)),
        "{seen:?}"
    );
}
