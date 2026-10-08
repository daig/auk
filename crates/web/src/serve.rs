//! `auk web serve` (§10.3, §12.5): serves the browser client for this machine's daemon.
//!
//! - **Binding:** `127.0.0.1:8377` by default. A non-loopback address is refused unless
//!   `--allow-remote` says so explicitly.
//! - **Auth:** the printed URL carries a one-time 256-bit token. The first page load trades it
//!   for an `HttpOnly`, `SameSite=Strict` session cookie and redirects to the same URL without
//!   the token, so it never stays in the address bar or history. Everything else needs the
//!   cookie.
//! - **Rebinding and cross-site requests:** on a loopback bind, requests whose `Host` isn't the
//!   loopback address and port are refused, and the WebSocket also needs a same-origin `Origin`.
//! - **Replication:** `/ws` is a WebSocket that bridges to the daemon socket. Each binary
//!   message carries exactly one frame of the replication protocol (§15.6) in each direction,
//!   unchanged, so the browser speaks the same protocol as native clients and the daemon
//!   refuses a client of another version itself (X9).
//!
//! The client's files (`auk_web.js` and `auk_web_bg.wasm`, the output of `wasm-bindgen
//! --target web` over the `auk-web` crate built for `wasm32-unknown-unknown`) come from the
//! assets directory: `--assets`, else `$AUK_WEB_ASSETS`, else `web` beside the executable (in
//! the app bundle, `Contents/Resources/web`). The grid's font is the system's monospace font,
//! served to the page, since the browser client can't read local fonts (§18.3).

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Write as _;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use auk_mirror::frame::split_frame;
use axum::Router;
use axum::body::Body;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::BytesMut;
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Tern's binding, which we keep (§10.3).
pub const DEFAULT_BIND: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8377);

/// The session cookie's name.
pub const COOKIE: &str = "auk_session";

/// The page. The script is a separate file so the content security policy needs no inline code.
const INDEX_HTML: &str = include_str!("../static/index.html");
const BOOT_JS: &str = include_str!("../static/boot.js");

/// Fonts tried for the grid, in order: the first that exists is served.
const FONT_CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/Menlo.ttc",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
    "/usr/share/fonts/dejavu-sans-mono-fonts/DejaVuSansMono.ttf",
];

/// `auk web serve`'s command line.
#[derive(Debug, Parser)]
#[command(
    name = "auk web serve",
    about = "Serve the browser client for this machine's daemon"
)]
pub struct Args {
    /// The address to listen on.
    #[arg(long, default_value_t = DEFAULT_BIND, conflicts_with = "port")]
    pub bind: SocketAddr,
    /// Listen on this port of the loopback address instead (0 picks a free one).
    #[arg(long)]
    pub port: Option<u16>,
    /// Allow a non-loopback `--bind`. Anyone who can reach the address and has the URL's token
    /// gets your shells.
    #[arg(long)]
    pub allow_remote: bool,
    /// The daemon socket (default: $AUK_DAEMON_SOCKET, else daemon.sock in the runtime dir).
    #[arg(long)]
    pub socket: Option<PathBuf>,
    /// The client's files: `auk_web.js` and `auk_web_bg.wasm` (default: $AUK_WEB_ASSETS, else
    /// `web` beside the executable).
    #[arg(long)]
    pub assets: Option<PathBuf>,
    /// The grid's font file (default: the system's monospace font).
    #[arg(long)]
    pub font: Option<PathBuf>,
}

#[derive(Debug, Parser)]
#[command(name = "auk web", about = "The browser client")]
struct WebCli {
    #[command(subcommand)]
    command: WebCommand,
}

#[derive(Debug, clap::Subcommand)]
enum WebCommand {
    /// Serve the browser client for this machine's daemon (§10.3).
    Serve(Args),
}

