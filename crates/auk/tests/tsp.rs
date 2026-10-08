//! The TSP suite (GOAL.md row `tsp`, spec §7, §12.1): one test per item of the conformance
//! checklist in `research/sdk-wire.md` §10 (except F2, Windows), named by its ID, plus the
//! handshake (AC3), lifetimes (AC6), silence (AC7), robustness (AC15) and advertising (D22).
//!
//! Every test drives real bytes through a daemon pane: the program is `tools/pty_probe.py`, so a
//! test controls exactly what the program writes and how its tty is set, and sees exactly what
//! reaches the PTY. State is read from the daemon's authoritative replica (`tsp PANE`) and, where
//! a window matters (acks, pointer events, attach), from a headless window's replica.

use std::time::{Duration, Instant};

use auk_conformance::harness::{Headless, TestEnv, quote, wait_for};
use auk_conformance::probe::{
    Input, Probe, events, frame, hello, open, open_with, split_input, tsp, tsp_bodies, tsp_json,
};
use base64::Engine as _;
use serde_json::{Value, json};

fn env(name: &str) -> TestEnv {
    let env = TestEnv::new(env!("CARGO_BIN_EXE_auk"), name);
    env.start_daemon();
    env
}

/// A daemon with one probe pane in raw mode.
fn setup(name: &str) -> (TestEnv, Probe) {
    let env = env(name);
    let probe = Probe::start(&env, &[]);
    (env, probe)
}

/// A daemon, a headless window (the pane's primary, so frames are acked) and a probe pane.
fn setup_window(name: &str) -> (TestEnv, Headless, Probe) {
    let env = env(name);
    let window = env.start_headless("w", &[]);
    let probe = Probe::start(&env, &[]);
    wait_for("the window to become the pane's primary", || {
        let panes = window.request("panes");
        panes["panes"]
            .as_array()?
            .iter()
            .any(|p| p["id"] == probe.pane && p["primary"] == true)
            .then_some(())
    });
    (env, window, probe)
}

/// The daemon's view of the pane's TSP state.
fn state(env: &TestEnv, probe: &Probe) -> Value {
    env.daemon(&format!("tsp {}", probe.pane))
}

fn surfaces(env: &TestEnv, probe: &Probe) -> Vec<Value> {
    state(env, probe)["surfaces"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The newest surface with `id` in the daemon's state.
fn surface(env: &TestEnv, probe: &Probe, id: &str) -> Value {
    surfaces(env, probe)
        .into_iter()
        .rev()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("no surface {id}: {}", state(env, probe)))
}

/// Finds a node by id in a document JSON.
fn node<'a>(doc: &'a Value, id: &str) -> Option<&'a Value> {
    if doc["id"] == id {
        return Some(doc);
    }
    doc["c"].as_array()?.iter().find_map(|c| node(c, id))
}

fn doc_node(env: &TestEnv, probe: &Probe, sf: &str, id: &str) -> Option<Value> {
    node(&surface(env, probe, sf)["doc"], id).cloned()
}

/// Writes `bytes` as the program and returns the TSP bodies the terminal answered with, up to
/// the DA1 fence.
fn exchange(probe: &Probe, bytes: &[u8]) -> Vec<Value> {
    probe.write(bytes);
    probe.fence_tsp()
}

fn text_doc() -> Value {
    json!([{"id": "t", "k": "text", "p": {"text": "hi"}}])
}

/// Reads until the TSP bodies the program got satisfy `done`; returns them all.
fn await_tsp(probe: &Probe, what: &str, done: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    let input = probe.read_until(what, |b| done(&tsp_bodies(&split_input(b))));
    tsp_bodies(&split_input(&input))
}

/// The errors among `bodies`.
fn errors(bodies: &[Value]) -> Vec<&Value> {
    events(bodies, "error")
}

/// Waits until the daemon's TSP state satisfies `done`.
fn wait_state(env: &TestEnv, probe: &Probe, what: &str, done: impl Fn(&Value) -> bool) -> Value {
    wait_for(what, || {
        let st = state(env, probe);
        done(&st).then_some(st)
    })
}

/// The window's view of the pane's TSP state.
fn window_state(window: &Headless, probe: &Probe) -> Value {
    window.request(&format!("tsp {}", probe.pane))
}

/// Sends a view event from the window.
fn window_event(window: &Headless, probe: &Probe, sf: &str, event: Value) {
    window.request(&format!(
        "event {} {} {}",
        probe.pane,
        quote(sf),
        quote(&event.to_string())
    ));
}

/// A pointer gesture in the window; returns the event it sent (null for none).
fn click(window: &Headless, probe: &Probe, sf: &str, node: &str, extra: &str) -> Value {
    window.request(&format!("click {} {sf} {node} {extra}", probe.pane))
}

/// Client facts from the window.
fn facts(window: &Headless, cell: [u16; 2], dark: bool, reduce: bool) {
    let f = json!({"cell_px": cell, "dark": dark, "reduce_motion": reduce, "hour12": false});
    window.request(&format!("facts {}", quote(&f.to_string())));
}

