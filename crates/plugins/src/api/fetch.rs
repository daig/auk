//! `tern.fetch`: one HTTP/1.1 request on its own thread; 30 s
//! default timeout covering redirects; up to 10 redirects; a 16 MiB body; network failures never
//! raise. The system trust store and the `*_PROXY` variables are used.

use std::collections::BTreeMap;
use std::time::Duration;

use mlua::{Function, Lua, Table, Value};

use super::ApiState;
use super::tasks::{Completer, Outcome};

/// Largest response body.
pub const MAX_BODY: u64 = 16 * 1024 * 1024;
/// The default timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// A finished request.
#[derive(Debug, Clone, Default)]
pub struct FetchResult {
    /// HTTP status; 0 with `error`.
    pub status: u16,
    /// Headers by lowercase name, repeats joined with `", "`.
    pub headers: BTreeMap<String, String>,
    /// The body.
    pub body: Vec<u8>,
    /// Why no whole response arrived.
    pub error: Option<String>,
    /// Whether the timeout ended it.
    pub timed_out: bool,
}

impl FetchResult {
    pub(crate) fn to_lua(&self, lua: &Lua) -> mlua::Result<Value> {
        let t = lua.create_table()?;
        t.set("status", f64::from(self.status))?;
        let headers = lua.create_table()?;
        for (k, v) in &self.headers {
            headers.set(k.as_str(), v.as_str())?;
        }
        t.set("headers", headers)?;
        t.set("body", lua.create_string(&self.body)?)?;
        if let Some(e) = &self.error {
            t.set("error", e.as_str())?;
        }
        t.set("timed_out", self.timed_out)?;
        Ok(Value::Table(t))
    }

    fn failed(error: String, timed_out: bool) -> Self {
        FetchResult {
            error: Some(error),
            timed_out,
            ..FetchResult::default()
        }
    }
}

/// One agent per VM, so connections are kept alive per VM.
pub(crate) fn agent() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(10)
        .tls_config(tls)
        .proxy(ureq::Proxy::try_from_env())
        .build()
        .new_agent()
}

struct Prepared {
    request: ureq::http::Request<Option<Vec<u8>>>,
    timeout: Duration,
}

fn prepare(url: &str, opts: Option<&Table>) -> mlua::Result<Prepared> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(mlua::Error::runtime(format!(
            "tern.fetch {url}: not an absolute http or https URL"
        )));
    }
    let bad = |e: &dyn std::fmt::Display| mlua::Error::runtime(format!("tern.fetch {url}: {e}"));
    let mut method = "GET".to_owned();
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut body: Option<Vec<u8>> = None;
    let mut timeout = DEFAULT_TIMEOUT;
    if let Some(t) = opts {
        if let Some(m) = t.get::<Option<String>>("method")? {
            method = m.to_ascii_uppercase();
        }
        if let Some(h) = t.get::<Option<Table>>("headers")? {
            for pair in h.pairs::<String, String>() {
                headers.push(pair?);
            }
            headers.sort();
        }
        body = t
            .get::<Option<mlua::LuaString>>("body")?
            .map(|b| b.as_bytes().to_vec());
        if let Some(ms) = t.get::<Option<f64>>("timeout_ms")?
            && ms > 0.0
        {
            timeout = if ms.is_finite() && ms < 1e12 {
                Duration::from_secs_f64(ms / 1000.0)
            } else {
                Duration::from_secs(10 * 365 * 24 * 3600)
            };
        }
    }
    let method = ureq::http::Method::from_bytes(method.as_bytes()).map_err(|e| bad(&e))?;
    let mut builder = ureq::http::Request::builder().method(method).uri(url);
    let has_agent = headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("user-agent"));
    for (k, v) in &headers {
        let name = ureq::http::HeaderName::from_bytes(k.as_bytes()).map_err(|e| bad(&e))?;
        let value = ureq::http::HeaderValue::from_str(v).map_err(|e| bad(&e))?;
        builder = builder.header(name, value);
    }
    if !has_agent {
        builder = builder.header("user-agent", "Auk");
    }
    let request = builder.body(body).map_err(|e| bad(&e))?;
    Ok(Prepared { request, timeout })
}