/// Runs `auk web …`; `args` starts with the program name, then `web`.
pub fn main(args: Vec<OsString>) -> ExitCode {
    // clap sees `auk web serve …` as the program `auk web` with its subcommands.
    let mut argv = vec![OsString::from("auk web")];
    argv.extend(args.into_iter().skip(2));
    let cli = match WebCli::try_parse_from(argv) {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            return ExitCode::from(if err.use_stderr() { 2 } else { 0 });
        }
    };
    let WebCommand::Serve(args) = cli.command;
    match serve(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("auk web serve: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Refuses a non-loopback address unless `allow_remote` (§12.5).
pub fn check_bind(addr: SocketAddr, allow_remote: bool) -> Result<(), String> {
    if addr.ip().is_loopback() || allow_remote {
        Ok(())
    } else {
        Err(format!(
            "refusing to listen on {addr}, which isn't a loopback address: anyone who can reach \
             it could try for your shells. Pass --allow-remote to allow it."
        ))
    }
}

/// A fresh 256-bit secret as 64 hex digits.
fn secret() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| format!("no system randomness: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Where the client's files are by default.
fn default_assets() -> PathBuf {
    if let Some(dir) = std::env::var_os("AUK_WEB_ASSETS").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    let exe = std::env::current_exe().unwrap_or_default();
    let dir = exe.parent().unwrap_or(Path::new("."));
    let bundled = dir.join("../Resources/web");
    if bundled.is_dir() {
        bundled
    } else {
        dir.join("web")
    }
}

struct Assets {
    js: Vec<u8>,
    wasm: Vec<u8>,
    font: Vec<u8>,
}

impl Assets {
    fn load(dir: &Path, font: Option<&Path>) -> Result<Assets, String> {
        let read = |name: &str| {
            let path = dir.join(name);
            std::fs::read(&path).map_err(|e| {
                format!(
                    "the web client isn't at {} ({e}). Build it with `cargo build --release -p \
                     auk-web --target wasm32-unknown-unknown`, run `wasm-bindgen --target web \
                     --out-dir DIR` on auk_web.wasm, and pass --assets DIR.",
                    path.display()
                )
            })
        };
        let js = read("auk_web.js")?;
        let wasm = read("auk_web_bg.wasm")?;
        let font_path = match font {
            Some(path) => path.to_owned(),
            None => FONT_CANDIDATES
                .iter()
                .map(PathBuf::from)
                .find(|p| p.is_file())
                .ok_or_else(|| {
                    format!("no monospace font found (tried {FONT_CANDIDATES:?}); pass --font")
                })?,
        };
        let font = std::fs::read(&font_path)
            .map_err(|e| format!("can't read the font {}: {e}", font_path.display()))?;
        Ok(Assets { js, wasm, font })
    }
}

struct Server {
    /// The URL's token, until a page load trades it for a cookie.
    token: Mutex<Option<String>>,
    sessions: Mutex<HashSet<String>>,
    socket: PathBuf,
    assets: Assets,
    /// The `Host` headers accepted; `None` accepts any (a remote bind).
    hosts: Option<Vec<String>>,
}

impl Server {
    fn host_ok(&self, headers: &HeaderMap) -> bool {
        let Some(hosts) = &self.hosts else {
            return true;
        };
        header_str(headers, header::HOST).is_some_and(|h| hosts.iter().any(|ok| ok == h))
    }

    fn session(&self, headers: &HeaderMap) -> bool {
        let sessions = lock(&self.sessions);
        headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(';'))
            .filter_map(|kv| kv.trim().split_once('='))
            .any(|(k, v)| k == COOKIE && sessions.iter().any(|s| eq_ct(s, v)))
    }

    /// Checks `Host` and the session cookie; the refusal's status and message otherwise.
    fn admit(&self, headers: &HeaderMap) -> Result<(), (StatusCode, &'static str)> {
        if !self.host_ok(headers) {
            return Err((StatusCode::FORBIDDEN, "unexpected Host header"));
        }
        if !self.session(headers) {
            return Err((
                StatusCode::UNAUTHORIZED,
                "Open the URL that `auk web serve` printed.",
            ));
        }
        Ok(())
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Compares secrets in time independent of where they differ.
fn eq_ct(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn header_str(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn text(status: StatusCode, body: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        format!("{body}\n"),
    )
        .into_response()
}

/// Headers every successful response carries.
fn secure(mut response: Response, content_type: &'static str) -> Response {
    let h = response.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    response
}

fn serve(args: Args) -> Result<(), String> {
    let bind = match args.port {
        Some(port) => SocketAddr::new(DEFAULT_BIND.ip(), port),
        None => args.bind,
    };
    check_bind(bind, args.allow_remote)?;
    let socket = args.socket.unwrap_or_else(auk_daemon::paths::daemon_socket);
    let assets_dir = args.assets.unwrap_or_else(default_assets);
    let assets = Assets::load(&assets_dir, args.font.as_deref())?;
    let token = secret()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("start the runtime: {e}"))?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(bind)
            .await
            .map_err(|e| format!("listen on {bind}: {e}"))?;
        let local = listener
            .local_addr()
            .map_err(|e| format!("listen on {bind}: {e}"))?;
        let hosts = local.ip().is_loopback().then(|| {
            let port = local.port();
            vec![
                format!("{local}"),
                format!("localhost:{port}"),
                format!("127.0.0.1:{port}"),
                format!("[::1]:{port}"),
            ]
        });
        let url_host = if local.ip().is_unspecified() {
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), local.port())
        } else {
            local
        };
        let server = Arc::new(Server {
            token: Mutex::new(Some(token.clone())),
            sessions: Mutex::new(HashSet::new()),
            socket: socket.clone(),
            assets,
            hosts,
        });
        let app = Router::new()
            .route("/", get(index))
            .route("/boot.js", get(boot_js))
            .route("/auk_web.js", get(client_js))
            .route("/auk_web_bg.wasm", get(client_wasm))
            .route("/font/{n}", get(font))
            .route("/ws", get(ws))
            .fallback(|| async { text(StatusCode::NOT_FOUND, "not found") })
            .with_state(server);
        if !local.ip().is_loopback() {
            eprintln!(
                "auk web serve: listening on {local}, beyond this machine (--allow-remote). \
                 Traffic is plain HTTP."
            );
        }
        eprintln!(
            "auk web serve: the browser client for the daemon at {}",
            socket.display()
        );
        // The URL alone on stdout's first line, for scripts.
        let mut out = std::io::stdout();
        let _ = writeln!(out, "http://{url_host}/?token={token}");
        let _ = out.flush();
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown())
            .await
            .map_err(|e| format!("serve: {e}"))
    })
}

