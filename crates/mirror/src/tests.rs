use std::collections::BTreeMap;
use std::io::Cursor;

use auk_replica::{Record, Settings, Snapshot};
use serde_json::json;

use super::*;
use crate::layout::{Layout, SplitDir};

fn client_messages() -> Vec<ClientMsg> {
    vec![
        ClientMsg::Hello(Hello::new(ClientKind::Headless, ClientFacts::default())),
        ClientMsg::Attach {
            window_key: "main".into(),
        },
        ClientMsg::Input {
            pane: 7,
            bytes: b"ls\r\x00\xff".to_vec(),
        },
        ClientMsg::ViewEvent {
            pane: 7,
            sf: "s1".into(),
            event: json!({"ev": "toggle"}),
        },
        ClientMsg::Ack {
            pane: 7,
            sf: "s1".into(),
            s: 12,
        },
        ClientMsg::Resize {
            pane: 7,
            cols: 120,
            rows: 40,
        },
        ClientMsg::Focus { pane: 7 },
        ClientMsg::Facts(ClientFacts {
            dark: true,
            ..ClientFacts::default()
        }),
        ClientMsg::Env {
            vars: BTreeMap::from([("LANG".into(), "C".into())]),
        },
        ClientMsg::LayoutEdit(LayoutEdit::Split {
            pane: 7,
            dir: SplitDir::Right,
        }),
        ClientMsg::Detach,
    ]
}

fn daemon_messages() -> Vec<DaemonMsg> {
    let mut layout = Layout::default();
    layout.new_session("main", None, 7);
    vec![
        DaemonMsg::Welcome {
            versions: Versions::current(),
            catalog: serde_json::Value::Null,
            sessions: layout.clone(),
            client: 3,
        },
        DaemonMsg::Refused(Refusal {
            reason: "old".into(),
            daemon: Versions::current(),
        }),
        DaemonMsg::Snapshot {
            pane: 7,
            snapshot: Snapshot {
                seq: 41,
                cols: 80,
                rows: 24,
                settings: Settings::default(),
                exit: None,
                bytes: b"\x1b[2J\x1b[Hhello".to_vec(),
                lens: Default::default(),
            },
        },
        DaemonMsg::Output {
            pane: 7,
            seq: 42,
            bytes: b"\x1b[31mred\x1b[0m".to_vec(),
        },
        DaemonMsg::Record {
            pane: 7,
            seq: 43,
            record: Record::Resize {
                cols: 100,
                rows: 30,
            },
        },
        DaemonMsg::Record {
            pane: 7,
            seq: 44,
            record: Record::Exit { status: 130 },
        },
        DaemonMsg::LensView {
            pane: 7,
            block: "b1".into(),
            node: json!({"k": "text"}),
        },
        DaemonMsg::Effect(Effect::Toast {
            level: "info".into(),
            text: "hi".into(),
            sub: None,
        }),
        DaemonMsg::Layout {
            window_key: "main".into(),
            window: layout.window("main").unwrap().clone(),
        },
        DaemonMsg::Catalog {
            catalog: json!({"plugins": []}),
        },
        DaemonMsg::Presence {
            clients: vec![ClientInfo {
                id: 3,
                kind: ClientKind::App,
                window_key: None,
            }],
        },
        DaemonMsg::PaneExit {
            pane: 7,
            status: 130,
        },
        DaemonMsg::Primary { pane: 7, you: true },
    ]
}

#[test]
fn every_message_round_trips_through_the_blocking_codec() {
    let mut wire = Vec::new();
    for msg in client_messages() {
        write_msg(&mut wire, &msg).unwrap();
    }
    let mut r = Cursor::new(wire);
    let mut buf = Vec::new();
    for msg in client_messages() {
        assert_eq!(read_msg::<ClientMsg>(&mut r, &mut buf).unwrap(), Some(msg));
    }
    assert!(read_msg::<ClientMsg>(&mut r, &mut buf).unwrap().is_none());

    let mut wire = Vec::new();
    for msg in daemon_messages() {
        write_msg(&mut wire, &msg).unwrap();
    }
    let mut r = Cursor::new(wire);
    for msg in daemon_messages() {
        assert_eq!(read_msg::<DaemonMsg>(&mut r, &mut buf).unwrap(), Some(msg));
    }
    assert!(read_msg::<DaemonMsg>(&mut r, &mut buf).unwrap().is_none());
}

