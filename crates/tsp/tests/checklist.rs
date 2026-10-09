//! The TSP conformance checklist (`research/sdk-wire.md` §10) at the library level, one test per
//! item, named by its ID. Items that live in another layer (the VT parser's APC framing and DA1,
//! the daemon's PTY writes, Windows) are covered there; the library side of each is here.

mod common;

use auk_tsp::blob::{MAX_BLOB, blob_id};
use auk_tsp::wire::MAX_APC;
use auk_tsp::{
    ALL_FEATURES, AnchorKey, Config, Delivery, Facts, Gate, Grid, Mode, Mods, Refused, RevealAt,
    ScrollBy, SheetChecker, State, T0_KINDS, T1_KINDS, T2_KINDS, TitleSource, TspPane, ViewEvent,
};
use common::{Back, Harness, TestGrid, decode, events, obj, payload};
use serde_json::{Value, json};
use tern_sdk::wire::{Event, Reply};

fn text_main(h: &mut Harness, sf: &str, mode: &str) {
    h.open(
        sf,
        mode,
        json!([{"id": "t", "k": "text", "p": {"text": "hi"}}]),
    );
}

fn errors(out: &[Value]) -> Vec<&Value> {
    events(out, "error")
}

// ---------------------------------------------------------------------------------------------
// A. Parsing program output

#[test]
fn a1_only_tsp_strings_route_to_tsp() {
    let mut h = Harness::new();
    h.pane.on_apc(&mut h.grid, b"Gf=100,a=T;AAAA");
    h.pane
        .on_apc(&mut h.grid, b"tspx;q;{\"q\":\"hello\",\"v\":[1]}");
    assert!(h.out().is_empty());
    h.pane
        .on_apc(&mut h.grid, b"tsp;q;{\"q\":\"hello\",\"v\":[1]}");
    assert_eq!(h.out().len(), 1);
}

#[test]
fn a2_c1_bytes_in_a_body_are_payload() {
    let mut h = Harness::new();
    h.send_json("o", json!({"id": "s1"}));
    // serde_json leaves U+009C raw: the bytes C2 9C sit inside the string.
    let body =
        json!({"sf": "s1", "dark": {"accent": "#fff"}, "name": {"dark": "a\u{9c}b"}}).to_string();
    assert!(body.as_bytes().windows(2).any(|w| w == [0xc2, 0x9c]));
    h.send("t", &body);
    let palette = h.pane.surfaces()[0].palette().unwrap();
    assert_eq!(palette.name, Some(json!({"dark": "a\u{9c}b"})));
    assert!(h.out().is_empty());
}

