//! The web client (GOAL.md `web`, §10.3, §12.5, M11): `auk web serve` serves the wasm client,
//! trades the URL's token for a cookie, binds loopback only unless told otherwise, and bridges
//! the replication protocol over a WebSocket (refusing other versions, X9). In Safari, driven
//! through safaridriver, the client attaches to a live pane, draws it over WebGPU and types
//! into the shell. And wasm replicas reach the native state hashes on fuzzed streams (AC2).
//!
//! The client, its JS glue (the pinned wasm-bindgen CLI) and the wasm32-wasip1 replica driver
//! are built fresh in each run (`auk_conformance::web::Builds`).

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use auk_conformance::fuzz;
use auk_conformance::harness::{TestEnv, wait_for};
use auk_conformance::web::{
    Builds, Rgba, Safari, WebServer, WsClient, WsMessage, cookie_value, http,
};
use auk_mirror::{ClientFacts, ClientKind, ClientMsg, DaemonMsg, Frame, Hello, Message, Versions};
use auk_web::replay::{self, Case, Step};
use serde_json::Value;

fn env(name: &str) -> TestEnv {
    TestEnv::new(env!("CARGO_BIN_EXE_auk"), name)
}

fn builds() -> Builds {
    Builds::new(env!("CARGO_TARGET_TMPDIR"))
}

/// A daemon with one pane at its prompt.
fn daemon_with_pane(env: &TestEnv) -> u64 {
    env.start_daemon();
    let pane = env.new_pane(&[]);
    env.wait(pane, "prompt");
    pane
}