#[test]
fn frames_have_the_documented_layout() {
    let bytes = DaemonMsg::Output {
        pane: 1,
        seq: 2,
        bytes: b"ab".to_vec(),
    }
    .to_bytes();
    let mut expected = 18u32.to_le_bytes().to_vec();
    expected.push(frame::kind::OUTPUT);
    expected.extend_from_slice(&1u64.to_le_bytes());
    expected.extend_from_slice(&2u64.to_le_bytes());
    expected.extend_from_slice(b"ab");
    assert_eq!(bytes, expected);

    let control = ClientMsg::Detach.to_bytes();
    assert_eq!(control[4], frame::kind::CONTROL);
    assert_eq!(&control[5..], br#"{"type":"detach"}"#);
}

#[test]
fn partial_and_oversized_frames() {
    let bytes = ClientMsg::Focus { pane: 1 }.to_bytes();
    for cut in 0..bytes.len() {
        assert!(frame::split_frame(&bytes[..cut]).unwrap().is_none());
    }
    let (f, used) = frame::split_frame(&bytes).unwrap().unwrap();
    assert_eq!(used, bytes.len());
    assert_eq!(ClientMsg::decode(f).unwrap(), ClientMsg::Focus { pane: 1 });

    let mut huge = ((MAX_PAYLOAD + 1) as u32).to_le_bytes().to_vec();
    huge.push(frame::kind::CONTROL);
    assert!(matches!(
        frame::split_frame(&huge),
        Err(WireError::TooLarge(_))
    ));
    let mut r = Cursor::new(huge);
    assert!(matches!(
        read_msg::<ClientMsg>(&mut r, &mut Vec::new()),
        Err(WireError::TooLarge(_))
    ));
}

#[test]
fn malformed_frames_are_errors_not_panics() {
    for (kind, payload) in [
        (frame::kind::INPUT, &b"\x01\x02"[..]),
        (frame::kind::CONTROL, b"{nope"),
        (frame::kind::CONTROL, br#"{"type":"bogus"}"#),
        (0x7f, b""),
    ] {
        assert!(
            ClientMsg::decode(Frame { kind, payload }).is_err(),
            "{kind} {payload:?}"
        );
    }
    for (kind, payload) in [
        (frame::kind::OUTPUT, &b"\x01\x02"[..]),
        (frame::kind::SNAPSHOT, b"\xff\x00\x00\x00{}"),
        (frame::kind::SNAPSHOT, b"\x02\x00\x00\x00{}"),
    ] {
        assert!(
            DaemonMsg::decode(Frame { kind, payload }).is_err(),
            "{kind} {payload:?}"
        );
    }
    // A truncated stream mid-frame is an error, not a clean end.
    let bytes = ClientMsg::Detach.to_bytes();
    let mut r = Cursor::new(&bytes[..bytes.len() - 1]);
    assert!(read_msg::<ClientMsg>(&mut r, &mut Vec::new()).is_err());
}

#[test]
fn mismatched_versions_are_refused() {
    let daemon = Versions::current();
    assert!(daemon.admit(&Versions::current()).is_ok());
    let cases = [
        Versions {
            protocol: PROTOCOL_VERSION + 1,
            ..Versions::current()
        },
        Versions {
            parser: "auk-replica/0.0.0+r0".into(),
            ..Versions::current()
        },
        Versions {
            auk: "9.9.9".into(),
            ..Versions::current()
        },
    ];
    for client in cases {
        let refusal = daemon.admit(&client).unwrap_err();
        assert_eq!(refusal.daemon, daemon);
        assert!(refusal.reason.contains("differs"), "{}", refusal.reason);
    }
}

#[test]
fn hello_carries_the_parser_version() {
    let hello = Hello::new(ClientKind::Web, ClientFacts::default());
    assert_eq!(hello.versions.parser, auk_replica::PARSER_VERSION);
    let json = serde_json::to_value(ClientMsg::Hello(hello)).unwrap();
    assert_eq!(json["type"], "hello");
    assert_eq!(json["kind"], "web");
    assert_eq!(json["versions"]["protocol"], PROTOCOL_VERSION);
}

#[cfg(unix)]
#[test]
fn blocking_client_handshake_and_refusal() {
    use std::os::unix::net::UnixStream;
    use std::thread;

    for accept in [true, false] {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let peer = thread::spawn(move || {
            let mut s = theirs;
            let mut buf = Vec::new();
            let Some(ClientMsg::Hello(hello)) = read_msg(&mut s, &mut buf).unwrap() else {
                panic!("expected hello");
            };
            assert_eq!(hello.kind, ClientKind::App);
            let answer = if accept {
                DaemonMsg::Welcome {
                    versions: Versions::current(),
                    catalog: serde_json::Value::Null,
                    sessions: Layout::default(),
                    client: 9,
                }
            } else {
                DaemonMsg::Refused(Refusal {
                    reason: "parser".into(),
                    daemon: Versions::current(),
                })
            };
            write_msg(&mut s, &answer).unwrap();
        });
        let result = Client::handshake(ours, &Hello::new(ClientKind::App, ClientFacts::default()));
        peer.join().unwrap();
        match (accept, result) {
            (true, Ok((_, welcome))) => assert_eq!(welcome.client, 9),
            (false, Err(HandshakeError::Refused(r))) => assert_eq!(r.reason, "parser"),
            (_, other) => panic!("unexpected {other:?}"),
        }
    }
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn tokio_codec_round_trips_split_writes() {
    use futures_util::SinkExt;
    use tokio::io::AsyncWriteExt;
    use tokio_util::codec::{FramedRead, FramedWrite};

    let (a, b) = tokio::io::duplex(64);
    let (_, mut write_half) = tokio::io::split(a);
    let reader = tokio::spawn(async move {
        use futures_util::StreamExt as _;
        let mut framed = FramedRead::new(b, codec::Codec::<DaemonMsg, ClientMsg>::new());
        let mut got = Vec::new();
        while let Some(msg) = framed.next().await {
            got.push(msg.unwrap());
        }
        got
    });
    // Write the frames a few bytes at a time to exercise reassembly.
    let mut wire = Vec::new();
    for msg in daemon_messages() {
        msg.encode(&mut wire);
    }
    for chunk in wire.chunks(7) {
        write_half.write_all(chunk).await.unwrap();
    }
    drop(write_half);
    assert_eq!(reader.await.unwrap(), daemon_messages());

    let (a, b) = tokio::io::duplex(1 << 16);
    let mut sink = FramedWrite::new(a, codec::Codec::<DaemonMsg, ClientMsg>::new());
    for msg in client_messages() {
        sink.send(msg).await.unwrap();
    }
    drop(sink);
    let mut framed =
        tokio_util::codec::FramedRead::new(b, codec::Codec::<ClientMsg, DaemonMsg>::new());
    use futures_util::StreamExt as _;
    let mut got = Vec::new();
    while let Some(msg) = framed.next().await {
        got.push(msg.unwrap());
    }
    assert_eq!(got, client_messages());
}