fn perform(agent: &ureq::Agent, prepared: Prepared) -> FetchResult {
    let Prepared { request, timeout } = prepared;
    let (parts, body) = request.into_parts();
    let result = match body {
        Some(bytes) => {
            let req = ureq::http::Request::from_parts(parts, bytes);
            let req = agent
                .configure_request(req)
                .timeout_global(Some(timeout))
                .build();
            agent.run(req)
        }
        None => {
            let req = ureq::http::Request::from_parts(parts, ());
            let req = agent
                .configure_request(req)
                .timeout_global(Some(timeout))
                .build();
            agent.run(req)
        }
    };
    let mut response = match result {
        Ok(r) => r,
        Err(ureq::Error::Timeout(_)) => {
            return FetchResult::failed("the request timed out".into(), true);
        }
        Err(e) => return FetchResult::failed(e.to_string(), false),
    };
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in response.headers() {
        let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
        headers
            .entry(name.as_str().to_ascii_lowercase())
            .and_modify(|v| {
                v.push_str(", ");
                v.push_str(&value);
            })
            .or_insert(value);
    }
    let status = response.status().as_u16();
    match response
        .body_mut()
        .with_config()
        // ureq refuses a body that reaches its limit; a body of exactly 16 MiB is within the cap.
        .limit(MAX_BODY + 1)
        .read_to_vec()
    {
        Ok(body) if body.len() as u64 > MAX_BODY => {
            FetchResult::failed("the body is over 16 MiB".into(), false)
        }
        Ok(body) => FetchResult {
            status,
            headers,
            body,
            error: None,
            timed_out: false,
        },
        Err(ureq::Error::BodyExceedsLimit(_)) => {
            FetchResult::failed("the body is over 16 MiB".into(), false)
        }
        Err(ureq::Error::Timeout(_)) => FetchResult::failed("the request timed out".into(), true),
        Err(e) => FetchResult::failed(e.to_string(), false),
    }
}

fn start(agent: ureq::Agent, prepared: Prepared, completer: Completer) {
    let _ = std::thread::Builder::new()
        .name("plugin fetch".into())
        .spawn(move || {
            let result = perform(&agent, prepared);
            completer.complete(Outcome::Fetch(result));
        });
}

/// `tern.fetch(url, opts?, cb?)`.
pub(super) fn fetch(
    lua: &Lua,
    state: &std::rc::Rc<ApiState>,
    args: mlua::MultiValue,
) -> mlua::Result<Value> {
    const BAD: &str = "tern.fetch(url, opts?, cb): bad arguments";
    let args: Vec<Value> = args.into_iter().collect();
    let (url, opts, cb): (mlua::LuaString, Option<Table>, Option<Function>) = match args.as_slice()
    {
        [Value::String(u)] | [Value::String(u), Value::Nil] => (u.clone(), None, None),
        [Value::String(u), Value::Table(o)] | [Value::String(u), Value::Table(o), Value::Nil] => {
            (u.clone(), Some(o.clone()), None)
        }
        [Value::String(u), Value::Function(f)]
        | [Value::String(u), Value::Nil, Value::Function(f)] => (u.clone(), None, Some(f.clone())),
        [Value::String(u), Value::Table(o), Value::Function(f)] => {
            (u.clone(), Some(o.clone()), Some(f.clone()))
        }
        _ => return Err(mlua::Error::runtime(BAD)),
    };
    let url = url.to_str()?.to_owned();
    let prepared = prepare(&url, opts.as_ref())?;
    let agent = state.fetch_agent();
    let callback = cb.map(|f| ("fetch", f));
    let awaitable = callback.is_none();
    let (id, completer) = state.tasks.borrow_mut().start(callback);
    start(agent, prepared, completer);
    if awaitable {
        super::awaitable(lua, state, "fetch", id)
    } else {
        Ok(Value::Nil)
    }
}
