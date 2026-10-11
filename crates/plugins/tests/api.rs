//! The shared API: `tern.json`, `tern.base64`,
//! `tern.kv`, `tern.fs`, `tern.process.run`, `tern.fetch`, `tern.sleep` and awaitables,
//! `tern.ui` and `tern.parse`; plus lens claims on replicas and reloading on change.

mod common;

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use auk_plugins::mlua::Value;
use auk_plugins::{Budget, Context, MEMORY_LIMIT, PluginInfo, Vm, VmOptions};
use serde_json::json;

struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical");
        std::fs::create_dir_all(root.join("pkg")).expect("mkdir");
        Fx { _dir: dir, root }
    }

    fn vm(&self, context: Context) -> Vm {
        Vm::new(VmOptions {
            context,
            plugin: PluginInfo {
                id: "t".into(),
                name: "T".into(),
                dir: self.root.join("pkg"),
                data: self.root.join("data"),
            },
            declared_blocks: vec![],
            declared_lenses: vec![],
            memory_limit: MEMORY_LIMIT,
            jit: true,
            log: Arc::new(|_| {}),
            panes: None,
            builtin_commands: None,
        })
        .expect("vm")
    }
}

fn eval<R: auk_plugins::mlua::FromLuaMulti>(vm: &Vm, src: &str) -> R {
    let f = vm
        .compile(src, &vm.plugin().dir.join("t.luau"))
        .expect("compile");
    vm.call("test", Budget::Host, &f, ())
        .unwrap_or_else(|e| panic!("{src}: {e}"))
}

fn err(vm: &Vm, src: &str) -> String {
    let f = vm
        .compile(src, &vm.plugin().dir.join("t.luau"))
        .expect("compile");
    match vm.call::<()>("test", Budget::Host, &f, ()) {
        // The first line, as a toast shows it (errors raised in Luau carry a traceback).
        Err(e) => e.first_line(),
        Ok(()) => panic!("{src} should raise"),
    }
}

fn json_of(vm: &Vm, src: &str) -> serde_json::Value {
    let v: Value = eval(vm, src);
    vm.to_json(&v).expect("json")
}