fn capture(env: &TestEnv, pane: u64) -> String {
    env.daemon(&format!("capture {pane}"))["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// The address a process listens on, from lsof.
fn listening(pid: u32) -> Vec<String> {
    let out = Command::new("/usr/sbin/lsof")
        .args([
            "-nP",
            "-a",
            "-p",
            &pid.to_string(),
            "-iTCP",
            "-sTCP:LISTEN",
            "-Fn",
        ])
        .output()
        .expect("run lsof");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix('n'))
        .map(str::to_owned)
        .collect()
}

fn decode(bytes: &[u8]) -> DaemonMsg {
    let (frame, used) = auk_mirror::frame::split_frame(bytes)
        .expect("a frame")
        .expect("a whole frame");
    assert_eq!(used, bytes.len(), "one frame per WebSocket message");
    DaemonMsg::decode(Frame {
        kind: frame.kind,
        payload: frame.payload,
    })
    .expect("a daemon message")
}

/// The next daemon message on a WebSocket.
fn next_msg(ws: &mut WsClient) -> DaemonMsg {
    match ws.recv() {
        Some(WsMessage::Binary(bytes)) => decode(&bytes),
        other => panic!("expected a binary frame, got {other:?}"),
    }
}

#[test]
fn web_serve_trades_the_url_token_for_a_session_cookie() {
    let env = env("webtoken");
    env.start_daemon();
    let assets = builds().web_client();
    let server = WebServer::start(&env, &assets, &["--port", "0"]);
    let host = server.host();
    let port = server.port;
    assert!(
        server.url.starts_with(&format!("http://{host}/?token=")),
        "{}",
        server.url
    );
    assert_eq!(server.token.len(), 64, "a 256-bit token as hex");
    assert!(server.token.bytes().all(|b| b.is_ascii_hexdigit()));

    // Nothing without the cookie.
    for path in [
        "/",
        "/auk_web.js",
        "/auk_web_bg.wasm",
        "/boot.js",
        "/font/0",
    ] {
        let r = http(port, "GET", path, &[("Host", &host)], b"");
        assert_eq!(r.status, 401, "{path}: {r:?}");
    }
    let wrong = format!("/?token={}", "0".repeat(64));
    assert_eq!(
        http(port, "GET", &wrong, &[("Host", &host)], b"").status,
        403
    );

    // The first load trades the token for an HttpOnly, SameSite=Strict cookie and redirects to
    // the same URL without the token.
    let first = http(
        port,
        "GET",
        &format!("/?window=main&token={}", server.token),
        &[("Host", &host)],
        b"",
    );
    assert_eq!(first.status, 303, "{first:?}");
    assert_eq!(first.header("location"), Some("/?window=main"));
    let set = first.header("set-cookie").expect("a cookie");
    for attr in ["HttpOnly", "SameSite=Strict", "Path=/"] {
        assert!(set.split("; ").any(|a| a == attr), "{set}");
    }
    let cookie = cookie_value(&first).expect("auk_session");
    assert_eq!(cookie.len(), 64);
    assert_ne!(cookie, server.token);

    // The token is one-time.
    let again = http(
        port,
        "GET",
        &format!("/?token={}", server.token),
        &[("Host", &host)],
        b"",
    );
    assert_eq!(again.status, 403, "{again:?}");

    // With the cookie: the page and the client's files, as built.
    let with = format!("auk_session={cookie}");
    let page = http(port, "GET", "/", &[("Host", &host), ("Cookie", &with)], b"");
    assert_eq!(page.status, 200);
    assert!(page.text().contains("/boot.js"), "{}", page.text());
    let csp = page.header("content-security-policy").expect("a CSP");
    assert!(csp.contains("default-src 'none'"), "{csp}");
    for (path, file, kind) in [
        ("/auk_web_bg.wasm", "auk_web_bg.wasm", "application/wasm"),
        ("/auk_web.js", "auk_web.js", "text/javascript"),
    ] {
        let r = http(
            port,
            "GET",
            path,
            &[("Host", &host), ("Cookie", &with)],
            b"",
        );
        assert_eq!(r.status, 200, "{path}");
        assert_eq!(r.header("content-type"), Some(kind));
        assert_eq!(r.body, std::fs::read(assets.join(file)).unwrap(), "{path}");
    }
    let font = http(
        port,
        "GET",
        "/font/0",
        &[("Host", &host), ("Cookie", &with)],
        b"",
    );
    assert_eq!(font.status, 200);
    assert!(font.body.len() > 10_000);

    // A forged cookie, another Host (DNS rebinding) and a cross-origin WebSocket are refused.
    let forged = format!("auk_session={}", "f".repeat(64));
    assert_eq!(
        http(
            port,
            "GET",
            "/",
            &[("Host", &host), ("Cookie", &forged)],
            b""
        )
        .status,
        401
    );
    let evil_host = format!("evil.example:{port}");
    assert_eq!(
        http(
            port,
            "GET",
            "/",
            &[("Host", &evil_host), ("Cookie", &with)],
            b""
        )
        .status,
        403
    );
    let origin = format!("http://{host}");
    let err = WsClient::connect(port, &[("Host", &host), ("Origin", &origin)]).unwrap_err();
    assert_eq!(err.status, 401);
    let err = WsClient::connect(
        port,
        &[
            ("Host", &host),
            ("Origin", "http://evil.example"),
            ("Cookie", &with),
        ],
    )
    .unwrap_err();
    assert_eq!(err.status, 403);
    WsClient::connect(
        port,
        &[("Host", &host), ("Origin", &origin), ("Cookie", &with)],
    )
    .expect("a same-origin WebSocket with the cookie");
}

#[test]
fn web_socket_bridges_the_replication_protocol_to_the_daemon() {
    let env = env("webbridge");
    let pane = daemon_with_pane(&env);
    env.run(pane, "echo before-$((40+2))");
    let assets = builds().web_client();
    let server = WebServer::start(&env, &assets, &["--port", "0"]);
    let cookie = format!("auk_session={}", server.login());
    let host = server.host();
    let origin = format!("http://{host}");
    let mut ws = WsClient::connect(
        server.port,
        &[("Host", &host), ("Origin", &origin), ("Cookie", &cookie)],
    )
    .expect("upgrade");

    ws.send_binary(
        &ClientMsg::Hello(Hello::new(ClientKind::Web, ClientFacts::default())).to_bytes(),
    );
    match next_msg(&mut ws) {
        DaemonMsg::Welcome { versions, .. } => assert_eq!(versions, Versions::current()),
        other => panic!("expected a welcome, got {other:?}"),
    }
    ws.send_binary(
        &ClientMsg::Attach {
            window_key: "main".into(),
        }
        .to_bytes(),
    );
    let mut replica = None;
    let deadline = Instant::now() + Duration::from_secs(20);
    while replica.is_none() {
        assert!(Instant::now() < deadline, "no snapshot");
        if let DaemonMsg::Snapshot { pane: p, snapshot } = next_msg(&mut ws)
            && p == pane
        {
            replica = Some(auk_replica::Replica::from_snapshot(
                &snapshot,
                auk_replica::Role::Follower,
                None,
            ));
        }
    }
    let mut replica = replica.unwrap();
    assert!(replica.text().contains("before-42"), "{}", replica.text());

    // Input through the bridge reaches the shell, and its output comes back as live items.
    ws.send_binary(
        &ClientMsg::Input {
            pane,
            bytes: b"echo through-the-bridge-$((6*7))\r".to_vec(),
        }
        .to_bytes(),
    );
    while !replica.screen_text().contains("through-the-bridge-42\n") {
        assert!(Instant::now() < deadline, "{}", replica.screen_text());
        match next_msg(&mut ws) {
            DaemonMsg::Output {
                pane: p,
                seq,
                bytes,
            } if p == pane => {
                replica
                    .apply(seq, auk_replica::Item::Output(&bytes))
                    .unwrap();
            }
            DaemonMsg::Record {
                pane: p,
                seq,
                record,
            } if p == pane => {
                replica
                    .apply(seq, auk_replica::Item::Record(&record))
                    .unwrap();
            }
            _ => {}
        }
    }
    assert!(capture(&env, pane).contains("through-the-bridge-42"));

    // A message that isn't one whole frame closes the connection.
    ws.send_binary(&[1, 2, 3]);
    loop {
        match ws.recv() {
            Some(WsMessage::Close(code, _)) => {
                assert_eq!(code, Some(1008));
                break;
            }
            Some(_) => {}
            None => panic!("closed without a close frame"),
        }
    }
}

#[test]
fn web_serve_refuses_a_client_of_another_version() {
    let env = env("webx9");
    env.start_daemon();
    let assets = builds().web_client();
    let server = WebServer::start(&env, &assets, &["--port", "0"]);
    let cookie = format!("auk_session={}", server.login());
    let host = server.host();
    let origin = format!("http://{host}");
    for change in ["parser", "protocol", "auk"] {
        let mut ws = WsClient::connect(
            server.port,
            &[("Host", &host), ("Origin", &origin), ("Cookie", &cookie)],
        )
        .expect("upgrade");
        let mut hello = Hello::new(ClientKind::Web, ClientFacts::default());
        match change {
            "parser" => hello.versions.parser.push_str("-other"),
            "protocol" => hello.versions.protocol += 1,
            _ => hello.versions.auk.push_str("-other"),
        }
        ws.send_binary(&ClientMsg::Hello(hello).to_bytes());
        match next_msg(&mut ws) {
            DaemonMsg::Refused(refusal) => {
                assert!(
                    refusal.reason.to_lowercase().contains(change),
                    "{}",
                    refusal.reason
                );
                assert_eq!(refusal.daemon, Versions::current());
            }
            other => panic!("{change}: expected a refusal, got {other:?}"),
        }
        // Then the connection closes.
        match ws.recv() {
            Some(WsMessage::Close(..)) | None => {}
            other => panic!("{change}: expected the close, got {other:?}"),
        }
    }
}

#[test]
fn web_serve_binds_loopback_only_by_default() {
    let env = env("webloop");
    let help = env.cli(&["web", "serve", "--help"]);
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("127.0.0.1:8377"), "{help}");

    let assets = builds().web_client();
    let server = WebServer::start(&env, &assets, &["--port", "0"]);
    assert!(
        server.url.starts_with("http://127.0.0.1:"),
        "{}",
        server.url
    );
    let addrs = wait_for("the server to listen", || {
        let a = listening(server.pid());
        (!a.is_empty()).then_some(a)
    });
    assert_eq!(addrs, vec![format!("127.0.0.1:{}", server.port)]);
}

