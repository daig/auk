//! Harness for the `web` suite (§10.3): building the browser client fresh, running
//! `auk web serve`, plain HTTP and WebSocket clients for the server, and a minimal WebDriver
//! client for Safari (`safaridriver`).
//!
//! Everything the suite runs is built in the run: the client (`cargo build --release -p auk-web
//! --target wasm32-unknown-unknown`, then the wasm-bindgen CLI at the exact version
//! `Cargo.lock` pins for the `wasm-bindgen` crate), and the wasm32-wasip1 replica driver.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};

use crate::harness::TestEnv;

/// The workspace's root directory.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root")
}

/// Cargo, as the one running the tests when known.
fn cargo() -> Command {
    let mut cmd = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cmd.current_dir(workspace_root());
    cmd
}

fn run(cmd: &mut Command, what: &str) {
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("{what}: can't run {cmd:?}: {e}"));
    assert!(
        out.status.success(),
        "{what} failed ({}):\n{}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The version of a crate in `Cargo.lock`.
pub fn locked_version(name: &str) -> String {
    let lock = std::fs::read_to_string(workspace_root().join("Cargo.lock")).expect("Cargo.lock");
    let needle = format!("name = \"{name}\"\nversion = \"");
    let start = lock
        .find(&needle)
        .unwrap_or_else(|| panic!("{name} isn't in Cargo.lock"))
        + needle.len();
    lock[start..]
        .split('"')
        .next()
        .expect("a version")
        .to_owned()
}

/// Where the suite builds things: beside the test's own target dir (`CARGO_TARGET_TMPDIR` is
/// `<target>/tmp`), in directories of its own so it never contends with the workspace build.
#[derive(Debug, Clone)]
pub struct Builds {
    pub root: PathBuf,
}

impl Builds {
    /// `target_tmpdir` is the test's `env!("CARGO_TARGET_TMPDIR")`.
    pub fn new(target_tmpdir: &str) -> Builds {
        let target = Path::new(target_tmpdir)
            .parent()
            .expect("CARGO_TARGET_TMPDIR is inside the target dir");
        Builds {
            root: target.join("web-suite"),
        }
    }

    fn target_dir(&self) -> PathBuf {
        self.root.join("target")
    }

    /// The wasm-bindgen CLI at the version `Cargo.lock` pins, installed (built from source by
    /// cargo) under the suite's directory. cargo keeps it while the version matches.
    pub fn wasm_bindgen(&self) -> PathBuf {
        let version = locked_version("wasm-bindgen");
        let tools = self.root.join("tools");
        let bin = tools.join("bin/wasm-bindgen");
        run(
            cargo()
                .args([
                    "install",
                    "wasm-bindgen-cli",
                    "--locked",
                    "--quiet",
                    "--version",
                ])
                .arg(format!("={version}"))
                .arg("--root")
                .arg(&tools)
                .env("CARGO_TARGET_DIR", self.root.join("tools-build")),
            "installing the wasm-bindgen CLI (it needs crates.io the first time)",
        );
        let out = Command::new(&bin)
            .arg("--version")
            .output()
            .unwrap_or_else(|e| panic!("run {}: {e}", bin.display()));
        let reported = String::from_utf8_lossy(&out.stdout);
        assert!(
            reported.split_whitespace().any(|w| w == version),
            "wasm-bindgen CLI reports {reported:?}, but the crate is {version}"
        );
        bin
    }

    /// Builds the browser client and its JS glue; returns the assets dir for `--assets`, which
    /// is removed when the returned value drops.
    pub fn web_client(&self) -> ClientAssets {
        let target = self.target_dir();
        run(
            cargo()
                .args(["build", "--release", "-p", "auk-web", "--target"])
                .arg("wasm32-unknown-unknown")
                .env("CARGO_TARGET_DIR", &target),
            "building the web client for wasm32-unknown-unknown",
        );
        let wasm = target.join("wasm32-unknown-unknown/release/auk_web.wasm");
        let bindgen = self.wasm_bindgen();
        // Each test process generates its own copy, so parallel tests never see a half-written
        // file.
        let out = self.root.join(format!("assets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        run(
            Command::new(bindgen)
                .args(["--target", "web", "--no-typescript", "--out-dir"])
                .arg(&out)
                .arg(&wasm),
            "generating the client's JS glue with wasm-bindgen",
        );
        for file in ["auk_web.js", "auk_web_bg.wasm"] {
            assert!(out.join(file).is_file(), "wasm-bindgen wrote no {file}");
        }
        ClientAssets { dir: out }
    }

    /// Builds the replica driver (`auk-web`'s `replay` example) for wasm32-wasip1.
    pub fn wasi_replay(&self) -> PathBuf {
        let target = self.target_dir();
        run(
            cargo()
                .args(["build", "--release", "-p", "auk-web", "--example", "replay"])
                .args(["--target", "wasm32-wasip1"])
                .env("CARGO_TARGET_DIR", &target),
            "building the replica driver for wasm32-wasip1 (rustup target add wasm32-wasip1)",
        );
        target.join("wasm32-wasip1/release/examples/replay.wasm")
    }
}

/// The built client's files (`auk_web.js`, `auk_web_bg.wasm`), one copy per test process.
#[derive(Debug)]
pub struct ClientAssets {
    pub dir: PathBuf,
}

impl std::ops::Deref for ClientAssets {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.dir
    }
}

impl Drop for ClientAssets {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A running `auk web serve`.
#[derive(Debug)]
pub struct WebServer {
    child: Child,
    /// The printed URL, token included.
    pub url: String,
    pub port: u16,
    pub token: String,
}

impl WebServer {
    /// Starts `auk web serve ARGS` in the test's environment and reads the URL it prints.
    pub fn start(env: &TestEnv, assets: &Path, args: &[&str]) -> WebServer {
        let mut child = env
            .command()
            .args(["web", "serve", "--assets"])
            .arg(assets)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn auk web serve");
        let mut line = String::new();
        BufReader::new(child.stdout.take().expect("stdout"))
            .read_line(&mut line)
            .expect("read the URL");
        if line.is_empty() {
            let mut err = String::new();
            let _ = child
                .stderr
                .take()
                .expect("stderr")
                .read_to_string(&mut err);
            let _ = child.wait();
            panic!("auk web serve printed no URL: {err}");
        }
        let url = line.trim().to_owned();
        let rest = url.strip_prefix("http://").expect("an http URL");
        let (host, query) = rest.split_once('/').expect("a path");
        let port = host
            .rsplit_once(':')
            .and_then(|(_, p)| p.parse().ok())
            .expect("a port");
        let token = query
            .strip_prefix("?token=")
            .expect("a token parameter")
            .to_owned();
        WebServer {
            child,
            url,
            port,
            token,
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn host(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// Trades the token for a session cookie with a plain HTTP request; returns the cookie's
    /// value.
    pub fn login(&self) -> String {
        let resp = http(
            self.port,
            "GET",
            &format!("/?token={}", self.token),
            &[("Host", &self.host())],
            b"",
        );
        assert_eq!(resp.status, 303, "{resp:?}");
        cookie_value(&resp).expect("a session cookie")
    }
}

impl Drop for WebServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The `auk_session` value a response sets.
pub fn cookie_value(resp: &HttpResponse) -> Option<String> {
    let set = resp.header("set-cookie")?;
    let first = set.split(';').next()?;
    first.strip_prefix("auk_session=").map(str::to_owned)
}

/// An HTTP response.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// A header's value; names compare case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// One HTTP/1.1 request to `127.0.0.1:port`, with `Connection: close`. `headers` should include
/// `Host` (the request is sent exactly as given).
pub fn http(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> HttpResponse {
    let mut stream =
        TcpStream::connect(("127.0.0.1", port)).unwrap_or_else(|e| panic!("connect :{port}: {e}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("a timeout");
    let mut req = format!("{method} {path} HTTP/1.1\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if !body.is_empty() || method == "POST" {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req.push_str("Connection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).expect("send a request");
    stream.write_all(body).expect("send a body");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read a response");
    parse_response(&raw).unwrap_or_else(|| {
        panic!(
            "bad HTTP response to {method} {path}: {:?}",
            String::from_utf8_lossy(&raw)
        )
    })
}

fn parse_head(head: &str) -> Option<(u16, Vec<(String, String)>)> {
    let mut lines = head.split("\r\n");
    let status = lines.next()?.split(' ').nth(1)?.parse().ok()?;
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    Some((status, headers))
}

fn parse_response(raw: &[u8]) -> Option<HttpResponse> {
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let (status, headers) = parse_head(std::str::from_utf8(&raw[..end]).ok()?)?;
    let mut body = raw[end + 4..].to_vec();
    let chunked = headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("transfer-encoding") && v.eq_ignore_ascii_case("chunked")
    });
    if chunked {
        let mut out = Vec::new();
        let mut rest = &body[..];
        loop {
            let line_end = rest.windows(2).position(|w| w == b"\r\n")?;
            let size =
                usize::from_str_radix(std::str::from_utf8(&rest[..line_end]).ok()?.trim(), 16)
                    .ok()?;
            rest = &rest[line_end + 2..];
            if size == 0 {
                break;
            }
            out.extend_from_slice(rest.get(..size)?);
            rest = rest.get(size + 2..)?;
        }
        body = out;
    }
    Some(HttpResponse {
        status,
        headers,
        body,
    })
}

/// What a WebSocket read returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsMessage {
    Binary(Vec<u8>),
    Text(String),
    Close(Option<u16>, String),
}

/// A minimal WebSocket client (RFC 6455) for the server tests.
#[derive(Debug)]
pub struct WsClient {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl WsClient {
    /// Opens `/ws`. `Err` carries the HTTP response when the server doesn't upgrade.
    pub fn connect(port: u16, headers: &[(&str, &str)]) -> Result<WsClient, HttpResponse> {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("a timeout");
        let key = base64::engine::general_purpose::STANDARD.encode(rand16());
        let mut req = format!(
            "GET /ws HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n"
        );
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes()).expect("send the upgrade");
        let mut raw = Vec::new();
        let mut byte = [0u8; 1];
        while !raw.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(1) => raw.push(byte[0]),
                _ => break,
            }
        }
        let (status, headers) =
            parse_head(std::str::from_utf8(&raw).unwrap_or_default().trim_end())
                .unwrap_or((0, Vec::new()));
        if status != 101 {
            // The server may keep the connection open: read just the body it announced.
            let len = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(0);
            let mut body = vec![0; len];
            let _ = stream.read_exact(&mut body);
            return Err(HttpResponse {
                status,
                headers,
                body,
            });
        }
        Ok(WsClient {
            stream,
            buf: Vec::new(),
        })
    }

    /// Sends one binary message (masked, as clients must).
    pub fn send_binary(&mut self, payload: &[u8]) {
        self.send(0x2, payload);
    }

    fn send(&mut self, opcode: u8, payload: &[u8]) {
        let mut frame = vec![0x80 | opcode];
        let len = payload.len();
        if len < 126 {
            frame.push(0x80 | len as u8);
        } else if len <= 0xffff {
            frame.push(0x80 | 126);
            frame.extend((len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend((len as u64).to_be_bytes());
        }
        let mask: [u8; 4] = rand16()[..4].try_into().expect("four bytes");
        frame.extend(mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.stream.write_all(&frame).expect("send a frame");
    }

    fn fill(&mut self, n: usize) -> bool {
        let mut chunk = [0u8; 64 << 10];
        while self.buf.len() < n {
            match self.stream.read(&mut chunk) {
                Ok(0) | Err(_) => return false,
                Ok(k) => self.buf.extend_from_slice(&chunk[..k]),
            }
        }
        true
    }

    /// The next message, or `None` when the connection ended without a close frame.
    pub fn recv(&mut self) -> Option<WsMessage> {
        let mut message = Vec::new();
        let mut first_opcode = None;
        loop {
            if !self.fill(2) {
                return None;
            }
            let (b0, b1) = (self.buf[0], self.buf[1]);
            let mut header = 2;
            let mut len = u64::from(b1 & 0x7f);
            if len == 126 {
                if !self.fill(4) {
                    return None;
                }
                len = u64::from(u16::from_be_bytes([self.buf[2], self.buf[3]]));
                header = 4;
            } else if len == 127 {
                if !self.fill(10) {
                    return None;
                }
                len = u64::from_be_bytes(self.buf[2..10].try_into().expect("eight bytes"));
                header = 10;
            }
            let masked = b1 & 0x80 != 0;
            let mask_len = if masked { 4 } else { 0 };
            let total = header + mask_len + len as usize;
            if !self.fill(total) {
                return None;
            }
            let mut payload = self.buf[header + mask_len..total].to_vec();
            if masked {
                let mask = &self.buf[header..header + 4];
                for (i, b) in payload.iter_mut().enumerate() {
                    *b ^= mask[i % 4];
                }
            }
            self.buf.drain(..total);
            let opcode = b0 & 0x0f;
            match opcode {
                0x9 => {
                    self.send(0xA, &payload);
                    continue;
                }
                0xA => continue,
                0x8 => {
                    let code =
                        (payload.len() >= 2).then(|| u16::from_be_bytes([payload[0], payload[1]]));
                    let reason =
                        String::from_utf8_lossy(payload.get(2..).unwrap_or_default()).into_owned();
                    return Some(WsMessage::Close(code, reason));
                }
                _ => {}
            }
            if opcode != 0 {
                first_opcode = Some(opcode);
            }
            message.extend(payload);
            if b0 & 0x80 != 0 {
                return Some(match first_opcode {
                    Some(0x1) => WsMessage::Text(String::from_utf8_lossy(&message).into_owned()),
                    _ => WsMessage::Binary(message),
                });
            }
        }
    }

    pub fn close(&mut self) {
        self.send(0x8, &1000u16.to_be_bytes());
        let _ = self.stream.shutdown(Shutdown::Write);
    }
}

/// 16 bytes that differ per call (WebSocket keys and masks need no secrecy).
fn rand16() -> [u8; 16] {
    use std::hash::{BuildHasher, Hasher};
    let mut out = [0u8; 16];
    for chunk in out.chunks_mut(8) {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(Instant::now().elapsed().as_nanos());
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    out
}

/// What the browser test needs the user to have done once (PROGRESS.md, "Needs the user").
pub const SAFARI_PREREQUISITE: &str = "Safari automation isn't enabled. Run `sudo safaridriver \
     --enable` once, then in Safari open Settings → Advanced, turn on \"Show features for web \
     developers\", and choose Develop → \"Allow Remote Automation\".";

/// A WebDriver session in Safari, through `safaridriver`.
#[derive(Debug)]
pub struct Safari {
    driver: Child,
    port: u16,
    session: String,
}

impl Safari {
    /// Starts safaridriver and a session; panics naming [`SAFARI_PREREQUISITE`] when Safari
    /// won't be automated.
    pub fn start() -> Safari {
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("a free port")
            .port();
        let driver = Command::new("/usr/bin/safaridriver")
            .args(["-p", &port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("safaridriver can't run ({e}). {SAFARI_PREREQUISITE}"));
        let mut safari = Safari {
            driver,
            port,
            session: String::new(),
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            if let Ok(Some(status)) = safari.driver.try_wait() {
                let mut err = String::new();
                if let Some(mut e) = safari.driver.stderr.take() {
                    let _ = e.read_to_string(&mut err);
                }
                panic!("safaridriver exited ({status}): {err}\n{SAFARI_PREREQUISITE}");
            }
            assert!(
                Instant::now() < deadline,
                "safaridriver didn't start listening"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let (status, value) = safari.request(
            "POST",
            "/session",
            &json!({"capabilities": {"alwaysMatch": {"browserName": "safari"}}}),
        );
        match value["sessionId"].as_str() {
            Some(id) if status == 200 => safari.session = id.to_owned(),
            _ => panic!(
                "Safari didn't start a WebDriver session (HTTP {status}: {}).\n{SAFARI_PREREQUISITE}",
                value
            ),
        }
        safari
    }

    fn request(&self, method: &str, path: &str, body: &Value) -> (u16, Value) {
        let body = if method == "POST" {
            serde_json::to_vec(body).expect("JSON")
        } else {
            Vec::new()
        };
        let host = format!("127.0.0.1:{}", self.port);
        let resp = http(
            self.port,
            method,
            path,
            &[("Host", &host), ("Content-Type", "application/json")],
            &body,
        );
        let value: Value = serde_json::from_slice(&resp.body).unwrap_or(Value::Null);
        (
            resp.status,
            value.get("value").cloned().unwrap_or(Value::Null),
        )
    }

    /// A command in this session; panics on a WebDriver error.
    fn command(&self, method: &str, path: &str, body: &Value) -> Value {
        let full = format!("/session/{}{path}", self.session);
        let (status, value) = self.request(method, &full, body);
        assert!(
            status == 200,
            "WebDriver {method} {path} failed (HTTP {status}): {value}"
        );
        value
    }

    pub fn navigate(&self, url: &str) {
        self.command("POST", "/url", &json!({ "url": url }));
    }

    pub fn current_url(&self) -> String {
        self.command("GET", "/url", &Value::Null)
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    /// Runs a script in the page (`return …;`) and returns its result.
    pub fn execute(&self, script: &str) -> Value {
        self.command(
            "POST",
            "/execute/sync",
            &json!({ "script": script, "args": [] }),
        )
    }

    /// The viewport as PNG bytes.
    pub fn screenshot(&self) -> Vec<u8> {
        let b64 = self.command("GET", "/screenshot", &Value::Null);
        base64::engine::general_purpose::STANDARD
            .decode(b64.as_str().expect("base64 PNG"))
            .expect("a base64 screenshot")
    }

    /// The element a CSS selector finds.
    pub fn element(&self, css: &str) -> String {
        let value = self.command(
            "POST",
            "/element",
            &json!({ "using": "css selector", "value": css }),
        );
        value
            .as_object()
            .and_then(|o| o.values().next())
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("no element for {css}: {value}"))
            .to_owned()
    }

    /// Clicks an element.
    pub fn click(&self, element: &str) {
        self.command("POST", &format!("/element/{element}/click"), &json!({}));
    }

    /// Presses and releases each character of `text` as a key, into whatever has focus
    /// (WebDriver key codes such as `\u{E007}`, Enter, included).
    pub fn press_keys(&self, text: &str) {
        let actions: Vec<Value> = text
            .chars()
            .flat_map(|c| {
                [
                    json!({"type": "keyDown", "value": c.to_string()}),
                    json!({"type": "keyUp", "value": c.to_string()}),
                ]
            })
            .collect();
        self.command(
            "POST",
            "/actions",
            &json!({"actions": [{"type": "key", "id": "keyboard", "actions": actions}]}),
        );
    }
}

impl Drop for Safari {
    fn drop(&mut self) {
        if !self.session.is_empty() {
            let full = format!("/session/{}", self.session);
            let host = format!("127.0.0.1:{}", self.port);
            let _ = std::panic::catch_unwind(|| {
                http(self.port, "DELETE", &full, &[("Host", &host)], b"")
            });
        }
        let _ = self.driver.kill();
        let _ = self.driver.wait();
    }
}

/// A decoded RGBA image.
#[derive(Debug, Clone)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<[u8; 4]>,
}

impl Rgba {
    /// Decodes a PNG.
    pub fn from_png(bytes: &[u8]) -> Rgba {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().expect("a PNG");
        let mut buf = vec![0; reader.output_buffer_size().expect("a buffer size")];
        let info = reader.next_frame(&mut buf).expect("a PNG frame");
        let channels = info.color_type.samples();
        let pixels = buf[..info.buffer_size()]
            .chunks(channels)
            .map(|c| match channels {
                1 => [c[0], c[0], c[0], 255],
                2 => [c[0], c[0], c[0], c[1]],
                3 => [c[0], c[1], c[2], 255],
                _ => [c[0], c[1], c[2], c[3]],
            })
            .collect();
        Rgba {
            width: info.width,
            height: info.height,
            pixels,
        }
    }

    pub fn get(&self, x: u32, y: u32) -> [u8; 4] {
        self.pixels[(y * self.width + x) as usize]
    }
}