/// Runs queued callbacks until `done()` or the deadline.
fn pump(vm: &Vm, within: Duration, done: impl Fn() -> bool) {
    let deadline = Instant::now() + within;
    while !done() {
        assert!(
            Instant::now() < deadline,
            "callbacks didn't finish in {within:?}"
        );
        while let Some(cb) = vm.next_callback(Instant::now()) {
            vm.call::<()>(
                cb.hook,
                Budget::Host,
                &cb.f,
                auk_plugins::mlua::MultiValue::from_iter(cb.args),
            )
            .expect("callback");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn json_and_base64() {
    let fx = Fx::new();
    let vm = fx.vm(Context::Host);
    assert_eq!(
        eval::<String>(
            &vm,
            r#"return tern.json.encode({b = 1, a = {3, 2.5}, c = tern.json.null, [2] = "x"})"#
        ),
        r#"{"2":"x","a":[3,2.5],"b":1,"c":null}"#
    );
    assert_eq!(eval::<String>(&vm, "return tern.json.encode({})"), "{}");
    assert_eq!(
        eval::<String>(&vm, "return tern.json.encode(tern.json.array())"),
        "[]"
    );
    assert_eq!(
        eval::<String>(&vm, "return tern.json.encode(tern.json.array({}))"),
        "[]"
    );
    assert_eq!(
        eval::<String>(&vm, "return tern.json.encode(2^53)"),
        "9007199254740992"
    );
    assert_eq!(
        eval::<String>(&vm, r#"return tern.json.encode({x = 1}, true)"#),
        "{\n  \"x\": 1\n}"
    );
    assert_eq!(
        err(&vm, "tern.json.encode(0/0)"),
        "cannot encode nan as JSON"
    );
    assert_eq!(
        err(&vm, "tern.json.encode(print)"),
        "cannot encode a function as JSON"
    );
    assert_eq!(
        err(&vm, "tern.json.encode({[true] = 1})"),
        "cannot encode a boolean key as JSON"
    );
    assert_eq!(
        err(&vm, "local t = {} t.t = t tern.json.encode(t)"),
        "cannot encode tables nested this deep (or cyclic) as JSON"
    );
    // Decoding: arrays tagged (so they re-encode as arrays), null members dropped, nulls in
    // arrays kept as tern.json.null.
    assert_eq!(
        eval::<String>(
            &vm,
            r#"local v = tern.json.decode('{"a":[],"b":null,"c":[1,null]}') return tern.json.encode(v)"#
        ),
        r#"{"a":[],"c":[1,null]}"#
    );
    assert!(eval::<bool>(
        &vm,
        r#"return tern.json.decode('[null]')[1] == tern.json.null"#
    ));
    assert!(err(&vm, "tern.json.decode('{')").starts_with("tern.json.decode: "));
    // Built-in libraries and tern's namespaces are read-only.
    assert!(err(&vm, "tern.json.encode = nil").contains("readonly"));

    assert_eq!(
        eval::<String>(&vm, r#"return tern.base64.encode("\255\254hi")"#),
        "//5oaQ=="
    );
    assert_eq!(
        eval::<String>(
            &vm,
            r#"return tern.base64.encode("\255\254hi", {url = true})"#
        ),
        "__5oaQ"
    );
    let decoded: auk_plugins::mlua::LuaString =
        eval(&vm, r#"return tern.base64.decode(" __5o\naQ ")"#);
    assert_eq!(decoded.as_bytes().to_vec(), b"\xff\xfehi");
    assert!(err(&vm, r#"tern.base64.decode("@@")"#).starts_with("tern.base64.decode: "));
}

#[test]
fn kv_is_one_file_shared_by_every_vm_of_the_plugin() {
    let fx = Fx::new();
    let host = fx.vm(Context::Host);
    let window = fx.vm(Context::Window);
    assert!(
        eval::<bool>(&host, "return tern.kv.get('x') == nil"),
        "a missing store is empty"
    );
    eval::<()>(&host, "tern.kv.set('x', {n = 1, list = tern.json.array()})");
    assert_eq!(
        json_of(&window, "return tern.kv.get('x')"),
        json!({"list": [], "n": 1})
    );
    // The file is rewritten whole through a rename; readers see the other VM's writes.
    std::thread::sleep(Duration::from_millis(5));
    eval::<()>(&window, "tern.kv.set('y', 'from the window')");
    assert_eq!(
        eval::<String>(&host, "return tern.kv.get('y')"),
        "from the window"
    );
    eval::<()>(&host, "tern.kv.set('x', nil)");
    let text = std::fs::read_to_string(fx.root.join("data/kv.json")).expect("kv");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text).expect("json"),
        json!({"y": "from the window"})
    );
    let leftovers: Vec<_> = std::fs::read_dir(fx.root.join("data"))
        .expect("dir")
        .collect();
    assert_eq!(leftovers.len(), 1, "no temp files left");
    // A broken file raises, and isn't overwritten.
    std::fs::write(fx.root.join("data/kv.json"), "{broken").expect("write");
    assert!(err(&host, "tern.kv.get('y')").starts_with("tern.kv.get: "));
    assert!(err(&host, "tern.kv.set('z', 1)").starts_with("tern.kv.set: "));
    assert_eq!(
        std::fs::read_to_string(fx.root.join("data/kv.json")).expect("kv"),
        "{broken"
    );
}

#[test]
fn fs_is_synchronous_and_relative_to_the_plugin() {
    let fx = Fx::new();
    let vm = fx.vm(Context::Host);
    eval::<()>(
        &vm,
        r#"tern.fs.mkdir("sub/deeper") tern.fs.write("sub/b.txt", "bee") tern.fs.write("sub/a.txt", "a\0b")"#,
    );
    assert_eq!(
        json_of(&vm, r#"return tern.fs.list("sub")"#),
        json!(["a.txt", "b.txt", "deeper"])
    );
    assert_eq!(
        eval::<auk_plugins::mlua::LuaString>(&vm, r#"return tern.fs.read("sub/a.txt")"#)
            .as_bytes()
            .to_vec(),
        b"a\0b"
    );
    assert!(eval::<bool>(
        &vm,
        r#"return tern.fs.exists("sub/b.txt") and not tern.fs.exists("nope")"#
    ));
    assert_eq!(
        eval::<String>(&vm, r#"return tern.fs.read("sub/b.txt", 3)"#),
        "bee"
    );
    let too_big = err(&vm, r#"tern.fs.read("sub/b.txt", 2)"#);
    assert!(
        too_big.starts_with(&format!(
            "tern.fs.read {}",
            fx.root.join("pkg/sub/b.txt").display()
        )),
        "{too_big}"
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(fx.root.join("pkg/sub/b.txt"), fx.root.join("pkg/link"))
            .expect("symlink");
        assert!(err(&vm, r#"tern.fs.read("link", 100)"#).starts_with("tern.fs.read "));
        assert_eq!(eval::<String>(&vm, r#"return tern.fs.read("link")"#), "bee");
    }
    eval::<()>(&vm, r#"tern.fs.remove("sub")"#);
    assert!(!fx.root.join("pkg/sub").exists());
    let missing = err(&vm, r#"tern.fs.read("/does/not/exist")"#);
    assert!(
        missing.starts_with("tern.fs.read /does/not/exist: "),
        "{missing}"
    );
}

#[test]
fn process_runs_off_thread_with_callbacks_and_awaitables() {
    let fx = Fx::new();
    let vm = fx.vm(Context::Host);
    eval::<()>(
        &vm,
        r#"
        results = {}
        tern.process.run({"sh", "-c", "printf out; printf err >&2; echo $TERN_PLUGIN_DATA $X; exit 3"}, {env = {X = "y"}}, function(r)
            results.cb = r
        end)
        local a = tern.process.run({"cat"}, {stdin = "piped"})
        assert(a.kind == "process" and type(a.id) == "number")
        a:next(function(r, e) results.await = r end)
        tern.process.run({"sleep", "5"}, {timeout_ms = 50}, function(r) results.timeout = r end)
        tern.process.run({"pwd"}, function(r) results.pwd = r end)
        "#,
    );
    // Nothing runs inline; callbacks wait for the owner.
    assert!(eval::<bool>(&vm, "return next(results) == nil"));
    pump(&vm, Duration::from_secs(5), || {
        eval::<bool>(
            &vm,
            "return results.cb ~= nil and results.await ~= nil and results.timeout ~= nil and results.pwd ~= nil",
        )
    });
    let r = json_of(&vm, "return results");
    assert_eq!(r["cb"]["status"], 3);
    assert_eq!(
        r["cb"]["stdout"],
        format!("out{} y\n", fx.root.join("data").display())
    );
    assert_eq!(r["cb"]["stderr"], "err");
    assert_eq!(r["await"]["stdout"], "piped");
    assert_eq!(r["timeout"]["timed_out"], true);
    assert_eq!(r["timeout"]["status"], -1);
    assert_eq!(
        r["pwd"]["stdout"],
        format!("{}\n", fx.root.join("pkg").display())
    );
    assert_eq!(
        err(&vm, "tern.process.run({})"),
        "tern.process.run: argv is empty"
    );
    assert!(
        err(&vm, r#"tern.process.run({"/no/such/program"})"#)
            .starts_with("tern.process.run /no/such/program: ")
    );
    assert_eq!(
        err(&vm, "tern.process.run('ls')"),
        "tern.process.run(argv, opts?, cb): bad arguments"
    );
}

#[test]
fn sleep_and_awaitables_queue_as_await_never_inline() {
    let fx = Fx::new();
    let vm = fx.vm(Context::Host);
    eval::<()>(
        &vm,
        r#"
        order = {}
        local s = tern.sleep(30)
        s:next(function(r, e) table.insert(order, "first") end)
        s:next(function(r, e) table.insert(order, "second")
            -- Settled already: still queued, not called inline.
            s:next(function() table.insert(order, "late") end)
            table.insert(order, "after late")
        end)
        tern.timer(10, function() table.insert(order, "timer") end)
        "#,
    );
    assert!(err(&vm, "tern.sleep(-1)").starts_with("tern.sleep: "));
    pump(&vm, Duration::from_secs(2), || {
        eval::<f64>(&vm, "return #order") >= 5.0
    });
    assert_eq!(
        json_of(&vm, "return order"),
        json!(["timer", "first", "second", "after late", "late"])
    );
}

/// A tiny HTTP/1.1 server answering each connection from `respond(path)`.
/// Reads one whole HTTP request: the headers, then `Content-Length` bytes of body. A single
/// `read` can return only part of it when the machine is loaded.
fn read_request(stream: &mut std::net::TcpStream) -> String {
    let mut data = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&data[..end]).to_ascii_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if data.len() >= end + 4 + len {
                break;
            }
        }
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => data.extend_from_slice(&buf[..n]),
        }
    }
    String::from_utf8_lossy(&data).into_owned()
}

fn serve(respond: impl Fn(&str, &str) -> String + Send + 'static) -> (u16, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            counter.fetch_add(1, Ordering::SeqCst);
            let req = read_request(&mut stream);
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_owned();
            let reply = respond(&path, &req);
            let reply = reply.replacen("\r\n", "\r\nConnection: close\r\n", 1);
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    (port, hits)
}

#[test]
fn fetch_sends_http_follows_redirects_and_never_raises_for_network_errors() {
    let (port, _) = serve(|path, req| match path {
        "/redirect" => "HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 0\r\n\r\n".into(),
        "/final" => {
            let agent = req
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("user-agent:"))
                .unwrap_or_default()
                .to_owned();
            let body = format!("got {agent}");
            format!(
                "HTTP/1.1 404 Not Found\r\nX-Rep: a\r\nX-Rep: b\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
        }
        _ => {
            let body = req.split("\r\n\r\n").nth(1).unwrap_or_default().to_owned();
            format!(
                "HTTP/1.1 201 Created\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
        }
    });
    let fx = Fx::new();
    let vm = fx.vm(Context::Host);
    eval::<()>(
        &vm,
        &format!(
            r#"
            got = {{}}
            tern.fetch("http://127.0.0.1:{port}/redirect", function(r) got.redirect = r end)
            tern.fetch("http://127.0.0.1:{port}/echo", {{method = "post", body = "payload", headers = {{["User-Agent"] = "probe"}}}})
                :next(function(r, e) got.post = r end)
            tern.fetch("http://127.0.0.1:1/", {{timeout_ms = 2000}}, function(r) got.refused = r end)
            "#
        ),
    );
    pump(&vm, Duration::from_secs(10), || {
        eval::<bool>(
            &vm,
            "return got.redirect ~= nil and got.post ~= nil and got.refused ~= nil",
        )
    });
    let got = json_of(&vm, "return got");
    assert_eq!(got["redirect"]["status"], 404, "a 404 is a response");
    assert_eq!(got["redirect"]["headers"]["x-rep"], "a, b");
    assert_eq!(got["redirect"]["body"], "got user-agent: Auk");
    assert_eq!(got["post"]["status"], 201);
    assert_eq!(got["post"]["body"], "payload");
    assert_eq!(got["refused"]["status"], 0);
    assert!(got["refused"]["error"].is_string());
    assert_eq!(got["refused"]["timed_out"], false);
    assert_eq!(
        err(&vm, r#"tern.fetch("ftp://x")"#),
        "tern.fetch ftp://x: not an absolute http or https URL"
    );
    assert!(
        err(&vm, r#"tern.fetch("http://x/", {method = "BAD METHOD"})"#)
            .starts_with("tern.fetch http://x/: ")
    );
    assert_eq!(
        err(&vm, "tern.fetch(1)"),
        "tern.fetch(url, opts?, cb): bad arguments"
    );
}

#[test]
fn ui_builders_produce_the_wire_form() {
    let fx = Fx::new();
    let vm = fx.vm(Context::Window);
    let ui = |src: &str| json_of(&vm, &format!("local ui = tern.ui return {src}"));
    assert_eq!(
        ui(r#"ui.span("a", "muted")"#),
        json!({"t": "a", "s": "muted"})
    );
    assert_eq!(ui(r#"ui.span(5, "")"#), json!({"t": "5"}));
    assert_eq!(
        ui(r#"ui.link("x", "https://e", "strong")"#),
        json!({"t": "x", "s": "strong", "href": "https://e"})
    );
    assert_eq!(
        ui(r#"ui.path("src/a b.rs", "/w")"#),
        json!({"t": "src/a b.rs", "s": "path", "href": "file:///w/src/a%20b.rs"})
    );
    assert_eq!(ui(r#"ui.path("~/x")"#), json!({"t": "~/x", "s": "path"}));
    assert_eq!(
        ui(r#"ui.text({"a", ui.span("b", "strong")})"#),
        json!({"k": "text", "p": {"spans": [{"t": "a"}, {"t": "b", "s": "strong"}]}})
    );
    assert_eq!(ui(r#"ui.col({ui.text("x")})"#)["p"], json!({"gap": "sm"}));
    assert_eq!(
        ui(r#"ui.lines({})"#),
        json!({"k": "col", "p": {"gap": "none"}})
    );
    assert_eq!(ui(r#"ui.rule("")"#), json!({"k": "rule"}));
    assert_eq!(
        ui(r#"ui.badge("ok", "success")"#),
        json!({"k": "badge", "p": {"text": "ok", "tone": "success"}})
    );
    assert_eq!(
        ui(r#"ui.kv({{"k", "v"}})"#),
        json!({"k": "kv", "p": {"items": [{"k": [{"t": "k"}], "v": [{"t": "v"}]}]}})
    );
    assert_eq!(ui(r#"ui.meter(1.5, "m")"#)["p"]["value"], 1);
    assert_eq!(
        ui(r#"ui.progress(nil, "p")"#),
        json!({"k": "progress", "p": {"label": [{"t": "p"}]}})
    );
    assert_eq!(
        ui(r#"ui.bars({{"a", 2}})"#),
        json!({"k": "chart", "p": {"kind": "bars", "series": [{"label": "a", "value": 2}]}})
    );
    assert_eq!(
        ui(
            r#"ui.table({{id = "n", head = "Name"}, {id = "s", head = "Size", align = "end"}}, {{"a", ui.meter_cell(0.5, {0.7, 0.9})}})"#
        ),
        json!({"k": "table", "p": {
            "cols": [
                {"id": "n", "head": "Name", "align": "start", "truncate": "end", "priority": 0, "grow": 0},
                {"id": "s", "head": "Size", "align": "end", "truncate": "end", "priority": 0, "grow": 0}
            ],
            "rows": [{"id": "r0", "cells": {"n": [{"t": "a"}], "s": {"meter": {"value": 0.5, "thresholds": {"warn": 0.7, "bad": 0.9}}}}}]
        }})
    );
    assert_eq!(
        ui(r#"ui.tree({{label = "a", children = {{label = "b"}}}, {label = "c"}})"#),
        json!({"k": "tree", "p": {"nodes": [
            {"id": "t0", "label": [{"t": "a"}], "open": false, "children": [{"id": "t1", "label": [{"t": "b"}]}]},
            {"id": "t2", "label": [{"t": "c"}]}
        ]}})
    );
    assert_eq!(
        ui(r#"ui.list({{label = "a", icon = "file"}})"#),
        json!({"k": "list", "c": [{"k": "item", "p": {"label": [{"t": "a"}], "icon": "file"}}]})
    );
    assert_eq!(
        ui(r#"ui.code("x", "rust", 3)"#),
        json!({"k": "code", "p": {"text": "x", "lang": "rust", "start": 3, "numbers": true}})
    );
    assert_eq!(
        ui(r##"ui.md("# h")"##),
        json!({"k": "md", "p": {"text": "# h"}})
    );
    assert_eq!(
        ui(r#"ui.el("div", {class = "c"}, {ui.text("x")})"#)["p"],
        json!({"tag": "div", "class": "c"})
    );
    assert_eq!(
        ui(r#"ui.overflow(3)"#)["p"]["spans"][0]["t"],
        "… 3 more (Raw shows everything)"
    );
    assert_eq!(
        ui(
            r#"ui.diagnostic({severity = "warning", message = "m", path = "a.rs", line = 2, col = 5}, "/w")"#
        )["p"]["key"],
        "a.rs:2:5:"
    );
    let msg = |src: &str| err(&vm, &format!("local ui = tern.ui {src}"));
    assert_eq!(
        msg("ui.span(nil)"),
        "tern.ui.span: t: expected a string, got nil"
    );
    assert_eq!(
        msg(r#"ui.col(ui.text("x"))"#),
        "tern.ui.col: children: expected a list of nodes, got one node (wrap it in {})"
    );
    assert_eq!(
        msg(r#"ui.table({{id = "a", head = "A", align = "left"}}, {})"#),
        "tern.ui.table: cols[1].align: unknown align \"left\" (start|center|end)"
    );
    assert_eq!(
        msg(r#"ui.list({{label = {1, {}}}})"#),
        "tern.ui.list: items[1].label[2]: expected a span (a table with a string `t`)"
    );
    assert_eq!(
        msg(r#"ui.diagnostic({severity = "fatal", message = "m"})"#),
        "tern.ui.diagnostic: d.severity: unknown severity \"fatal\" (error|warning|note|help)"
    );
}

#[test]
fn parse_reads_columns_listings_json_locations_and_sizes() {
    let fx = Fx::new();
    let vm = fx.vm(Context::Host);
    assert_eq!(
        json_of(
            &vm,
            r#"return tern.parse.columns({
            "NAME          READY   STATUS    RESTARTS   AGE",
            "",
            "web-7d4b9     1/1     Running   0          5d",
            "db-0          0/1     Pending   12         10m",
        })"#
        ),
        json!({"head": ["NAME", "READY", "STATUS", "RESTARTS", "AGE"], "rows": [
            ["web-7d4b9", "1/1", "Running", "0", "5d"],
            ["db-0", "0/1", "Pending", "12", "10m"]
        ]})
    );
    assert!(eval::<bool>(
        &vm,
        r#"return tern.parse.columns({"just some prose here", "and more"}) == nil"#
    ));
    assert_eq!(
        json_of(
            &vm,
            r#"return tern.parse.packed({"a.txt    c.rs     e", "b.md     d.toml"})"#
        ),
        json!(["a.txt", "b.md", "c.rs", "d.toml", "e"])
    );
    let tree = json_of(
        &vm,
        r#"return tern.parse.json_tree('{"a": [1, 2], "b": {"c": "d"}}')"#,
    );
    assert_eq!(tree["k"], "tree");
    assert_eq!(
        tree["p"]["nodes"][0]["label"][0],
        json!({"t": "a", "s": "info"})
    );
    assert_eq!(
        tree["p"]["nodes"][0]["children"][0]["label"][2],
        json!({"t": "1", "s": "num"})
    );
    assert!(eval::<bool>(
        &vm,
        r#"return tern.parse.json_tree("42") == nil and tern.parse.json_tree("nope") == nil"#
    ));
    assert_eq!(
        json_of(
            &vm,
            r#"return {tern.parse.location("src/main.rs:12:5: error")}"#
        ),
        json!(["src/main.rs", 12, 5])
    );
    assert_eq!(
        json_of(&vm, r#"return {tern.parse.location("x.py:7")}"#),
        json!(["x.py", 7])
    );
    assert_eq!(
        json_of(
            &vm,
            r#"return {tern.parse.size("4.0K"), tern.parse.size("1.2GB"), tern.parse.size("3KiB"), tern.parse.size("512")}"#
        ),
        json!([4096, 1_200_000_000, 3072, 512])
    );
    assert!(eval::<bool>(
        &vm,
        r#"return tern.parse.size("-") == nil and tern.parse.size("") == nil"#
    ));
    assert_eq!(
        err(&vm, "tern.parse.columns({1})"),
        "tern.parse.columns: lines[1]: expected a string, got number"
    );
}

#[test]
fn ac12_a_lens_decision_is_the_same_on_every_replica_without_a_window_half() {
    let root = common::Root::new();
    root.example("terraform");
    root.package(
        "aa",
        &[
            ("plugin.toml", "schema = 1\nid = \"aa\"\nname = \"AA\"\nversion = \"1\"\nhost = \"h.luau\"\n[[lenses]]\nid = \"first\"\nmatch = [\"tofu plan *\"]\n[[lenses]]\nid = \"second\"\nmatch = [\"tofu *\"]\n"),
            ("h.luau", "for _, id in {'first', 'second'} do tern.lens.define(id, {open = print, line = print, finish = print, view = print}) end"),
        ],
    );
    let host = root.host(None);
    // Replicas receive the catalog as JSON, run no Lua and no window half.
    let replica: auk_plugins::Catalog =
        serde_json::from_value(host.catalog().to_json()).expect("catalog");
    let pre = auk_plugins::Preconditions {
        shell_integration: true,
        lenses_on: true,
        main_screen: true,
        surface_open: false,
    };
    for line in [
        "terraform plan",
        "sudo -u ops terraform -chdir=infra plan -out x 2>&1",
        "tofu plan -var a=b",
        "tofu apply",
        "terraform plan | less",
        "TF_LOG=debug /usr/local/bin/terraform plan",
    ] {
        assert_eq!(
            host.catalog().claim(line, pre),
            replica.claim(line, pre),
            "{line}"
        );
    }
    // Id order then manifest order: `aa` beats `terraform`, its first matching lens wins.
    let claim = replica.claim("tofu plan -var a=b", pre).expect("claimed");
    assert_eq!(
        (claim.plugin.as_str(), claim.lens.as_str()),
        ("aa", "first")
    );
    assert_eq!(
        replica.claim("tofu apply", pre).map(|c| c.lens),
        Some("second".into())
    );
    // The capture every replica runs over the same bytes gives the same lines and aborts.
    let bytes = b"one\r\n\x1b[32mtwo\x1b[0m\r\nthree\r\n";
    let run = |cols| {
        let mut c = auk_plugins::capture::Capture::new(cols);
        let mut lines = c.feed(bytes).expect("ok");
        lines.extend(c.finish());
        lines
    };
    assert_eq!(run(80), run(80));
}

#[test]
fn the_watcher_reloads_on_change_natively_and_by_polling() {
    for native in [true, false] {
        let root = common::Root::new();
        let pkg = root.package(
            "w",
            &[
                (
                    "plugin.toml",
                    "schema = 1\nid = \"w\"\nname = \"W\"\nversion = \"1\"\nhost = \"h.luau\"\n",
                ),
                ("h.luau", ""),
            ],
        );
        let (tx, rx) = crossbeam_channel_like();
        let watcher = if native {
            auk_plugins::Watcher::start(root.plugins(), move || {
                let _ = tx.send(());
            })
        } else {
            auk_plugins::Watcher::polling(root.plugins(), move || {
                let _ = tx.send(());
            })
        };
        assert_eq!(watcher.is_native(), native);
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(pkg.join("h.luau"), "print('changed')").expect("write");
        let wait = if native {
            Duration::from_secs(3)
        } else {
            Duration::from_secs(5)
        };
        rx.recv_timeout(wait)
            .unwrap_or_else(|_| panic!("no reload (native: {native})"));
        // One batch for one change.
        assert!(rx.recv_timeout(Duration::from_millis(700)).is_err());
        drop(watcher);
    }
}

fn crossbeam_channel_like() -> (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>) {
    std::sync::mpsc::channel()
}

#[test]
fn process_output_is_capped_at_16_mib_per_stream_and_the_rest_is_drained() {
    let fx = Fx::new();
    let vm = fx.vm(Context::Host);
    eval::<()>(
        &vm,
        r#"
        big = nil
        -- 17 MB on each stream: the cap keeps 16 MiB of each; the rest is read and dropped, so
        -- the process isn't blocked on a full pipe and its status comes back.
        tern.process.run({"sh", "-c", "head -c 17000000 /dev/zero | tr '\\0' a; head -c 17000000 /dev/zero | tr '\\0' b >&2; exit 5"},
            function(r) big = {status = r.status, out = #r.stdout, err = #r.stderr, timed_out = r.timed_out,
                tail = string.sub(r.stdout, -1), etail = string.sub(r.stderr, -1)} end)
        "#,
    );
    pump(&vm, Duration::from_secs(30), || {
        eval::<bool>(&vm, "return big ~= nil")
    });
    let big = json_of(&vm, "return big");
    let cap = 16 * 1024 * 1024;
    assert_eq!(
        big,
        json!({"status": 5, "out": cap, "err": cap, "timed_out": false, "tail": "a", "etail": "b"})
    );
}

#[test]
fn fetch_bodies_are_capped_at_16_mib_and_redirects_at_10() {
    let cap = 16 * 1024 * 1024;
    let (port, _) = serve(move |path, _| match path {
        "/exact" => format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {cap}\r\n\r\n{}",
            "e".repeat(cap)
        ),
        "/over" => format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            cap + 1,
            "o".repeat(cap + 1)
        ),
        hop => match hop
            .strip_prefix("/hop/")
            .and_then(|n| n.parse::<u32>().ok())
        {
            Some(0) => "HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nend".into(),
            Some(n) => format!(
                "HTTP/1.1 302 Found\r\nLocation: /hop/{}\r\nContent-Length: 0\r\n\r\n",
                n - 1
            ),
            None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".into(),
        },
    });
    let fx = Fx::new();
    let vm = fx.vm(Context::Host);
    eval::<()>(
        &vm,
        &format!(
            r#"
            got = {{}}
            local base = "http://127.0.0.1:{port}"
            tern.fetch(base .. "/exact", function(r) got.exact = {{status = r.status, len = #r.body, error = r.error}} end)
            tern.fetch(base .. "/over", function(r) got.over = {{status = r.status, len = #r.body, error = r.error}} end)
            tern.fetch(base .. "/hop/10", function(r) got.ten = {{status = r.status, body = r.body, error = r.error}} end)
            tern.fetch(base .. "/hop/11", function(r) got.eleven = {{status = r.status, error = r.error}} end)
            "#
        ),
    );
    pump(&vm, Duration::from_secs(30), || {
        eval::<bool>(
            &vm,
            "return got.exact ~= nil and got.over ~= nil and got.ten ~= nil and got.eleven ~= nil",
        )
    });
    let got = json_of(&vm, "return got");
    assert_eq!(got["exact"], json!({"status": 200, "len": cap}), "{got}");
    // Over the cap: an error, never raised.
    assert_eq!(got["over"]["status"], 0, "{got}");
    assert_eq!(got["over"]["error"], "the body is over 16 MiB", "{got}");
    // Ten redirects are followed; the eleventh is an error.
    assert_eq!(got["ten"], json!({"status": 200, "body": "end"}), "{got}");
    assert_eq!(got["eleven"]["status"], 0, "{got}");
    assert!(got["eleven"]["error"].is_string(), "{got}");
}

#[test]
fn fetch_times_out_after_30_s_by_default() {
    // A server that takes the request and never answers.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            held.push(stream);
        }
    });
    let fx = Fx::new();
    let vm = fx.vm(Context::Host);
    let started = Instant::now();
    eval::<()>(
        &vm,
        &format!(
            r#"
            got = {{}}
            tern.fetch("http://127.0.0.1:{port}/", function(r) got.default = r end)
            -- ≤0 means the default too.
            tern.fetch("http://127.0.0.1:{port}/", {{timeout_ms = 0}}, function(r) got.zero = r end)
            tern.fetch("http://127.0.0.1:{port}/", {{timeout_ms = 200}}, function(r) got.short = r; got.short_at = tern.now() end)
            "#
        ),
    );
    pump(&vm, Duration::from_secs(45), || {
        eval::<bool>(&vm, "return got.default ~= nil and got.zero ~= nil")
    });
    let took = started.elapsed();
    let got = json_of(&vm, "return got");
    for key in ["default", "zero", "short"] {
        assert_eq!(got[key]["timed_out"], true, "{key}: {got}");
        assert_eq!(got[key]["status"], 0, "{key}: {got}");
    }
    assert!(
        took >= Duration::from_secs(29) && took < Duration::from_secs(40),
        "{took:?}"
    );
}