#[test]
fn web_serve_refuses_a_non_loopback_bind_without_the_flag() {
    let env = env("webremote");
    let assets = builds().web_client();
    for bind in ["0.0.0.0:0", "[::]:0"] {
        let out = env
            .command()
            .args(["web", "serve", "--assets"])
            .arg(&*assets)
            .args(["--bind", bind])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run auk web serve");
        assert!(!out.status.success(), "{bind} was accepted");
        assert!(out.stdout.is_empty(), "printed a URL for {bind}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("--allow-remote"), "{err}");
    }
    // With the flag it listens on every interface.
    let server = WebServer::start(&env, &assets, &["--bind", "0.0.0.0:0", "--allow-remote"]);
    let addrs = wait_for("the server to listen", || {
        let a = listening(server.pid());
        (!a.is_empty()).then_some(a)
    });
    assert_eq!(addrs, vec![format!("*:{}", server.port)]);
}

/// Polls the page's state hook until `f` accepts it.
fn page_state(safari: &Safari, what: &str, mut f: impl FnMut(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last = Value::Null;
    loop {
        let raw = safari.execute("return window.aukDebug ? window.aukDebug() : null;");
        if let Some(s) = raw.as_str() {
            last = serde_json::from_str(s).unwrap_or(Value::Null);
            if f(&last) {
                return last;
            }
        }
        if Instant::now() > deadline {
            let err = safari.execute("return window.aukError || null;");
            panic!(
                "timed out waiting for {what}; page state: {last}; page error: {err}; {}",
                page_focus(safari)
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn pane_state(state: &Value, pane: u64) -> Option<&Value> {
    state["panes"].as_array()?.iter().find(|p| p["id"] == pane)
}

#[test]
fn web_client_in_safari_attaches_draws_over_webgpu_and_types() {
    // First, so a missing prerequisite fails fast and says what to do.
    let safari = Safari::start();
    let env = env("websafari");
    web_plugin(&env);
    let pane = daemon_with_pane(&env);
    // Exact colors, whatever the page's theme: red text, then text on a green background.
    env.run(
        pane,
        "printf '\\e[38;2;255;0;0mred\\e[0m \\e[48;2;0;200;0mgreen\\e[0m web-marker-%d\\n' 7",
    );
    let assets = builds().web_client();
    let server = WebServer::start(&env, &assets, &["--port", "0"]);

    safari.navigate(&server.url);
    // The token is gone from the URL after the first load.
    let url = safari.current_url();
    assert!(!url.contains("token"), "{url}");

    // Attached, drawn, and the same state as the daemon's replica.
    let state = page_state(&safari, "the client to draw the pane", |s| {
        s["status"] == "attached"
            && s["frames"].as_u64().unwrap_or(0) > 0
            && pane_state(s, pane).is_some_and(|p| !p["rect"].is_null())
    });
    assert!(
        state["adapter"]
            .as_str()
            .unwrap_or_default()
            .contains("BrowserWebGpu"),
        "not drawing over WebGPU: {}",
        state["adapter"]
    );
    // The shell may redraw after the page's resize: compare the screen at the same position too.
    let state = page_state(&safari, "the page's replica to match the daemon's", |s| {
        let (hash, seq) = env.daemon_hash(pane);
        pane_state(s, pane).is_some_and(|p| {
            p["hash"] == hash.as_str() && p["seq"] == seq && p["screen"].as_str() == Some(&capture(&env, pane))
        })
    });
    let p = pane_state(&state, pane).expect("the pane");
    assert!(p["screen"].as_str().unwrap().contains("web-marker-7"));
    assert!(
        state["errors"].as_array().is_some_and(Vec::is_empty),
        "{state}"
    );

    // Typed keys reach the shell. A click focuses the pane (and its text field); keys go where
    // the focus is.
    safari.click(&safari.element("#screen"));
    safari.press_keys("echo typed-in-safari-$((6*7))\u{E007}");
    wait_for("the shell to run the typed command", || {
        capture(&env, pane)
            .contains("\ntyped-in-safari-42")
            .then_some(())
    });
    let state = page_state(&safari, "the page to show the output", |s| {
        pane_state(s, pane).is_some_and(|p| {
            p["screen"]
                .as_str()
                .is_some_and(|t| t.contains("\ntyped-in-safari-42"))
        })
    });
    // As the pane's primary, the page's size applies to the pane.
    let p = pane_state(&state, pane).unwrap();
    assert_eq!(p["primary"], true, "{p}");
    // §4.5: the page reports the browser's 12/24-hour clock, read here through Intl in the page.
    let cycle = safari.execute(
        "return new Intl.DateTimeFormat(undefined, {hour: 'numeric'}).resolvedOptions().hourCycle;",
    );
    let hour12 = matches!(cycle.as_str(), Some("h11" | "h12"));
    wait_for("the page's clock as the pane's fact", || {
        (env.daemon(&format!("tsp {pane}"))["facts"]["hour12"] == hour12).then_some(())
    });
    let info = env.pane_info(pane);
    assert_eq!(
        (info["cols"].as_u64(), info["rows"].as_u64()),
        (p["cols"].as_u64(), p["rows"].as_u64()),
        "{info}"
    );

    // The screenshot shows the grid: in the pane's area, ink that isn't the background,
    // including the program's red and green. WebKit's snapshots can miss a WebGPU canvas drawn
    // before the window was on screen, so this waits for a screenshot showing the frames drawn
    // after the typing.
    let rect = p["rect"].clone();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (ink, reddish, greenish) = screen_ink(&safari, &rect);
        if ink > 500 && reddish > 20 && greenish > 100 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the screenshot shows only {ink} drawn pixels in the pane's area (red {reddish}, \
             green {greenish})"
        );
        std::thread::sleep(Duration::from_millis(250));
    }

    safari_surfaces_lens_views_and_blocks(&safari, &env, pane);
}

/// Where the page drew a node (`window.aukNodeRect`), in page CSS pixels.
fn page_node_rect(safari: &Safari, pane: u64, sf: &str, id: &str) -> Value {
    let what = format!("{sf}/{id} of pane {pane} to be drawn");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let raw = safari.execute(&format!(
            "return window.aukNodeRect ? window.aukNodeRect({pane}, {}, {}) : 'null';",
            Value::from(sf),
            Value::from(id)
        ));
        let rect: Value = raw
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        if rect["w"].as_f64().is_some_and(|w| w > 0.0) {
            return rect;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Whether the page has focus and how big its viewport is, for failure messages.
fn page_focus(safari: &Safari) -> String {
    let v = safari.execute(
        "return {focus: document.hasFocus(), w: window.innerWidth, h: window.innerHeight};",
    );
    format!("page focus {}, viewport {}x{}", v["focus"], v["w"], v["h"])
}

/// Where the page drew a node once nothing is moving it: the page's replica matches the
/// daemon's (so no output is still arriving), and the node's rect and the viewport read the
/// same twice, 250 ms apart. Focuses the page's window if it lost focus.
fn settled_node_rect(safari: &Safari, env: &TestEnv, pane: u64, sf: &str, id: &str) -> Value {
    page_state(safari, "the page's replica to match the daemon's", |s| {
        let (hash, seq) = env.daemon_hash(pane);
        pane_state(s, pane).is_some_and(|p| p["hash"] == hash.as_str() && p["seq"] == seq)
    });
    if safari.execute("return document.hasFocus();") != true {
        safari.execute("window.focus(); return null;");
    }
    let viewport = || safari.execute("return [window.innerWidth, window.innerHeight];");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let before = (page_node_rect(safari, pane, sf, id), viewport());
        std::thread::sleep(Duration::from_millis(250));
        let after = (page_node_rect(safari, pane, sf, id), viewport());
        if before == after {
            return after.0;
        }
        assert!(
            Instant::now() < deadline,
            "{sf}/{id} of pane {pane} kept moving: {before:?}, then {after:?}"
        );
    }
}

/// What a press at a point of the page would land on (`window.aukHitTest`).
fn page_hit(safari: &Safari, x: f64, y: f64) -> Value {
    let raw = safari.execute(&format!(
        "return window.aukHitTest ? window.aukHitTest({x}, {y}) : 'null';"
    ));
    raw.as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null)
}

/// Clicks the center of a node once it has settled ([`settled_node_rect`]), after checking that
/// a press there lands on it, and checks that the page's press did.
fn click_node(safari: &Safari, env: &TestEnv, pane: u64, sf: &str, id: &str) {
    let rect = settled_node_rect(safari, env, pane, sf, id);
    let c = |k: &str| rect[k].as_f64().unwrap();
    // WebDriver clicks whole CSS pixels.
    let (x, y) = (
        (c("x") + c("w") / 2.0).round(),
        (c("y") + c("h") / 2.0).round(),
    );
    let lands = |hit: &Value| {
        hit["outcome"] == "node" && hit["pane"] == pane && hit["sf"] == sf && hit["id"] == id
    };
    let hit = page_hit(safari, x, y);
    assert!(
        lands(&hit),
        "a press at ({x}, {y}), the center of {sf}/{id} at {rect}, would land on {hit}; {}",
        page_focus(safari)
    );
    let raw = safari.execute("return window.aukDebug ? window.aukDebug() : null;");
    let before = raw
        .as_str()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|s| s["last_press"]["n"].as_u64())
        .unwrap_or(0);
    safari.click_at(x, y);
    let state = page_state(safari, &format!("the page's press on {sf}/{id}"), |s| {
        s["last_press"]["n"].as_u64() > Some(before)
    });
    let press = &state["last_press"];
    assert!(
        lands(press),
        "the click at ({x}, {y}) on {sf}/{id} landed on {press}; {}",
        page_focus(safari)
    );
}

fn click_center(safari: &Safari, rect: &Value) {
    let c = |k: &str| rect[k].as_f64().unwrap();
    safari.click_at(c("x") + c("w") / 2.0, c("y") + c("h") / 2.0);
}

/// The id of a node in a JSON tree whose `text` starts with `prefix`.
fn json_node_with_text(v: &Value, prefix: &str) -> Option<String> {
    if v["p"]["text"]
        .as_str()
        .is_some_and(|t| t.starts_with(prefix))
    {
        return v["id"].as_str().map(str::to_owned);
    }
    v["c"]
        .as_array()?
        .iter()
        .find_map(|c| json_node_with_text(c, prefix))
}

/// The live surface of a pane in the daemon: its id and the id of the node showing `prefix`.
fn daemon_node(env: &TestEnv, pane: u64, prefix: &str) -> Option<(String, String)> {
    let tsp = env.daemon(&format!("tsp {pane}"));
    tsp["surfaces"].as_array()?.iter().rev().find_map(|s| {
        let id = json_node_with_text(&s["doc"], prefix)?;
        Some((s["id"].as_str()?.to_owned(), id))
    })
}

/// §10.3, §10.4 in the browser: a program's surface, a lens view (with the plugin's manifest
/// style and its effects) and a plugin block are drawn, and clicks on them reach their owners.
fn safari_surfaces_lens_views_and_blocks(safari: &Safari, env: &TestEnv, pane: u64) {
    // A program's surface in the flow.
    env.run(pane, &web_surface_program(env));
    let state = page_state(safari, "the surface to be drawn", |s| {
        pane_state(s, pane).is_some_and(|p| {
            p["surfaces"].as_array().is_some_and(|a| {
                a.iter()
                    .any(|sf| sf["sf"] == "w1" && sf["h"].as_f64() > Some(0.0))
            })
        })
    });
    page_node_rect(safari, pane, "w1", "t");
    assert_eq!(
        pane_state(&state, pane).unwrap()["styles"],
        serde_json::json!(["webdemo"])
    );

    // A lensed command: its block shows the lens view, painted with the manifest style.
    env.run(pane, &web_cmd(env));
    let state = page_state(safari, "the lens view to be drawn", |s| {
        pane_state(s, pane).is_some_and(|p| {
            p["lens"].as_array().is_some_and(|a| {
                a.iter()
                    .any(|l| l["view"] == true && l["h"].as_f64() > Some(0.0))
            })
        })
    });
    let lens = pane_state(&state, pane).unwrap()["lens"][0].clone();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (_, _, greenish) = screen_ink(safari, &lens);
        if greenish > 100 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the lens block shows only {greenish} pixels of the manifest style's green"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    // A click on the view's node: the lens copies and toasts, in this page.
    click_node(safari, env, pane, "lens:0", "go");
    page_state(safari, "the lens's toast", |s| {
        s["effects"].as_array().is_some_and(|e| {
            e.iter()
                .any(|e| e["effect"] == "toast" && e["text"] == "Copied from the lens")
                && e.iter()
                    .any(|e| e["effect"] == "copy" && e["text"] == "copied-from-lens")
        })
    });
    let toast = safari.execute("return document.getElementById('toast').textContent;");
    assert!(
        toast
            .as_str()
            .is_some_and(|t| t.contains("Copied from the lens")),
        "{toast}"
    );

    // A plugin block in a new session of the window key: its tab shows in the tab bar, and a
    // click there shows the block, drawn as a screen surface that takes clicks.
    let block = env.daemon("block webdemo.panel --window main")["pane"]
        .as_u64()
        .expect("a block pane");
    let (sf, id) = wait_for("the block's view in the daemon", || {
        daemon_node(env, block, "block-panel 0")
    });
    // A12: the tab bar is the app's chrome drawn on the canvas. A second session brings the
    // session chip; its menu's row for the block's session shows the block.
    let session = wait_for("the block's session", || {
        env.daemon("ls")["panes"]
            .as_array()?
            .iter()
            .find(|p| p["id"] == block)?["session"]
            .as_u64()
    });
    let state = page_state(safari, "the session chip", |s| chrome_rect(s, "session-menu").is_some());
    // The bar is drawn: the strip above the panes isn't the page's background.
    let strip = serde_json::json!({"x": 0.0, "y": 0.0, "w": 400.0, "h": 28.0});
    let (ink, _, _) = screen_ink(safari, &strip);
    assert!(ink > 200, "the tab bar strip shows only {ink} drawn pixels");
    // Assistive technology hears the tab bar through the page's mirror of it.
    let selected = safari.execute(
        "return document.querySelectorAll('#chrome-a11y [role=tab][aria-selected=true]').length;",
    );
    assert_eq!(selected.as_u64(), Some(1), "one selected tab in the mirror");
    click_center(safari, &chrome_rect(&state, "session-menu").unwrap());
    let row = format!("session:{session}");
    let state = page_state(safari, "the session menu", |s| chrome_rect(s, &row).is_some());
    assert!(
        chrome_rect(&state, "new-session").is_none() && chrome_rect(&state, "new-tab").is_none(),
        "the browser creates nothing: {state}"
    );
    click_center(safari, &chrome_rect(&state, &row).unwrap());
    page_state(safari, "the block to be drawn", |s| {
        pane_state(s, block).is_some_and(|p| {
            !p["rect"].is_null()
                && p["surfaces"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|x| x["sf"] == sf.as_str()))
        })
    });
    click_node(safari, env, block, &sf, &id);
    wait_for("the block to take the click", || {
        daemon_node(env, block, "block-panel 1")
    });
    page_state(safari, "the block's toast in the page", |s| {
        s["effects"].as_array().is_some_and(|e| {
            e.iter()
                .any(|e| e["effect"] == "toast" && e["text"] == "bumped 1")
        })
    });
    let state = page_state(safari, "the page's block replica to match", |s| {
        let (hash, seq) = env.daemon_hash(block);
        pane_state(s, block).is_some_and(|p| p["hash"] == hash.as_str() && p["seq"] == seq)
    });
    assert!(
        state["errors"].as_array().is_some_and(Vec::is_empty),
        "{state}"
    );
}

/// Where the page drew a chrome element with this action, in page CSS pixels.
fn chrome_rect(state: &Value, action: &str) -> Option<Value> {
    state["chrome"]
        .as_array()?
        .iter()
        .find(|c| c["action"] == action)
        .cloned()
}

/// Drawn pixels in a page rect (CSS pixels) of a screenshot: (unlike the background, red,
/// green).
fn screen_ink(safari: &Safari, rect: &Value) -> (u32, u32, u32) {
    let shot = Rgba::from_png(&safari.screenshot());
    let css_width = safari
        .execute("return window.innerWidth;")
        .as_f64()
        .unwrap();
    let ratio = f64::from(shot.width) / css_width;
    let px = |v: &Value| (v.as_f64().unwrap() * ratio) as u32;
    let (x0, y0) = (px(&rect["x"]), px(&rect["y"]));
    let x1 = (x0 + px(&rect["w"])).min(shot.width);
    let y1 = (y0 + px(&rect["h"])).min(shot.height);
    let background = shot.get(x1 - 2, y1 - 2);
    let (mut ink, mut reddish, mut greenish) = (0u32, 0u32, 0u32);
    for y in y0..y1 {
        for x in x0..x1 {
            let c = shot.get(x, y);
            let diff: u32 = (0..3)
                .map(|i| u32::from(c[i].abs_diff(background[i])))
                .sum();
            if diff > 60 {
                ink += 1;
            }
            // By hue, so a color-managed screenshot still counts.
            let [r, g, b, _] = c.map(i32::from);
            if r > g + 100 && r > b + 100 {
                reddish += 1;
            }
            if g > r + 60 && g > b + 60 {
                greenish += 1;
            }
        }
    }
    (ink, reddish, greenish)
}

/// Fuzzed cases: the native AC2 shape (records among byte soup, a late snapshot follower) with
/// TSP traffic mixed in.
fn fuzz_cases() -> Vec<Case> {
    (1..=40u64)
        .map(|seed| {
            let mut rng = fuzz::Rng::new(seed);
            let mut steps = Vec::new();
            for step in 0..60 {
                if step == 30 {
                    steps.push(Step::Snapshot);
                }
                match fuzz::record(&mut rng) {
                    Some(record) => steps.push(Step::Record(record)),
                    None => {
                        let len = 1 + rng.below(400) as usize;
                        steps.push(Step::Output(fuzz::bytes(&mut rng, len)));
                    }
                }
            }
            Case {
                cols: 20 + rng.below(100) as u16,
                rows: 5 + rng.below(40) as u16,
                steps,
            }
        })
        .collect()
}

#[test]
fn ac02_wasm_replicas_reach_the_native_state_hashes_under_fuzzing() {
    let wasm = builds().wasi_replay();
    let cases = fuzz_cases();
    let native = replay::run(&cases);
    for (i, line) in native.iter().enumerate() {
        let words: Vec<&str> = line.split(' ').collect();
        assert_eq!(words.len(), 6, "case {i}: {line}");
        // The authoritative replica, both followers, the late follower and a replica rebuilt
        // from the final snapshot (often taken in the middle of an escape sequence, a TSP string
        // or a chunk sequence) agree.
        assert!(
            words[1..].iter().all(|h| *h == words[1]),
            "case {i}: native replicas disagree: {line}"
        );
    }

    let mut child = Command::new("wasmtime")
        .arg("run")
        .arg(&wasm)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("wasmtime can't run ({e}): brew install wasmtime"));
    let input = replay::encode(&cases);
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        use std::io::Write;
        stdin.write_all(&input).expect("feed the cases");
    });
    let out = child.wait_with_output().expect("wasmtime");
    writer.join().unwrap();
    assert!(
        out.status.success(),
        "the wasm replica driver failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let wasm_lines: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(wasm_lines.len(), native.len());
    for (i, (w, n)) in wasm_lines.iter().zip(&native).enumerate() {
        assert_eq!(w, n, "case {i}: wasm32 and native replicas differ");
    }
}