async fn shutdown() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install a SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

#[derive(Debug, serde::Deserialize)]
struct IndexQuery {
    token: Option<String>,
}

/// The page. With the URL's token, trades it for a session cookie and redirects to the URL
/// without it.
async fn index(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Query(query): Query<IndexQuery>,
    uri: axum::http::Uri,
) -> Response {
    if !server.host_ok(&headers) {
        return text(StatusCode::FORBIDDEN, "unexpected Host header");
    }
    if let Some(offered) = query.token {
        let traded = {
            let mut token = lock(&server.token);
            match token.as_deref() {
                Some(t) if eq_ct(t, &offered) => {
                    *token = None;
                    true
                }
                _ => false,
            }
        };
        if !traded {
            // A used or wrong token. A browser that already traded it still has its cookie.
            if server.session(&headers) {
                return redirect_without_token(&uri, None);
            }
            return text(
                StatusCode::FORBIDDEN,
                "This link's token was already used or is wrong. Restart `auk web serve` for a \
                 new one.",
            );
        }
        let session = match secret() {
            Ok(s) => s,
            Err(e) => return text(StatusCode::INTERNAL_SERVER_ERROR, &e),
        };
        lock(&server.sessions).insert(session.clone());
        return redirect_without_token(&uri, Some(&session));
    }
    if let Err((status, why)) = server.admit(&headers) {
        return text(status, why);
    }
    let host = header_str(&headers, header::HOST).unwrap_or_default();
    let mut response = secure(
        Response::new(Body::from(INDEX_HTML)),
        "text/html; charset=utf-8",
    );
    // Same-origin scripts, the wasm module, and the WebSocket back to this server only.
    let csp = format!(
        "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; connect-src 'self' ws://{host}; \
         style-src 'unsafe-inline'; img-src 'self'; base-uri 'none'; form-action 'none'; \
         frame-ancestors 'none'"
    );
    if let Ok(v) = HeaderValue::from_str(&csp) {
        response
            .headers_mut()
            .insert(header::CONTENT_SECURITY_POLICY, v);
    }
    response
}