#[test]
fn a3_parameter_grammar_keeps_bodies_whole() {
    let mut h = Harness::new();
    h.send_json("o", json!({"id": "s1"}));
    h.pane
        .on_apc(&mut h.grid, br#"tsp;s;{"name":"a","css":"x=1;y"}"#);
    let sheets: Vec<_> = h.pane.surfaces()[0]
        .sheets()
        .iter()
        .map(|(n, c)| (n.to_owned(), c.to_owned()))
        .collect();
    assert_eq!(sheets, [("a".to_owned(), "x=1;y".to_owned())]);
    h.pane.on_apc(&mut h.grid, br#"tsp;f;{"sf":"s1","s":1,"ops":[["add","main","s1",null,{"id":"main","k":"col","p":{"role":"x;y=1;"}}]]}"#);
    assert_eq!(h.node("s1", "main").unwrap()["p"]["role"], "x;y=1;");
    // Parameters come before the body; the first non-parameter segment starts it.
    let m = auk_tsp::wire::parse(b"tsp;b;mime=text/plain;id=;aGk=").unwrap();
    assert_eq!(
        m.params,
        [
            ("mime".to_owned(), "text/plain".to_owned()),
            ("id".to_owned(), String::new())
        ]
    );
    assert_eq!(m.body, b"aGk=");
    assert!(h.out().is_empty());
}

#[test]
fn a4_unknowns_are_ignored() {
    let mut h = Harness::new();
    h.send("z", "{\"anything\":1}");
    h.send("Q", "{\"q\":\"hello\",\"v\":[1]}");
    h.send("qq", "{\"q\":\"hello\",\"v\":[1]}");
    assert!(h.out().is_empty(), "unknown verbs get no reply");
    h.send_params(
        "o",
        &[
            ("future", "1"),
            ("key", "99"),
            ("buf", "alt"),
            ("seeded", "1"),
        ],
        r#"{"id":"s1","mode":"flow","later":{"x":1}}"#,
    );
    let s = &h.pane.surfaces()[0];
    assert_eq!((s.id(), s.mode()), ("s1", Mode::Flow));
    // Reserved parameters are ignored on a program's `o`: the engine chose the anchor.
    assert_eq!(s.anchor(), Some(AnchorKey(1)));
    h.send_json("f", json!({"sf": "s1", "s": 1, "ops": [], "extra": true}));
    assert!(h.out().is_empty());
}

#[test]
fn a5_malformed_known_verb_errors() {
    let mut h = Harness::new();
    for verb in ["q", "o", "f", "t", "s", "x"] {
        h.send(verb, "{nope");
        let out = h.out();
        assert_eq!(out.len(), 1);
        let msg = out[0]["msg"].as_str().unwrap();
        assert!(
            msg.starts_with(&format!("malformed {verb} body: ")),
            "{msg}"
        );
        assert!(out[0].get("sf").is_none() && out[0].get("op").is_none());
    }
    h.send("f", "[1,2]");
    assert_eq!(
        h.out()[0]["msg"],
        "malformed f body: expected a JSON object"
    );
    // Subject to the listening rules: after a prompt, nothing.
    h.pane.on_prompt(&mut h.grid);
    h.send("f", "{nope");
    assert!(h.out().is_empty());
}

#[test]
fn a6_apc_length_limit() {
    let mut h = Harness::new();
    text_main(&mut h, "s1", "flow");
    let head = r#"{"sf":"s1","s":2,"ops":[["text","t","append",""#;
    let tail = r#""]]}"#;
    let pad = |n: usize| n - b"tsp;f;".len() - head.len() - tail.len();
    let fill = |n: usize| payload("f", &[], &format!("{head}{}{tail}", "x".repeat(pad(n))));
    let len = |h: &Harness| {
        h.node("s1", "t").unwrap()["p"]["text"]
            .as_str()
            .unwrap()
            .len()
    };
    let exact = fill(MAX_APC);
    assert_eq!(exact.len(), MAX_APC);
    h.pane.on_apc(&mut h.grid, &exact);
    assert_eq!(len(&h), 2 + pad(MAX_APC));
    h.pane.on_apc(&mut h.grid, &fill(MAX_APC + 1));
    assert_eq!(len(&h), 2 + pad(MAX_APC), "abandoned");
    assert!(h.out().is_empty(), "abandoned silently");
}

#[test]
fn a7_chunks_rejoin() {
    let mut h = Harness::new();
    text_main(&mut h, "s1", "flow");
    let n = 1000;
    let text: String = "é😀ab;k=v;".chars().cycle().take(3 * n).collect();
    let body = json!({"sf": "s1", "s": 2, "ops": [["text", "t", "replace", text]]}).to_string();
    assert!(body.len() >= 3 * n);
    // The SDK's encoder cuts exactly as Tern's chunker does.
    let bytes = tern_sdk::wire::encode("f", &[], body.as_bytes(), n, "k7");
    assert!(common::apc_payloads(&bytes).len() >= 3);
    h.write(&bytes);
    assert_eq!(h.node("s1", "t").unwrap()["p"]["text"], json!(text));

    // A chunked blob keeps the first chunk's parameters.
    let data: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
    let blob = tern_sdk::wire::Blob::new(data.clone(), Some("image/png".into())).unwrap();
    let mut enc = tern_sdk::wire::Encoder::new(512);
    let bytes = enc.encode(&tern_sdk::wire::Message::Blob(blob)).unwrap();
    let payloads = common::apc_payloads(&bytes);
    assert!(payloads.len() > 3 && payloads[0].starts_with(b"tsp;b;mime=image/png;c=1;m=1;"));
    h.write(&bytes);
    let stored = h.pane.blobs().get(&blob_id(&data)).expect("joined blob");
    assert_eq!(
        (stored.mime.as_deref(), &stored.bytes),
        (Some("image/png"), &data)
    );
    assert!(h.out().is_empty());
}

#[test]
fn a8_chunk_sequences_drop_with_errors() {
    let mut h = Harness::new();
    // Interrupted: the intervening message is still handled.
    h.send_params("f", &[("c", "k7"), ("m", "1")], r#"{"sf":"s1","#);
    h.send_json("o", json!({"id": "s1", "mode": "flow"}));
    let out = h.out();
    assert_eq!(
        out,
        [json!({"ev": "error", "msg": "chunked f message c=k7 dropped: interrupted"})]
    );
    assert_eq!(h.pane.surfaces().len(), 1);

    // Timed out: the daemon's record.
    h.send_params("f", &[("c", "k8"), ("m", "1")], r#"{"sf":"s1","#);
    h.pane.expire_chunks();
    assert_eq!(
        h.out(),
        [json!({"ev": "error", "msg": "chunked f message c=k8 dropped: timed out"})]
    );
    h.pane.expire_chunks();
    assert!(h.out().is_empty());

    // Over 24 MiB.
    let piece = "a".repeat(MAX_APC - 32);
    for _ in 0..(25_165_824 / piece.len() + 1) {
        h.send_params("b", &[("c", "big"), ("m", "1")], &piece);
    }
    h.send_params("b", &[("c", "big")], &piece);
    assert_eq!(
        h.out(),
        [json!({"ev": "error", "msg": "chunked b message c=big dropped: over 25165824 bytes"})]
    );

    // RIS drops silently: no error now, and no "interrupted" for the next message.
    h.send_params("f", &[("c", "k9"), ("m", "1")], r#"{"sf":"s1","#);
    h.pane.on_reset(&mut h.grid);
    h.send_json("q", json!({"q": "hello", "v": [1]}));
    let out = h.out();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["r"], "hello");
}

#[test]
fn a9_blobs_are_hashed_and_checked() {
    let mut h = Harness::new();
    let data = b"hello".to_vec();
    // What every SDK sends: `mime`, no `id`.
    let blob = tern_sdk::wire::Blob::new(data.clone(), Some("text/plain".into())).unwrap();
    h.write(
        &tern_sdk::wire::Encoder::default()
            .encode(&tern_sdk::wire::Message::Blob(blob))
            .unwrap(),
    );
    assert!(h.pane.blobs().has(&blob_id(&data)));
    assert!(h.out().is_empty());
    // A matching id is accepted; a mismatched one, bad base64 and over-size blobs are rejected.
    let id = blob_id(b"other");
    h.send_params("b", &[("id", &id)], "b3RoZXI=");
    assert!(h.pane.blobs().has(&id) && h.out().is_empty());
    h.send_params("b", &[("id", &blob_id(b"x"))], "aGVsbG8=");
    h.send("b", "not base64!");
    let big = vec![7u8; MAX_BLOB + 1];
    let blob = tern_sdk::wire::Blob {
        id: blob_id(&big),
        mime: None,
        data: big,
    };
    h.write(
        &tern_sdk::wire::Encoder::default()
            .encode(&tern_sdk::wire::Message::Blob(blob))
            .unwrap(),
    );
    let out = h.out();
    assert_eq!(errors(&out).len(), 3, "{out:?}");
    assert!(out[0]["msg"].as_str().unwrap().contains("doesn't match"));
    assert!(
        out[1]["msg"]
            .as_str()
            .unwrap()
            .starts_with("malformed b body")
    );
    assert!(out[2]["msg"].as_str().unwrap().contains("over 16777216"));
}

#[test]
fn a10_serialization_variance_is_accepted() {
    let mut h = Harness::new();
    h.send("q", r#"{"features":[],"v":[1],"q":"hello"}"#);
    assert_eq!(h.out().len(), 1);
    h.send(
        "o",
        r#"{"listen":true,"adopt":false,"mode":"inline","role":null,"title":null,"id":"s1"}"#,
    );
    h.send(
        "f",
        r#"{"ops":[["add","main","s1",null,{"c":[],"p":{},"k":"col","id":"main"}]],"s":1,"sf":"s1"}"#,
    );
    for removal in [
        r#"{"sf":"s1","name":"a","css":null}"#,
        r#"{"sf":"s1","name":"a","css":""}"#,
        r#"{"sf":"s1","name":"a"}"#,
    ] {
        h.send("s", r#"{"sf":"s1","name":"a","css":"p{}"}"#);
        assert_eq!(h.pane.surfaces()[0].sheets().iter().count(), 1);
        h.send("s", removal);
        assert!(h.pane.surfaces()[0].sheets().is_empty(), "{removal}");
    }
    // Go writes `adopt` before `listen` and an explicit `css:null`.
    h.send(
        "o",
        r#"{"id":"s2","mode":"flow","adopt":false,"listen":false}"#,
    );
    assert!(h.out().is_empty());
    assert_eq!(h.pane.surfaces().len(), 2);
}

// ---------------------------------------------------------------------------------------------
// B. Handshake

#[test]
fn b1_hello_is_answered_at_once() {
    let mut h = Harness::new();
    h.grid.cols = 120;
    h.pane.on_apc(&mut h.grid, br#"tsp;q;{"q":"hello","v":[1],"app":"omp","ver":"14.2.0","features":["edit","undo","send"]}"#);
    // The reply is queued while handling the hello, before the replica parses the `ESC [ c`
    // that follows it in the same write, so it reaches the PTY before the DA1 answer.
    let out = h.pane.take_output();
    let back = decode(&out);
    assert!(matches!(&back[..], [Back::Reply(Reply::Hello(hello))] if hello.cols == 120));
}

#[test]
fn b2_hello_without_v1_gets_no_reply() {
    let mut h = Harness::new();
    h.send_json("q", json!({"q": "hello", "v": [2]}));
    h.send_json("q", json!({"q": "hello"}));
    h.send_json("q", json!({"q": "hello", "v": 1}));
    assert!(h.out().is_empty());
    assert!(h.pane.hello().is_none());
}

#[test]
fn b4_reply_fields_have_exact_types() {
    let mut h = Harness::new();
    h.grid.cols = 97;
    h.pane.set_facts(
        &h.grid,
        Facts {
            cell: (7.5, 17.0),
            dark: false,
            reduce_motion: true,
            hour12: true,
            visible: true,
        },
    );
    h.send_json("q", json!({"q": "hello", "v": [1]}));
    let out = h.pane.take_output();
    let reply = &out[0].body;
    let fields: Vec<&str> = obj(reply).keys().map(String::as_str).collect();
    assert_eq!(
        fields,
        [
            "r",
            "v",
            "term",
            "ver",
            "kinds",
            "features",
            "apc",
            "credits",
            "cols",
            "cell",
            "dark",
            "reduceMotion",
            "hour12"
        ]
    );
    assert_eq!(reply["v"], json!(1));
    assert!(
        reply["v"].is_u64()
            && reply["apc"].is_u64()
            && reply["credits"].is_u64()
            && reply["cols"].is_u64()
    );
    assert!(reply["term"].is_string() && reply["ver"].is_string());
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
    assert!(reply["cell"]["w"].is_number() && reply["cell"]["h"].is_u64());
    assert!(
        reply["dark"].is_boolean()
            && reply["reduceMotion"].is_boolean()
            && reply["hour12"].is_boolean()
    );
    // The Rust SDK drops a mistyped reply; this one decodes typed, every value intact.
    match &decode(&out)[..] {
        [Back::Reply(Reply::Hello(hello))] => {
            assert_eq!(
                (
                    hello.v,
                    hello.term.as_str(),
                    hello.apc,
                    hello.credits,
                    hello.cols
                ),
                (1, "auk", 65536, 2, 97)
            );
            assert_eq!(hello.cell.map(|c| (c.w, c.h)), Some((7.5, 17.0)));
            assert!(!hello.dark && hello.reduce_motion && hello.hour12);
            assert_eq!(hello.ver, env!("CARGO_PKG_VERSION"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn b5_apc_and_credits_are_usable() {
    let mut h = Harness::with(Config {
        apc: 0,
        credits: 0,
        ..Config::default()
    });
    let reply = h.hello(&[]);
    assert_eq!(
        (reply["apc"].as_u64(), reply["credits"].as_u64()),
        (Some(1), Some(1))
    );
    let mut h = Harness::with(Config {
        apc: 10_000_000,
        ..Config::default()
    });
    let apc = h.hello(&[])["apc"].as_u64().unwrap() as usize;
    // A full chunk with its prefix and parameters fits one APC string.
    assert!(b"tsp;b;mime=application/octet-stream;c=zzzzzzzzzzzzz;m=1;".len() + apc <= MAX_APC);
    let reply = Harness::new().hello(&[]);
    assert_eq!(
        (reply["apc"].as_u64(), reply["credits"].as_u64()),
        (Some(65536), Some(2))
    );
}

#[test]
fn b6_advertises_exactly_what_is_drawn() {
    let reply = Harness::new().hello(&[]);
    let mut kinds: Vec<&str> = reply["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap())
        .collect();
    let mut want: Vec<&str> = T0_KINDS.iter().chain(&T1_KINDS).chain(T2_KINDS).copied().collect();
    kinds.sort_unstable();
    want.sort_unstable();
    assert_eq!(kinds, want);
    // `image` joined T1 (D24).
    assert!(kinds.contains(&"image"));
    // Every T2 kind is drawn and advertised (A23, D26).
    for t2 in ["math", "chart", "picker", "prefs", "effort", "block"] {
        assert!(kinds.contains(&t2), "{t2} is advertised");
    }
    for f in reply["features"].as_array().unwrap() {
        assert!(ALL_FEATURES.contains(&f.as_str().unwrap()));
    }
    assert!(
        reply["features"]
            .as_array()
            .unwrap()
            .contains(&json!("aside")),
        "prefs, so aside"
    );
    // And `add` takes only advertised kinds.
    let mut h = Harness::new();
    h.send_json("o", json!({"id": "s1"}));
    h.frame("s1", 1, json!([["add", "main", "s1", null, {"id": "main", "k": "col", "c": [{"id": "i", "k": "sparkle"}]}]]));
    assert_eq!(h.out()[0]["msg"], "i: unknown kind sparkle");
}

#[test]
fn b6_a_t0_t1_hello_has_no_t2_and_no_aside() {
    let config = Config {
        kinds: T0_KINDS.iter().chain(&T1_KINDS).map(|k| (*k).to_owned()).collect(),
        features: ALL_FEATURES
            .iter()
            .filter(|f| **f != "aside")
            .map(|f| (*f).to_owned())
            .collect(),
        ..Config::default()
    };
    let reply = Harness::with(config.clone()).hello(&[]);
    let kinds = reply["kinds"].as_array().unwrap();
    for t2 in T2_KINDS {
        assert!(!kinds.contains(&json!(t2)), "{t2} not advertised");
    }
    assert!(!reply["features"].as_array().unwrap().contains(&json!("aside")));
    let mut h = Harness::with(config);
    h.send_json("o", json!({"id": "s1"}));
    h.frame("s1", 1, json!([["add", "main", "s1", null, {"id": "main", "k": "col", "c": [{"id": "i", "k": "math"}]}]]));
    assert_eq!(h.out()[0]["msg"], "i: unknown kind math");
}

#[test]
fn b7_blobs_query_answers_in_request_order() {
    let mut h = Harness::new();
    let (a, b) = (blob_id(b"a"), blob_id(b"b"));
    h.send("b", "YQ==");
    h.send("b", "Yg==");
    h.send_json("q", json!({"q": "blobs", "ids": [b, "missing", a]}));
    let out = h.pane.take_output();
    match &decode(&out)[..] {
        [Back::Reply(Reply::Blobs(r))] => assert_eq!(r.have, [b, a]),
        other => panic!("{other:?}"),
    }
    // Not advertised: not answered.
    let mut h = Harness::with(Config {
        features: vec![],
        ..Config::default()
    });
    h.send_json("q", json!({"q": "blobs", "ids": []}));
    assert!(h.out().is_empty());
}

#[test]
fn b7_a_blobs_query_counts_as_a_use_for_lru_eviction() {
    use base64::Engine as _;
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    // A store holding two of these blobs: a third evicts the least recently used.
    let blob = |c: u8| vec![c; 1000];
    let mut h = Harness::with(Config::default());
    h.pane = TspPane::with(
        Config::default(),
        Box::new(auk_tsp::AcceptAll),
        Box::new(auk_tsp::MemoryBlobs::new(2000)),
    );
    let ids: Vec<String> = b"abc".map(|c| blob_id(&blob(c))).to_vec();
    h.send("b", &b64(&blob(b'a')));
    h.send("b", &b64(&blob(b'b')));
    // Without the query, `a` (the oldest) would go first. Asking about it is a use.
    h.send_json("q", json!({"q": "blobs", "ids": [ids[0]]}));
    assert_eq!(h.out(), [json!({"r": "blobs", "have": [ids[0]]})]);
    h.send("b", &b64(&blob(b'c')));
    let blobs = h.pane.blobs();
    assert!(blobs.has(&ids[0]), "the queried blob stays");
    assert!(!blobs.has(&ids[1]), "the least recently used goes");
    assert!(blobs.has(&ids[2]));
    // And the control: with no query, the oldest goes.
    let mut h = Harness::with(Config::default());
    h.pane = TspPane::with(
        Config::default(),
        Box::new(auk_tsp::AcceptAll),
        Box::new(auk_tsp::MemoryBlobs::new(2000)),
    );
    for c in *b"abc" {
        h.send("b", &b64(&blob(c)));
    }
    assert!(!h.pane.blobs().has(&ids[0]));
    assert!(h.pane.blobs().has(&ids[1]));
}

#[test]
fn b8_other_queries_are_unanswered() {
    let mut h = Harness::new();
    h.send_json("q", json!({"q": "version"}));
    h.send_json("q", json!({"v": [1]}));
    h.send_json("q", json!({"q": 7}));
    assert!(h.out().is_empty());
}

#[test]
fn b9_hello_replaces_and_is_forgotten() {
    let mut h = Harness::new();
    h.send_json(
        "q",
        json!({"q": "hello", "v": [1], "app": "omp", "features": ["edit", "send"]}),
    );
    assert_eq!(h.pane.hello().unwrap().app.as_deref(), Some("omp"));
    assert_eq!(
        h.pane.app(),
        None,
        "app applies only while a listening surface is open"
    );
    text_main(&mut h, "s1", "inline");
    assert_eq!(h.pane.app(), Some("omp"));
    h.send_json("q", json!({"q": "hello", "v": [1]}));
    let hello = h.pane.hello().unwrap();
    assert!(
        hello.app.is_none() && hello.features.is_empty(),
        "a later hello clears them"
    );
    assert_eq!(h.pane.app(), None);

    for gone in [TspPane::on_prompt, TspPane::on_exit, TspPane::on_reset]
        as [fn(&mut TspPane, &mut dyn Grid); 3]
    {
        h.send_json("q", json!({"q": "hello", "v": [1], "app": "x"}));
        assert!(h.pane.hello().is_some());
        gone(&mut h.pane, &mut h.grid);
        assert!(h.pane.hello().is_none());
    }
    // A listen:false surface doesn't make the app the pane's.
    h.send_json("q", json!({"q": "hello", "v": [1], "app": "ls"}));
    h.send_json("o", json!({"id": "ls", "mode": "flow", "listen": false}));
    assert_eq!(h.pane.app(), None);
}

#[test]
fn b10_native_editing_events_need_program_features() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.open(
        "s1",
        "inline",
        json!([{"id": "ed", "k": "editor", "p": {"text": "ab", "sendable": true}}]),
    );
    let edit = ViewEvent::Edit {
        id: "ed".into(),
        from: 0,
        to: 0,
        text: "x".into(),
        cursor: 1,
        len: 2,
    };
    let undo = ViewEvent::Undo { id: "ed".into() };
    let send = ViewEvent::Send {
        id: "ed".into(),
        text: "go".into(),
    };
    for ev in [edit.clone(), undo.clone(), send.clone()] {
        assert_eq!(h.pane.view_event(&h.grid, "s1", ev), Err(Refused::Feature));
    }
    h.hello(&["undo", "send"]);
    assert_eq!(
        h.pane.view_event(&h.grid, "s1", undo.clone()),
        Err(Refused::Feature),
        "undo needs edit too"
    );
    assert!(h.out().is_empty());
    h.hello(&["edit", "undo", "send"]);
    for ev in [edit, undo, send] {
        assert_eq!(h.pane.view_event(&h.grid, "s1", ev), Ok(Delivery::Sent));
    }
    let names: Vec<_> = h
        .out()
        .iter()
        .map(|e| e["ev"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["edit", "undo", "send"]);
}

#[test]
fn b11_early_frames_before_the_hello_reply() {
    // omp's optimistic start: `o`, `t` and `f` before the hello is even sent.
    let mut h = Harness::new();
    h.send_json(
        "o",
        json!({"id": "s:1", "mode": "inline", "title": "omp", "role": "omp.session"}),
    );
    h.send_json("t", json!({"sf": "s:1", "dark": {"accent": "#00b4ff"}}));
    h.frame(
        "s:1",
        1,
        json!([["add", "main", "s:1", null, {"id": "main", "k": "col"}]]),
    );
    h.hello(&["edit"]);
    let s = &h.pane.surfaces()[0];
    assert!(s.palette().is_some() && s.document().region("main").is_some());
    // Unconfirmed: the program closes it unkept.
    h.send_json("x", json!({"id": "s:1", "keep": false}));
    assert!(h.pane.surfaces().is_empty() && h.out().is_empty());
}

#[test]
fn b12_tsp_off_ignores_everything() {
    let mut h = Harness::with(Config {
        enabled: false,
        ..Config::default()
    });
    h.send_json("q", json!({"q": "hello", "v": [1]}));
    h.send_json("o", json!({"id": "s1"}));
    h.send("f", "{nope");
    assert!(h.out().is_empty() && h.pane.surfaces().is_empty());
}

// ---------------------------------------------------------------------------------------------
// C. Surfaces

#[test]
fn c1_bad_opens_are_errors() {
    let mut h = Harness::new();
    h.send_json("o", json!({"mode": "inline"}));
    h.send_json("o", json!({"id": ""}));
    h.send_json("o", json!({"id": "s1", "mode": "sideways"}));
    h.grid.alt = true;
    h.send_json("o", json!({"id": "s2", "mode": "inline"}));
    h.send_json("o", json!({"id": "s3", "mode": "flow"}));
    h.send_json("o", json!({"id": "s4"}));
    let out = h.out();
    assert_eq!(errors(&out).len(), 6, "{out:?}");
    assert_eq!(
        out[2],
        json!({"ev": "error", "sf": "s1", "msg": "unknown mode \"sideways\""})
    );
    assert_eq!(out[3]["sf"], "s2");
    h.send_json("o", json!({"id": "s5", "mode": "screen"}));
    assert!(h.out().is_empty());
    assert_eq!(h.pane.surfaces().len(), 1);
}

#[test]
fn c2_replacement_and_stacking() {
    let mut h = Harness::new();
    let state = |h: &Harness| -> Vec<(String, State)> {
        h.pane
            .surfaces()
            .iter()
            .map(|s| (s.id().to_owned(), s.state()))
            .collect()
    };
    h.send_json("o", json!({"id": "f1", "mode": "flow"}));
    h.send_json("o", json!({"id": "i1", "mode": "inline"}));
    h.send_json("o", json!({"id": "c1", "mode": "screen"}));
    // A screen `o` never touches the inline surface; neither touches a flow surface.
    assert!(state(&h).iter().all(|(_, s)| *s == State::Live));
    assert!(h.pane.covered(h.pane.surfaces()[1].key()));
    // A new screen surface discards the old one.
    h.send_json("o", json!({"id": "c2", "mode": "screen"}));
    assert_eq!(state(&h).len(), 3);
    // A new inline replaces the live inline (kept) and discards the covering screen.
    h.send_json("o", json!({"id": "i2", "mode": "inline"}));
    let ids: Vec<(String, State)> = state(&h);
    assert_eq!(
        ids,
        [
            ("f1".into(), State::Live),
            ("i1".into(), State::Closed),
            ("i2".into(), State::Live)
        ]
    );
    // A new flow replaces the live flow, kept.
    h.send_json("o", json!({"id": "f2", "mode": "flow"}));
    assert_eq!(h.pane.surfaces()[0].state(), State::Closed);
    // One screen surface per buffer.
    h.send_json("o", json!({"id": "m", "mode": "screen"}));
    h.grid.alt = true;
    h.send_json("o", json!({"id": "a", "mode": "screen"}));
    let screens: Vec<_> = h
        .pane
        .surfaces()
        .iter()
        .filter(|s| s.mode() == Mode::Screen && s.is_live())
        .collect();
    assert_eq!(screens.len(), 2);
    assert!(h.out().is_empty());
}

#[test]
fn c3_frames_route_to_the_active_buffer_first() {
    let mut h = Harness::new();
    h.open("s1", "screen", json!([]));
    h.grid.alt = true;
    h.open("s1", "screen", json!([{"id": "x", "k": "text"}]));
    assert!(h.pane.surfaces()[1].document().get("x").is_some());
    assert!(h.pane.surfaces()[0].document().get("x").is_none());
    h.grid.alt = false;
    h.frame(
        "s1",
        2,
        json!([["add", "y", "main", null, {"id": "y", "k": "text"}]]),
    );
    assert!(h.pane.surfaces()[0].document().get("y").is_some());
    assert_eq!(
        h.pane.find("s1", true).unwrap().key(),
        h.pane.surfaces()[1].key()
    );
}

#[test]
fn c4_inline_and_flow_open_an_anchor() {
    let mut h = Harness::new();
    h.send_json("o", json!({"id": "f", "mode": "flow"}));
    h.send_json("o", json!({"id": "i", "mode": "inline"}));
    h.send_json("o", json!({"id": "c", "mode": "screen"}));
    let anchors: Vec<_> = h.pane.surfaces().iter().map(|s| s.anchor()).collect();
    assert_eq!(anchors, [Some(AnchorKey(1)), Some(AnchorKey(2)), None]);
    assert_eq!(
        h.grid.anchors.keys().copied().collect::<Vec<_>>(),
        [AnchorKey(1), AnchorKey(2)]
    );
}

#[test]
fn c5_a_lost_anchor_ends_its_surface() {
    let mut h = Harness::new();
    text_main(&mut h, "s1", "flow");
    h.send_json("o", json!({"id": "quiet", "mode": "flow", "listen": false}));
    h.send_json("o", json!({"id": "s2", "mode": "inline"}));
    h.out();
    let key = |h: &Harness, id: &str| {
        h.pane
            .surfaces()
            .iter()
            .find(|s| s.id() == id)
            .unwrap()
            .anchor()
            .unwrap()
    };
    let (k1, kq) = (key(&h, "s1"), key(&h, "quiet"));
    h.pane.on_anchor_gone(&mut h.grid, kq);
    assert!(h.out().is_empty(), "listen:false hears nothing");
    h.pane.on_anchor_gone(&mut h.grid, k1);
    assert_eq!(h.out(), [json!({"ev": "gone", "sf": "s1", "ids": ["s1"]})]);
    assert_eq!(h.pane.surfaces().len(), 1);
    h.frame("s1", 2, json!([]));
    assert_eq!(h.out()[0]["msg"], "unknown surface s1");
}

#[test]
fn c6_close_keeps_main_or_removes() {
    let mut h = Harness::new();
    h.send_json("o", json!({"id": "s1"}));
    h.frame("s1", 1, json!([
        ["add", "main", "s1", null, {"id": "main", "k": "col", "c": [{"id": "t", "k": "text"}]}],
        ["add", "dock", "s1", null, {"id": "dock", "k": "col", "c": [{"id": "ed", "k": "editor"}]}],
        ["add", "layer", "s1", null, {"id": "layer", "k": "col"}],
        ["focus", "ed"]
    ]));
    h.send_json("x", json!({"id": "s1"}));
    let s = &h.pane.surfaces()[0];
    assert_eq!(s.state(), State::Closed);
    let doc = s.document();
    assert!(
        doc.region("main").is_some()
            && doc.region("dock").is_none()
            && doc.region("layer").is_none()
    );
    assert_eq!(doc.focus(), None);
    // A closed surface takes no more frames.
    h.frame("s1", 2, json!([["del", "t"]]));
    assert_eq!(
        h.out(),
        [json!({"ev": "error", "sf": "s1", "s": 2, "msg": "surface s1 is closed"})]
    );
    assert!(h.pane.surfaces()[0].document().get("t").is_some());
    // keep:false removes it and its anchor.
    h.send_json("o", json!({"id": "s2", "mode": "flow"}));
    let anchor = h.pane.surfaces()[1].anchor().unwrap();
    h.send_json("x", json!({"id": "s2", "keep": false}));
    assert_eq!(h.pane.surfaces().len(), 1);
    assert!(h.grid.dropped.contains(&anchor));
    h.send_json("x", json!({"id": "nope"}));
    assert_eq!(h.out()[0]["msg"], "unknown surface nope");
}

#[test]
fn c7_adopt() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.send_json("o", json!({"id": "s1", "title": "one", "role": "r1"}));
    h.frame("s1", 1, json!([
        ["add", "main", "s1", null, {"id": "main", "k": "col", "c": [{"id": "t", "k": "text", "p": {"text": "a"}}]}],
        ["add", "dock", "s1", null, {"id": "dock", "k": "col"}]
    ]));
    h.send_json("x", json!({"id": "s1", "keep": true}));
    h.send_json("o", json!({"id": "c", "mode": "screen"}));
    h.send_json("o", json!({"id": "s2"}));
    h.send_json(
        "o",
        json!({"id": "s1", "mode": "screen", "adopt": true, "title": "two"}),
    );
    let s1 = h.pane.surfaces().iter().find(|s| s.id() == "s1").unwrap();
    assert_eq!(
        (s1.state(), s1.mode(), s1.title(), s1.role()),
        (State::Live, Mode::Inline, Some("two"), Some("r1"))
    );
    assert!(s1.document().get("t").is_some() && s1.document().region("dock").is_none());
    let s2 = h.pane.surfaces().iter().find(|s| s.id() == "s2").unwrap();
    assert_eq!(
        s2.state(),
        State::Closed,
        "the live inline closes as if kept"
    );
    assert!(
        h.pane.surfaces().iter().all(|s| s.mode() != Mode::Screen),
        "a screen surface is discarded"
    );
    // Fully editable again.
    h.frame("s1", 2, json!([["text", "t", "append", "b"]]));
    assert_eq!(h.node("s1", "t").unwrap()["p"]["text"], "ab");
    // Adopting the live id does nothing; an unknown id hears gone without sf.
    let before = h.pane.state_hash();
    h.send_json("o", json!({"id": "s1", "adopt": true, "title": "three"}));
    assert_eq!(h.pane.state_hash(), before);
    h.send_json("o", json!({"id": "nope", "adopt": true}));
    assert_eq!(h.out(), [json!({"ev": "gone", "ids": ["nope"]})]);
    h.send_json("o", json!({"id": "nope", "adopt": true, "listen": false}));
    assert!(h.out().is_empty());
    // Not on the alternate screen.
    h.grid.alt = true;
    h.send_json("o", json!({"id": "s2", "mode": "screen", "adopt": true}));
    assert_eq!(
        h.out()[0]["msg"],
        "inline surfaces can't open on the alternate screen"
    );
    assert_eq!(
        h.pane
            .surfaces()
            .iter()
            .find(|s| s.id() == "s2")
            .unwrap()
            .state(),
        State::Closed
    );
}

#[test]
fn c6_x_keep_true_on_a_screen_surface_discards_it() {
    let mut h = Harness::new();
    text_main(&mut h, "i", "inline");
    text_main(&mut h, "c", "screen");
    assert!(h.pane.covered(h.pane.surfaces()[0].key()));
    // The modes table's screen row: "On x keep:true: discarded". Nothing is kept, the inline
    // surface underneath is uncovered, and the id is gone.
    h.send_json("x", json!({"id": "c", "keep": true}));
    assert!(h.out().is_empty());
    let left: Vec<(String, State)> = h
        .pane
        .surfaces()
        .iter()
        .map(|s| (s.id().to_owned(), s.state()))
        .collect();
    assert_eq!(left, [("i".to_owned(), State::Live)]);
    assert!(!h.pane.covered(h.pane.surfaces()[0].key()));
    h.frame("c", 2, json!([]));
    assert_eq!(h.out()[0]["msg"], "unknown surface c");
    // The default (no keep) does the same on the alternate screen.
    h.grid.alt = true;
    text_main(&mut h, "a", "screen");
    h.send_json("x", json!({"id": "a"}));
    assert!(h.pane.surfaces().iter().all(|s| s.mode() != Mode::Screen));
}

#[test]
fn c7_adopt_picks_the_newest_of_several_closed_inline_surfaces() {
    let mut h = Harness::new();
    for (text, other) in [("first", "o1"), ("second", "o2")] {
        h.open("s1", "inline", json!([{"id": "t", "k": "text", "p": {"text": text}}]));
        h.send_json("x", json!({"id": "s1", "keep": true}));
        // An unrelated closed inline surface in between, with another id.
        h.open(other, "inline", json!([]));
        h.send_json("x", json!({"id": other}));
    }
    let closed = |h: &Harness| -> Vec<(String, State)> {
        h.pane
            .surfaces()
            .iter()
            .map(|s| (s.id().to_owned(), s.state()))
            .collect()
    };
    assert_eq!(
        closed(&h),
        [
            ("s1".into(), State::Closed),
            ("o1".into(), State::Closed),
            ("s1".into(), State::Closed),
            ("o2".into(), State::Closed)
        ]
    );
    h.send_json("o", json!({"id": "s1", "adopt": true}));
    assert!(h.out().is_empty());
    assert_eq!(
        closed(&h),
        [
            ("s1".into(), State::Closed),
            ("o1".into(), State::Closed),
            ("s1".into(), State::Live),
            ("o2".into(), State::Closed)
        ]
    );
    // The adopted one is the newer: its text is "second", and frames reach it.
    h.frame("s1", 2, json!([["text", "t", "append", "!"]]));
    assert_eq!(h.node("s1", "t").unwrap()["p"]["text"], "second!");
    let first = &h.pane.surfaces()[0];
    assert_eq!(first.document().get("t").unwrap().to_json()["p"]["text"], "first");
}

#[test]
fn c8_listen_false_is_silent() {
    let mut h = Harness::new();
    h.send_json("o", json!({"id": "ls", "mode": "flow", "listen": false}));
    h.send("f", "{nope");
    h.frame("ls", 1, json!([["set", "nope", {}]]));
    h.frame("other", 1, json!([]));
    h.send_json("x", json!({"id": "unknown"}));
    h.pane.ack(&h.grid, "ls", 1);
    h.pane.set_facts(
        &h.grid,
        Facts {
            dark: false,
            ..Facts::default()
        },
    );
    assert!(h.out().is_empty(), "no acks, events, errors or answers");
    assert!(!h.pane.surfaces()[0].interactive(), "controls don't flip");
    assert_eq!(
        h.pane
            .view_event(&h.grid, "ls", ViewEvent::Focus { id: "main".into() }),
        Err(Refused::NotListening)
    );
    // Until the program's next `q` or listening `o`.
    h.send_json("q", json!({"q": "hello", "v": [1]}));
    assert_eq!(h.out().len(), 1);
    h.send_json("o", json!({"id": "ls2", "mode": "flow", "listen": false}));
    h.send_json("o", json!({"id": "s1"}));
    h.frame("zz", 1, json!([]));
    assert_eq!(h.out().len(), 1);
}

#[test]
fn c9_exit_and_reset() {
    let mut h = Harness::new();
    h.hello(&[]);
    text_main(&mut h, "f", "flow");
    text_main(&mut h, "i", "inline");
    text_main(&mut h, "c", "screen");
    h.pane.on_exit(&mut h.grid);
    let states: Vec<_> = h
        .pane
        .surfaces()
        .iter()
        .map(|s| (s.id(), s.state()))
        .collect();
    assert_eq!(states, [("f", State::Closed), ("i", State::Closed)]);
    // Nothing until a `q` or `o`.
    h.frame("f", 2, json!([]));
    h.pane.set_facts(
        &h.grid,
        Facts {
            dark: false,
            ..Facts::default()
        },
    );
    assert!(h.out().is_empty());
    h.send_json("o", json!({"id": "n", "mode": "flow"}));
    h.frame("zz", 1, json!([]));
    assert_eq!(h.out().len(), 1);
    h.pane.on_reset(&mut h.grid);
    assert!(h.pane.surfaces().is_empty());
    h.frame("n", 1, json!([]));
    assert!(h.out().is_empty());
}

#[test]
fn c10_prompt_discards_inline_and_screen_and_closes_flow() {
    let mut h = Harness::new();
    text_main(&mut h, "kept", "inline");
    h.send_json("x", json!({"id": "kept"}));
    text_main(&mut h, "f", "flow");
    text_main(&mut h, "i", "inline");
    text_main(&mut h, "c", "screen");
    let inline = h
        .pane
        .surfaces()
        .iter()
        .find(|s| s.id() == "i")
        .unwrap()
        .anchor()
        .unwrap();
    let kept = h.pane.surfaces()[0].anchor().unwrap();
    h.grid.written_below.push(kept);
    h.pane.on_prompt(&mut h.grid);
    let states: Vec<_> = h
        .pane
        .surfaces()
        .iter()
        .map(|s| (s.id(), s.state()))
        .collect();
    assert_eq!(states, [("f", State::Closed)]);
    assert_eq!(
        h.grid.undone,
        [inline],
        "nothing was written below the live inline anchor"
    );
    assert_eq!(h.grid.dropped, [kept]);
    assert_eq!(h.pane.gate(), Gate::Gone);
    // A late `x` gets no answer.
    h.send_json("x", json!({"id": "i"}));
    assert!(h.out().is_empty());
}

#[test]
fn c11_alt_screen_surfaces_end_with_the_screen() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.open("main-screen", "screen", json!([]));
    h.grid.alt = true;
    h.open("alt", "screen", json!([]));
    h.pane.on_screen_cleared(&mut h.grid);
    assert_eq!(
        h.out(),
        [json!({"ev": "gone", "sf": "alt", "ids": ["alt"]})]
    );
    h.open("alt2", "screen", json!([]));
    h.grid.alt = false;
    h.pane.on_alt_screen(&mut h.grid, false);
    assert_eq!(
        h.out(),
        [json!({"ev": "gone", "sf": "alt2", "ids": ["alt2"]})]
    );
    h.pane.on_screen_cleared(&mut h.grid);
    let ids: Vec<_> = h.pane.surfaces().iter().map(|s| s.id()).collect();
    assert_eq!(ids, ["main-screen"]);
}

#[test]
fn c12_suspend_and_resume() {
    let mut h = Harness::new();
    text_main(&mut h, "s1", "inline");
    h.pane.take_changes();
    h.frame("s1", 2, json!([["suspend"]]));
    let s = &h.pane.surfaces()[0];
    assert!(s.suspended() && !s.interactive() && s.is_live());
    assert!(h.pane.take_changes().surfaces[0].meta);
    h.frame("s1", 3, json!([["text", "t", "append", "!"], ["resume"]]));
    assert!(!h.pane.surfaces()[0].suspended());
    assert_eq!(
        h.node("s1", "t").unwrap()["p"]["text"],
        "hi!",
        "frames still apply"
    );
}

#[test]
fn c13_roles_and_app_for_region_attributes() {
    let mut h = Harness::new();
    h.send_json("q", json!({"q": "hello", "v": [1], "app": "omp"}));
    h.send_json("o", json!({"id": "s1", "role": "omp.session"}));
    h.frame("s1", 1, json!([["add", "main", "s1", null, {"id": "main", "k": "row", "p": {"role": "transcript", "tone": "x"}}]]));
    let s = &h.pane.surfaces()[0];
    assert_eq!(s.role(), Some("omp.session"));
    assert_eq!(
        s.document().region("main").unwrap().prop("role"),
        Some(&json!("transcript"))
    );
    assert_eq!(h.pane.app(), Some("omp"));
}

#[test]
fn c14_title_rule() {
    let mut h = Harness::new();
    assert_eq!(h.pane.title(), &TitleSource::Grid);
    h.send_json(
        "o",
        json!({"id": "q", "mode": "flow", "listen": false, "title": "ignored"}),
    );
    assert_eq!(
        h.pane.title(),
        &TitleSource::Grid,
        "only listening surfaces"
    );
    h.send_json("o", json!({"id": "s1", "mode": "flow"}));
    assert_eq!(
        h.pane.title(),
        &TitleSource::Surface(None),
        "the shell's title is dropped"
    );
    h.send_json("o", json!({"id": "s2", "title": "omp"}));
    assert_eq!(h.pane.title(), &TitleSource::Surface(Some("omp".into())));
    h.send_json("o", json!({"id": "s3"}));
    assert_eq!(h.pane.title(), &TitleSource::Surface(Some("omp".into())));
    h.pane.on_title();
    assert_eq!(
        h.pane.title(),
        &TitleSource::Grid,
        "OSC 0/2 names the pane again"
    );
    h.pane.on_prompt(&mut h.grid);
    h.send_json("o", json!({"id": "s4", "mode": "flow"}));
    assert_eq!(
        h.pane.title(),
        &TitleSource::Surface(None),
        "the next command's first surface"
    );
}

// ---------------------------------------------------------------------------------------------
// D. Frames and ops

#[test]
fn d1_frames_apply_atomically() {
    let mut h = Harness::new();
    text_main(&mut h, "s1", "inline");
    h.pane.take_changes();
    h.frame(
        "s1",
        2,
        json!([
            ["add", "a", "main", null, {"id": "a", "k": "text"}],
            ["set", "t", {"tone": "info"}],
            ["text", "a", "append", "x"]
        ]),
    );
    // One frame is one batch of changes: a view draws all of it or none.
    let changes = h.pane.take_changes();
    let doc = &changes.surfaces[0].doc;
    assert!(doc.children.contains("main") && doc.nodes.contains("t") && doc.nodes.contains("a"));
    assert!(h.pane.take_changes().is_empty());
}

#[test]
fn d2_a_failing_op_is_rejected_alone() {
    let mut h = Harness::new();
    text_main(&mut h, "s1", "inline");
    h.frame(
        "s1",
        2,
        json!([
            ["add", "a", "main", null, {"id": "a", "k": "text"}],
            ["set", "nope", {}],
            ["set", "a", {"tone": "ok"}]
        ]),
    );
    assert_eq!(
        h.out(),
        [json!({"ev": "error", "sf": "s1", "s": 2, "op": 1, "msg": "unknown id nope"})]
    );
    assert_eq!(h.node("s1", "a").unwrap()["p"]["tone"], "ok");
    // Later ops see the document without the failed one.
    h.frame(
        "s1",
        3,
        json!([["add", "b", "main", null, {"id": "b", "k": "nope"}], ["del", "b"]]),
    );
    let out = h.out();
    assert_eq!(
        (out[0]["op"].as_u64(), out[1]["op"].as_u64()),
        (Some(0), Some(1))
    );
}

#[test]
fn d3_whole_frame_drops() {
    let mut h = Harness::new();
    text_main(&mut h, "s1", "flow");
    h.send_json("x", json!({"id": "s1"}));
    h.send_json("o", json!({"id": "s2"}));
    h.send_json("f", json!({"s": 1, "ops": []}));
    h.send_json("f", json!({"sf": "zz", "s": 2, "ops": []}));
    h.send_json("f", json!({"sf": "s1", "s": 3, "ops": []}));
    h.send_json("f", json!({"sf": "s2", "s": 4, "ops": {"add": 1}}));
    h.send("f", "not json");
    let out = h.out();
    assert_eq!(out.len(), 5);
    assert!(
        out.iter()
            .all(|e| e["ev"] == "error" && e.get("op").is_none()),
        "{out:?}"
    );
    assert_eq!(out[0].get("sf"), None);
    assert_eq!(
        (out[1]["sf"].as_str(), out[1]["s"].as_u64()),
        (Some("zz"), Some(2))
    );
    assert_eq!(out[3]["msg"], "ops must be an array");
}

#[test]
fn d4_s_is_echoed_not_ordered() {
    let mut h = Harness::new();
    text_main(&mut h, "s1", "flow");
    h.frame("s1", 5, json!([["text", "t", "append", "5"]]));
    h.frame(
        "s1",
        3,
        json!([["text", "t", "append", "3"], ["set", "nope", {}]]),
    );
    assert_eq!(h.node("s1", "t").unwrap()["p"]["text"], "hi53");
    assert_eq!(h.out()[0]["s"], 3);
    assert_eq!(h.pane.surfaces()[0].last_s(), Some(3));
    h.pane.ack(&h.grid, "s1", 3);
    let out = h.out();
    assert_eq!(
        out.last().unwrap(),
        &json!({"ev": "ack", "sf": "s1", "s": 3})
    );
}

#[test]
fn d5_add_validation() {
    let mut h = Harness::new();
    text_main(&mut h, "s1", "inline");
    let add = |id: &str, parent: &str, before: Value, node: Value| {
        json!(["add", id, parent, before, node])
    };
    h.frame("s1", 2, json!([
        add("a", "main", Value::Null, json!({"id": "b", "k": "text"})),
        ["add", "a", "main", {"id": "a", "k": "text"}],
        add("a", "main", json!("nope"), json!({"id": "a", "k": "text"})),
        add("a", "main", Value::Null, json!({"id": "a", "k": "col", "c": [{"id": "t", "k": "text"}]})),
        add("a", "main", Value::Null, json!({"id": "a", "k": "col", "c": [{"id": "a1", "k": "sparkle"}]})),
        add("a", "nope", Value::Null, json!({"id": "a", "k": "text"})),
        add("a", "main", Value::Null, json!({"id": "a", "k": "text", "p": 3}))
    ]));
    let out = h.out();
    assert_eq!(out.len(), 7, "{out:?}");
    assert_eq!(h.pane.surfaces()[0].document().len(), 3, "all or nothing");

    // 200 000 nodes, root included.
    let children: Vec<Value> = (0..199_997)
        .map(|i| json!({"id": format!("n{i}"), "k": "spacer"}))
        .collect();
    let body = json!({"sf": "s1", "s": 3, "ops": [add("big", "main", Value::Null, json!({"id": "big", "k": "col", "c": children}))]});
    h.write(&tern_sdk::wire::encode(
        "f",
        &[],
        body.to_string().as_bytes(),
        65536,
        "1",
    ));
    assert_eq!(
        h.out(),
        [json!({"ev": "error", "sf": "s1", "s": 3, "op": 0, "msg": "big: more than 200000 nodes"})]
    );
    let mut children = body["ops"][0][4]["c"].as_array().unwrap().clone();
    children.pop();
    let body = json!({"sf": "s1", "s": 4, "ops": [add("big", "main", Value::Null, json!({"id": "big", "k": "col", "c": children}))]});
    h.write(&tern_sdk::wire::encode(
        "f",
        &[],
        body.to_string().as_bytes(),
        65536,
        "2",
    ));
    assert!(h.out().is_empty());
    assert_eq!(h.pane.surfaces()[0].document().len(), 200_000);
}

#[test]
fn d6_set_merges_and_nulls_drop() {
    let mut h = Harness::new();
    h.open(
        "s1",
        "inline",
        json!([{"id": "a", "k": "text", "p": {"text": "x", "tone": null, "title": "t"}}]),
    );
    assert_eq!(
        h.node("s1", "a").unwrap()["p"],
        json!({"text": "x", "title": "t"})
    );
    h.frame(
        "s1",
        2,
        json!([["set", "a", {"title": null, "hidden": true, "key": "k"}]]),
    );
    assert_eq!(
        h.node("s1", "a").unwrap()["p"],
        json!({"text": "x", "hidden": true, "key": "k"})
    );
}

#[test]
fn d7_text_append_and_replace_on_text_kinds() {
    let mut h = Harness::new();
    let kinds = [
        "text", "md", "code", "ansi", "math", "editor", "input", "shimmer", "el",
    ];
    let config = Config {
        kinds: kinds
            .iter()
            .chain(&["col"])
            .map(|k| (*k).to_owned())
            .collect(),
        ..Config::default()
    };
    let mut h2 = Harness::with(config);
    let nodes: Vec<Value> = kinds.iter().map(|k| json!({"id": k, "k": k})).collect();
    h2.open("s1", "inline", json!(nodes));
    let ops: Vec<Value> = kinds
        .iter()
        .flat_map(|k| {
            [
                json!(["text", k, "append", "ab"]),
                json!(["text", k, "append", "cd"]),
            ]
        })
        .collect();
    h2.frame("s1", 2, json!(ops));
    for k in kinds {
        assert_eq!(h2.node("s1", k).unwrap()["p"]["text"], "abcd", "{k}");
    }
    h2.frame("s1", 3, json!([["text", "md", "replace", "# new"]]));
    assert_eq!(h2.node("s1", "md").unwrap()["p"]["text"], "# new");
    assert!(h2.out().is_empty());
    // The SDK's streaming op (RT:242).
    h.open(
        "s1",
        "inline",
        json!([{"id": "main.t", "k": "text", "p": {"text": "ab"}}]),
    );
    h.frame("s1", 2, json!([["text", "main.t", "append", "cd"]]));
    assert_eq!(h.node("s1", "main.t").unwrap()["p"]["text"], "abcd");
    h.frame("s1", 3, json!([["text", "main", "append", "x"]]));
    assert_eq!(h.out().len(), 1, "col has no primary text");
}

#[test]
fn d8_splice_in_utf16() {
    let mut h = Harness::new();
    h.open(
        "s1",
        "inline",
        json!([{"id": "ed", "k": "editor", "p": {"text": "a😀b"}}]),
    );
    h.frame(
        "s1",
        2,
        json!([
            ["splice", "ed", 3, 1, "c"],
            ["splice", "ed", 2, 0, "x"],
            ["splice", "ed", 4, 1, "x"],
            ["splice", "ed", 0, 1, "Z"]
        ]),
    );
    assert_eq!(h.node("s1", "ed").unwrap()["p"]["text"], "Z😀c");
    let out = h.out();
    assert_eq!(
        out.iter()
            .map(|e| e["op"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [1, 2]
    );
    // omp's composer clears with a splice.
    h.frame("s1", 3, json!([["splice", "ed", 0, 4, ""]]));
    assert_eq!(h.node("s1", "ed").unwrap()["p"]["text"], "");
}

#[test]
fn d9_move_validation() {
    let mut h = Harness::new();
    h.open(
        "s1",
        "inline",
        json!([
            {"id": "a", "k": "col", "c": [{"id": "a1", "k": "text"}]},
            {"id": "b", "k": "text"}
        ]),
    );
    h.frame(
        "s1",
        2,
        json!([
            ["move", "a", "a1", null],
            ["move", "a", "a", null],
            ["move", "b", "main", "b"],
            ["move", "b", "main", "a1"],
            ["move", "s1", "main", null],
            ["del", "s1"],
            ["move", "b", "main", "a"]
        ]),
    );
    assert_eq!(h.out().len(), 6);
    let main = h.node("s1", "main").unwrap();
    let ids: Vec<&str> = main["c"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["b", "a"]);
}

#[test]
fn d10_del_takes_focus_with_it() {
    let mut h = Harness::new();
    h.open(
        "s1",
        "inline",
        json!([{"id": "box", "k": "col", "c": [{"id": "ed", "k": "editor"}]}]),
    );
    h.frame("s1", 2, json!([["focus", "ed"]]));
    assert_eq!(h.pane.surfaces()[0].document().focus(), Some("ed"));
    h.frame("s1", 3, json!([["del", "box"]]));
    let doc = h.pane.surfaces()[0].document();
    assert_eq!(
        (doc.focus(), doc.get("ed").is_none(), doc.len()),
        (None, true, 2)
    );
}

#[test]
fn d11_settle_is_a_hint_cleared_by_later_ops() {
    let mut h = Harness::new();
    h.open("s1", "inline", json!([{"id": "m1", "k": "col", "c": [{"id": "t", "k": "text"}]}, {"id": "m2", "k": "text"}]));
    h.frame("s1", 2, json!([["settle", "m1"], ["settle", "m2"]]));
    assert_eq!(h.pane.surfaces()[0].document().settled(), ["m1", "m2"]);
    h.frame("s1", 3, json!([["text", "t", "append", "x"]]));
    assert_eq!(h.pane.surfaces()[0].document().settled(), ["m2"]);
    // A settled node still takes ops.
    h.frame("s1", 4, json!([["set", "m2", {"tone": "info"}]]));
    assert!(h.pane.surfaces()[0].document().settled().is_empty());
    assert!(h.out().is_empty());
}

#[test]
fn d12_focus_reveal_scroll() {
    let mut h = Harness::new();
    h.open(
        "s1",
        "inline",
        json!([{"id": "ed", "k": "editor"}, {"id": "t", "k": "text"}]),
    );
    h.pane.take_changes();
    h.frame(
        "s1",
        2,
        json!([
            ["focus", "ed"],
            ["reveal", "t", "start"],
            ["reveal", "ed", "middle"],
            ["scroll", "t", "page-down"],
            ["scroll", "t", "line-up"]
        ]),
    );
    let ch = h.pane.take_changes();
    let s = &ch.surfaces[0];
    assert!(s.doc.focus);
    assert_eq!(
        s.reveal.as_ref().map(|r| (r.id.as_str(), r.at)),
        Some(("ed", RevealAt::Nearest)),
        "only the latest"
    );
    assert_eq!(
        s.scrolls.iter().map(|r| r.by).collect::<Vec<_>>(),
        [ScrollBy::PageDown, ScrollBy::LineUp]
    );
    h.frame(
        "s1",
        3,
        json!([
            ["focus", null],
            ["scroll", "t", "sideways"],
            ["focus", "nope"]
        ]),
    );
    assert_eq!(h.pane.surfaces()[0].document().focus(), None);
    assert_eq!(h.out().len(), 2);
    for by in [
        "line-up",
        "line-down",
        "page-up",
        "page-down",
        "start",
        "end",
    ] {
        h.frame("s1", 4, json!([["scroll", "t", by]]));
    }
    assert!(h.out().is_empty());
}

#[test]
fn d13_sdk_reconcile_patterns() {
    use tern_sdk::reconcile::Doc;
    let mut h = Harness::new();
    h.send_json("o", json!({"id": "s1"}));
    let views = [
        json!({"main": {"k": "col", "c": [{"k": "text", "p": {"text": "a", "key": "t"}}, {"k": "text", "p": {"key": "u"}}]}, "dock": {"k": "col", "c": [{"k": "editor", "p": {"text": ""}}]}}),
        json!({"main": {"k": "col", "c": [{"k": "text", "p": {"key": "u", "tone": "info"}}, {"k": "text", "p": {"text": "ab", "key": "t"}}, {"k": "card", "p": {"head": "x"}, "c": [{"k": "text", "p": {"text": "in"}}]}]}}),
        json!({"main": {"k": "row", "c": [{"k": "text", "p": {"text": "abc", "key": "t"}}]}}),
        json!({"main": {"k": "row", "c": [{"k": "text", "p": {"key": "t"}}, {"k": "text", "p": {"key": "v"}}]}, "layer": {"k": "col"}}),
    ];
    let mut prev = Doc::new();
    for (n, view) in views.iter().enumerate() {
        let next = Doc::from_json(view).unwrap();
        let ops = serde_json::to_value(prev.ops(&next, "s1")).unwrap();
        h.frame("s1", n as u64 + 1, ops);
        prev = next;
    }
    assert!(h.out().is_empty());
    let main = h.node("s1", "main").unwrap();
    assert_eq!(main["k"], "row");
    assert_eq!(main["c"].as_array().unwrap().len(), 2);
    assert!(h.pane.surfaces()[0].document().region("layer").is_some());
    assert!(h.pane.surfaces()[0].document().region("dock").is_none());

    // Adopting a surface the program doesn't remember: `["del","main"]` first (alone in Python).
    h.send_json("x", json!({"id": "s1"}));
    h.send_json("o", json!({"id": "s1", "adopt": true}));
    h.frame("s1", 1, json!([["del", "main"]]));
    let fresh =
        Doc::from_json(&json!({"main": {"k": "col", "c": [{"k": "text", "p": {"text": "new"}}]}}))
            .unwrap();
    h.frame(
        "s1",
        2,
        serde_json::to_value(Doc::new().ops(&fresh, "s1")).unwrap(),
    );
    assert!(h.out().is_empty());
    assert_eq!(h.node("s1", "main.0").unwrap()["p"]["text"], "new");
}

// ---------------------------------------------------------------------------------------------
// E. Acks and flow control

#[test]
fn e1_acks_follow_drawing() {
    let mut h = Harness::new();
    text_main(&mut h, "s1", "inline");
    h.frame("s1", 2, json!([]));
    assert!(h.out().is_empty(), "never before drawing");
    h.grid.cols = 100;
    h.pane.ack(&h.grid, "s1", 2);
    assert_eq!(
        h.out(),
        [
            json!({"ev": "resize", "sf": "s1", "cols": 100, "cell": {"w": 8, "h": 16}, "visible": true}),
            json!({"ev": "ack", "sf": "s1", "s": 2})
        ]
    );
    h.pane.ack(&h.grid, "s1", 2);
    assert!(h.out().is_empty(), "at most one per displayed frame");
    h.frame("s1", 3, json!([]));
    h.frame("s1", 4, json!([]));
    h.pane.ack(&h.grid, "s1", 4);
    assert_eq!(
        h.out(),
        [json!({"ev": "ack", "sf": "s1", "s": 4})],
        "one ack covers both"
    );
}

#[test]
fn e3_no_acks_for_listen_false() {
    let mut h = Harness::new();
    h.send_json("o", json!({"id": "ls", "mode": "flow", "listen": false}));
    h.frame("ls", 1, json!([]));
    h.pane.ack(&h.grid, "ls", 1);
    assert!(h.out().is_empty());
}

#[test]
fn e4_no_acks_without_a_drawing_client() {
    let mut h = Harness::new();
    text_main(&mut h, "s1", "inline");
    for s in 2..10 {
        h.frame("s1", s, json!([]));
    }
    assert!(
        h.out().is_empty(),
        "acks come only from the primary's draws"
    );
    h.pane.ack(&h.grid, "s1", 9);
    assert_eq!(
        events(&h.out(), "ack").len(),
        1,
        "an attaching window draws the current document and acks it"
    );
}

#[test]
fn e5_retention_evicts_in_order() {
    let config = Config {
        retention: 4000,
        ..Config::default()
    };
    let mut h = Harness::with(config);
    let big = |id: &str| json!({"id": id, "k": "text", "p": {"text": "x".repeat(700)}});
    h.open("old", "flow", json!([big("o1")]));
    h.send_json("x", json!({"id": "old"}));
    h.open("s1", "flow", json!([big("a"), big("b")]));
    h.open("s2", "inline", json!([big("c")]));
    assert!(h.pane.retention_cost() <= 4000);
    assert_eq!(h.pane.surfaces().len(), 3);
    h.frame(
        "s2",
        2,
        json!([["settle", "c"], ["add", "d", "main", null, big("d")]]),
    );
    // Closed surfaces go first, whole and without a word.
    assert!(h.pane.surfaces().iter().all(|s| s.id() != "old"));
    assert!(h.out().is_empty());
    // Then settled top-level children of `main`, oldest first, with `gone`.
    h.frame(
        "s1",
        2,
        json!([
            ["settle", "b"],
            ["settle", "a"],
            ["add", "e", "main", null, big("e")]
        ]),
    );
    assert_eq!(h.out(), [json!({"ev": "gone", "sf": "s1", "ids": ["a"]})]);
    h.frame("s1", 3, json!([["set", "a", {}]]));
    assert_eq!(h.out()[0]["msg"], "unknown id a");
    // With nothing settled left, the pane stays over budget.
    h.frame(
        "s2",
        3,
        json!([
            ["add", "f", "main", null, big("f")],
            ["add", "g", "main", null, big("g")],
            ["add", "i", "main", null, big("i")]
        ]),
    );
    assert_eq!(
        h.out(),
        [
            json!({"ev": "gone", "sf": "s1", "ids": ["b"]}),
            json!({"ev": "gone", "sf": "s2", "ids": ["c"]})
        ]
    );
    assert!(h.pane.retention_cost() > 4000);
    assert_eq!(
        h.pane
            .surfaces()
            .iter()
            .map(|s| s.document().len())
            .sum::<usize>(),
        2 * 2 + 1 + 4
    );
}

// ---------------------------------------------------------------------------------------------
// F. Events and the PTY input

#[test]
fn f1_messages_are_single_st_strings() {
    let mut h = Harness::new();
    text_main(&mut h, "s1", "inline");
    h.frame("s1", 2, json!([["set", "line\nbreak\u{1b}\u{7}", {}]]));
    let out = h.pane.take_output();
    let bytes = out[0].encode();
    assert!(bytes.starts_with(b"\x1b_tsp;e;{") && bytes.ends_with(b"\x1b\\"));
    let inner = &bytes[2..bytes.len() - 2];
    assert!(
        !inner.iter().any(|&b| b < 0x20),
        "control characters JSON-escaped"
    );
    decode(&out);
}

#[test]
fn f3_originated_events_need_an_open_surface() {
    let mut h = Harness::new();
    h.hello(&[]);
    let dark = |d| Facts {
        dark: d,
        ..Facts::default()
    };
    h.pane.set_facts(&h.grid, dark(false));
    assert!(h.out().is_empty(), "no surface open");
    h.send_json("o", json!({"id": "ls", "mode": "flow", "listen": false}));
    h.pane.set_facts(&h.grid, dark(true));
    assert!(h.out().is_empty(), "only listen:false open");
    text_main(&mut h, "s1", "flow");
    h.pane.set_facts(&h.grid, dark(false));
    assert_eq!(h.out(), [json!({"ev": "theme", "dark": false})]);
    h.send_json("x", json!({"id": "s1"}));
    h.pane.ack(&h.grid, "s1", 1);
    assert_eq!(
        h.pane
            .view_event(&h.grid, "s1", ViewEvent::Focus { id: "t".into() }),
        Err(Refused::NotListening)
    );
    h.pane.set_facts(&h.grid, dark(true));
    assert!(h.out().is_empty(), "about a closed surface, or none open");
}

#[test]
fn f4_answers_stop_while_the_program_is_gone() {
    let mut h = Harness::new();
    text_main(&mut h, "s1", "flow");
    for gone in [TspPane::on_prompt, TspPane::on_exit, TspPane::on_reset]
        as [fn(&mut TspPane, &mut dyn Grid); 3]
    {
        h.send_json("q", json!({"q": "hello", "v": [1]}));
        h.frame("zz", 1, json!([]));
        assert_eq!(h.out().len(), 2);
        gone(&mut h.pane, &mut h.grid);
        h.send_json("x", json!({"id": "zz"}));
        h.frame("s1", 9, json!([["set", "nope", {}]]));
        h.send("t", "{nope");
        h.send_json("o", json!({"id": "x", "adopt": true, "listen": false}));
        h.send("b", "!!");
        assert!(h.out().is_empty());
    }
    // The next `o` reopens answers.
    h.send_json("o", json!({"id": "nope", "adopt": true}));
    assert_eq!(h.out(), [json!({"ev": "gone", "ids": ["nope"]})]);
}

#[test]
fn f5_surface_events_carry_sf() {
    let mut h = Harness::new();
    h.hello(&["edit", "undo", "send"]);
    h.open("s1", "inline", json!([{"id": "ed", "k": "editor", "p": {"sendable": true}}, {"id": "t", "k": "text", "p": {"key": "k"}}]));
    let g = &h.grid;
    h.pane.ack(g, "s1", 1);
    for ev in [
        ViewEvent::Toggle {
            id: "t".into(),
            collapsed: true,
            key: None,
        },
        ViewEvent::Select {
            id: "t".into(),
            item: "t".into(),
            values: None,
        },
        ViewEvent::Activate {
            id: "t".into(),
            item: "t".into(),
            values: None,
        },
        ViewEvent::Action {
            id: "t".into(),
            action: "x".into(),
            mods: Mods::NONE,
            values: None,
        },
        ViewEvent::Change {
            id: "t".into(),
            value: json!("on"),
            checked: true,
            name: None,
            values: None,
        },
        ViewEvent::PrefsChange {
            id: "t".into(),
            item: "i".into(),
            value: json!(1),
        },
        ViewEvent::Focus { id: "ed".into() },
        ViewEvent::Edit {
            id: "ed".into(),
            from: 0,
            to: 0,
            text: "a".into(),
            cursor: 1,
            len: 0,
        },
        ViewEvent::Undo { id: "ed".into() },
        ViewEvent::Send {
            id: "ed".into(),
            text: "go".into(),
        },
    ] {
        assert_eq!(h.pane.view_event(g, "s1", ev), Ok(Delivery::Sent));
    }
    h.pane.set_facts(
        g,
        Facts {
            dark: false,
            reduce_motion: true,
            visible: false,
            ..Facts::default()
        },
    );
    h.frame("s1", 2, json!([["set", "nope", {}]]));
    h.send_json("o", json!({"id": "gone", "adopt": true}));
    let out = h.out();
    for e in &out {
        let unsf =
            matches!(e["ev"].as_str(), Some("theme" | "motion")) || e["ids"] == json!(["gone"]);
        assert_eq!(e.get("sf").is_none(), unsf, "{e}");
    }
    assert_eq!(out.len(), 17);
}

#[test]
fn f6_resize_reports_width_cell_and_visibility() {
    let mut h = Harness::new();
    text_main(&mut h, "s1", "inline");
    h.pane.ack(&h.grid, "s1", 1);
    h.out();
    h.grid.cols = 132;
    h.pane.on_resize(&h.grid);
    h.pane.on_resize(&h.grid);
    h.pane.set_facts(
        &h.grid,
        Facts {
            cell: (9.0, 18.5),
            ..Facts::default()
        },
    );
    let out = h.out();
    assert_eq!(
        out,
        [
            json!({"ev": "resize", "sf": "s1", "cols": 132, "cell": {"w": 8, "h": 16}, "visible": true}),
            json!({"ev": "resize", "sf": "s1", "cols": 132, "cell": {"w": 9, "h": 18.5}, "visible": true}),
        ]
    );
    // A screen surface on top takes them instead.
    text_main(&mut h, "c", "screen");
    h.pane.ack(&h.grid, "c", 1);
    h.out();
    h.grid.cols = 90;
    h.pane.on_resize(&h.grid);
    assert_eq!(
        h.out()
            .iter()
            .map(|e| e["sf"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        ["c"]
    );
}

#[test]
fn f7_theme_motion_visible() {
    let mut h = Harness::new();
    text_main(&mut h, "s1", "flow");
    h.pane.set_facts(
        &h.grid,
        Facts {
            dark: false,
            reduce_motion: true,
            visible: false,
            ..Facts::default()
        },
    );
    assert_eq!(
        h.out(),
        [
            json!({"ev": "theme", "dark": false}),
            json!({"ev": "motion", "reduce": true}),
            json!({"ev": "visible", "sf": "s1", "visible": false})
        ]
    );
    let config = Config {
        features: vec![],
        ..Config::default()
    };
    let mut h = Harness::with(config);
    h.send_json("o", json!({"id": "s1"}));
    h.pane.set_facts(
        &h.grid,
        Facts {
            reduce_motion: true,
            ..Facts::default()
        },
    );
    assert!(h.out().is_empty(), "motion needs the reduce-motion feature");
}

#[test]
fn f8_pointer_actions_map_to_events() {
    let mut h = Harness::new();
    h.open(
        "s1",
        "inline",
        json!([
            {"id": "card", "k": "card", "p": {"key": "k1", "collapsible": true}},
            {"id": "list", "k": "list", "c": [{"id": "it", "k": "item"}]}
        ]),
    );
    let g = &h.grid;
    h.pane
        .view_event(
            g,
            "s1",
            ViewEvent::Toggle {
                id: "card".into(),
                collapsed: true,
                key: None,
            },
        )
        .unwrap();
    h.pane
        .view_event(
            g,
            "s1",
            ViewEvent::Select {
                id: "list".into(),
                item: "it".into(),
                values: None,
            },
        )
        .unwrap();
    h.pane
        .view_event(
            g,
            "s1",
            ViewEvent::Activate {
                id: "list".into(),
                item: "it".into(),
                values: None,
            },
        )
        .unwrap();
    let mods = Mods {
        ctrl: true,
        shift: true,
        ..Mods::NONE
    };
    h.pane
        .view_event(
            g,
            "s1",
            ViewEvent::Action {
                id: "card".into(),
                action: "open-file=src/a.rs".into(),
                mods,
                values: None,
            },
        )
        .unwrap();
    h.pane
        .view_event(
            g,
            "s1",
            ViewEvent::Action {
                id: "card".into(),
                action: "rerun".into(),
                mods: Mods::NONE,
                values: None,
            },
        )
        .unwrap();
    let out = h.pane.take_output();
    let back = decode(&out);
    assert!(
        matches!(&back[0], Back::Event(Event::Toggle(t)) if t.collapsed && t.key == Some(json!("k1")))
    );
    assert!(matches!(&back[1], Back::Event(Event::Select(s)) if s.id == "list" && s.item == "it"));
    assert!(matches!(&back[2], Back::Event(Event::Activate(s)) if s.item == "it"));
    assert!(
        matches!(&back[3], Back::Event(Event::Action(a)) if a.act == "open-file" && a.value.as_deref() == Some("src/a.rs") && a.mods == ["shift", "ctrl"])
    );
    assert!(
        matches!(&back[4], Back::Event(Event::Action(a)) if a.act == "rerun" && a.value.is_none())
    );
    assert!(
        out[4].body.get("mods").is_none(),
        "mods absent when none held"
    );
    assert_eq!(
        h.pane
            .view_event(g, "s1", ViewEvent::Focus { id: "nope".into() }),
        Err(Refused::UnknownNode)
    );
}

#[test]
fn f9_form_values() {
    let mut h = Harness::new();
    // The book's canonical form (docs-ask), as recorded.
    let recs =
        auk_conformance::load(&auk_conformance::fixtures_dir().join("docs-ask.jsonl")).unwrap();
    for r in recs.iter().filter(|r| {
        r.dir == auk_conformance::Dir::Out && ["o", "s", "f"].contains(&r.verb.as_str())
    }) {
        h.send(&r.verb, &r.body.to_string());
    }
    let s = &h.pane.surfaces()[0];
    let doc = s.document();
    let button = recs
        .iter()
        .find(|r| r.dir == auk_conformance::Dir::In && r.body["ev"] == "action")
        .map(|r| r.body.clone())
        .unwrap();
    let flips = recs
        .iter()
        .filter(|r| r.dir == auk_conformance::Dir::In && r.body["ev"] == "change")
        .map(|r| r.body.clone())
        .collect::<Vec<_>>();
    let flipped: Vec<(String, bool)> = flips
        .iter()
        .map(|c| {
            (
                c["id"].as_str().unwrap().to_owned(),
                c["checked"].as_bool().unwrap(),
            )
        })
        .collect();
    let values = doc
        .form_values(button["id"].as_str().unwrap(), &|id| {
            flipped.iter().find(|(f, _)| f == id).map(|(_, c)| *c)
        })
        .unwrap();
    // Tern's own answer in the recording.
    assert_eq!(Value::Object(values.clone()), button["values"]);
    let ev = ViewEvent::Action {
        id: button["id"].as_str().unwrap().into(),
        action: button["act"].as_str().unwrap().into(),
        mods: Mods::NONE,
        values: Some(values),
    };
    h.pane
        .view_event(&h.grid, s.id().to_owned().as_str(), ev)
        .unwrap();
    let mut sent = h.out().pop().unwrap();
    obj(&sent);
    sent.as_object_mut().unwrap().remove("sf");
    let mut want = button.clone();
    want.as_object_mut().unwrap().remove("sf");
    assert_eq!(sent, want);
}

#[test]
fn f10_keys_are_never_events() {
    // Every event the engine can emit is one of the 16; only edit/undo/send stand in for keys,
    // each behind its feature (B10). There is no key event to send.
    let mut h = Harness::new();
    h.hello(&["edit", "undo", "send"]);
    h.open(
        "s1",
        "inline",
        json!([{"id": "ed", "k": "editor", "p": {"sendable": true}}]),
    );
    h.pane.ack(&h.grid, "s1", 1);
    h.pane
        .view_event(
            &h.grid,
            "s1",
            ViewEvent::Edit {
                id: "ed".into(),
                from: 0,
                to: 0,
                text: "k".into(),
                cursor: 1,
                len: 0,
            },
        )
        .unwrap();
    h.pane.set_facts(
        &h.grid,
        Facts {
            dark: false,
            ..Facts::default()
        },
    );
    for e in h.out() {
        assert!(auk_conformance::EVENTS.contains(&e["ev"].as_str().unwrap()));
    }
    // A field with a Vim mode or readonly keeps its keys.
    h.frame("s1", 2, json!([["set", "ed", {"mode": "normal"}]]));
    let edit = ViewEvent::Edit {
        id: "ed".into(),
        from: 0,
        to: 0,
        text: "k".into(),
        cursor: 1,
        len: 0,
    };
    assert_eq!(
        h.pane.view_event(&h.grid, "s1", edit.clone()),
        Err(Refused::Field)
    );
    h.frame(
        "s1",
        3,
        json!([["set", "ed", {"mode": null, "readonly": true}]]),
    );
    assert_eq!(
        h.pane
            .view_event(&h.grid, "s1", ViewEvent::Undo { id: "ed".into() }),
        Err(Refused::Field)
    );
}

#[test]
fn f11_edit_fields() {
    let mut h = Harness::new();
    h.hello(&["edit"]);
    h.open(
        "s1",
        "inline",
        json!([{"id": "ed", "k": "input", "p": {"text": "a😀"}}]),
    );
    let ev = ViewEvent::Edit {
        id: "ed".into(),
        from: 1,
        to: 3,
        text: "b".into(),
        cursor: 2,
        len: 3,
    };
    h.pane.view_event(&h.grid, "s1", ev).unwrap();
    let out = h.pane.take_output();
    match &decode(&out)[..] {
        [Back::Event(Event::Edit(e))] => {
            assert_eq!(
                (
                    e.id.as_str(),
                    e.from,
                    e.to,
                    e.text.as_str(),
                    e.cursor,
                    e.len
                ),
                ("ed", 1, 3, "b", 2, 3)
            );
            assert_eq!(e.sf.as_deref(), Some("s1"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn f12_send_goes_only_to_a_sendable_field() {
    let mut h = Harness::new();
    h.hello(&["send"]);
    h.open(
        "s1",
        "inline",
        json!([{"id": "ed", "k": "editor", "p": {"sendable": false}}, {"id": "t", "k": "text"}]),
    );
    let send = |text: &str| ViewEvent::Send {
        id: "ed".into(),
        text: text.into(),
    };
    assert_eq!(
        h.pane.view_event(
            &h.grid,
            "s1",
            ViewEvent::Send {
                id: "t".into(),
                text: "x".into()
            }
        ),
        Err(Refused::Field)
    );
    assert_eq!(
        h.pane.view_event(&h.grid, "s1", send("held")),
        Ok(Delivery::Held)
    );
    assert!(h.out().is_empty());
    h.frame(
        "s1",
        2,
        json!([["set", "ed", {"sendable": true, "readonly": true}]]),
    );
    assert!(h.out().is_empty(), "readonly isn't editable");
    h.frame("s1", 3, json!([["set", "ed", {"readonly": null}]]));
    assert_eq!(
        h.out(),
        [json!({"ev": "send", "sf": "s1", "id": "ed", "text": "held"})]
    );
    assert_eq!(
        h.pane.view_event(&h.grid, "s1", send("now")),
        Ok(Delivery::Sent)
    );
    h.out();
    // Held prompts go with their field.
    h.frame("s1", 4, json!([["set", "ed", {"hidden": true}]]));
    assert_eq!(
        h.pane.view_event(&h.grid, "s1", send("lost")),
        Ok(Delivery::Held)
    );
    h.frame("s1", 5, json!([["del", "ed"]]));
    h.frame(
        "s1",
        6,
        json!([["add", "ed", "main", null, {"id": "ed", "k": "editor", "p": {"sendable": true}}]]),
    );
    assert!(h.out().is_empty());
}

#[test]
fn f13_focus_event() {
    let mut h = Harness::new();
    h.open("s1", "inline", json!([{"id": "ed", "k": "editor"}]));
    h.pane
        .view_event(&h.grid, "s1", ViewEvent::Focus { id: "ed".into() })
        .unwrap();
    assert_eq!(h.out(), [json!({"ev": "focus", "sf": "s1", "id": "ed"})]);
}

#[test]
fn f14_nothing_is_written_when_nothing_listens() {
    let mut h = Harness::new();
    h.hello(&["edit", "undo", "send"]);
    h.open(
        "s1",
        "inline",
        json!([{"id": "ed", "k": "editor", "p": {"sendable": true}}]),
    );
    h.pane.on_exit(&mut h.grid);
    h.out();
    h.pane.ack(&h.grid, "s1", 1);
    h.pane.set_facts(
        &h.grid,
        Facts {
            dark: false,
            reduce_motion: true,
            visible: false,
            cell: (1.0, 1.0),
            hour12: true,
        },
    );
    h.grid.cols = 10;
    h.pane.on_resize(&h.grid);
    assert!(
        h.pane
            .view_event(&h.grid, "s1", ViewEvent::Undo { id: "ed".into() })
            .is_err()
    );
    h.pane.on_anchor_gone(&mut h.grid, AnchorKey(1));
    h.pane.expire_chunks();
    h.send("f", "junk");
    assert!(h.out().is_empty());
}

// ---------------------------------------------------------------------------------------------
// G. Palette, stylesheets and blobs

#[test]
fn g1_palette() {
    let mut h = Harness::new();
    h.send_json("t", json!({"dark": {"accent": "#fff"}}));
    assert_eq!(h.out(), [json!({"ev": "error", "msg": "no live surface"})]);
    h.send_json("o", json!({"id": "s1"}));
    h.send_json("t", json!({"dark": {"accent": "#ABC", "muted": "#9ca3b0cc", "bad": "red"}, "light": {"accent": "#000000"}}));
    let p = h.pane.surfaces()[0].palette().unwrap().clone();
    assert_eq!(
        p.dark,
        [
            ("accent".to_owned(), "#aabbcc".to_owned()),
            ("muted".to_owned(), "#9ca3b0".to_owned())
        ]
    );
    // It replaces the palette whole.
    h.send_json("t", json!({"sf": "s1", "light": {"accent": "#111"}}));
    let p = h.pane.surfaces()[0].palette().unwrap().clone();
    assert!(p.dark.is_empty() && p.light.len() == 1);
    h.send_json("t", json!({"sf": "zz", "dark": {}}));
    assert_eq!(
        h.out(),
        [json!({"ev": "error", "sf": "zz", "msg": "unknown surface zz"})]
    );
    // It stays after close.
    h.send_json("x", json!({"id": "s1"}));
    assert!(h.pane.surfaces()[0].palette().is_some());
}

#[derive(Debug)]
struct NoUrls;

impl SheetChecker for NoUrls {
    fn check(&self, css: &str) -> Vec<String> {
        css.match_indices("url(")
            .map(|(at, _)| format!("1:{}: url() is dropped", at + 1))
            .collect()
    }
}

#[test]
fn g2_stylesheets() {
    let mut h = Harness {
        pane: TspPane::with(
            Config::default(),
            Box::new(NoUrls),
            Box::new(auk_tsp::MemoryBlobs::default()),
        ),
        grid: TestGrid::new(),
    };
    h.send_json("o", json!({"id": "s1"}));
    h.send_json("s", json!({"name": "base", "css": "a{}"}));
    h.send_json("s", json!({"sf": "s1", "name": "more", "css": "b{}"}));
    h.send_json(
        "s",
        json!({"sf": "s1", "name": "base", "css": "c{background:url(x)}"}),
    );
    let names: Vec<_> = h.pane.surfaces()[0]
        .sheets()
        .iter()
        .map(|(n, _)| n.to_owned())
        .collect();
    assert_eq!(names, ["base", "more"], "a replacement keeps its place");
    assert_eq!(
        h.out(),
        [json!({"ev": "error", "sf": "s1", "sheet": "base", "msg": "1:14: url() is dropped"})]
    );
    h.send_json("s", json!({"name": "bad name", "css": "x{}"}));
    h.send_json("s", json!({"name": "x".repeat(65), "css": "x{}"}));
    h.send_json("s", json!({"name": "n", "css": 5}));
    let sheet = |css: String| json!({"name": "huge", "css": css}).to_string();
    h.write(&tern_sdk::wire::encode(
        "s",
        &[],
        sheet("x".repeat(256 * 1024 - 22)).as_bytes(),
        65536,
        "1",
    ));
    let out = h.out();
    assert_eq!(out.len(), 4);
    assert!(
        out.iter()
            .all(|e| e["sf"] == "s1" && e.get("sheet").is_some())
    );
    assert_eq!(out[3]["msg"], "over 262144 bytes of CSS");
    h.write(&tern_sdk::wire::encode(
        "s",
        &[],
        sheet("x".repeat(256 * 1024 - 23)).as_bytes(),
        65536,
        "2",
    ));
    assert!(h.out().is_empty(), "exactly 256 KiB fits");
    h.send_json("x", json!({"id": "s1"}));
    assert_eq!(
        h.pane.surfaces()[0].sheets().iter().count(),
        3,
        "sheets stay after close"
    );
}

#[test]
fn g3_blobs_are_shared_and_late_ones_redraw() {
    let mut h = Harness::new();
    let id = blob_id(b"png");
    h.open(
        "s1",
        "inline",
        json!([{"id": "img", "k": "image", "p": {"blob": id.to_uppercase()}}]),
    );
    h.open("s2", "flow", json!([]));
    h.pane.take_changes();
    // A window asks the daemon for what its documents name and it lacks.
    assert_eq!(h.pane.missing_blobs(), std::slice::from_ref(&id));
    h.send("b", "cG5n");
    let changes = h.pane.take_changes();
    assert_eq!(changes.blobs, std::slice::from_ref(&id));
    assert!(h.pane.blobs().has(&id));
    h.send_params("b", &[("mime", "image/png")], "cG5n");
    assert_eq!(
        h.pane.take_changes().blobs,
        std::slice::from_ref(&id),
        "content-addressed: the same blob again"
    );
    assert!(h.pane.missing_blobs().is_empty());
    // A late window gets blobs outside the stream: checked against their id, then redrawn.
    let mut late = Harness::new();
    late.open(
        "s1",
        "inline",
        json!([{"id": "img", "k": "image", "p": {"blob": id}}]),
    );
    late.pane.take_changes();
    assert!(!late.pane.deliver_blob(&id, None, b"not png".to_vec()));
    assert!(late.pane.take_changes().blobs.is_empty());
    assert!(
        late.pane
            .deliver_blob(&id, Some("image/png"), b"png".to_vec())
    );
    assert_eq!(late.pane.take_changes().blobs, [id]);
    assert!(late.pane.missing_blobs().is_empty());
}

// ---------------------------------------------------------------------------------------------
// H. Persistence

/// A pane with every kind of state a snapshot must carry.
fn rich() -> Harness {
    let mut h = Harness::new();
    h.send_json(
        "q",
        json!({"q": "hello", "v": [1], "app": "omp", "features": ["edit", "send"]}),
    );
    text_main(&mut h, "old", "inline");
    h.send_json(
        "t",
        json!({"sf": "old", "dark": {"accent": "#123456"}, "name": {"dark": "n"}}),
    );
    h.send_json("x", json!({"id": "old"}));
    h.send_json(
        "o",
        json!({"id": "fl", "mode": "flow", "listen": false, "title": "ls"}),
    );
    h.send_json(
        "s",
        json!({"sf": "fl", "name": "main", "css": ".size{text-align:right}"}),
    );
    h.frame("fl", 1, json!([["add", "main", "fl", null, {"id": "main", "k": "col", "c": [{"id": "e", "k": "elapsed", "p": {"age": 1500}}]}]]));
    h.send_json("x", json!({"id": "fl"}));
    h.send_json(
        "o",
        json!({"id": "s:1", "mode": "inline", "title": "omp", "role": "omp.session"}),
    );
    h.send_json(
        "t",
        json!({"dark": {"accent": "#00b4ff"}, "light": {"accent": "#0060a0"}}),
    );
    h.frame("s:1", 7, json!([
        ["add", "main", "s:1", null, {"id": "main", "k": "col", "c": [
            {"id": "m1", "k": "md", "p": {"text": "# hi"}},
            {"id": "m2", "k": "tool", "p": {"age": 20, "name": "bash"}, "c": [{"id": "m2.o", "k": "ansi", "p": {"text": "ok"}}]}
        ]}],
        ["add", "dock", "s:1", null, {"id": "dock", "k": "col", "p": {"role": "composer"}, "c": [{"id": "ed", "k": "editor", "p": {"text": "x😀"}}]}],
        ["add", "layer", "s:1", null, {"id": "layer", "k": "col"}],
        ["settle", "m1"], ["settle", "m2.o"], ["focus", "ed"], ["suspend"]
    ]));
    h.grid.alt = true;
    h.open("pager", "screen", json!([{"id": "p", "k": "text"}]));
    h.send_params(
        "f",
        &[("c", "9"), ("m", "1")],
        r#"{"sf":"pager","s":2,"ops":[["text","p","append","é"#,
    );
    h.out();
    h
}

#[test]
fn h1_snapshot_replays_to_an_equal_state() {
    let h = rich();
    let snap = h.pane.snapshot();
    let msgs: Vec<auk_tsp::Message> = common::apc_payloads(&snap)
        .iter()
        .map(|p| auk_tsp::wire::parse(p).unwrap())
        .collect();
    let order: Vec<String> = msgs
        .iter()
        .map(|m| {
            let body: Value = serde_json::from_slice(&m.body).unwrap_or(Value::Null);
            format!(
                "{}:{}",
                m.verb,
                body.get("id")
                    .or(body.get("sf"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
            )
        })
        .collect();
    assert_eq!(
        order,
        [
            "q:", "o:old", "t:old", "f:old", "x:old", "o:fl", "s:fl", "f:fl", "x:fl", "o:s:1",
            "t:s:1", "f:s:1", "o:pager", "f:pager", "f:"
        ]
    );
    let hello: Value = serde_json::from_slice(&msgs[0].body).unwrap();
    assert_eq!(
        (hello["app"].as_str(), &hello["features"]),
        (Some("omp"), &json!(["edit", "send"]))
    );
    assert_eq!(msgs[1].param("seeded"), Some("1"));
    assert_eq!(msgs[1].param("buf"), Some("main"));
    assert_eq!(msgs[12].param("buf"), Some("alt"));
    assert_eq!(msgs[1].param("key"), Some("1"));
    let frame: Value = serde_json::from_slice(&msgs[11].body).unwrap();
    assert_eq!(frame["s"], 7);
    let tail: Vec<&Value> = frame["ops"].as_array().unwrap()[3..].iter().collect();
    assert_eq!(
        tail,
        [
            &json!(["settle", "m1"]),
            &json!(["settle", "m2.o"]),
            &json!(["focus", "ed"]),
            &json!(["suspend"])
        ]
    );

    let mut fresh = h.replay();
    assert_eq!(fresh.pane.state_hash(), h.pane.state_hash());
    assert!(
        fresh.pane.take_output().is_empty(),
        "no answer to the replay"
    );
    // And a replay of the replay.
    assert_eq!(fresh.replay().pane.state_hash(), h.pane.state_hash());
    // The rebuilt replica keeps joining the pending chunk and goes on like the original.
    let mut a = h;
    let mut b = fresh;
    for x in [&mut a, &mut b] {
        x.send_params("f", &[("c", "9")], r#""]]}"#);
        x.pane.on_prompt(&mut x.grid);
        x.send_json("o", json!({"id": "next", "mode": "flow"}));
    }
    assert_eq!(a.node("pager", "p"), b.node("pager", "p"));
    assert_eq!(a.pane.state_hash(), b.pane.state_hash());
    assert_eq!(
        a.pane.surfaces().last().unwrap().anchor(),
        b.pane.surfaces().last().unwrap().anchor()
    );
}

#[test]
fn h1_snapshot_keeps_pane_order() {
    // A flow surface opened before an inline one that has since closed: the snapshot replays
    // the closed one first, and `at` puts them back in pane order, which decides what closes
    // and is adopted later.
    let mut h = Harness::new();
    h.hello(&["adopt", "flow"]);
    h.open("fl", "flow", json!([]));
    h.open("a", "inline", json!([]));
    h.open("b", "inline", json!([]));
    h.out();
    let ids = |h: &Harness| -> Vec<(String, bool)> {
        h.pane
            .surfaces()
            .iter()
            .map(|s| (s.id().to_owned(), s.is_live()))
            .collect()
    };
    let snap = h.pane.snapshot();
    let ats: Vec<Option<String>> = common::apc_payloads(&snap)
        .iter()
        .map(|p| auk_tsp::wire::parse(p).unwrap())
        .filter(|m| m.verb == "o")
        .map(|m| m.param("at").map(str::to_owned))
        .collect();
    assert_eq!(
        ats,
        [Some("1".into()), Some("0".into()), Some("2".into())],
        "closed first, each with its place"
    );
    let mut fresh = h.replay();
    assert_eq!(ids(&fresh), ids(&h));
    assert_eq!(fresh.pane.state_hash(), h.pane.state_hash());
    for x in [&mut h, &mut fresh] {
        x.send_json("x", json!({"id": "fl", "keep": true}));
        x.send_json("o", json!({"id": "a", "adopt": true}));
    }
    assert_eq!(ids(&fresh), ids(&h));
    assert_eq!(fresh.pane.state_hash(), h.pane.state_hash());
}

#[test]
fn h1_snapshot_ages_are_rewritten_without_changing_the_state() {
    let h = rich();
    let snap = h.pane.snapshot_with(&mut |_, node, props| {
        if let Some(age) = props.get_mut("age") {
            *age = json!(age.as_i64().unwrap() + 60_000);
            assert!(["elapsed", "tool"].contains(&node.kind()));
        }
    });
    assert!(String::from_utf8_lossy(&snap).contains("\"age\":61500"));
    let mut fresh = Harness::with(h.pane.config().clone());
    fresh.grid.anchors = h.grid.anchors.clone();
    fresh.grid.alt = true;
    fresh.pane.begin_replay();
    fresh.write(&snap);
    fresh.pane.end_replay();
    assert_eq!(fresh.pane.state_hash(), h.pane.state_hash());
}

#[test]
fn h2_restore_after_a_crash() {
    let h = rich();
    let mut restored = h.replay();
    restored.pane.on_exit(&mut restored.grid);
    let states: Vec<_> = restored
        .pane
        .surfaces()
        .iter()
        .map(|s| (s.id(), s.mode(), s.state()))
        .collect();
    assert_eq!(
        states,
        [
            ("old", Mode::Inline, State::Closed),
            ("fl", Mode::Flow, State::Closed),
            ("s:1", Mode::Inline, State::Closed)
        ]
    );
    assert!(restored.pane.surfaces().iter().all(|s| !s.interactive()));
}

#[test]
fn a_modal_or_full_overlay_or_a_picker_in_the_layer_covers_the_surface() {
    // `picker` is T2 (advertised once drawn, D22): accept it here.
    let mut config = Config::default();
    config.kinds.push("picker".into());
    let mut h = Harness::with(config);
    text_main(&mut h, "s1", "inline");
    let key = h.pane.surfaces()[0].key();
    let covered = |h: &Harness| h.pane.covered(key);
    h.frame(
        "s1",
        2,
        json!([["add", "layer", "s1", null, {"id": "layer", "k": "col"}]]),
    );
    assert!(!covered(&h), "an empty layer covers nothing");
    // A plain popup doesn't cover; a modal one does, inside the layer's column.
    h.frame("s1", 3, json!([["add", "ov", "layer", null, {"id": "ov", "k": "overlay", "p": {"anchor": "bottom"}}]]));
    assert!(!covered(&h));
    h.frame("s1", 4, json!([["set", "ov", {"modal": true}]]));
    assert!(covered(&h));
    h.frame(
        "s1",
        5,
        json!([["set", "ov", {"modal": false, "size": "full"}]]),
    );
    assert!(covered(&h), "a full sheet covers");
    h.frame("s1", 6, json!([["del", "ov"]]));
    assert!(!covered(&h), "uncovered when the sheet goes");
    h.frame("s1", 7, json!([["add", "pk", "layer", null, {"id": "pk", "k": "col", "c": [{"id": "p", "k": "picker"}]}]]));
    assert!(covered(&h), "a picker covers");
    assert!(h.out().is_empty(), "{:?}", h.out());
}