/// §4.2, §10.3, §10.4: the browser runs no Lua at all. The wasm client's dependency graph has
/// no Luau (no `mlua`, `mlua-sys` or `luau*` crate) and takes `auk-plugins` without its
/// `runtime` feature; and the built client, wasm and JS glue, carries no Luau or mlua symbol, so
/// it hosts no window half.
#[test]
fn web_client_builds_without_lua() {
    let out = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .current_dir(auk_conformance::web::workspace_root())
        .args([
            "tree",
            "-p",
            "auk-web",
            "--target",
            "wasm32-unknown-unknown",
            "-e",
            "normal",
        ])
        .args(["--prefix", "none", "-f", "{p} [{f}]"])
        .output()
        .expect("run cargo tree");
    assert!(
        out.status.success(),
        "cargo tree: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let tree = String::from_utf8(out.stdout).expect("utf-8");
    let names: Vec<(&str, &str)> = tree
        .lines()
        .filter_map(|l| {
            let name = l.split_whitespace().next()?;
            let features = l
                .split_once(" [")
                .map_or("", |(_, f)| f.split(']').next().unwrap_or(""));
            Some((name, features))
        })
        .collect();
    assert!(names.iter().any(|(n, _)| *n == "auk-web"), "{tree}");
    for (name, _) in &names {
        assert!(
            !(*name == "mlua" || *name == "mlua-sys" || name.starts_with("luau")),
            "the wasm client depends on {name}:\n{tree}"
        );
    }
    let plugins: Vec<&str> = names
        .iter()
        .filter(|(n, _)| *n == "auk-plugins")
        .map(|(_, f)| *f)
        .collect();
    assert!(
        !plugins.is_empty(),
        "the client takes auk-plugins' runtime-free half:\n{tree}"
    );
    for features in plugins {
        assert!(
            !features.split(',').any(|f| f.trim() == "runtime"),
            "auk-plugins with its runtime feature ({features}):\n{tree}"
        );
    }
    // The built client: no Luau VM (its C API's `lua_`/`luaL_` symbols, its `Luau` names) and
    // no mlua bindings in the wasm, nothing of them in the glue. Matched by case and at the start
    // of a word: the data section runs strings together (the UA style sheet's `html` meets its
    // `ua` label), so a bare `mlua` is found inside `htmlua`.
    let assets = builds().web_client();
    for file in ["auk_web_bg.wasm", "auk_web.js"] {
        let bytes = std::fs::read(assets.join(file)).expect("read the built client");
        for needle in ["mlua", "Luau", "luau", "LUAU", "lua_", "luaL_", "luaU_"] {
            let starts_word = |at: usize| at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
            if let Some(at) = (0..=bytes.len().saturating_sub(needle.len()))
                .find(|&at| bytes[at..].starts_with(needle.as_bytes()) && starts_word(at))
            {
                let around = &bytes[at.saturating_sub(40)..(at + 40).min(bytes.len())];
                panic!(
                    "{file} has {needle:?} at {at}: {:?}",
                    String::from_utf8_lossy(around)
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Plugins in the web client (§9.4, §10.3, §10.4): the catalog's manifest styles, plugin blocks,
// lens views and effects. The client runs no Lua; all of it comes from the serving daemon.

/// A plugin with a lens on `webdemo-cmd *` whose view has a clickable node (`go`, which copies
/// and toasts), a block type `panel` whose node `bump`s a counter and toasts, and a manifest
/// style that paints the lens's block green.
fn web_plugin(env: &TestEnv) {
    let dir = env.state.join("plugins/webdemo");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(
        dir.join("plugin.toml"),
        "schema = 1\nid = \"webdemo\"\nname = \"Web demo\"\nversion = \"1\"\n\
         host = \"host.luau\"\nstyles = [\"web.css\"]\n\
         [[lenses]]\nid = \"echo\"\nmatch = [\"webdemo-cmd *\"]\n\
         [[blocks]]\nid = \"panel\"\ntitle = \"Panel\"\n",
    )
    .expect("write plugin.toml");
    std::fs::write(dir.join("web.css"), WEB_CSS).expect("write web.css");
    std::fs::write(
        dir.join("host.luau"),
        r#"tern.lens.define("echo", {
            open = function(run) return { lines = 0 } end,
            line = function(s, l) s.lines += 1 end,
            finish = function(s, status) end,
            view = function(s)
                return { k = "col", c = {
                    { k = "text", p = { text = "lens-view " .. s.lines .. " lines" } },
                    { id = "go", k = "text", p = { text = "Copy it", actions = { click = "copy-it" } } },
                } }
            end,
            event = function(s, ev, cx)
                if ev.ev == "action" and ev.act == "copy-it" then
                    cx:copy("copied-from-lens")
                    cx:toast("success", "Copied from the lens", "webdemo")
                end
            end,
        })
        tern.block.define("panel", {
            init = function(cx, args, saved) return { n = 0 } end,
            view = function(s)
                return { main = { k = "col", c = {
                    { k = "text", p = { text = "block-panel " .. s.n, actions = { click = "bump" } } },
                } } }
            end,
            event = function(s, ev, cx)
                if ev.ev == "action" and ev.act == "bump" then
                    s.n += 1
                    cx:toast("info", "bumped " .. s.n)
                    cx:render()
                end
            end,
        })"#,
    )
    .expect("write host.luau");
}

const WEB_CSS: &str =
    "[data-role=\"lens.plugin.webdemo.echo\"] { background-color: rgb(0, 200, 0); }\n";

/// A program that draws a flow surface `w1` showing `web-surface` (node `t`) and closes it,
/// keeping it in scrollback.
fn web_surface_program(env: &TestEnv) -> String {
    let tsp = env.root.join("bin/websurface");
    std::fs::create_dir_all(tsp.parent().unwrap()).expect("mkdir");
    std::fs::write(
        &tsp,
        r#"#!/bin/sh
printf '\033_tsp;o;{"id":"w1","mode":"flow","listen":false}\033\\'
printf '\033_tsp;f;{"sf":"w1","s":1,"ops":[["add","main","w1",null,{"id":"main","k":"col","c":[{"id":"t","k":"text","p":{"text":"web-surface"}}]}]]}\033\\'
printf '\033_tsp;x;{"id":"w1","keep":true}\033\\'
"#,
    )
    .expect("write the surface program");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tsp, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    tsp.display().to_string()
}

/// The lensed command: a stand-in program printing three lines.
fn web_cmd(env: &TestEnv) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = env.root.join("bin/webdemo-cmd");
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
    std::fs::write(&path, "#!/bin/sh\nprintf 'one\\ntwo\\nthree\\n'\n").expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    format!("{} go", path.display())
}

/// The browser client's session ([`auk_web::session::Session`]) over a real WebSocket to `auk
/// web serve`: what the wasm client runs, without the page.
struct WebSession {
    ws: WsClient,
    s: auk_web::session::Session,
}

impl WebSession {
    fn connect(server: &WebServer) -> WebSession {
        let cookie = format!("auk_session={}", server.login());
        let host = server.host();
        let origin = format!("http://{host}");
        let ws = WsClient::connect(
            server.port,
            &[("Host", &host), ("Origin", &origin), ("Cookie", &cookie)],
        )
        .expect("upgrade");
        let s = auk_web::session::Session::new("main", ClientFacts::default());
        WebSession { ws, s }
    }

    fn flush(&mut self) {
        for frame in self.s.take_outbox() {
            self.ws.send_binary(&frame);
        }
    }

    /// Feeds the session messages until `f` finds what it waits for.
    fn until<T>(
        &mut self,
        what: &str,
        mut f: impl FnMut(&mut auk_web::session::Session) -> Option<T>,
    ) -> T {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.flush();
            if let Some(t) = f(&mut self.s) {
                return t;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            match self.ws.recv() {
                Some(WsMessage::Binary(bytes)) => {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs_f64();
                    self.s.set_time(now);
                    self.s.on_message(&bytes);
                    assert!(self.s.errors.is_empty(), "{:?}", self.s.errors);
                }
                Some(_) => {}
                None => panic!("the connection ended waiting for {what}"),
            }
        }
    }
}

/// The newest surface with a node whose text starts with `prefix`: the surface's id and the
/// node's.
fn node_with_text(tsp: &auk_tsp::TspPane, prefix: &str) -> Option<(String, String)> {
    fn walk(n: auk_tsp::NodeRef<'_>, prefix: &str) -> Option<String> {
        if n.props()
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|t| t.starts_with(prefix))
        {
            return Some(n.id().to_owned());
        }
        n.children().find_map(|c| walk(c, prefix))
    }
    tsp.surfaces()
        .iter()
        .rev()
        .find_map(|s| walk(s.document().root(), prefix).map(|id| (s.id().to_owned(), id)))
}

/// A pane drawn by the web client's drawing code at 80x24 cells of 8x16 px (Ahem, so text
/// measures the same everywhere).
fn web_draw(s: &mut auk_web::session::Session, d: &mut auk_web::pane::PaneDraw, pane: u64) {
    let (_, changes) = s.take_changes(pane);
    let p = &s.panes[&pane];
    for c in &changes {
        d.sync(p.replica.tsp(), c, 0.0);
    }
    d.layout(&p.replica, (640.0, 384.0), true, 0.0);
    d.arrange(&p.replica, 16.0, 384.0);
}

fn new_web_draw() -> auk_web::pane::PaneDraw {
    const AHEM: &[u8] = include_bytes!("../../../fixtures/fonts/Ahem.ttf");
    let settings = auk_views::ViewSettings {
        cell: (8.0, 16.0),
        ..auk_views::ViewSettings::default()
    };
    auk_web::pane::PaneDraw::new(settings, 1.0, Some(std::sync::Arc::new(AHEM.to_vec())))
}

fn center(rect: [f32; 4]) -> auk_web::pane::Press {
    auk_web::pane::Press {
        x: rect[0] + rect[2] / 2.0,
        y: rect[1] + rect[3] / 2.0,
        clicks: 1,
        ..auk_web::pane::Press::default()
    }
}

/// Sends what a click asked for through the session.
fn web_click(
    s: &mut auk_web::session::Session,
    d: &mut auk_web::pane::PaneDraw,
    pane: u64,
    sf: &str,
    id: &str,
) {
    let p = &s.panes[&pane];
    let rect = d
        .node_rect(p.replica.tsp(), sf, id)
        .unwrap_or_else(|| panic!("{sf}/{id} isn't drawn"));
    let actions = d
        .click(pane, &p.replica, center(rect), 384.0)
        .unwrap_or_else(|| panic!("the click on {sf}/{id} reached no view"));
    assert!(!actions.is_empty(), "the click on {sf}/{id} did nothing");
    for a in actions {
        match a {
            auk_web::pane::Action::Send(ClientMsg::ViewEvent { pane, sf, event }) => {
                s.view_event(pane, &sf, event)
            }
            other => panic!("expected a view event, got {other:?}"),
        }
    }
}

#[test]
fn web_session_gets_manifest_styles_lens_views_blocks_and_effects_through_the_socket() {
    let env = env("webplugins");
    web_plugin(&env);
    let cmd = web_cmd(&env);
    let pane = daemon_with_pane(&env);
    let assets = builds().web_client();
    let server = WebServer::start(&env, &assets, &["--port", "0"]);
    let mut c = WebSession::connect(&server);

    // The welcome brings the catalog, and with it the Ready plugin's manifest styles.
    c.until("the welcome", |s| (s.client_id.is_some()).then_some(()));
    assert_eq!(
        c.s.styles,
        vec![("webdemo".to_owned(), WEB_CSS.to_owned())],
        "{}",
        c.s.catalog
    );
    c.until("the pane's snapshot", |s| {
        s.panes.contains_key(&pane).then_some(())
    });
    c.s.focus(pane);

    // A program's surface: the client draws it in the flow.
    env.run(pane, &web_surface_program(&env));
    let mut d = new_web_draw();
    c.until("the surface", |s| {
        node_with_text(s.panes[&pane].replica.tsp(), "web-surface").filter(|(sf, _)| sf == "w1")
    });
    web_draw(&mut c.s, &mut d, pane);
    assert!(
        d.surface_node_rect(c.s.panes[&pane].replica.tsp(), "w1", "t")
            .is_some(),
        "{d:?}"
    );

    // A lensed command: the block, then its view out of band.
    env.run(pane, &cmd);
    let view = c.until("the lens view", |s| {
        let b = s.panes[&pane].replica.lens_blocks().first()?.clone();
        b.view
            .filter(|v| v.to_string().contains("lens-view 3 lines"))
    });
    assert!(view.to_string().contains("copy-it"), "{view}");

    // The client draws the block with its view and the manifest style, and a click on the
    // view's node is a view event the lens answers with effects.
    d.set_styles(c.s.styles_serial, &c.s.styles);
    web_draw(&mut c.s, &mut d, pane);
    assert_eq!(d.style_ids(), ["webdemo"]);
    assert_eq!(d.lens_placed.len(), 1, "{d:?}");
    assert!(
        d.lens_node_rect(0, "go").is_some(),
        "the lens view is drawn"
    );
    web_click(&mut c.s, &mut d, pane, "lens:0", "go");
    let effects = c.until("the lens's effects", |s| {
        let e = s.take_effects();
        (!e.is_empty()).then_some(e)
    });
    let mut effects = effects;
    if effects.len() < 2 {
        effects.extend(c.until("the toast", |s| {
            let e = s.take_effects();
            (!e.is_empty()).then_some(e)
        }));
    }
    assert_eq!(
        effects,
        vec![
            auk_mirror::Effect::Copy {
                text: "copied-from-lens".into()
            },
            auk_mirror::Effect::Toast {
                level: "success".into(),
                text: "Copied from the lens".into(),
                sub: Some("webdemo".into()),
            },
        ]
    );

    // A plugin block opens in the attached window key: a pane with a screen surface the client
    // draws, whose clicks reach the block.
    let block = env.daemon("block webdemo.panel --window main")["pane"]
        .as_u64()
        .expect("a block pane");
    let (sf, id) = c.until("the block's view", |s| {
        node_with_text(s.panes.get(&block)?.replica.tsp(), "block-panel 0")
    });
    // A12: the client's tab bar is the app's chrome widgets: the block's session shown (its one
    // tab), the chip for two sessions, whose menu lists both, and nothing that creates.
    let mut bar = c.s.chrome_bar(Some(block), None);
    (bar.width, bar.height, bar.dark) = (800.0, 500.0, true);
    let block_tab = c.s.tab(Some(block)).expect("the block's tab").id;
    let ids: Vec<u64> = c.s.window.sessions.iter().map(|s| s.id).collect();
    assert_eq!(ids.len(), 2, "{:?}", c.s.window);
    let mut chrome = auk_views::chrome::ChromeDoc::new(2.0, "");
    assert!(chrome.update(&bar));
    let acts: Vec<String> = chrome.actions().into_iter().map(|(a, _)| a).collect();
    assert_eq!(acts, ["session-menu".to_owned(), format!("tab:{block_tab}")], "{acts:?}");
    let (_, r) = chrome.actions().into_iter().find(|(a, _)| a.starts_with("tab:")).unwrap();
    assert_eq!(
        chrome.action_at(r.x + r.w / 2.0, r.y + r.h / 2.0),
        Some(auk_views::chrome::ChromeAction::Tab(block_tab))
    );
    bar.session_menu = true;
    chrome.update(&bar);
    let acts: Vec<String> = chrome.actions().into_iter().map(|(a, _)| a).collect();
    for id in &ids {
        assert!(acts.contains(&format!("session:{id}")), "{acts:?}");
    }
    assert!(!acts.iter().any(|a| a.ends_with("-session") || a == "new-tab"), "{acts:?}");
    assert!(
        chrome
            .accessible()
            .iter()
            .any(|n| n.role == "tab" && n.selected && n.action == Some(format!("tab:{block_tab}")))
    );
    // The first session's pane is what its row shows.
    assert_eq!(c.s.session_pane(ids[0], None), Some(pane));
    c.s.focus(block);
    let mut bd = new_web_draw();
    web_draw(&mut c.s, &mut bd, block);
    let placed = bd.surfaces_placed(c.s.panes[&block].replica.tsp());
    assert_eq!(
        placed.first().map(|p| p.0.as_str()),
        Some(sf.as_str()),
        "{placed:?}"
    );
    web_click(&mut c.s, &mut bd, block, &sf, &id);
    c.until("the block to re-render", |s| {
        node_with_text(s.panes.get(&block)?.replica.tsp(), "block-panel 1")
    });
    let toast = c.until("the block's toast", |s| {
        s.take_effects()
            .into_iter()
            .find(|e| matches!(e, auk_mirror::Effect::Toast { .. }))
    });
    assert_eq!(
        toast,
        auk_mirror::Effect::Toast {
            level: "info".into(),
            text: "bumped 1".into(),
            sub: None
        }
    );
}