/// What the pane's screen shows, line by line.
fn screen_lines(env: &TestEnv, probe: &Probe) -> Vec<String> {
    env.daemon(&format!("capture {}", probe.pane))["text"]
        .as_str()
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn line_of(lines: &[String], needle: &str) -> usize {
    lines
        .iter()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle:?} in {lines:#?}"))
}

fn hello_reply(bodies: &[Value]) -> &Value {
    bodies
        .iter()
        .find(|b| b["r"] == "hello")
        .unwrap_or_else(|| panic!("no hello reply in {bodies:?}"))
}

// ---------------------------------------------------------------------------------------------
// The handshake (AC3) and advertising (D22)

#[test]
fn ac03_hello_is_answered_before_da1_in_raw_mode_and_v2_gets_only_da1() {
    let (_env, probe) = setup("ac03");
    let mut bytes = tsp_json("q", &json!({"q": "hello", "v": [1]}));
    bytes.extend_from_slice(b"\x1b[c");
    probe.write(&bytes);
    let input = probe.read_until("the DA1 answer", |b| {
        split_input(b).iter().any(|i| matches!(i, Input::Da1(_)))
    });
    let items = split_input(&input);
    let hello_at = items
        .iter()
        .position(|i| i.body().is_some_and(|b| b["r"] == "hello"));
    let da1_at = items.iter().position(|i| matches!(i, Input::Da1(_)));
    assert!(
        matches!((hello_at, da1_at), (Some(h), Some(d)) if h < d),
        "the hello reply must come before DA1: {items:?}"
    );

    let mut bytes = tsp_json("q", &json!({"q": "hello", "v": [2]}));
    bytes.extend_from_slice(b"\x1b[c");
    probe.write(&bytes);
    let input = probe.read_until("the DA1 answer", |b| {
        split_input(b).iter().any(|i| matches!(i, Input::Da1(_)))
    });
    let items = split_input(&input);
    assert!(
        items.iter().all(|i| !i.is_tsp()),
        "v:[2] gets only DA1: {items:?}"
    );
    assert!(items.iter().any(|i| matches!(i, Input::Da1(_))));
}

#[test]
fn advertises_exactly_the_t0_and_t1_kinds() {
    let (_env, probe) = setup("advert");
    let bodies = exchange(&probe, &hello(&[]));
    let reply = hello_reply(&bodies);
    let kinds: Vec<&str> = reply["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap())
        .collect();
    // §8.2, D22: T0 then T1, nothing else.
    let t0 =
        "col row card section rule spacer text badge kv icon spinner elapsed progress ansi rows el";
    let t1 = "md code diff editor input status seg overlay toast shimmer rate meter list item tabs table tree kbd tool agent checklist image";
    let mut want: Vec<&str> = t0.split(' ').chain(t1.split(' ')).collect();
    let mut got = kinds.clone();
    want.sort_unstable();
    got.sort_unstable();
    assert_eq!(got, want);
    assert_eq!(kinds.len(), 38, "no duplicates");
}

// ---------------------------------------------------------------------------------------------
// A. Parsing program output

#[test]
fn a1_only_tsp_apc_strings_route_to_tsp_and_none_are_drawn() {
    let (env, probe) = setup("a1");
    let mut bytes = b"before|".to_vec();
    bytes.extend_from_slice(b"\x1b_Gf=100,a=T;AAAA\x1b\\");
    bytes.extend_from_slice(b"\x1b_tspx;q;{\"q\":\"hello\",\"v\":[1]}\x1b\\");
    bytes.extend_from_slice(b"\x1b_tspq;{\"q\":\"hello\",\"v\":[1]}\x1b\\");
    bytes.extend_from_slice(b"|after");
    assert!(
        exchange(&probe, &bytes).is_empty(),
        "other APC strings aren't TSP"
    );
    let replies = exchange(&probe, &hello(&[]));
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["r"], "hello");
    let screen = screen_lines(&env, &probe).join("\n");
    assert!(screen.contains("before||after"), "{screen}");
    assert!(
        !screen.contains("tsp") && !screen.contains("Gf="),
        "{screen}"
    );
}

#[test]
fn a2_st_and_bel_end_a_tsp_string_and_0x9c_is_payload() {
    let (env, probe) = setup("a2");
    exchange(&probe, &open("s1", "flow"));
    let body =
        json!({"sf": "s1", "dark": {"accent": "#fff"}, "name": {"dark": "a\u{9c}b"}}).to_string();
    assert!(body.as_bytes().windows(2).any(|w| w == [0xc2, 0x9c]));
    assert!(exchange(&probe, &tsp("t", &[], &body)).is_empty());
    assert_eq!(
        surface(&env, &probe, "s1")["palette"]["name"]["dark"],
        "a\u{9c}b"
    );
    // BEL ends a TSP string as kitty does.
    let replies = exchange(&probe, b"\x1b_tsp;q;{\"q\":\"hello\",\"v\":[1]}\x07");
    assert_eq!(hello_reply(&replies)["r"], "hello");
    let screen = screen_lines(&env, &probe).join("\n");
    assert!(!screen.contains("tsp"), "{screen}");
}

#[test]
fn a3_parameters_need_a_following_semicolon_and_bodies_stay_whole() {
    let (env, probe) = setup("a3");
    exchange(&probe, &open("s1", "flow"));
    let bodies = exchange(&probe, &tsp("s", &[], r#"{"name":"a","css":"x=1;y"}"#));
    assert!(
        errors(&bodies).iter().all(|e| e.get("sheet").is_some()),
        "{bodies:?}"
    );
    assert_eq!(
        surface(&env, &probe, "s1")["sheets"],
        json!([["a", "x=1;y"]])
    );
    // A parameter-shaped segment before the body is a parameter (unknown, ignored).
    let bodies = exchange(
        &probe,
        &tsp(
            "f",
            &[("zz", "1")],
            r#"{"sf":"s1","s":2,"ops":[["add","main","s1",null,{"id":"main","k":"col","p":{"role":"x;y=1;"}}]]}"#,
        ),
    );
    assert!(bodies.is_empty(), "{bodies:?}");
    assert_eq!(
        doc_node(&env, &probe, "s1", "main").unwrap()["p"]["role"],
        "x;y=1;"
    );
}

#[test]
fn a4_unknown_verbs_parameters_and_fields_are_ignored() {
    let (env, probe) = setup("a4");
    let mut bytes = tsp("z", &[], r#"{"anything":1}"#);
    bytes.extend(tsp("Q", &[], r#"{"q":"hello","v":[1]}"#));
    bytes.extend(tsp("qq", &[], r#"{"q":"hello","v":[1]}"#));
    assert!(
        exchange(&probe, &bytes).is_empty(),
        "unknown verbs get no reply"
    );
    let bodies = exchange(
        &probe,
        &tsp(
            "o",
            &[("future", "1"), ("key", "99")],
            r#"{"id":"s1","mode":"flow","later":{"x":1}}"#,
        ),
    );
    assert!(bodies.is_empty(), "{bodies:?}");
    let s = surface(&env, &probe, "s1");
    assert_eq!(s["mode"], "flow");
    assert_ne!(
        s["anchor"]["key"], 99,
        "reserved parameters are ignored on a program's o"
    );
    let bodies = exchange(
        &probe,
        &tsp_json("f", &json!({"sf": "s1", "s": 1, "ops": [], "extra": true})),
    );
    assert!(bodies.is_empty(), "{bodies:?}");
}

#[test]
fn a5_a_known_verb_with_a_malformed_body_gets_an_error() {
    let (_env, probe) = setup("a5");
    for verb in ["q", "o", "f", "t", "s", "x"] {
        let bodies = exchange(&probe, &tsp(verb, &[], "{nope"));
        assert_eq!(bodies.len(), 1, "{verb}: {bodies:?}");
        let msg = bodies[0]["msg"].as_str().unwrap();
        assert!(
            msg.starts_with(&format!("malformed {verb} body: ")),
            "{msg}"
        );
        assert_eq!(bodies[0]["ev"], "error");
    }
    // Subject to the listening rules: after a prompt, nothing.
    exchange(&probe, b"\x1b]133;A\x1b\\");
    assert!(exchange(&probe, &tsp("f", &[], "{nope")).is_empty());
}

#[test]
fn a6_one_apc_string_holds_262144_bytes_and_a_longer_one_is_abandoned() {
    let (env, probe) = setup("a6");
    exchange(&probe, &open_with("s1", "flow", text_doc()));
    let head = r#"{"sf":"s1","s":2,"ops":[["text","t","replace",""#;
    let tail = r#""]]}"#;
    let fill = |n: usize, c: &str| {
        let pad = n - b"tsp;f;".len() - head.len() - tail.len();
        tsp("f", &[], &format!("{head}{}{tail}", c.repeat(pad)))
    };
    let exact = fill(262_144, "x");
    assert_eq!(exact.len() - 4, 262_144);
    assert!(exchange(&probe, &exact).is_empty());
    let len = |env: &TestEnv| {
        doc_node(env, &probe, "s1", "t").unwrap()["p"]["text"]
            .as_str()
            .unwrap()
            .len()
    };
    let full = len(&env);
    assert!(full > 262_000, "{full}");
    assert!(
        exchange(&probe, &fill(262_145, "y")).is_empty(),
        "abandoned silently"
    );
    assert_eq!(len(&env), full);
    assert!(
        !doc_node(&env, &probe, "s1", "t").unwrap()["p"]["text"]
            .as_str()
            .unwrap()
            .contains('y')
    );
}

#[test]
fn a7_chunks_rejoin_bytewise_keeping_the_first_chunks_parameters() {
    let (env, probe) = setup("a7");
    exchange(&probe, &open_with("s1", "flow", text_doc()));
    let n = 1000;
    let text: String = "é😀ab;k=v;".chars().cycle().take(3 * n).collect();
    let body = json!({"sf": "s1", "s": 2, "ops": [["text", "t", "replace", text]]}).to_string();
    // The SDK's chunker, at apc = n.
    let bytes = tern_sdk::wire::encode("f", &[], body.as_bytes(), n, "k7");
    assert!(split_input(&bytes).len() >= 3);
    assert!(exchange(&probe, &bytes).is_empty());
    assert_eq!(
        doc_node(&env, &probe, "s1", "t").unwrap()["p"]["text"],
        json!(text)
    );

    // A chunked blob keeps the first chunk's parameters (its mime).
    exchange(&probe, &hello(&[]));
    let data: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
    let blob = tern_sdk::wire::Blob::new(data.clone(), Some("image/png".into())).unwrap();
    let mut enc = tern_sdk::wire::Encoder::new(512);
    let bytes = enc.encode(&tern_sdk::wire::Message::Blob(blob)).unwrap();
    assert!(bytes.starts_with(b"\x1b_tsp;b;mime=image/png;c="));
    assert!(exchange(&probe, &bytes).is_empty());
    let id = sha256(&data);
    let have = exchange(&probe, &tsp_json("q", &json!({"q": "blobs", "ids": [id]})));
    assert_eq!(have, [json!({"r": "blobs", "have": [id]})]);
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn a8_chunk_sequences_drop_with_errors() {
    let (env, probe) = setup("a8");
    // Interrupted: the intervening message is still handled.
    let mut bytes = tsp("f", &[("c", "k7"), ("m", "1")], r#"{"sf":"s1","#);
    bytes.extend(open("s1", "flow"));
    let bodies = exchange(&probe, &bytes);
    assert_eq!(
        bodies,
        [json!({"ev": "error", "msg": "chunked f message c=k7 dropped: interrupted"})]
    );
    assert_eq!(surface(&env, &probe, "s1")["state"], "live");

    // Timed out: 1 s without a chunk.
    let bodies = exchange(
        &probe,
        &tsp("f", &[("c", "k8"), ("m", "1")], r#"{"sf":"s1","#),
    );
    assert!(bodies.is_empty());
    let started = Instant::now();
    let bodies = await_tsp(&probe, "the chunk timeout", |b| !b.is_empty());
    let waited = started.elapsed();
    assert_eq!(
        bodies,
        [json!({"ev": "error", "msg": "chunked f message c=k8 dropped: timed out"})]
    );
    assert!(
        waited > Duration::from_millis(700) && waited < Duration::from_secs(3),
        "{waited:?}"
    );

    // Over 24 MiB joined.
    let piece = "x".repeat(65_000);
    let mut bytes = Vec::new();
    for _ in 0..390 {
        bytes.extend(tsp("f", &[("c", "big"), ("m", "1")], &piece));
    }
    let bodies = exchange(&probe, &bytes);
    assert_eq!(
        bodies,
        [json!({"ev": "error", "msg": "chunked f message c=big dropped: over 25165824 bytes"})]
    );
    // The rest of that sequence is swallowed silently.
    assert!(exchange(&probe, &tsp("f", &[("c", "big")], "tail")).is_empty());

    // RIS drops a sequence silently.
    let mut bytes = tsp("f", &[("c", "k9"), ("m", "1")], r#"{"sf":"s1","#);
    bytes.extend_from_slice(b"\x1bc");
    bytes.extend(hello(&[]));
    let bodies = exchange(&probe, &bytes);
    assert_eq!(bodies.len(), 1, "only the hello reply: {bodies:?}");
    assert!(state(&env, &probe)["chunk_pending"].is_null());
}

#[test]
fn a9_blobs_hash_their_bytes_and_bad_ones_are_rejected() {
    let (env, probe) = setup("a9");
    exchange(&probe, &hello(&[]));
    let b64 = base64::engine::general_purpose::STANDARD;
    let data = b"hello blob";
    let id = sha256(data);
    assert!(
        exchange(
            &probe,
            &tsp("b", &[("mime", "text/plain")], &b64.encode(data))
        )
        .is_empty()
    );
    let have = exchange(
        &probe,
        &tsp_json("q", &json!({"q": "blobs", "ids": [id, "00"]})),
    );
    assert_eq!(have, [json!({"r": "blobs", "have": [id]})]);
    // On disk under the state dir, by SHA-256.
    assert_eq!(
        std::fs::read(env.state.join("blobs").join(&id)).unwrap(),
        data
    );

    let bad_id = tsp("b", &[("id", &"0".repeat(64))], &b64.encode(b"other"));
    let bodies = exchange(&probe, &bad_id);
    assert_eq!(errors(&bodies).len(), 1, "{bodies:?}");
    let bodies = exchange(&probe, &tsp("b", &[], "not*base64"));
    assert_eq!(errors(&bodies).len(), 1, "{bodies:?}");
    // More than 16 MiB decoded.
    let big = vec![7u8; (16 << 20) + 1];
    let bytes = tern_sdk::wire::encode("b", &[], b64.encode(&big).as_bytes(), 65_536, "bb");
    let bodies = exchange(&probe, &bytes);
    assert_eq!(errors(&bodies).len(), 1, "{bodies:?}");
    // A matching id is accepted.
    let ok = tsp("b", &[("id", &sha256(b"x"))], &b64.encode(b"x"));
    assert!(exchange(&probe, &ok).is_empty());
}

#[test]
fn a10_sdk_serialization_variance_is_accepted() {
    let (env, probe) = setup("a10");
    let mut bytes = tsp("q", &[], r#"{"v":[1],"features":[],"q":"hello"}"#);
    bytes.extend(tsp(
        "o",
        &[],
        r#"{"mode":"flow","adopt":false,"listen":true,"id":"s1"}"#,
    ));
    bytes.extend(tsp(
        "s",
        &[],
        r#"{"sf":"s1","name":"a","css":".a{color:red}"}"#,
    ));
    bytes.extend(tsp("s", &[], r#"{"sf":"s1","name":"a","css":null}"#));
    bytes.extend(tsp("s", &[], r#"{"sf":"s1","name":"b","css":""}"#));
    bytes.extend(tsp("s", &[], r#"{"sf":"s1","name":"c"}"#));
    let bodies = exchange(&probe, &bytes);
    assert_eq!(bodies.len(), 1, "only the hello reply: {bodies:?}");
    let s = surface(&env, &probe, "s1");
    assert_eq!(
        (s["listen"].clone(), s["sheets"].clone()),
        (json!(true), json!([]))
    );
}

// ---------------------------------------------------------------------------------------------
// B. Handshake

#[test]
fn b1_hello_is_answered_before_the_da1_that_follows_it() {
    let (_env, probe) = setup("b1");
    let mut bytes = hello(&[]);
    bytes.extend_from_slice(b"\x1b[c");
    probe.write(&bytes);
    let input = probe.read_until("DA1", |b| {
        split_input(b).iter().any(|i| matches!(i, Input::Da1(_)))
    });
    let items = split_input(&input);
    assert!(
        matches!(&items[..], [Input::Tsp { verb, body, .. }, Input::Da1(_)] if verb == "r" && body["r"] == "hello"),
        "{items:?}"
    );
}

#[test]
fn b2_a_hello_without_v1_gets_no_reply_but_da1_is_answered() {
    let (_env, probe) = setup("b2");
    let mut bytes = tsp_json("q", &json!({"q": "hello", "v": [2]}));
    bytes.extend_from_slice(b"\x1b[c");
    probe.write(&bytes);
    let input = probe.read_until("DA1", |b| {
        split_input(b).iter().any(|i| matches!(i, Input::Da1(_)))
    });
    let items = split_input(&input);
    assert!(matches!(&items[..], [Input::Da1(_)]), "{items:?}");
}

#[test]
fn b3_da1_answers_with_at_least_one_digit() {
    let (_env, probe) = setup("b3");
    probe.write(b"\x1b[c");
    let input = probe.read_until("DA1", |b| b.ends_with(b"c"));
    let text = String::from_utf8(input).unwrap();
    let inner = text
        .strip_prefix("\x1b[?")
        .and_then(|t| t.strip_suffix('c'))
        .unwrap_or_else(|| panic!("{text:?}"));
    assert!(
        inner
            .split(';')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())),
        "{text:?}"
    );
    // Go's matcher, and the SDK's parser, read it as DA1.
    let mut parser = tern_sdk::input::Parser::new();
    let items = parser.feed(text.as_bytes());
    assert!(
        matches!(&items[..], [tern_sdk::input::Item::Da1]),
        "{items:?}"
    );
}

#[test]
fn b4_the_hello_reply_carries_every_field_with_exact_types() {
    let (_env, probe) = setup("b4");
    probe.write(&hello(&["edit"]));
    let input = probe.read_until("the hello reply", |b| {
        split_input(b).iter().any(Input::is_tsp)
    });
    let reply = tsp_bodies(&split_input(&input)).remove(0);
    let obj = reply.as_object().unwrap();
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "apc",
            "cell",
            "cols",
            "credits",
            "dark",
            "features",
            "hour12",
            "kinds",
            "r",
            "reduceMotion",
            "term",
            "v",
            "ver"
        ]
    );
    assert_eq!(reply["r"], "hello");
    assert_eq!(reply["v"], 1);
    assert_eq!(reply["term"], "auk");
    assert!(reply["ver"].is_string());
    assert!(
        reply["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .all(Value::is_string)
    );
    assert!(
        reply["features"]
            .as_array()
            .unwrap()
            .iter()
            .all(Value::is_string)
    );
    for k in ["apc", "credits", "cols"] {
        assert!(reply[k].is_u64(), "{k}: {reply}");
    }
    assert!(reply["cell"]["w"].is_number() && reply["cell"]["h"].is_number());
    for k in ["dark", "reduceMotion", "hour12"] {
        assert!(reply[k].is_boolean(), "{k}: {reply}");
    }
    assert_eq!(reply["cols"], 80);
    // The Rust SDK, which drops a mistyped reply, reads it as a hello.
    let mut parser = tern_sdk::input::Parser::new();
    let items = parser.feed(&input);
    assert!(
        matches!(
            &items[..],
            [tern_sdk::input::Item::Reply(tern_sdk::wire::Reply::Hello(
                _
            ))]
        ),
        "{items:?}"
    );
}

#[test]
fn b5_apc_and_credits_are_usable() {
    let (_env, probe) = setup("b5");
    let reply = hello_reply(&exchange(&probe, &hello(&[]))).clone();
    let apc = reply["apc"].as_u64().unwrap();
    assert!(apc >= 1);
    // The longest prefix a chunk carries, plus a full chunk, fits one APC string.
    let prefix = "tsp;f;c=zzzzzzzzzzzz;m=1;".len() as u64;
    assert!(prefix + apc <= 262_144, "{apc}");
    assert!(reply["credits"].as_u64().unwrap() >= 1);
}

#[test]
fn b6_kinds_and_features_list_only_what_is_implemented() {
    let (_env, probe) = setup("b6");
    let reply = hello_reply(&exchange(&probe, &hello(&[]))).clone();
    let kinds = reply["kinds"].as_array().unwrap();
    for t2 in ["math", "chart", "picker", "prefs", "effort", "block"] {
        assert!(!kinds.contains(&json!(t2)), "{t2} isn't drawn in v1");
    }
    // omp sends `image` unguarded, so it is T1 (D24).
    assert!(kinds.contains(&json!("image")));
    assert_eq!(kinds.len(), 38);
    let allowed = [
        "blobs",
        "settle",
        "adopt",
        "dock",
        "program-palette",
        "reduce-motion",
        "aside",
        "scroll",
        "styles",
        "flow",
    ];
    let features: Vec<&str> = reply["features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    assert!(features.iter().all(|f| allowed.contains(f)), "{features:?}");
    // The aside docking of `prefs` isn't implemented (prefs is T2).
    assert!(!features.contains(&"aside"));
    // An unadvertised kind fails as unknown.
    exchange(&probe, &open("s1", "flow"));
    let bodies = exchange(
        &probe,
        &frame(
            "s1",
            1,
            json!([["add", "main", "s1", null, {"id": "main", "k": "col", "c": [{"id": "i", "k": "math"}]}]]),
        ),
    );
    assert_eq!(errors(&bodies).len(), 1, "{bodies:?}");
}

#[test]
fn b7_blobs_queries_answer_in_request_order_within_a_second() {
    let (_env, probe) = setup("b7");
    exchange(&probe, &hello(&[]));
    let b64 = base64::engine::general_purpose::STANDARD;
    let (a, b) = (b"first".as_slice(), b"second".as_slice());
    exchange(&probe, &tsp("b", &[], &b64.encode(a)));
    exchange(&probe, &tsp("b", &[], &b64.encode(b)));
    let ids = [sha256(b), "f".repeat(64), sha256(a)];
    let started = Instant::now();
    probe.write(&tsp_json("q", &json!({"q": "blobs", "ids": ids})));
    let bodies = await_tsp(&probe, "the blobs reply", |b| !b.is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(bodies, [json!({"r": "blobs", "have": [ids[0], ids[2]]})]);
}

#[test]
fn b8_no_other_query_is_answered() {
    let (_env, probe) = setup("b8");
    let mut bytes = tsp_json("q", &json!({"q": "version"}));
    bytes.extend(tsp_json("q", &json!({"q": "kinds", "v": [1]})));
    bytes.extend(tsp_json("q", &json!({"nothing": true})));
    assert!(exchange(&probe, &bytes).is_empty());
}

#[test]
fn b9_each_hello_replaces_the_last_and_is_forgotten_when_the_program_is_gone() {
    let (env, probe) = setup("b9");
    exchange(
        &probe,
        &tsp_json(
            "q",
            &json!({"q": "hello", "v": [1], "app": "one", "features": ["edit"]}),
        ),
    );
    exchange(
        &probe,
        &tsp_json(
            "q",
            &json!({"q": "hello", "v": [1], "app": "two", "features": ["send"]}),
        ),
    );
    let st = state(&env, &probe);
    assert_eq!(st["hello"], json!({"app": "two", "features": ["send"]}));
    // `app` applies only while a listening surface is open.
    assert!(st["app"].is_null());
    exchange(&probe, &open("s1", "flow"));
    assert_eq!(state(&env, &probe)["app"], "two");
    exchange(
        &probe,
        &tsp_json(
            "o",
            &json!({"id": "quiet", "mode": "inline", "listen": false}),
        ),
    );
    exchange(&probe, b"\x1b]133;A\x1b\\");
    let st = state(&env, &probe);
    assert!(st["hello"].is_null() && st["app"].is_null(), "{st}");
}

#[test]
fn b10_edit_undo_and_send_go_only_to_programs_that_listed_them() {
    let (env, window, probe) = setup_window("b10");
    let field = json!([{"id": "ed", "k": "editor", "p": {"text": "abc", "sendable": true}}]);
    exchange(&probe, &hello(&[]));
    probe.write(&open_with("s1", "inline", field.clone()));
    await_tsp(&probe, "the first ack", |b| !events(b, "ack").is_empty());
    let edit =
        json!({"ev": "edit", "id": "ed", "from": 0, "to": 0, "text": "x", "cursor": 1, "len": 3});
    window_event(&window, &probe, "s1", edit.clone());
    window_event(&window, &probe, "s1", json!({"ev": "undo", "id": "ed"}));
    window_event(
        &window,
        &probe,
        "s1",
        json!({"ev": "send", "id": "ed", "text": "hi"}),
    );
    std::thread::sleep(Duration::from_millis(300));
    let bodies = probe.fence_tsp();
    assert!(
        bodies
            .iter()
            .all(|b| b["ev"] == "ack" || b["ev"] == "resize"),
        "no feature, no event: {bodies:?}"
    );

    exchange(&probe, &hello(&["edit", "undo", "send"]));
    window_event(&window, &probe, "s1", edit);
    window_event(&window, &probe, "s1", json!({"ev": "undo", "id": "ed"}));
    window_event(
        &window,
        &probe,
        "s1",
        json!({"ev": "send", "id": "ed", "text": "hi"}),
    );
    let bodies = await_tsp(&probe, "edit, undo and send", |b| {
        b.iter().filter(|e| e["ev"] != "ack").count() >= 3
    });
    let names: Vec<&str> = bodies
        .iter()
        .map(|b| b["ev"].as_str().unwrap())
        .filter(|e| *e != "ack")
        .collect();
    assert_eq!(names, ["edit", "undo", "send"]);
    let _ = env;
}

#[test]
fn b11_term_program_is_set_and_early_frames_are_accepted() {
    let env = env("b11");
    let pane = env.new_pane(&[
        "--",
        "sh",
        "-c",
        "'printf \"[%s|%s]\" \"$TERM_PROGRAM\" \"$TERM_PROGRAM_VERSION\"; sleep 30'",
    ]);
    let shown = wait_for("the program's environment", || {
        let text = env.daemon(&format!("capture {pane}"))["text"]
            .as_str()?
            .to_owned();
        text.contains(']').then_some(text)
    });
    let probe = Probe::start(&env, &[]);
    let reply = hello_reply(&exchange(&probe, &hello(&[]))).clone();
    assert!(
        shown.contains(&format!("[auk|{}]", reply["ver"].as_str().unwrap())),
        "{shown}"
    );

    // omp's optimistic start: o, t and f before the hello is answered.
    let (env2, probe2) = setup("b11b");
    let mut bytes = open("s1", "inline");
    bytes.extend(tsp_json("t", &json!({"dark": {"cardBg": "#112233"}})));
    bytes.extend(frame(
        "s1",
        1,
        json!([["add", "main", "s1", null, {"id": "main", "k": "col"}]]),
    ));
    bytes.extend(hello(&[]));
    let bodies = exchange(&probe2, &bytes);
    assert_eq!(bodies.len(), 1, "{bodies:?}");
    let s = surface(&env2, &probe2, "s1");
    assert_eq!(s["palette"]["dark"]["cardBg"], "#112233");
    assert!(node(&s["doc"], "main").is_some());
}

#[test]
fn b12_tsp_can_be_switched_off_as_a_host_setting() {
    let (env, probe) = setup("b12");
    env.daemon("settings --native-surfaces off");
    wait_state(&env, &probe, "TSP off", |st| st["enabled"] == false);
    let mut bytes = hello(&[]);
    bytes.extend(open("s1", "flow"));
    bytes.extend(tsp("f", &[], "{nope"));
    assert!(
        exchange(&probe, &bytes).is_empty(),
        "no hello answered, every message ignored"
    );
    assert!(surfaces(&env, &probe).is_empty());
    let screen = screen_lines(&env, &probe).join("\n");
    assert!(
        !screen.contains("tsp"),
        "TSP strings are still consumed: {screen}"
    );
    env.daemon("settings --native-surfaces on");
    assert_eq!(exchange(&probe, &hello(&[])).len(), 1);
}

// ---------------------------------------------------------------------------------------------
// C. Surfaces

#[test]
fn c1_o_without_id_unknown_mode_or_inline_on_the_alt_screen_is_an_error() {
    let (env, probe) = setup("c1");
    assert_eq!(
        errors(&exchange(&probe, &tsp_json("o", &json!({"mode": "flow"})))).len(),
        1
    );
    assert_eq!(
        errors(&exchange(
            &probe,
            &tsp_json("o", &json!({"id": "s1", "mode": "sideways"}))
        ))
        .len(),
        1
    );
    probe.write(b"\x1b[?1049h");
    for mode in ["inline", "flow"] {
        let bodies = exchange(&probe, &open("a", mode));
        assert_eq!(errors(&bodies).len(), 1, "{mode}: {bodies:?}");
        assert_eq!(errors(&bodies)[0]["sf"], "a");
    }
    assert!(exchange(&probe, &open("scr", "screen")).is_empty());
    let all = surfaces(&env, &probe);
    assert_eq!(all.len(), 1);
    assert_eq!(
        (all[0]["mode"].clone(), all[0]["buffer"].clone()),
        (json!("screen"), json!("alt"))
    );
}

#[test]
fn c2_one_live_surface_per_mode_per_buffer_with_replacement_rules() {
    let (env, probe) = setup("c2");
    exchange(&probe, &open_with("i1", "inline", text_doc()));
    exchange(&probe, &open_with("i2", "inline", text_doc()));
    let all = surfaces(&env, &probe);
    let state_of = |id: &str| {
        all.iter()
            .find(|s| s["id"] == id)
            .map(|s| s["state"].clone())
    };
    assert_eq!(
        state_of("i1"),
        Some(json!("closed")),
        "replaced inline is closed as if kept"
    );
    assert_eq!(state_of("i2"), Some(json!("live")));
    exchange(&probe, &open("sc1", "screen"));
    exchange(&probe, &open("sc2", "screen"));
    exchange(&probe, &open("f1", "flow"));
    let all = surfaces(&env, &probe);
    let ids: Vec<&str> = all
        .iter()
        .filter(|s| s["state"] == "live")
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["i2", "sc2", "f1"],
        "the replaced screen surface is discarded"
    );
    assert!(all.iter().all(|s| s["id"] != "sc1"));
    // A new inline o discards the covering screen surface; flow is untouched.
    exchange(&probe, &open("i3", "inline"));
    let all = surfaces(&env, &probe);
    assert!(all.iter().all(|s| s["id"] != "sc2"));
    assert!(all.iter().any(|s| s["id"] == "f1" && s["state"] == "live"));
}

#[test]
fn c3_f_and_x_route_to_the_first_open_surface_on_the_active_buffer() {
    let (env, probe) = setup("c3");
    exchange(&probe, &open("a", "inline"));
    probe.write(b"\x1b[?1049h");
    exchange(&probe, &open("a", "screen"));
    exchange(
        &probe,
        &frame(
            "a",
            1,
            json!([["add", "main", "a", null, {"id": "main", "k": "col"}]]),
        ),
    );
    let all = surfaces(&env, &probe);
    let alt = all.iter().find(|s| s["buffer"] == "alt").unwrap();
    let main = all.iter().find(|s| s["buffer"] == "main").unwrap();
    assert!(
        node(&alt["doc"], "main").is_some(),
        "the active buffer's surface took the frame"
    );
    assert!(node(&main["doc"], "main").is_none());
    exchange(&probe, &tsp_json("x", &json!({"id": "a", "keep": false})));
    let all = surfaces(&env, &probe);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0]["buffer"], "main");
}

#[test]
fn c4_inline_and_flow_surfaces_anchor_below_a_nonempty_cursor_row() {
    let (env, probe) = setup("c4");
    probe.write(b"\x1b[2J\x1b[Habc");
    exchange(&probe, &open("s1", "flow"));
    probe.write(b"X");
    let lines: Vec<String> = env.daemon(&format!("capture {} --scrollback", probe.pane))["text"]
        .as_str()
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    let abc = line_of(&lines, "abc");
    let x = line_of(&lines, "X");
    let anchor = surface(&env, &probe, "s1")["anchor"]["line"]
        .as_u64()
        .unwrap() as usize;
    assert_eq!(
        anchor,
        abc + 1,
        "the anchor is the row after abc: {lines:?}"
    );
    assert_eq!(
        x,
        abc + 2,
        "the cursor goes to column 0 of the row below the anchor"
    );
    assert!(lines[x].starts_with('X'));
}

#[test]
fn c5_a_surface_ends_with_its_anchor_row() {
    let (env, probe) = setup("c5");
    exchange(&probe, &hello(&[]));
    // Erased.
    exchange(&probe, &open("e", "flow"));
    let bodies = exchange(&probe, b"\x1b[2J");
    assert!(
        bodies.contains(&json!({"ev": "gone", "sf": "e", "ids": ["e"]})),
        "{bodies:?}"
    );
    // The scrollback cleared.
    probe.write(b"\x1b[H");
    exchange(&probe, &open("c", "flow"));
    probe.write(&b"line\r\n".repeat(40));
    env.daemon(&format!("clear-scrollback {}", probe.pane));
    let bodies = probe.fence_tsp();
    assert!(
        bodies.contains(&json!({"ev": "gone", "sf": "c", "ids": ["c"]})),
        "{bodies:?}"
    );
    // Scrolled out of a scroll region.
    probe.write(b"\x1b[2J\x1b[3;7r\x1b[3;1H");
    exchange(&probe, &open("r", "flow"));
    let bodies = exchange(&probe, &b"\n".repeat(10));
    assert!(
        bodies.contains(&json!({"ev": "gone", "sf": "r", "ids": ["r"]})),
        "{bodies:?}"
    );
    probe.write(b"\x1b[r");
    // Leaving the scrollback.
    env.daemon("settings --scrollback 10");
    exchange(&probe, &open("s", "flow"));
    let bodies = exchange(&probe, &b"x\r\n".repeat(60));
    assert!(
        bodies.contains(&json!({"ev": "gone", "sf": "s", "ids": ["s"]})),
        "{bodies:?}"
    );
    assert!(surfaces(&env, &probe).is_empty());
}

#[test]
fn c6_x_keeps_main_by_default_and_keep_false_removes() {
    let (env, probe) = setup("c6");
    exchange(&probe, &open("s1", "inline"));
    exchange(
        &probe,
        &frame(
            "s1",
            1,
            json!([
                ["add", "main", "s1", null, {"id": "main", "k": "col", "c": [{"id": "ed", "k": "editor"}]}],
                ["add", "dock", "s1", null, {"id": "dock", "k": "col"}],
                ["add", "layer", "s1", null, {"id": "layer", "k": "col"}],
                ["focus", "ed"],
            ]),
        ),
    );
    assert_eq!(surface(&env, &probe, "s1")["focus"], "ed");
    exchange(&probe, &tsp_json("x", &json!({"id": "s1"})));
    let s = surface(&env, &probe, "s1");
    assert_eq!(s["state"], "closed");
    assert!(node(&s["doc"], "main").is_some());
    assert!(node(&s["doc"], "dock").is_none() && node(&s["doc"], "layer").is_none());
    assert!(s["focus"].is_null());
    let bodies = exchange(&probe, &frame("s1", 2, json!([])));
    assert_eq!(
        errors(&bodies).len(),
        1,
        "a closed surface takes no frames: {bodies:?}"
    );
    exchange(&probe, &tsp_json("x", &json!({"id": "s1", "keep": false})));
    assert!(surfaces(&env, &probe).is_empty());
}

#[test]
fn c7_adopt_reopens_the_newest_closed_inline_surface() {
    let (env, probe) = setup("c7");
    exchange(&probe, &hello(&[]));
    exchange(&probe, &open_with("e", "inline", text_doc()));
    exchange(&probe, &tsp_json("x", &json!({"id": "e"})));
    assert_eq!(surface(&env, &probe, "e")["state"], "closed");
    let bodies = exchange(
        &probe,
        &tsp_json(
            "o",
            &json!({"id": "e", "adopt": true, "mode": "flow", "title": "again"}),
        ),
    );
    assert!(bodies.is_empty(), "{bodies:?}");
    let s = surface(&env, &probe, "e");
    assert_eq!(
        (s["state"].clone(), s["mode"].clone(), s["title"].clone()),
        (json!("live"), json!("inline"), json!("again"))
    );
    assert!(node(&s["doc"], "t").is_some(), "the same document");
    let bodies = exchange(&probe, &tsp_json("o", &json!({"id": "zz", "adopt": true})));
    assert_eq!(bodies, [json!({"ev": "gone", "ids": ["zz"]})]);
}

#[test]
fn c8_listen_false_is_silent_until_the_next_q_or_listening_o() {
    let (env, window, probe) = setup_window("c8");
    exchange(
        &probe,
        &tsp_json("o", &json!({"id": "q1", "mode": "flow", "listen": false})),
    );
    exchange(
        &probe,
        &frame(
            "q1",
            1,
            json!([["add", "main", "q1", null, {"id": "main", "k": "col", "c": [{"id": "b", "k": "el", "p": {"tag": "input", "type": "checkbox", "name": "c"}}]}]]),
        ),
    );
    assert!(exchange(&probe, &tsp("f", &[], "{nope")).is_empty());
    assert!(exchange(&probe, &frame("q1", 2, json!([["set", "nope", {}]]))).is_empty());
    // Controls don't flip, and nothing is acked.
    assert_eq!(
        click(&window, &probe, "q1", "b", ""),
        json!({"event": null})
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(probe.fence_tsp().is_empty());
    assert_eq!(surface(&env, &probe, "q1")["interactive"], false);
    // The next q is answered.
    assert_eq!(exchange(&probe, &hello(&[])).len(), 1);
}

#[test]
fn c9_exit_closes_inline_and_flow_and_discards_screen_and_ris_clears_all() {
    let (env, probe) = setup("c9");
    exchange(&probe, &open_with("f", "flow", text_doc()));
    exchange(&probe, &open_with("i", "inline", text_doc()));
    exchange(&probe, &open_with("sc", "screen", text_doc()));
    // RIS clears everything; afterwards nothing until q or o.
    let bodies = exchange(&probe, b"\x1bc");
    assert!(bodies.is_empty());
    assert!(surfaces(&env, &probe).is_empty());
    assert!(exchange(&probe, &tsp("f", &[], "{nope")).is_empty());
    exchange(&probe, &open_with("f", "flow", text_doc()));
    exchange(&probe, &open_with("i", "inline", text_doc()));
    exchange(&probe, &open_with("sc", "screen", text_doc()));
    probe.exit(0);
    env.wait(probe.pane, "exit");
    let st = state(&env, &probe);
    let all = st["surfaces"].as_array().unwrap();
    let states: Vec<(&str, &str)> = all
        .iter()
        .map(|s| (s["id"].as_str().unwrap(), s["state"].as_str().unwrap()))
        .collect();
    assert_eq!(states, [("f", "closed"), ("i", "closed")], "{st}");
    assert_eq!(st["gate"], "gone");
}

#[test]
fn c10_a_prompt_discards_inline_and_screen_surfaces_and_closes_flow() {
    let (env, probe) = setup("c10");
    exchange(&probe, &open_with("kept", "inline", text_doc()));
    exchange(&probe, &tsp_json("x", &json!({"id": "kept"})));
    exchange(&probe, &open_with("f", "flow", text_doc()));
    probe.write(b"below the flow\r\n");
    exchange(&probe, &open_with("sc", "screen", text_doc()));
    probe.write(b"\x1b[2J\x1b[H");
    let _ = surfaces(&env, &probe);
    // An inline surface with nothing below its anchor: the row is undone at the prompt.
    exchange(&probe, &open_with("f2", "flow", text_doc()));
    probe.write(b"abc\r\n");
    exchange(&probe, &open_with("i", "inline", text_doc()));
    exchange(&probe, b"\x1b]133;A\x1b\\");
    probe.write(b"Y");
    let all = surfaces(&env, &probe);
    let summary: Vec<(String, String)> = all
        .iter()
        .map(|s| {
            (
                s["id"].as_str().unwrap().to_owned(),
                s["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(summary, [("f2".to_owned(), "closed".to_owned())], "{all:?}");
    let lines = screen_lines(&env, &probe);
    assert_eq!(
        line_of(&lines, "Y"),
        line_of(&lines, "abc") + 1,
        "the inline anchor row was undone: {lines:?}"
    );
}

#[test]
fn c11_a_screen_surface_on_the_alt_screen_goes_with_it() {
    let (env, probe) = setup("c11");
    exchange(&probe, &hello(&[]));
    probe.write(b"\x1b[?1049h");
    exchange(&probe, &open("a", "screen"));
    let bodies = exchange(&probe, b"\x1b[?1049l");
    assert_eq!(bodies, [json!({"ev": "gone", "sf": "a", "ids": ["a"]})]);
    probe.write(b"\x1b[?1049h");
    exchange(&probe, &open("b", "screen"));
    let bodies = exchange(&probe, b"\x1b[2J");
    assert_eq!(bodies, [json!({"ev": "gone", "sf": "b", "ids": ["b"]})]);
    assert!(surfaces(&env, &probe).is_empty());
}

#[test]
fn c12_suspend_hands_the_pane_back_to_the_grid() {
    let (env, window, probe) = setup_window("c12");
    exchange(
        &probe,
        &open_with(
            "s1",
            "inline",
            json!([{"id": "b", "k": "el", "p": {"tag": "button", "actions": {"click": "go"}}}]),
        ),
    );
    exchange(&probe, &frame("s1", 2, json!([["suspend"]])));
    let s = surface(&env, &probe, "s1");
    assert_eq!(
        (s["suspended"].clone(), s["interactive"].clone()),
        (json!(true), json!(false))
    );
    probe.write(b"grid output while suspended\r\n");
    assert!(
        screen_lines(&env, &probe)
            .iter()
            .any(|l| l.contains("grid output while suspended"))
    );
    assert_eq!(
        click(&window, &probe, "s1", "b", ""),
        json!({"event": null}),
        "no pointer focus"
    );
    exchange(&probe, &frame("s1", 3, json!([["resume"]])));
    let s = surface(&env, &probe, "s1");
    assert_eq!(
        (s["suspended"].clone(), s["state"].clone()),
        (json!(false), json!("live"))
    );
    assert_eq!(click(&window, &probe, "s1", "b", "")["act"], "go");
}

#[test]
fn c13_role_region_role_and_app_reach_every_replica() {
    let (env, window, probe) = setup_window("c13");
    exchange(
        &probe,
        &tsp_json("q", &json!({"q": "hello", "v": [1], "app": "omp"})),
    );
    exchange(
        &probe,
        &tsp_json(
            "o",
            &json!({"id": "s1", "mode": "flow", "role": "transcript"}),
        ),
    );
    exchange(
        &probe,
        &frame(
            "s1",
            1,
            json!([["add", "main", "s1", null, {"id": "main", "k": "col", "p": {"role": "log"}}]]),
        ),
    );
    let st = wait_for("the window's replica", || {
        let st = window_state(&window, &probe);
        (st["surfaces"].as_array()?.len() == 1 && node(&st["surfaces"][0]["doc"], "main").is_some())
            .then_some(st)
    });
    for st in [st, state(&env, &probe)] {
        assert_eq!(st["app"], "omp", "data-app");
        assert_eq!(st["surfaces"][0]["role"], "transcript", "data-surface");
        assert_eq!(
            node(&st["surfaces"][0]["doc"], "main").unwrap()["p"]["role"],
            "log",
            "data-role"
        );
    }
}

#[test]
fn c14_the_first_listening_surface_drops_the_shell_title() {
    let (env, probe) = setup("c14");
    probe.write(b"\x1b]2;shell title\x1b\\");
    assert_eq!(
        state(&env, &probe)["title"],
        json!({"source": "grid", "title": "shell title"})
    );
    exchange(
        &probe,
        &tsp_json("o", &json!({"id": "s1", "mode": "flow", "title": "Build"})),
    );
    assert_eq!(
        state(&env, &probe)["title"],
        json!({"source": "surface", "title": "Build"})
    );
    probe.write(b"\x1b]2;program title\x1b\\");
    assert_eq!(
        state(&env, &probe)["title"],
        json!({"source": "grid", "title": "program title"})
    );
}

// ---------------------------------------------------------------------------------------------
// D. Frames and ops

#[test]
fn d1_a_frame_applies_atomically_between_draws() {
    let (env, window, probe) = setup_window("d1");
    exchange(&probe, &open_with("s1", "flow", text_doc()));
    let ops: Vec<Value> = (0..50)
        .map(|i| json!(["add", format!("n{i}"), "main", null, {"id": format!("n{i}"), "k": "text", "p": {"text": "x".repeat(200)}}]))
        .collect();
    let body = json!({"sf": "s1", "s": 2, "ops": ops}).to_string();
    let bytes = tern_sdk::wire::encode("f", &[], body.as_bytes(), 1000, "d1");
    let pieces: Vec<&[u8]> = {
        let mut out = Vec::new();
        let mut at = 0;
        while let Some(end) = bytes[at..].windows(2).position(|w| w == b"\x1b\\") {
            out.push(&bytes[at..at + end + 2]);
            at += end + 2;
        }
        out
    };
    assert!(pieces.len() > 5);
    for piece in &pieces[..pieces.len() - 1] {
        probe.write(piece);
        probe.fence();
        let st = window_state(&window, &probe);
        let doc = &st["surfaces"][0]["doc"];
        assert!(
            node(doc, "n0").is_none(),
            "nothing applies before the frame is whole"
        );
    }
    probe.write(pieces[pieces.len() - 1]);
    probe.fence();
    let st = wait_for("the window's replica", || {
        let st = window_state(&window, &probe);
        node(&st["surfaces"][0]["doc"], "n49")
            .is_some()
            .then_some(st)
    });
    assert!(node(&st["surfaces"][0]["doc"], "n0").is_some());
    assert!(doc_node(&env, &probe, "s1", "n49").is_some());
}

#[test]
fn d2_a_failing_op_is_rejected_alone() {
    let (env, probe) = setup("d2");
    exchange(&probe, &open_with("s1", "flow", json!([])));
    let bodies = exchange(
        &probe,
        &frame(
            "s1",
            7,
            json!([
                ["add", "a", "main", null, {"id": "a", "k": "text"}],
                ["set", "nope", {}],
                ["set", "a", {"text": "ok"}],
            ]),
        ),
    );
    assert_eq!(bodies.len(), 1, "{bodies:?}");
    let e = &bodies[0];
    assert_eq!(
        (
            e["ev"].clone(),
            e["sf"].clone(),
            e["s"].clone(),
            e["op"].clone()
        ),
        (json!("error"), json!("s1"), json!(7), json!(1))
    );
    assert!(e["msg"].is_string());
    assert_eq!(
        doc_node(&env, &probe, "s1", "a").unwrap()["p"]["text"],
        "ok"
    );
}

#[test]
fn d3_a_frame_is_dropped_whole_for_a_bad_surface_ops_or_body() {
    let (env, probe) = setup("d3");
    exchange(&probe, &open_with("s1", "flow", json!([])));
    exchange(&probe, &open_with("old", "inline", json!([])));
    exchange(&probe, &tsp_json("x", &json!({"id": "old"})));
    let cases = [
        tsp_json("f", &json!({"s": 1, "ops": []})),
        frame("nope", 2, json!([])),
        frame("old", 3, json!([])),
        tsp_json("f", &json!({"sf": "s1", "s": 4, "ops": {"add": 1}})),
        tsp("f", &[], "[not json"),
    ];
    for bytes in cases {
        let bodies = exchange(&probe, &bytes);
        assert_eq!(bodies.len(), 1, "{bodies:?}");
        assert_eq!(bodies[0]["ev"], "error");
        assert!(bodies[0].get("op").is_none(), "{bodies:?}");
    }
    assert!(
        node(&surface(&env, &probe, "s1")["doc"], "main")
            .unwrap()
            .get("c")
            .is_none()
    );
}

#[test]
fn d4_s_is_echoed_and_never_checked_for_order() {
    let (env, _window, probe) = setup_window("d4");
    probe.write(&open_with("s1", "flow", json!([])));
    await_tsp(&probe, "the first ack", |b| !events(b, "ack").is_empty());
    probe.write(&frame(
        "s1",
        5,
        json!([["add", "a", "main", null, {"id": "a", "k": "text"}]]),
    ));
    let acks = await_tsp(&probe, "the ack of s=5", |b| {
        events(b, "ack").iter().any(|a| a["s"] == 5)
    });
    assert!(!acks.is_empty());
    probe.write(&frame(
        "s1",
        3,
        json!([["add", "b", "main", null, {"id": "b", "k": "text"}]]),
    ));
    await_tsp(&probe, "the ack of s=3", |b| {
        events(b, "ack").iter().any(|a| a["s"] == 3)
    });
    assert!(
        doc_node(&env, &probe, "s1", "a").is_some() && doc_node(&env, &probe, "s1", "b").is_some()
    );
    let bodies = exchange(&probe, &frame("s1", 9, json!([["set", "zz", {}]])));
    assert_eq!(errors(&bodies)[0]["s"], 9);
}

#[test]
fn d5_add_checks_ids_before_kinds_duplicates_and_limits() {
    let (env, probe) = setup("d5");
    exchange(&probe, &open_with("s1", "flow", json!([])));
    let add = |id: &str, node: Value| json!(["add", id, "main", null, node]);
    let bad = [
        add("x", json!({"id": "y", "k": "text"})),
        json!(["add", "x", "main", {"id": "x", "k": "text"}]),
        add("x", json!({"id": "x", "k": "math"})),
        add("main", json!({"id": "main", "k": "text"})),
        add(
            "p",
            json!({"id": "p", "k": "col", "c": [{"id": "ok1", "k": "text"}, {"id": "q", "k": "nope"}]}),
        ),
    ];
    for op in bad {
        let bodies = exchange(&probe, &frame("s1", 1, json!([op])));
        assert_eq!(errors(&bodies).len(), 1, "{op}: {bodies:?}");
    }
    assert!(
        doc_node(&env, &probe, "s1", "ok1").is_none(),
        "all or nothing"
    );
    // Depth over 64.
    let mut deep = json!({"id": "d65", "k": "col"});
    for i in (1..65).rev() {
        deep = json!({"id": format!("d{i}"), "k": "col", "c": [deep]});
    }
    let bodies = exchange(&probe, &frame("s1", 2, json!([add("d1", deep)])));
    assert_eq!(errors(&bodies).len(), 1, "{bodies:?}");
    // More than 200 000 nodes, root included.
    let children: Vec<Value> = (0..200_000)
        .map(|i| json!({"id": format!("{i}"), "k": "spacer"}))
        .collect();
    let body = json!({"sf": "s1", "s": 3, "ops": [add("big", json!({"id": "big", "k": "col", "c": children}))]}).to_string();
    let bytes = tern_sdk::wire::encode("f", &[], body.as_bytes(), 65_536, "big");
    let bodies = exchange(&probe, &bytes);
    assert_eq!(errors(&bodies).len(), 1, "{bodies:?}");
    assert!(doc_node(&env, &probe, "s1", "big").is_none());
}

#[test]
fn d6_set_merges_shallowly_and_null_deletes() {
    let (env, probe) = setup("d6");
    exchange(
        &probe,
        &open_with(
            "s1",
            "flow",
            json!([{"id": "b", "k": "badge", "p": {"text": "x", "tone": "info", "gone": null}}]),
        ),
    );
    assert_eq!(
        doc_node(&env, &probe, "s1", "b").unwrap()["p"],
        json!({"text": "x", "tone": "info"})
    );
    exchange(
        &probe,
        &frame(
            "s1",
            2,
            json!([["set", "b", {"tone": null, "icon": "check"}]]),
        ),
    );
    assert_eq!(
        doc_node(&env, &probe, "s1", "b").unwrap()["p"],
        json!({"text": "x", "icon": "check"})
    );
}

#[test]
fn d7_text_appends_and_replaces_on_text_kinds() {
    let (env, probe) = setup("d7");
    let kinds = [
        "text", "md", "code", "ansi", "editor", "input", "shimmer", "el",
    ];
    let nodes: Vec<Value> = kinds
        .iter()
        .map(|k| json!({"id": format!("main.{k}"), "k": k}))
        .collect();
    exchange(&probe, &open_with("s1", "flow", json!(nodes)));
    let mut ops = Vec::new();
    for k in kinds {
        ops.push(json!(["text", format!("main.{k}"), "append", "ab"]));
        ops.push(json!(["text", format!("main.{k}"), "append", "cd"]));
    }
    assert!(exchange(&probe, &frame("s1", 2, json!(ops))).is_empty());
    for k in kinds {
        assert_eq!(
            doc_node(&env, &probe, "s1", &format!("main.{k}")).unwrap()["p"]["text"],
            "abcd",
            "{k}"
        );
    }
    exchange(
        &probe,
        &frame("s1", 3, json!([["text", "main.md", "replace", "# new"]])),
    );
    assert_eq!(
        doc_node(&env, &probe, "s1", "main.md").unwrap()["p"]["text"],
        "# new"
    );
}

#[test]
fn d8_splice_counts_utf16_units() {
    let (env, probe) = setup("d8");
    exchange(
        &probe,
        &open_with(
            "s1",
            "flow",
            json!([{"id": "e", "k": "editor", "p": {"text": "a😀b"}}]),
        ),
    );
    assert!(exchange(&probe, &frame("s1", 2, json!([["splice", "e", 3, 1, "B"]]))).is_empty());
    assert_eq!(
        doc_node(&env, &probe, "s1", "e").unwrap()["p"]["text"],
        "a😀B"
    );
    for op in [
        json!(["splice", "e", 2, 0, "x"]),
        json!(["splice", "e", 9, 0, "x"]),
        json!(["splice", "e", 0, 9, ""]),
    ] {
        let bodies = exchange(&probe, &frame("s1", 3, json!([op])));
        assert_eq!(errors(&bodies).len(), 1, "{op}: {bodies:?}");
    }
}

#[test]
fn d9_move_is_validated() {
    let (env, probe) = setup("d9");
    exchange(
        &probe,
        &open_with(
            "s1",
            "flow",
            json!([
                {"id": "a", "k": "col", "c": [{"id": "a1", "k": "text"}]},
                {"id": "b", "k": "text"},
                {"id": "c", "k": "text"},
            ]),
        ),
    );
    assert!(exchange(&probe, &frame("s1", 2, json!([["move", "c", "main", "a"]]))).is_empty());
    let main = doc_node(&env, &probe, "s1", "main").unwrap();
    let order: Vec<&str> = main["c"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(order, ["c", "a", "b"]);
    for op in [
        json!(["move", "b", "main", "a1"]),
        json!(["move", "b", "main", "b"]),
        json!(["move", "a", "a1", null]),
        json!(["move", "a", "a", null]),
        json!(["move", "s1", "main", null]),
        json!(["del", "s1"]),
    ] {
        let bodies = exchange(&probe, &frame("s1", 3, json!([op])));
        assert_eq!(errors(&bodies).len(), 1, "{op}: {bodies:?}");
    }
}

#[test]
fn d10_del_removes_the_subtree_and_focus_inside_it() {
    let (env, probe) = setup("d10");
    exchange(
        &probe,
        &open_with(
            "s1",
            "inline",
            json!([{"id": "box", "k": "col", "c": [{"id": "ed", "k": "editor"}]}]),
        ),
    );
    exchange(&probe, &frame("s1", 2, json!([["focus", "ed"]])));
    assert_eq!(surface(&env, &probe, "s1")["focus"], "ed");
    exchange(&probe, &frame("s1", 3, json!([["del", "box"]])));
    let s = surface(&env, &probe, "s1");
    assert!(node(&s["doc"], "ed").is_none() && s["focus"].is_null());
    assert_eq!(
        errors(&exchange(
            &probe,
            &frame("s1", 4, json!([["set", "ed", {}]]))
        ))
        .len(),
        1
    );
}

#[test]
fn d11_settle_is_a_hint_cleared_by_later_ops_inside() {
    let (env, probe) = setup("d11");
    exchange(
        &probe,
        &open_with(
            "s1",
            "flow",
            json!([{"id": "a", "k": "col", "c": [{"id": "a1", "k": "text"}]}, {"id": "b", "k": "text"}]),
        ),
    );
    exchange(
        &probe,
        &frame("s1", 2, json!([["settle", "a"], ["settle", "b"]])),
    );
    let settled = |env: &TestEnv| surface(env, &probe, "s1")["settled"].clone();
    assert_eq!(settled(&env), json!(["a", "b"]));
    exchange(
        &probe,
        &frame("s1", 3, json!([["set", "a1", {"text": "changed"}]])),
    );
    assert_eq!(
        settled(&env),
        json!(["b"]),
        "an op inside clears the hint up the ancestors"
    );
}

#[test]
fn d12_focus_reveal_and_scroll_are_accepted() {
    let (env, probe) = setup("d12");
    exchange(
        &probe,
        &open_with(
            "s1",
            "screen",
            json!([{"id": "ed", "k": "editor"}, {"id": "l", "k": "list"}]),
        ),
    );
    let bodies = exchange(
        &probe,
        &frame(
            "s1",
            2,
            json!([
                ["focus", "ed"],
                ["reveal", "l", "start"],
                ["reveal", "l", "end"],
                ["reveal", "l", "sideways"],
                ["scroll", "l", "line-up"],
                ["scroll", "l", "line-down"],
                ["scroll", "l", "page-up"],
                ["scroll", "l", "page-down"],
                ["scroll", "l", "start"],
                ["scroll", "l", "end"],
            ]),
        ),
    );
    assert!(bodies.is_empty(), "{bodies:?}");
    assert_eq!(surface(&env, &probe, "s1")["focus"], "ed");
    exchange(&probe, &frame("s1", 3, json!([["focus", null]])));
    assert!(surface(&env, &probe, "s1")["focus"].is_null());
    // An inline surface's main ignores scroll.
    exchange(&probe, &open_with("i", "inline", json!([])));
    assert!(
        exchange(
            &probe,
            &frame("i", 2, json!([["scroll", "main", "page-down"]]))
        )
        .is_empty()
    );
}

#[test]
fn d13_the_sdks_reconcile_patterns_are_accepted() {
    let (env, probe) = setup("d13");
    exchange(&probe, &hello(&[]));
    // Adopting an unknown id, then `del main` alone, as Python does.
    let bodies = exchange(&probe, &tsp_json("o", &json!({"id": "s1", "adopt": true})));
    assert_eq!(bodies, [json!({"ev": "gone", "ids": ["s1"]})]);
    exchange(&probe, &open("s1", "inline"));
    let bodies = exchange(&probe, &frame("s1", 1, json!([["del", "main"]])));
    assert!(errors(&bodies).len() <= 1, "{bodies:?}");
    let bodies = exchange(
        &probe,
        &frame(
            "s1",
            2,
            json!([
                ["add", "main", "s1", null, {"id": "main", "k": "col", "c": [{"id": "main.0", "k": "text"}, {"id": "main.1", "k": "text"}, {"id": "main.k", "k": "text"}]}],
                ["add", "dock", "s1", null, {"id": "dock", "k": "col"}],
                ["move", "main.k", "main", "main.0"],
                ["set", "main.0", {"text": "a", "tone": null}],
            ]),
        ),
    );
    assert!(bodies.is_empty(), "{bodies:?}");
    let main = doc_node(&env, &probe, "s1", "main").unwrap();
    let order: Vec<&str> = main["c"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(order, ["main.k", "main.0", "main.1"]);
}

// ---------------------------------------------------------------------------------------------
// E. Acks and flow control

#[test]
fn e1_acks_carry_the_highest_s_drawn_after_drawing() {
    let (_env, _window, probe) = setup_window("e1");
    probe.write(&open_with("s1", "flow", text_doc()));
    let bodies = await_tsp(&probe, "the first ack", |b| !events(b, "ack").is_empty());
    // The first draw reports the size, then the ack.
    let names: Vec<&str> = bodies.iter().map(|b| b["ev"].as_str().unwrap()).collect();
    assert_eq!(names, ["resize", "ack"], "{bodies:?}");
    assert_eq!(bodies[1], json!({"ev": "ack", "sf": "s1", "s": 1}));
    let mut bytes = Vec::new();
    for s in 2..=4 {
        bytes.extend(frame(
            "s1",
            s,
            json!([["set", "t", {"text": format!("{s}")}]]),
        ));
    }
    probe.write(&bytes);
    let bodies = await_tsp(&probe, "the ack of s=4", |b| {
        events(b, "ack").iter().any(|a| a["s"] == 4)
    });
    let acks = events(&bodies, "ack");
    assert!(acks.len() <= 3, "at most one per displayed frame: {acks:?}");
    assert!(
        acks.windows(2)
            .all(|w| w[0]["s"].as_u64() < w[1]["s"].as_u64())
    );
}

#[test]
fn e2_frames_keep_flowing_while_a_window_draws() {
    let (_env, _window, probe) = setup_window("e2");
    probe.write(&open_with("s1", "flow", text_doc()));
    await_tsp(&probe, "the first ack", |b| !events(b, "ack").is_empty());
    // A program with 2 credits: each frame waits for the ack of the one two before.
    for s in 2..=30u64 {
        probe.write(&frame(
            "s1",
            s,
            json!([["set", "t", {"text": format!("{s}")}]]),
        ));
        if s % 2 == 0 {
            await_tsp(&probe, &format!("the ack of s={s}"), |b| {
                events(b, "ack").iter().any(|a| a["s"] == s)
            });
        }
    }
}

#[test]
fn e3_listen_false_surfaces_get_no_acks() {
    let (_env, _window, probe) = setup_window("e3");
    probe.write(&tsp_json(
        "o",
        &json!({"id": "q", "mode": "flow", "listen": false}),
    ));
    probe.write(&frame(
        "q",
        1,
        json!([["add", "main", "q", null, {"id": "main", "k": "col"}]]),
    ));
    probe.write(&frame(
        "q",
        2,
        json!([["add", "a", "main", null, {"id": "a", "k": "text"}]]),
    ));
    std::thread::sleep(Duration::from_millis(500));
    assert!(probe.fence_tsp().is_empty());
}

#[test]
fn e4_no_acks_while_no_window_shows_the_pane_and_an_attaching_window_acks() {
    let (env, probe) = setup("e4");
    probe.write(&open_with("s1", "flow", text_doc()));
    std::thread::sleep(Duration::from_millis(500));
    assert!(probe.fence_tsp().is_empty(), "nobody draws it");
    let _window = env.start_headless("late", &[]);
    let bodies = await_tsp(&probe, "the attaching window's ack", |b| {
        !events(b, "ack").is_empty()
    });
    assert!(
        events(&bodies, "ack").iter().any(|a| a["s"] == 1),
        "{bodies:?}"
    );
}

#[test]
fn e5_the_retention_budget_evicts_closed_surfaces_then_settled_children() {
    let (env, probe) = setup("e5");
    exchange(&probe, &hello(&[]));
    env.daemon("settings --retention 8000");
    wait_state(&env, &probe, "the budget", |st| st["retention"] == 8000);
    let filler = |id: &str| json!({"id": id, "k": "text", "p": {"text": "x".repeat(1500)}});
    exchange(&probe, &open_with("old", "inline", json!([filler("o1")])));
    exchange(&probe, &tsp_json("x", &json!({"id": "old"})));
    exchange(&probe, &open_with("s1", "flow", json!([])));
    let mut evicted: Vec<Value> = Vec::new();
    for i in 0..8 {
        let id = format!("c{i}");
        let bodies = exchange(
            &probe,
            &frame(
                "s1",
                i + 2,
                json!([["add", id, "main", null, filler(&id)], ["settle", id]]),
            ),
        );
        evicted.extend(events(&bodies, "gone").into_iter().cloned());
    }
    let st = state(&env, &probe);
    assert!(
        st["surfaces"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["id"] != "old"),
        "closed surfaces go first"
    );
    assert!(st["cost"].as_u64().unwrap() <= 8000, "{st}");
    assert!(!evicted.is_empty());
    assert!(evicted.iter().all(|g| g["sf"] == "s1"));
    assert_eq!(
        evicted[0]["ids"],
        json!(["c0"]),
        "oldest settled child first"
    );
    let bodies = exchange(&probe, &frame("s1", 99, json!([["set", "c0", {}]])));
    assert_eq!(errors(&bodies).len(), 1, "an evicted id is unknown");
}

// ---------------------------------------------------------------------------------------------
// F. Events and the PTY input

#[test]
fn f1_each_reply_and_event_is_one_st_terminated_apc_string_without_parameters() {
    let (_env, window, probe) = setup_window("f1");
    probe.write(&hello(&[]));
    probe.write(&open_with("s1", "flow", json!([{"id": "b", "k": "el", "p": {"tag": "button", "actions": {"click": "go\u{1b}\u{7}\u{9c}"}}}])));
    await_tsp(&probe, "the first ack", |b| !events(b, "ack").is_empty());
    click(&window, &probe, "s1", "b", "");
    probe.write(&tsp("f", &[], "{bad"));
    let input = probe.read_until("an action and an error", |b| {
        let bodies = tsp_bodies(&split_input(b));
        !events(&bodies, "action").is_empty() && !events(&bodies, "error").is_empty()
    });
    let items = split_input(&input);
    assert!(items.iter().all(Input::is_tsp), "{items:?}");
    let mut parser = tern_sdk::input::Parser::new();
    let parsed = parser.feed(&input);
    assert_eq!(parsed.len(), items.len());
    for item in &items {
        let Input::Tsp { verb, raw, .. } = item else {
            unreachable!()
        };
        assert!(verb == "r" || verb == "e");
        assert!(
            raw.starts_with(format!("\x1b_tsp;{verb};{{").as_bytes()),
            "no parameters"
        );
        assert!(raw.ends_with(b"\x1b\\"));
        let inner = &raw[2..raw.len() - 2];
        assert!(
            !inner.contains(&0x1b) && !inner.contains(&0x07),
            "controls are escaped"
        );
    }
}

#[test]
fn f3_events_the_terminal_originates_need_an_open_surface() {
    let (env, window, probe) = setup_window("f3");
    exchange(&probe, &hello(&["edit"]));
    facts(&window, [8, 16], true, false);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        probe.fence_tsp().is_empty(),
        "no surface open: no theme event"
    );
    probe.write(&open_with("s1", "flow", text_doc()));
    await_tsp(&probe, "the first ack", |b| !events(b, "ack").is_empty());
    facts(&window, [8, 16], false, false);
    let bodies = await_tsp(&probe, "a theme event", |b| !events(b, "theme").is_empty());
    assert_eq!(
        events(&bodies, "theme")[0],
        &json!({"ev": "theme", "dark": false})
    );
    exchange(&probe, &tsp_json("x", &json!({"id": "s1"})));
    facts(&window, [8, 16], true, false);
    std::thread::sleep(Duration::from_millis(300));
    assert!(probe.fence_tsp().is_empty(), "the surface closed: nothing");
    let _ = env;
}

#[test]
fn f4_answers_stop_once_the_program_is_gone_until_its_next_q_or_o() {
    let (_env, probe) = setup("f4");
    exchange(&probe, &open_with("i", "inline", text_doc()));
    exchange(&probe, b"\x1b]133;A\x1b\\");
    let mut late = frame("i", 2, json!([]));
    late.extend(tsp_json("x", &json!({"id": "i"})));
    late.extend(tsp("f", &[], "{bad"));
    assert!(exchange(&probe, &late).is_empty());
    assert_eq!(exchange(&probe, &hello(&[])).len(), 1);
    exchange(&probe, b"\x1b]133;A\x1b\\");
    exchange(&probe, &open("j", "flow"));
    assert_eq!(
        errors(&exchange(&probe, &frame("nope", 1, json!([])))).len(),
        1
    );
}

#[test]
fn f5_surface_events_carry_sf_except_theme_motion_and_adopt_gone() {
    let (_env, window, probe) = setup_window("f5");
    exchange(&probe, &hello(&[]));
    probe.write(&open_with("s1", "flow", text_doc()));
    let bodies = await_tsp(&probe, "the first ack", |b| !events(b, "ack").is_empty());
    assert!(bodies.iter().all(|b| b["sf"] == "s1"), "{bodies:?}");
    facts(&window, [8, 16], true, true);
    let bodies = await_tsp(&probe, "theme", |b| !events(b, "theme").is_empty());
    assert!(events(&bodies, "theme")[0].get("sf").is_none());
    let bodies = exchange(&probe, &frame("s1", 2, json!([["set", "nope", {}]])));
    assert_eq!(errors(&bodies)[0]["sf"], "s1");
    let bodies = exchange(&probe, &tsp_json("o", &json!({"id": "zz", "adopt": true})));
    let gone = events(&bodies, "gone");
    assert_eq!(gone.len(), 1, "{bodies:?}");
    assert!(gone[0].get("sf").is_none());
    let bodies = exchange(&probe, &tsp_json("t", &json!({"sf": "nope", "dark": {}})));
    assert_eq!(
        errors(&bodies)[0]["sf"],
        "nope",
        "an error names the surface the message named"
    );
}

#[test]
fn f6_resize_reports_columns_cell_size_and_visibility() {
    let (env, window, probe) = setup_window("f6");
    probe.write(&open_with("s1", "flow", text_doc()));
    let bodies = await_tsp(&probe, "the first resize", |b| {
        !events(b, "resize").is_empty()
    });
    assert_eq!(
        events(&bodies, "resize")[0],
        &json!({"ev": "resize", "sf": "s1", "cols": 80, "cell": {"w": 8, "h": 16}, "visible": true})
    );
    env.daemon(&format!("resize {} 100 30", probe.pane));
    let bodies = await_tsp(&probe, "a resize for 100 columns", |b| {
        events(b, "resize").iter().any(|r| r["cols"] == 100)
    });
    assert_eq!(events(&bodies, "resize").last().unwrap()["cols"], 100);
    facts(&window, [10, 21], false, false);
    let bodies = await_tsp(&probe, "a resize for the cell size", |b| {
        events(b, "resize").iter().any(|r| r["cell"]["w"] == 10)
    });
    assert_eq!(
        events(&bodies, "resize").last().unwrap()["cell"],
        json!({"w": 10, "h": 21})
    );
}

#[test]
fn f7_theme_motion_and_visible_events() {
    let (env, window, probe) = setup_window("f7");
    exchange(&probe, &hello(&[]));
    probe.write(&open_with("s1", "flow", text_doc()));
    await_tsp(&probe, "the first ack", |b| !events(b, "ack").is_empty());
    facts(&window, [8, 16], true, true);
    let bodies = await_tsp(&probe, "theme and motion", |b| {
        !events(b, "motion").is_empty()
    });
    assert!(events(&bodies, "theme").contains(&&json!({"ev": "theme", "dark": true})));
    assert!(events(&bodies, "motion").contains(&&json!({"ev": "motion", "reduce": true})));
    env.kill9(window.pid);
    let bodies = await_tsp(&probe, "visible:false", |b| {
        !events(b, "visible").is_empty()
    });
    assert_eq!(
        events(&bodies, "visible")[0],
        &json!({"ev": "visible", "sf": "s1", "visible": false})
    );
    let _again = env.start_headless("again", &[]);
    let bodies = await_tsp(&probe, "visible:true", |b| !events(b, "visible").is_empty());
    assert_eq!(
        events(&bodies, "visible")[0],
        &json!({"ev": "visible", "sf": "s1", "visible": true})
    );
}

#[test]
fn f8_pointer_actions_map_to_events() {
    let (_env, window, probe) = setup_window("f8");
    exchange(&probe, &hello(&[]));
    probe.write(&open_with("s1", "flow", json!([
        {"id": "card", "k": "card", "p": {"key": "k1", "actions": {"click": "toggle"}}},
        {"id": "cp", "k": "text", "p": {"text": "x", "actions": {"click": "copy", "dblclick": "open", "menu": ["zoom", "open-file=a=b"]}}},
        {"id": "l", "k": "list", "c": [{"id": "l.a", "k": "item", "p": {"actions": {"click": "select", "dblclick": "activate"}}}]},
        {"id": "b", "k": "el", "p": {"tag": "button", "actions": {"click": "rerun=fast=1"}}},
        {"id": "f", "k": "el", "p": {"tag": "form"}, "c": [
            {"id": "f.c", "k": "el", "p": {"tag": "input", "type": "checkbox", "name": "ok", "checked": true}},
            {"id": "f.go", "k": "el", "p": {"tag": "button", "actions": {"click": "submit"}}},
        ]},
    ])));
    await_tsp(&probe, "the first ack", |b| !events(b, "ack").is_empty());
    let sent = [
        click(&window, &probe, "s1", "card", ""),
        click(&window, &probe, "s1", "cp", ""),
        click(&window, &probe, "s1", "cp", "--double"),
        click(&window, &probe, "s1", "cp", "--menu zoom"),
        click(&window, &probe, "s1", "l.a", ""),
        click(&window, &probe, "s1", "l.a", "--double"),
        click(&window, &probe, "s1", "b", "--mods meta,shift"),
        click(
            &window,
            &probe,
            "s1",
            "cp",
            "--menu open-file=a=b --mods ctrl",
        ),
        click(&window, &probe, "s1", "f.go", ""),
    ];
    assert_eq!(
        sent[1..4],
        [
            json!({"event": null}),
            json!({"event": null}),
            json!({"event": null})
        ],
        "copy, open and zoom are local"
    );
    let bodies = await_tsp(&probe, "six events", |b| {
        b.iter()
            .filter(|e| e["ev"] != "ack" && e["ev"] != "resize")
            .count()
            >= 6
    });
    let got: Vec<&Value> = bodies
        .iter()
        .filter(|e| e["ev"] != "ack" && e["ev"] != "resize")
        .collect();
    assert_eq!(
        got[0],
        &json!({"ev": "toggle", "sf": "s1", "id": "card", "collapsed": true, "key": "k1"})
    );
    assert_eq!(
        got[1],
        &json!({"ev": "select", "sf": "s1", "id": "l", "item": "l.a"})
    );
    assert_eq!(
        got[2],
        &json!({"ev": "activate", "sf": "s1", "id": "l", "item": "l.a"})
    );
    assert_eq!(
        got[3],
        &json!({"ev": "action", "sf": "s1", "id": "b", "act": "rerun", "value": "fast=1", "mods": ["shift", "meta"]})
    );
    assert_eq!(
        got[4],
        &json!({"ev": "action", "sf": "s1", "id": "cp", "act": "open-file", "value": "a=b"}),
        "a menu pick has no mods"
    );
    assert_eq!(
        got[5],
        &json!({"ev": "action", "sf": "s1", "id": "f.go", "act": "submit", "values": {"ok": true}})
    );
}

#[test]
fn f9_form_values_follow_the_rules() {
    let (_env, window, probe) = setup_window("f9");
    let input = |id: &str, ty: &str, name: &str, value: &str, extra: Value| {
        let mut p = json!({"tag": "input", "type": ty, "name": name, "value": value});
        for (k, v) in extra.as_object().unwrap() {
            p[k] = v.clone();
        }
        json!({"id": id, "k": "el", "p": p})
    };
    probe.write(&open_with("s1", "flow", json!([{"id": "form", "k": "el", "p": {"tag": "form"}, "c": [
        input("r1", "radio", "size", "s", json!({})),
        input("r2", "radio", "size", "l", json!({})),
        input("solo", "checkbox", "agree", "yes", json!({})),
        input("t1", "checkbox", "tags", "a", json!({"checked": true})),
        input("t2", "checkbox", "tags", "b", json!({})),
        input("t3", "checkbox", "tags", "c", json!({"checked": true})),
        input("off", "checkbox", "disabled", "x", json!({"disabled": true, "checked": true})),
        input("anon", "checkbox", "", "x", json!({"checked": true})),
        {"id": "inner", "k": "el", "p": {"tag": "form"}, "c": [input("n", "checkbox", "nested", "x", json!({"checked": true}))]},
        {"id": "go", "k": "el", "p": {"tag": "button", "actions": {"click": "submit"}}},
    ]}])));
    await_tsp(&probe, "the first ack", |b| !events(b, "ack").is_empty());
    let first = click(&window, &probe, "s1", "go", "");
    assert_eq!(
        first["values"],
        json!({"size": null, "agree": false, "tags": ["a", "c"]})
    );
    click(&window, &probe, "s1", "r2", "");
    click(&window, &probe, "s1", "t1", "");
    let last = click(&window, &probe, "s1", "go", "");
    assert_eq!(
        last["values"],
        json!({"size": "l", "agree": false, "tags": ["c"]})
    );
    let bodies = await_tsp(&probe, "four events", |b| {
        b.iter()
            .filter(|e| e["ev"] == "action" || e["ev"] == "change")
            .count()
            >= 4
    });
    let changes = events(&bodies, "change");
    assert_eq!(
        changes[0],
        &json!({"ev": "change", "sf": "s1", "id": "r2", "value": "l", "checked": true, "name": "size", "values": {"size": "l", "agree": false, "tags": ["a", "c"]}})
    );
    assert_eq!(
        events(&bodies, "action").last().unwrap()["values"],
        json!({"size": "l", "agree": false, "tags": ["c"]})
    );
}

#[test]
fn f10_keys_reach_the_program_as_pty_bytes_never_as_events() {
    let (_env, window, probe) = setup_window("f10");
    exchange(&probe, &hello(&["edit", "undo", "send"]));
    probe.write(&open_with(
        "s1",
        "inline",
        json!([{"id": "ed", "k": "editor"}]),
    ));
    probe.write(&frame("s1", 2, json!([["focus", "ed"]])));
    await_tsp(&probe, "the first ack", |b| !events(b, "ack").is_empty());
    window.request(&format!("send {} text héllo", probe.pane));
    window.request(&format!("send {} keys Enter C-z", probe.pane));
    let input = probe.read_until("the typed bytes", |b| b.ends_with(&[0x1a]));
    let items = split_input(&input);
    let other: Vec<u8> = items
        .iter()
        .filter_map(|i| match i {
            Input::Other(b) => Some(b.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(
        other.ends_with("héllo\r\x1a".as_bytes()),
        "{:?}",
        String::from_utf8_lossy(&other)
    );
    assert!(
        tsp_bodies(&items)
            .iter()
            .all(|b| b["ev"] == "ack" || b["ev"] == "resize")
    );
}

#[test]
fn f11_edit_events_carry_utf16_offsets_and_the_length_seen() {
    let (_env, window, probe) = setup_window("f11");
    exchange(&probe, &hello(&["edit"]));
    probe.write(&open_with(
        "s1",
        "inline",
        json!([{"id": "ed", "k": "editor", "p": {"text": "a😀b"}}]),
    ));
    await_tsp(&probe, "the first ack", |b| !events(b, "ack").is_empty());
    let edit =
        json!({"ev": "edit", "id": "ed", "from": 1, "to": 3, "text": "x", "cursor": 2, "len": 4});
    window_event(&window, &probe, "s1", edit);
    let bodies = await_tsp(&probe, "the edit", |b| !events(b, "edit").is_empty());
    assert_eq!(
        events(&bodies, "edit")[0],
        &json!({"ev": "edit", "sf": "s1", "id": "ed", "from": 1, "to": 3, "text": "x", "cursor": 2, "len": 4})
    );
    // A Vim-mode or readonly field takes keys, not edits.
    exchange(
        &probe,
        &frame("s1", 2, json!([["set", "ed", {"readonly": true}]])),
    );
    window_event(
        &window,
        &probe,
        "s1",
        json!({"ev": "edit", "id": "ed", "from": 0, "to": 0, "text": "y", "cursor": 1, "len": 3}),
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(events(&probe.fence_tsp(), "edit").is_empty());
}

#[test]
fn f12_send_waits_for_a_sendable_field() {
    let (_env, window, probe) = setup_window("f12");
    exchange(&probe, &hello(&["send"]));
    probe.write(&open_with("s1", "inline", json!([{"id": "ed", "k": "editor"}, {"id": "ro", "k": "input", "p": {"readonly": true, "sendable": true}}])));
    await_tsp(&probe, "the first ack", |b| !events(b, "ack").is_empty());
    window_event(
        &window,
        &probe,
        "s1",
        json!({"ev": "send", "id": "ed", "text": "held"}),
    );
    window_event(
        &window,
        &probe,
        "s1",
        json!({"ev": "send", "id": "ro", "text": "dropped"}),
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        events(&probe.fence_tsp(), "send").is_empty(),
        "not sendable yet"
    );
    probe.write(&frame("s1", 2, json!([["set", "ed", {"sendable": true}]])));
    let bodies = await_tsp(&probe, "the held send", |b| !events(b, "send").is_empty());
    assert_eq!(
        events(&bodies, "send"),
        [&json!({"ev": "send", "sf": "s1", "id": "ed", "text": "held"})]
    );
}

#[test]
fn f13_a_click_on_a_field_asks_for_the_keys() {
    let (_env, window, probe) = setup_window("f13");
    probe.write(&open_with(
        "s1",
        "inline",
        json!([{"id": "ed", "k": "editor"}]),
    ));
    await_tsp(&probe, "the first ack", |b| !events(b, "ack").is_empty());
    assert_eq!(click(&window, &probe, "s1", "ed", "")["ev"], "focus");
    let bodies = await_tsp(&probe, "focus", |b| !events(b, "focus").is_empty());
    assert_eq!(
        events(&bodies, "focus")[0],
        &json!({"ev": "focus", "sf": "s1", "id": "ed"})
    );
}

#[test]
fn f14_nothing_is_written_when_nothing_listens() {
    let (env, window, probe) = setup_window("f14");
    exchange(&probe, &hello(&[]));
    facts(&window, [9, 18], true, true);
    env.daemon(&format!("resize {} 90 20", probe.pane));
    window.request(&format!("send {} text z", probe.pane));
    let input = probe.read_until("the typed z", |b| b.ends_with(b"z"));
    assert!(split_input(&input).iter().all(|i| !i.is_tsp()), "{input:?}");
    assert!(probe.fence_tsp().is_empty());
    let gate = &state(&env, &probe)["gate_stats"];
    assert_eq!(
        gate["dropped"], 0,
        "the engine produced nothing to drop either"
    );
}

// ---------------------------------------------------------------------------------------------
// G. Palette, stylesheets and blobs

#[test]
fn g1_the_palette_replaces_whole_without_a_frame_and_stays_after_close() {
    let (env, probe) = setup("g1");
    let bodies = exchange(&probe, &tsp_json("t", &json!({"dark": {"cardBg": "#000"}})));
    assert_eq!(errors(&bodies).len(), 1, "no live surface: {bodies:?}");
    exchange(&probe, &open("s1", "flow"));
    exchange(
        &probe,
        &tsp_json(
            "t",
            &json!({"dark": {"cardBg": "#123456", "accent": "#abc"}, "light": {"cardBg": "#fff"}}),
        ),
    );
    exchange(
        &probe,
        &tsp_json(
            "t",
            &json!({"sf": "s1", "dark": {"infoBg": "#00ff0080", "bad": "blue"}}),
        ),
    );
    let s = surface(&env, &probe, "s1");
    assert_eq!(
        s["palette"],
        json!({"sf": "s1", "dark": {"infoBg": "#00ff00"}, "light": {}})
    );
    assert!(s["last_s"].is_null(), "applied without a frame");
    let bodies = exchange(&probe, &tsp_json("t", &json!({"sf": "nope", "dark": {}})));
    assert_eq!(errors(&bodies).len(), 1);
    exchange(&probe, &tsp_json("x", &json!({"id": "s1"})));
    assert_eq!(
        surface(&env, &probe, "s1")["palette"]["dark"]["infoBg"],
        "#00ff00"
    );
}

#[test]
fn g2_stylesheets_are_named_limited_ordered_and_checked() {
    let (env, probe) = setup("g2");
    exchange(&probe, &open("s1", "flow"));
    for bad in [
        json!({"name": "", "css": "a{}"}),
        json!({"name": "a.b", "css": "a{}"}),
        json!({"name": "x".repeat(65), "css": "a{}"}),
    ] {
        assert_eq!(
            errors(&exchange(&probe, &tsp_json("s", &bad))).len(),
            1,
            "{bad}"
        );
    }
    exchange(
        &probe,
        &tsp_json("s", &json!({"name": "one", "css": ".a{color:red}"})),
    );
    exchange(
        &probe,
        &tsp_json("s", &json!({"name": "two", "css": ".b{color:blue}"})),
    );
    exchange(
        &probe,
        &tsp_json("s", &json!({"name": "one", "css": ".a{color:green}"})),
    );
    assert_eq!(
        surface(&env, &probe, "s1")["sheets"],
        json!([["one", ".a{color:green}"], ["two", ".b{color:blue}"]])
    );
    exchange(&probe, &tsp_json("s", &json!({"name": "two", "css": null})));
    assert_eq!(
        surface(&env, &probe, "s1")["sheets"],
        json!([["one", ".a{color:green}"]])
    );
    let big = format!(".a{{content:\"{}\"}}", "x".repeat(256 * 1024));
    let body = json!({"name": "big", "css": big}).to_string();
    let bytes = tern_sdk::wire::encode("s", &[], body.as_bytes(), 65_536, "css");
    assert_eq!(errors(&exchange(&probe, &bytes)).len(), 1);
    let bodies = exchange(
        &probe,
        &tsp_json(
            "s",
            &json!({"name": "bad", "css": ".a{background:url(x.png)}\n.b{position:fixed}\n@import 'c.css';\n@font-face{font-family:x}"}),
        ),
    );
    let errs = errors(&bodies);
    assert_eq!(errs.len(), 1, "one error per change: {bodies:?}");
    assert_eq!(
        (errs[0]["sf"].clone(), errs[0]["sheet"].clone()),
        (json!("s1"), json!("bad"))
    );
    let msg = errs[0]["msg"].as_str().unwrap();
    let lines: Vec<&str> = msg
        .split("; ")
        .map(|m| m.split(':').next().unwrap())
        .collect();
    assert_eq!(
        lines,
        ["1", "2", "3"],
        "@font-face is ignored silently: {msg}"
    );
}

/// An SVG of `w`×`h` px: the smallest picture a blob can carry as text.
fn svg_blob(w: u32, h: u32, fill: &str) -> String {
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\">\
         <rect width=\"{w}\" height=\"{h}\" fill=\"{fill}\"/></svg>"
    )
}

/// The `.sf-image` element of node `id` in a window's drawn pane, with everything under it:
/// `(class, [(class, box)])`, once it is drawn.
fn window_image(
    window: &Headless,
    probe: &Probe,
    id: &str,
) -> Option<(String, Vec<(String, Value)>)> {
    let tree = window.request(&format!("tree {} .sf-image", probe.pane));
    let els = tree.as_array()?;
    let at = els.iter().position(|e| e["id"] == id)?;
    let class = els[at]["class"].as_str()?.to_owned();
    let inner = els[at + 1..]
        .iter()
        .take_while(|e| e["id"].is_null())
        .map(|e| {
            (
                e["class"].as_str().unwrap_or("").to_owned(),
                e["box"].clone(),
            )
        })
        .collect();
    Some((class, inner))
}

/// Waits until a window draws node `id` as a picture (no placeholder) of `w`×`h` CSS px.
fn await_drawn(window: &Headless, probe: &Probe, id: &str, w: f64, h: f64, who: &str) {
    wait_for(&format!("{who} to draw {id}"), || {
        let (class, inner) = window_image(window, probe, id)?;
        let pic = inner.iter().find(|(c, _)| c.contains("sf-img-svg"))?;
        (!class.contains("missing") && pic.1[2].as_f64() == Some(w) && pic.1[3].as_f64() == Some(h))
            .then_some(())
    });
}

#[test]
fn g3_blobs_are_content_addressed_and_shared_by_the_panes_surfaces() {
    let (env, window, probe) = setup_window("g3");
    exchange(&probe, &hello(&[]));
    let b64 = base64::engine::general_purpose::STANDARD;
    exchange(&probe, &open("one", "flow"));
    exchange(&probe, &tsp("b", &[], &b64.encode(b"shared bytes")));
    exchange(&probe, &tsp_json("x", &json!({"id": "one"})));
    exchange(&probe, &open("two", "flow"));
    let id = sha256(b"shared bytes");
    let have = exchange(
        &probe,
        &tsp_json("q", &json!({"q": "blobs", "ids": [id.to_uppercase(), id]})),
    );
    assert_eq!(
        have[0]["have"],
        json!([id]),
        "lowercase hex SHA-256 ids: {have:?}"
    );
    assert!(env.state.join("blobs").join(&id).is_file());

    // An image whose blob arrives after the frame that uses it (omp never waits): the window
    // shows a placeholder, then draws the picture when the blob comes, with no change to the node.
    let late = svg_blob(24, 12, "#22aa55");
    let late_id = sha256(late.as_bytes());
    let bodies = exchange(
        &probe,
        &frame(
            "two",
            1,
            json!([["add", "main", "two", null, {"id": "main", "k": "col", "c": [
                {"id": "pic", "k": "image", "p": {"blob": late_id, "alt": "Late"}}
            ]}]]),
        ),
    );
    assert!(errors(&bodies).is_empty(), "{bodies:?}");
    let (class, inner) = window_image(&window, &probe, "pic").expect("the image node is drawn");
    assert!(class.contains("missing"), "a placeholder first: {class}");
    assert!(
        inner.iter().any(|(c, _)| c.contains("sf-img-card")),
        "{inner:?}"
    );
    exchange(
        &probe,
        &tsp("b", &[("mime", "image/svg+xml")], &b64.encode(&late)),
    );
    await_drawn(&window, &probe, "pic", 24.0, 12.0, "the window");

    // A blob sent before any node names it, then a window that attaches later: the snapshot
    // carries no blobs, so the daemon sends the ones its documents name, and the window asks for
    // others when a later frame names them.
    let early = svg_blob(10, 30, "#3355ff");
    let early_id = sha256(early.as_bytes());
    exchange(
        &probe,
        &tsp("b", &[("mime", "image/svg+xml")], &b64.encode(&early)),
    );
    let second = env.start_headless("w2", &[]);
    await_drawn(&second, &probe, "pic", 24.0, 12.0, "the late window");
    let bodies = exchange(
        &probe,
        &frame(
            "two",
            2,
            json!([["add", "pic2", "main", null,
                {"id": "pic2", "k": "image", "p": {"blob": early_id}}]]),
        ),
    );
    assert!(errors(&bodies).is_empty(), "{bodies:?}");
    await_drawn(&second, &probe, "pic2", 10.0, 30.0, "the late window");
    await_drawn(&window, &probe, "pic2", 10.0, 30.0, "the window");
    assert!(second.errors().is_empty(), "{:?}", second.errors());
    assert!(window.errors().is_empty(), "{:?}", window.errors());
}

// ---------------------------------------------------------------------------------------------
// H. Persistence

/// The TSP messages of a snapshot, in order, as (verb, params, body).
fn snapshot_messages(env: &TestEnv, pane: u64) -> Vec<(String, String, Value)> {
    let snap = env.daemon(&format!("snapshot {pane}"));
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(snap["bytes"].as_str().unwrap())
        .unwrap();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(start) = bytes[at..].windows(6).position(|w| w == b"\x1b_tsp;") {
        let from = at + start + 6;
        let end = from
            + bytes[from..]
                .windows(2)
                .position(|w| w == b"\x1b\\")
                .unwrap();
        let payload = String::from_utf8_lossy(&bytes[from..end]).into_owned();
        let (verb, rest) = payload.split_once(';').unwrap();
        let body_at = rest.find(['{', '[']).unwrap_or(rest.len());
        let (params, body) = rest.split_at(body_at);
        out.push((
            verb.to_owned(),
            params.to_owned(),
            serde_json::from_str(body).unwrap_or(Value::Null),
        ));
        at = end + 2;
    }
    out
}

#[test]
fn h1_snapshots_replay_in_order_without_answers_and_ages_rewritten() {
    let (env, window, probe) = setup_window("h1");
    exchange(
        &probe,
        &tsp_json(
            "q",
            &json!({"q": "hello", "v": [1], "app": "h", "features": ["edit"]}),
        ),
    );
    exchange(&probe, &open_with("old", "inline", text_doc()));
    exchange(&probe, &tsp_json("x", &json!({"id": "old"})));
    exchange(
        &probe,
        &tsp_json(
            "o",
            &json!({"id": "live", "mode": "inline", "title": "T", "role": "r"}),
        ),
    );
    exchange(
        &probe,
        &tsp_json("t", &json!({"dark": {"cardBg": "#111111"}})),
    );
    exchange(
        &probe,
        &tsp_json("s", &json!({"name": "a", "css": ".a{color:red}"})),
    );
    exchange(
        &probe,
        &frame(
            "live",
            41,
            json!([
                ["add", "main", "live", null, {"id": "main", "k": "col", "c": [{"id": "el", "k": "elapsed", "p": {"age": 1000}}, {"id": "done", "k": "text"}, {"id": "ed", "k": "editor"}]}],
                ["settle", "done"], ["focus", "ed"],
            ]),
        ),
    );
    probe.write(&frame("live", 42, json!([["suspend"]])));
    await_tsp(&probe, "acks", |b| {
        events(b, "ack").iter().any(|a| a["s"] == 42)
    });

    let msgs = snapshot_messages(&env, probe.pane);
    let summary: Vec<String> = msgs
        .iter()
        .map(|(v, _, b)| match v.as_str() {
            "o" | "x" => format!("{v}:{}", b["id"].as_str().unwrap()),
            "f" | "t" | "s" => format!("{v}:{}", b["sf"].as_str().unwrap()),
            other => other.to_owned(),
        })
        .collect();
    assert_eq!(
        summary,
        [
            "q", "o:old", "f:old", "x:old", "o:live", "t:live", "s:live", "f:live"
        ]
    );
    assert_eq!(msgs[0].2["app"], "h");
    assert_eq!(msgs[0].2["features"], json!(["edit"]));
    let live_o = &msgs[4];
    assert!(
        live_o.1.contains("seeded=1") && live_o.1.contains("buf=main") && live_o.1.contains("key=")
    );
    assert_eq!(
        (live_o.2["title"].clone(), live_o.2["role"].clone()),
        (json!("T"), json!("r"))
    );
    let f = &msgs[7].2;
    assert_eq!(f["s"], 42);
    let ops = f["ops"].as_array().unwrap();
    let tail: Vec<Value> = ops[ops.len() - 3..].to_vec();
    assert_eq!(
        tail,
        [
            json!(["settle", "done"]),
            json!(["focus", "ed"]),
            json!(["suspend"])
        ]
    );

    // A window attaching now replays it: same state, no answer reaches the program, and the
    // elapsed age is rewritten to the snapshot time.
    std::thread::sleep(Duration::from_millis(1200));
    let late = env.start_headless("late", &[]);
    let daemon = env.daemon_hash(probe.pane);
    wait_for("the late window to match the daemon", || {
        (late.hash(probe.pane) == Some(daemon.clone())).then_some(())
    });
    let st = window_state(&late, &probe);
    let el = node(&st["surfaces"][1]["doc"], "el").unwrap();
    assert!(el["p"]["age"].as_i64().unwrap() >= 2100, "{el}");
    assert!(probe.fence_tsp().is_empty(), "the replay answered nothing");
    let _ = window;
}

#[test]
fn h2_after_the_program_is_gone_surfaces_come_back_closed_and_read_only() {
    let (env, probe) = setup("h2");
    exchange(&probe, &open_with("f", "flow", text_doc()));
    exchange(
        &probe,
        &open_with(
            "i",
            "inline",
            json!([{"id": "b", "k": "el", "p": {"tag": "button", "actions": {"click": "go"}}}]),
        ),
    );
    exchange(&probe, &open_with("sc", "screen", text_doc()));
    probe.exit(0);
    env.wait(probe.pane, "exit");
    // Restore rebuilds a pane from its snapshot (§16): replay it into a fresh replica.
    let snap = env.daemon(&format!("snapshot {}", probe.pane));
    let mut snapshot: auk_replica::Snapshot = serde_json::from_value(snap.clone()).unwrap();
    snapshot.bytes = base64::engine::general_purpose::STANDARD
        .decode(snap["bytes"].as_str().unwrap())
        .unwrap();
    let restored =
        auk_replica::Replica::from_snapshot(&snapshot, auk_replica::Role::Authoritative, None);
    let states: Vec<(&str, bool, bool)> = restored
        .tsp()
        .surfaces()
        .iter()
        .map(|s| (s.id(), s.is_live(), s.interactive()))
        .collect();
    assert_eq!(
        states,
        [("f", false, false), ("i", false, false)],
        "screen surfaces don't come back"
    );
    assert_eq!(restored.state_hash(), {
        let (hash, _) = env.daemon_hash(probe.pane);
        let bytes: Vec<u8> = (0..32)
            .map(|i| u8::from_str_radix(&hash[2 * i..2 * i + 2], 16).unwrap())
            .collect();
        <[u8; 32]>::try_from(bytes).unwrap()
    });
}

// ---------------------------------------------------------------------------------------------
// I. VT prerequisites

#[test]
fn i1_bracketed_paste() {
    let (_env, window, probe) = setup_window("i1");
    probe.write(b"\x1b[?2004h");
    window.request(&format!("send {} paste pasted text", probe.pane));
    let input = probe.read_until("the paste", |b| b.ends_with(b"\x1b[201~"));
    assert_eq!(input, b"\x1b[200~pasted text\x1b[201~");
    probe.write(b"\x1b[?2004l");
    window.request(&format!("send {} paste plain", probe.pane));
    assert_eq!(
        probe.read_until("plain", |b| b.ends_with(b"plain")),
        b"plain"
    );
}

#[test]
fn i2_kitty_keyboard_flag_1_push_and_pop() {
    let (_env, window, probe) = setup_window("i2");
    probe.write(b"\x1b[>1u");
    probe.fence();
    window.request(&format!("send {} keys Esc", probe.pane));
    assert_eq!(
        probe.read_until("CSI 27 u", |b| b.ends_with(b"u")),
        b"\x1b[27u"
    );
    window.request(&format!("send {} keys a", probe.pane));
    assert_eq!(
        probe.read_until("a", |b| b.ends_with(b"a")),
        b"a",
        "no release events or extras"
    );
    probe.write(b"\x1b[<u");
    probe.fence();
    window.request(&format!("send {} keys Esc", probe.pane));
    assert_eq!(
        probe.read_until("a plain Escape", |b| !b.is_empty()),
        b"\x1b"
    );
}

#[test]
fn i3_da1_is_answered() {
    let (_env, probe) = setup("i3");
    probe.write(b"\x1b[c");
    let input = probe.read_until("DA1", |b| b.ends_with(b"c"));
    assert!(
        matches!(&split_input(&input)[..], [Input::Da1(_)]),
        "{input:?}"
    );
}

#[test]
fn i4_osc_133_a_marks_a_prompt() {
    let (env, probe) = setup("i4");
    exchange(&probe, &open_with("i", "inline", text_doc()));
    exchange(&probe, b"\x1b]133;A\x07");
    assert!(surfaces(&env, &probe).is_empty());
    assert_eq!(state(&env, &probe)["gate"], "gone");
}

// ---------------------------------------------------------------------------------------------
// Lifetimes (AC6), silence (AC7), robustness (AC15)

#[test]
fn ac06_a_kept_flow_surface_stays_in_scrollback_inert_past_the_next_prompt() {
    let env = env("ac06");
    let window = env.start_headless("w", &[]);
    let shell = env.new_pane(&[]);
    env.wait(shell, "prompt");
    let probe = Probe::run_in_shell(&env, shell);
    exchange(&probe, &hello(&[]));
    exchange(
        &probe,
        &open_with(
            "f",
            "flow",
            json!([{"id": "b", "k": "el", "p": {"tag": "button", "actions": {"click": "go"}}}]),
        ),
    );
    exchange(&probe, &tsp_json("x", &json!({"id": "f", "keep": true})));
    probe.tty("cooked");
    probe.exit(0);
    env.wait(shell, "prompt");
    env.run(shell, "echo next-command");
    let st = env.daemon(&format!("tsp {shell}"));
    let f = st["surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "f")
        .cloned()
        .unwrap_or_else(|| panic!("{st}"));
    assert_eq!(
        (f["state"].clone(), f["interactive"].clone()),
        (json!("closed"), json!(false))
    );
    assert!(
        f["anchor"]["line"].is_u64(),
        "still anchored in the buffer: {f}"
    );
    assert!(node(&f["doc"], "b").is_some());
    let reply = auk_conformance::harness::Control::connect(&window.control)
        .send(&format!("click {shell} f b"));
    assert_eq!(reply["ok"], false, "inert: {reply}");
}

#[test]
fn ac07_nothing_reaches_a_pty_whose_program_isnt_listening() {
    // After a prompt, in a shell.
    let env = env("ac07");
    let window = env.start_headless("w", &[]);
    let shell = env.new_pane(&[]);
    env.wait(shell, "prompt");
    let probe = Probe::run_in_shell(&env, shell);
    exchange(&probe, &hello(&[]));
    probe.write(&open_with("f", "flow", text_doc()));
    probe.write(&open_with("i", "inline", text_doc()));
    await_tsp(&probe, "acks", |b| events(b, "ack").len() >= 2);
    probe.tty("cooked");
    probe.exit(0);
    env.wait(shell, "prompt");
    let before = env.daemon(&format!("tsp {shell}"))["gate_stats"].clone();
    facts(&window, [11, 22], true, true);
    env.daemon(&format!("resize {shell} 70 20"));
    std::thread::sleep(Duration::from_millis(500));
    let after = env.daemon(&format!("tsp {shell}"))["gate_stats"].clone();
    assert_eq!(
        before["written"], after["written"],
        "after a prompt: {before} {after}"
    );
    let line = env.daemon(&format!("capture {shell}"))["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        !line.contains("tsp"),
        "nothing typed into the shell: {line}"
    );

    // After exit.
    let p2 = Probe::start(&env, &[]);
    exchange(&p2, &open_with("f", "flow", text_doc()));
    p2.exit(0);
    env.wait(p2.pane, "exit");
    assert!(
        env.daemon(&format!("tsp {}", p2.pane))["gate_stats"].is_null(),
        "no PTY to write to"
    );

    // After RIS.
    let p3 = Probe::start(&env, &[]);
    p3.write(&open_with("f", "flow", text_doc()));
    // The window draws and acks it while the program listens.
    await_tsp(&p3, "the ack", |b| !events(b, "ack").is_empty());
    let mut bytes = b"\x1bc".to_vec();
    bytes.extend(tsp("f", &[], "{bad"));
    bytes.extend(frame("f", 2, json!([])));
    assert!(exchange(&p3, &bytes).is_empty(), "after RIS");

    // Under listen:false.
    let p4 = Probe::start(&env, &[]);
    exchange(
        &p4,
        &tsp_json("o", &json!({"id": "q", "mode": "flow", "listen": false})),
    );
    p4.write(&frame(
        "q",
        1,
        json!([["add", "main", "q", null, {"id": "main", "k": "col"}]]),
    ));
    p4.write(&tsp("f", &[], "{bad"));
    std::thread::sleep(Duration::from_millis(300));
    assert!(p4.fence_tsp().is_empty(), "under listen:false");

    // While the tty is canonical: the engine answers, the gate drops it.
    let p5 = Probe::start_cooked(&env, &[]);
    let mut bytes = hello(&[]);
    bytes.extend(open("s", "flow"));
    bytes.extend(tsp("f", &[], "{bad"));
    p5.write(&bytes);
    wait_for("the gate to drop the answers", || {
        let st = env.daemon(&format!("tsp {}", p5.pane));
        (st["gate_stats"]["dropped"].as_u64()? >= 2).then_some(())
    });
    p5.tty("raw");
    let items = p5.fence();
    assert!(
        items.iter().all(|i| !i.is_tsp()),
        "while canonical: {items:?}"
    );
    assert_eq!(
        env.daemon(&format!("tsp {}", p5.pane))["gate_stats"]["written"],
        0
    );
}

/// xorshift64*: deterministic, no dependencies.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn ac15_malformed_frames_and_unknown_verbs_never_kill_the_pane_under_fuzzing() {
    let (env, window, probe) = setup_window("ac15");
    let records =
        auk_conformance::load(&auk_conformance::fixtures_dir().join("omp-session.jsonl")).unwrap();
    let messages: Vec<Vec<u8>> = records
        .iter()
        .filter(|r| r.dir == auk_conformance::Dir::Out && r.verb.len() == 1)
        .map(|r| {
            let body = match &r.body {
                Value::String(s) => s.clone(),
                v => v.to_string(),
            };
            tsp(&r.verb, &[], &body)
        })
        .collect();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut batch = Vec::new();
    let mut sent = 0;
    for msg in messages.iter().cycle().take(3000) {
        let mut m = msg.clone();
        match rng.below(8) {
            0 => {}
            1 => {
                let at = 6 + rng.below(m.len() - 8);
                m[at] = rng.next() as u8;
            }
            2 => {
                let cut = 7 + rng.below(m.len() - 8);
                m.truncate(cut);
                m.extend_from_slice(b"\x1b\\");
            }
            3 => {
                let at = 6 + rng.below(m.len() - 8);
                let junk: &[u8] = [
                    &b";c=1;m=1;"[..],
                    b"\xc2\x9c",
                    b"\x00",
                    b"]]",
                    b"{",
                    b"\x1b[2J",
                ][rng.below(6)];
                m.splice(at..at, junk.iter().copied());
            }
            4 => {
                m = tsp(
                    ["z", "Q", "qq", "\u{1}", "r", "e"][rng.below(6)],
                    &[],
                    "{\"q\":\"hello\"}",
                )
            }
            5 => {
                m = tsp(
                    "f",
                    &[],
                    &format!(
                        "{{\"sf\":\"{}\",\"s\":{},\"ops\":[[\"{}\",{}]]}}",
                        ["s1", "main", "x"][rng.below(3)],
                        rng.next() % 100,
                        ["add", "set", "del", "move", "text", "splice", "bogus"][rng.below(7)],
                        ["null", "1", "\"a\"", "[]", "{}"][rng.below(5)]
                    ),
                )
            }
            6 => m = tsp("f", &[("c", "fz"), ("m", "1")], "{\"sf\":"),
            _ => m.extend_from_slice(b"\x1b_tsp;"),
        }
        batch.extend(m);
        sent += 1;
        if batch.len() > 32 * 1024 {
            probe.write(&std::mem::take(&mut batch));
        }
    }
    probe.write(&batch);
    probe.write(b"\x1b\\\x1bc");
    assert_eq!(sent, 3000);
    // The pane is alive, its program still talks TSP, and the window agrees with the daemon.
    let replies = exchange(&probe, &hello(&[]));
    assert_eq!(
        replies.iter().filter(|b| b["r"] == "hello").count(),
        1,
        "{replies:?}"
    );
    assert!(env.pane_info(probe.pane)["exit"].is_null());
    let daemon = env.daemon_hash(probe.pane);
    wait_for("the window to agree with the daemon", || {
        (window.hash(probe.pane) == Some(daemon.clone())).then_some(())
    });
    assert!(window.errors().is_empty());
}

// ---------------------------------------------------------------------------------------------
// surface-play (§7.15)

/// The documents a recording's `out` messages build in a fresh replica.
fn replay_fixture(path: &std::path::Path, until: Option<usize>) -> Vec<Value> {
    let records = auk_conformance::load(path).unwrap();
    let mut replica = auk_replica::Replica::new(
        80,
        24,
        auk_replica::Settings::default(),
        auk_replica::Role::Follower,
    );
    let mut frames = 0;
    for r in records
        .iter()
        .filter(|r| r.dir == auk_conformance::Dir::Out)
    {
        if !matches!(r.verb.as_str(), "o" | "t" | "s" | "f" | "x" | "b") {
            continue;
        }
        let body = match &r.body {
            Value::String(s) => s.clone(),
            v => v.to_string(),
        };
        let params: Vec<(String, String)> = r
            .params
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned()))
            .collect();
        let params: Vec<(&str, &str)> = params
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        replica.push(auk_replica::Item::Output(&tern_sdk::wire::encode(
            &r.verb,
            &params,
            body.as_bytes(),
            65_536,
            "p",
        )));
        if r.verb == "f" {
            frames += 1;
            if until == Some(frames) {
                break;
            }
        }
    }
    let st = auk_daemon::tsp::state_json(replica.tsp(), replica.terminal());
    st["surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| json!([s["id"], s["state"], s["doc"], s["sheets"]]))
        .collect()
}

fn played_docs(env: &TestEnv, pane: u64) -> Vec<Value> {
    let st = env.daemon(&format!("tsp {pane}"));
    st["surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| json!([s["id"], s["state"], s["doc"], s["sheets"]]))
        .collect()
}

#[test]
fn surface_play_feeds_recordings_into_a_stand_in_program() {
    let env = env("play");
    let shell = env.new_pane(&[]);
    env.wait(shell, "prompt");
    let fixtures = auk_conformance::fixtures_dir();
    let ask = fixtures.join("python-ask.jsonl");
    let played = env.daemon(&format!(
        "surface-play --pane {shell} {}",
        quote(&ask.to_string_lossy())
    ));
    assert_eq!(played["frames"], 1);
    let st = env.daemon(&format!("tsp {shell}"));
    assert_eq!(
        st["stand_in"], true,
        "the first play restarts the pane as a stand-in"
    );
    assert_eq!(played_docs(&env, shell), replay_fixture(&ask, None));
    assert!(env.pane_info(shell)["exit"].is_null());

    // `until` stops after that frame; `paced` keeps the recorded gaps (here 50× faster).
    let progress = fixtures.join("python-progress.jsonl");
    let out = env.cli(&[
        "surface-play",
        "--pane",
        &shell.to_string(),
        &progress.to_string_lossy(),
        "until",
        "3",
        "paced",
        "50",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let reply: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(reply["frames"], 3);
    let docs = played_docs(&env, shell);
    let want = replay_fixture(&progress, Some(3));
    assert_eq!(docs.last(), want.last());
    // Answers went to the stand-in, never onto a shell's command line.
    let screen = env.daemon(&format!("capture {shell}"))["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!screen.contains("tsp;"), "{screen}");
}

// ---------------------------------------------------------------------------------------------
// The paste rule (§12.1) and effects

#[test]
fn tsp_input_waits_while_a_bracketed_paste_is_open() {
    let (_env, window, probe) = setup_window("paste");
    exchange(&probe, &hello(&[]));
    probe.write(b"\x1b[?2004h");
    probe.write(&open_with("s1", "flow", text_doc()));
    await_tsp(&probe, "the first ack", |b| !events(b, "ack").is_empty());
    // The client opens a paste and hasn't closed it yet.
    window.request(&format!(
        "send {} text {}",
        probe.pane,
        quote("\x1b[200~half a paste")
    ));
    probe.read_until("the open paste", |b| b.ends_with(b"half a paste"));
    facts(&window, [8, 16], true, false);
    std::thread::sleep(Duration::from_millis(400));
    let held = probe.read_for(Duration::from_millis(100));
    assert!(
        split_input(&held).iter().all(|i| !i.is_tsp()),
        "nothing inside the paste: {held:?}"
    );
    window.request(&format!(
        "send {} text {}",
        probe.pane,
        quote(" and the rest\x1b[201~")
    ));
    let input = probe.read_until("the theme event after the paste", |b| {
        !events(&tsp_bodies(&split_input(b)), "theme").is_empty()
    });
    let end = input
        .windows(6)
        .position(|w| w == b"\x1b[201~")
        .expect("the paste end");
    let theme = input
        .windows(6)
        .position(|w| w == b"\x1b_tsp;")
        .expect("the event");
    assert!(end < theme, "the event comes after the paste ends");
}

#[test]
fn effects_reach_the_windows_showing_a_window_key() {
    let env = env("effects");
    let window = env.start_headless("w", &[]);
    let other = env.start_headless("o", &["--window", "elsewhere"]);
    let sent = env.daemon("effect toast 'build finished'");
    assert_eq!(sent["delivered"], 1);
    env.daemon("effect --window elsewhere copy 'some text'");
    let got = wait_for("the toast", || {
        let e = window.request("effects");
        (e.as_array()?.len() == 1).then_some(e)
    });
    assert_eq!(got, json!([{"effect": "toast", "level": "info", "text": "build finished"}]));
    let got = wait_for("the copy", || {
        let e = other.request("effects");
        (e.as_array()?.len() == 1).then_some(e)
    });
    assert_eq!(got, json!([{"effect": "copy", "text": "some text"}]));
}