/// A redirect to `uri` without its `token` parameter, setting the session cookie if given.
fn redirect_without_token(uri: &axum::http::Uri, session: Option<&str>) -> Response {
    let rest: Vec<&str> = uri
        .query()
        .unwrap_or_default()
        .split('&')
        .filter(|kv| !kv.is_empty() && !kv.starts_with("token=") && *kv != "token")
        .collect();
    let location = if rest.is_empty() {
        uri.path().to_owned()
    } else {
        format!("{}?{}", uri.path(), rest.join("&"))
    };
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::SEE_OTHER;
    let h = response.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&location) {
        h.insert(header::LOCATION, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    if let Some(session) = session
        && let Ok(v) = HeaderValue::from_str(&format!(
            "{COOKIE}={session}; HttpOnly; SameSite=Strict; Path=/"
        ))
    {
        h.insert(header::SET_COOKIE, v);
    }
    response
}

async fn boot_js(State(server): State<Arc<Server>>, headers: HeaderMap) -> Response {
    asset(&server, &headers, BOOT_JS.as_bytes(), "text/javascript")
}

async fn client_js(State(server): State<Arc<Server>>, headers: HeaderMap) -> Response {
    asset(&server, &headers, &server.assets.js, "text/javascript")
}

async fn client_wasm(State(server): State<Arc<Server>>, headers: HeaderMap) -> Response {
    asset(&server, &headers, &server.assets.wasm, "application/wasm")
}

async fn font(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    UrlPath(n): UrlPath<u32>,
) -> Response {
    if n != 0 {
        return text(StatusCode::NOT_FOUND, "no such font");
    }
    // A TrueType collection (Menlo.ttc) or a single font.
    let kind = if server.assets.font.starts_with(b"ttcf") {
        "font/collection"
    } else if server.assets.font.starts_with(b"OTTO") {
        "font/otf"
    } else {
        "font/ttf"
    };
    asset(&server, &headers, &server.assets.font, kind)
}

fn asset(
    server: &Server,
    headers: &HeaderMap,
    bytes: &[u8],
    content_type: &'static str,
) -> Response {
    if let Err((status, why)) = server.admit(headers) {
        return text(status, why);
    }
    secure(Response::new(Body::from(bytes.to_vec())), content_type)
}

/// The replication WebSocket.
async fn ws(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if let Err((status, why)) = server.admit(&headers) {
        return text(status, why);
    }
    // Cookies alone don't stop another site's page from opening a WebSocket here.
    let host = header_str(&headers, header::HOST).unwrap_or_default();
    let origin = header_str(&headers, header::ORIGIN).unwrap_or_default();
    if origin != format!("http://{host}") {
        return text(StatusCode::FORBIDDEN, "cross-origin WebSocket refused");
    }
    let socket = server.socket.clone();
    upgrade
        .max_message_size(auk_mirror::MAX_PAYLOAD + auk_mirror::frame::HEADER_LEN)
        .on_upgrade(move |ws| bridge(ws, socket))
}

/// Bridges a WebSocket to the daemon socket, one replication frame per message.
async fn bridge(ws: WebSocket, path: PathBuf) {
    let (tx, mut rx) = ws.split();
    let tx_cell = tokio::sync::Mutex::new(tx);
    let tx = &tx_cell;
    let close = move |code: u16, reason: String| async move {
        let _ = tx
            .lock()
            .await
            .send(Message::Close(Some(CloseFrame {
                code,
                reason: truncate(&reason).into(),
            })))
            .await;
    };
    let daemon = match UnixStream::connect(&path).await {
        Ok(stream) => stream,
        Err(err) => {
            close(1011, format!("no daemon at {}: {err}", path.display())).await;
            return;
        }
    };
    let (mut rd, mut wr) = daemon.into_split();
    let up = async {
        while let Some(Ok(msg)) = rx.next().await {
            match msg {
                Message::Binary(bytes) => {
                    // Exactly one whole frame per message.
                    match split_frame(&bytes) {
                        Ok(Some((_, used))) if used == bytes.len() => {}
                        _ => return Some("each message must be one replication frame"),
                    }
                    if wr.write_all(&bytes).await.is_err() {
                        return None;
                    }
                }
                Message::Close(_) => return None,
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Text(_) => return Some("text messages aren't part of the protocol"),
            }
        }
        None
    };
    let down = async {
        let mut buf = BytesMut::with_capacity(64 << 10);
        loop {
            loop {
                match split_frame(&buf) {
                    Ok(Some((_, used))) => {
                        let frame = buf.split_to(used).freeze();
                        if tx.lock().await.send(Message::Binary(frame)).await.is_err() {
                            return None;
                        }
                    }
                    Ok(None) => break,
                    Err(err) => return Some((1011, err.to_string())),
                }
            }
            match rd.read_buf(&mut buf).await {
                Ok(0) | Err(_) => return Some((1000, "the daemon closed the connection".into())),
                Ok(_) => {}
            }
        }
    };
    tokio::select! {
        reason = up => {
            if let Some(reason) = reason {
                close(1008, reason.to_owned()).await;
            }
        }
        end = down => {
            if let Some((code, reason)) = end {
                close(code, reason).await;
            }
        }
    }
}

/// Close reasons are limited to 123 bytes.
fn truncate(s: &str) -> String {
    let mut end = s.len().min(120);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bind_is_tern_loopback_port() {
        assert_eq!(DEFAULT_BIND.to_string(), "127.0.0.1:8377");
        assert!(check_bind(DEFAULT_BIND, false).is_ok());
    }

    #[test]
    fn non_loopback_needs_the_flag() {
        for addr in ["0.0.0.0:8377", "[::]:8377", "192.168.1.2:1"] {
            let addr: SocketAddr = addr.parse().unwrap();
            let err = check_bind(addr, false).unwrap_err();
            assert!(err.contains("--allow-remote"), "{err}");
            assert!(check_bind(addr, true).is_ok());
        }
        assert!(check_bind("[::1]:1".parse().unwrap(), false).is_ok());
    }

    #[test]
    fn secrets_are_256_bits_of_hex() {
        let a = secret().unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(a, secret().unwrap());
    }

    #[test]
    fn constant_time_compare() {
        assert!(eq_ct("abc", "abc"));
        assert!(!eq_ct("abc", "abd"));
        assert!(!eq_ct("abc", "ab"));
    }
}
