//! One plugin VM: sandbox, `require`, the `tern` surface, budgets, the memory cap and native
//! codegen.

use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use auk_plugins::mlua::{self, Function, Value};
use auk_plugins::{
    Budget, CallError, Context, LogLevel, LogSink, MEMORY_LIMIT, PluginInfo, Vm, VmOptions,
    codegen_supported,
};

type Logs = Arc<Mutex<Vec<(LogLevel, String)>>>;

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    logs: Logs,
}

impl Fixture {
    fn new(files: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("pkg");
        for (name, text) in files {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, text).expect("write");
        }
        std::fs::create_dir_all(&root).expect("mkdir");
        let root = root.canonicalize().expect("canonical");
        Fixture {
            _dir: dir,
            root,
            logs: Logs::default(),
        }
    }

    fn options(&self, context: Context) -> VmOptions {
        let logs = Arc::clone(&self.logs);
        let log: LogSink = Arc::new(move |r| {
            logs.lock()
                .expect("logs")
                .push((r.level, r.message.to_owned()))
        });
        VmOptions {
            context,
            plugin: PluginInfo {
                id: "test".into(),
                name: "Test".into(),
                dir: self.root.clone(),
                data: self.root.join("data"),
            },
            declared_blocks: vec!["pods".into()],
            declared_lenses: vec!["get".into()],
            memory_limit: MEMORY_LIMIT,
            jit: true,
            log,
            panes: None,
            builtin_commands: None,
        }
    }

    fn vm(&self, context: Context) -> Vm {
        Vm::new(self.options(context)).expect("vm")
    }
}

/// Compiles `src` as the package file `name` and calls it as `hook`.
fn run<R: mlua::FromLuaMulti>(
    vm: &Vm,
    root: &Path,
    src: &str,
    hook: &str,
    budget: Budget,
) -> Result<R, CallError> {
    let f = vm.compile(src, &root.join("chunk.luau")).expect("compile");
    vm.call(hook, budget, &f, ())
}

fn eval<R: mlua::FromLuaMulti>(vm: &Vm, root: &Path, src: &str) -> R {
    run(vm, root, src, "test", Budget::Host).unwrap_or_else(|e| panic!("{src}: {e}"))
}

fn lua_err(vm: &Vm, root: &Path, src: &str) -> String {
    match run::<()>(vm, root, src, "test", Budget::Host) {
        Err(e) => e.message(),
        Ok(()) => panic!("{src} should raise"),
    }
}

#[test]
fn sandbox_libraries() {
    let fx = Fixture::new(&[]);
    let vm = fx.vm(Context::Host);
    let r = &fx.root;
    assert!(eval::<bool>(
        &vm,
        r,
        "return io == nil and package == nil and dofile == nil and loadfile == nil"
    ));
    let os: String = eval(
        &vm,
        r,
        "local k = {} for name in os do table.insert(k, name) end table.sort(k) return table.concat(k, ',')",
    );
    assert_eq!(os, "clock,date,difftime,time");
    let debug: String = eval(
        &vm,
        r,
        "local k = {} for name in debug do table.insert(k, name) end table.sort(k) return table.concat(k, ',')",
    );
    assert_eq!(debug, "info,traceback");
    for lib in [
        "string",
        "table",
        "math",
        "coroutine",
        "utf8",
        "bit32",
        "buffer",
        "vector",
    ] {
        assert!(
            eval::<bool>(&vm, r, &format!("return type({lib}) == 'table'")),
            "{lib}"
        );
    }
    assert!(lua_err(&vm, r, "string.upper = nil").contains("readonly"));
    assert!(lua_err(&vm, r, "tern.now = nil").contains("readonly"));
    // Plugin globals live in a writable layer, shared by later chunks.
    eval::<()>(&vm, r, "shared_value = 42");
    assert_eq!(eval::<i64>(&vm, r, "return shared_value"), 42);
    assert_eq!(
        eval::<f64>(&vm, r, "return collectgarbage('count') > 0 and 1 or 0"),
        1.0
    );
    assert!(lua_err(&vm, r, "collectgarbage('collect')").contains("collectgarbage"));
}

#[test]
fn tern_surface() {
    let fx = Fixture::new(&[]);
    let r = &fx.root;
    let host = fx.vm(Context::Host);
    assert!(eval::<bool>(
        &host,
        r,
        "return auk == tern and require('tern') == tern and require('auk') == tern"
    ));
    assert_eq!(eval::<String>(&host, r, "return tern.context"), "host");
    let plugin: (String, String, String, String) = eval(
        &host,
        r,
        "return tern.plugin.id, tern.plugin.name, tern.plugin.dir, tern.plugin.data",
    );
    assert_eq!(plugin.0, "test");
    assert_eq!(plugin.1, "Test");
    assert_eq!(PathBuf::from(&plugin.2), fx.root);
    assert_eq!(PathBuf::from(&plugin.3), fx.root.join("data"));
    let os: String = eval(&host, r, "return tern.runtime.os");
    assert_eq!(
        os,
        if cfg!(target_os = "macos") {
            "macos"
        } else {
            std::env::consts::OS
        }
    );
    assert!(eval::<bool>(
        &host,
        r,
        "local a = tern.now() local b = tern.now() return b >= a"
    ));
    assert_eq!(
        eval::<Option<String>>(&host, r, "return tern.getenv('AUK_SURELY_UNSET_VARIABLE')"),
        None
    );
    assert_eq!(
        eval::<Option<String>>(&host, r, "return tern.getenv('PATH')"),
        std::env::var("PATH").ok()
    );

    eval::<()>(
        &host,
        r,
        "print('a', 1, nil, true) tern.log.warn('w') tern.log.debug() tern.log.error(setmetatable({}, {__tostring = function() return 'obj' end}))",
    );
    let logs = fx.logs.lock().expect("logs").clone();
    assert_eq!(
        logs,
        [
            (LogLevel::Info, "a\t1\tnil\ttrue".to_owned()),
            (LogLevel::Warn, "w".to_owned()),
            (LogLevel::Debug, String::new()),
            (LogLevel::Error, "obj".to_owned())
        ]
    );

    // Members of the other context are absent, so Luau raises its own error.
    let window = fx.vm(Context::Window);
    assert_eq!(eval::<String>(&window, r, "return tern.context"), "window");
    assert!(
        lua_err(&window, r, "tern.block.define('pods', {})")
            .contains("attempt to index nil with 'define'")
    );
    assert!(
        lua_err(&window, r, "tern.lens.define('get', {})")
            .contains("attempt to index nil with 'define'")
    );
    assert_eq!(
        lua_err(&window, r, "tern.on('spawn', function() end)"),
        "tern.on: unknown window event \"spawn\" (window_start, focus, pane_created, pane_closed, tab_created, tab_closed, command_started, command_finished, cwd, title)"
    );
    eval::<()>(&window, r, "tern.on('focus', function() end)");
    assert_eq!(window.handlers("focus").len(), 1);
}

#[test]
fn host_registration() {
    let fx = Fixture::new(&[]);
    let r = &fx.root;
    let vm = fx.vm(Context::Host);
    assert!(lua_err(&vm, r, "tern.on('nope', function() end)").ends_with(
        "tern.on: unknown host event \"nope\" (spawn, command_started, command_finished, cwd, title, pane_exited)"
    ));
    assert_eq!(
        lua_err(
            &vm,
            r,
            "tern.block.define('x', {init = print, view = print})"
        ),
        "tern.block.define: block \"x\" is not declared in plugin.toml"
    );
    assert_eq!(
        lua_err(&vm, r, "tern.block.define('pods', {init = print})"),
        "tern.block.define: pods: `view` must be a function"
    );
    assert_eq!(
        lua_err(
            &vm,
            r,
            "tern.block.define('pods', {init = print, view = print, save = 1})"
        ),
        "tern.block.define: pods: `save` must be a function"
    );
    assert_eq!(
        lua_err(
            &vm,
            r,
            "tern.lens.define('get', {open = print, line = print, view = print})"
        ),
        "tern.lens.define: get: `finish` must be a function"
    );
    assert!(vm.block("pods").is_none());
    eval::<()>(
        &vm,
        r,
        "tern.block.define('pods', {init = print, view = print, title = print})",
    );
    eval::<()>(
        &vm,
        r,
        "tern.lens.define('get', {open = print, line = print, finish = print, view = print})",
    );
    let pods = vm.block("pods").expect("defined");
    assert!(pods.title.is_some() && pods.key.is_none());
    assert!(vm.lens("get").is_some());
    eval::<()>(
        &vm,
        r,
        "tern.on('cwd', print) tern.on('cwd', print) tern.on('spawn', print)",
    );
    assert_eq!(vm.handlers("cwd").len(), 2);
    assert!(vm.spawn_flag().load(std::sync::atomic::Ordering::Acquire));
}

#[test]
fn entry_checks_declarations() {
    let fx = Fixture::new(&[
        (
            "ok.luau",
            "tern.block.define('pods', {init = print, view = print}) tern.lens.define('get', {open = print, line = print, finish = print, view = print})",
        ),
        (
            "no_block.luau",
            "tern.lens.define('get', {open = print, line = print, finish = print, view = print})",
        ),
        (
            "no_lens.luau",
            "tern.block.define('pods', {init = print, view = print})",
        ),
        ("raises.luau", "error('boom')"),
        ("syntax.luau", "local = 1"),
    ]);
    assert_eq!(fx.vm(Context::Host).run_entry("ok.luau"), Ok(()));
    assert_eq!(
        fx.vm(Context::Host).run_entry("no_block.luau"),
        Err("block pods is declared in plugin.toml but not defined with tern.block.define".into())
    );
    assert_eq!(
        fx.vm(Context::Host).run_entry("no_lens.luau"),
        Err("lens get is declared in plugin.toml but not defined with tern.lens.define".into())
    );
    let raised = fx
        .vm(Context::Host)
        .run_entry("raises.luau")
        .expect_err("raises");
    assert_eq!(
        raised.lines().next(),
        Some(format!("{}:1: boom", fx.root.join("raises.luau").display()).as_str()),
        "chunks are named after their absolute path"
    );
    assert!(fx.vm(Context::Host).run_entry("syntax.luau").is_err());
    assert!(
        fx.vm(Context::Host)
            .run_entry("missing.luau")
            .expect_err("missing")
            .starts_with("cannot read missing.luau: ")
    );
}

#[test]
fn require_is_confined_to_the_package() {
    let fx = Fixture::new(&[
        ("host.luau", ""),
        ("a.luau", "count = (count or 0) + 1 return 'a'"),
        ("b.lua", "return 'b'"),
        ("dir/init.luau", "return 'dir:' .. require('./inner')"),
        ("dir/inner.luau", "return 'inner'"),
        (
            "lib/deep.luau",
            "return require('../a') .. '+' .. require('./sib')",
        ),
        ("lib/sib.luau", "return 'sib'"),
        ("amb.luau", "return 1"),
        ("amb.lua", "return 2"),
        (".luaurc", "{\"aliases\": {\"x\": \"./a\"}}"),
    ]);
    let outside = fx.root.parent().expect("parent").join("outside.luau");
    std::fs::write(&outside, "return 'outside'").expect("write");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, fx.root.join("link.luau")).expect("symlink");

    let vm = fx.vm(Context::Host);
    let entry = |src: &str| -> Result<Value, CallError> {
        let f = vm
            .compile(src, &fx.root.join("host.luau"))
            .expect("compile");
        vm.call("load", Budget::Host, &f, ())
    };
    let string = |src: &str| match entry(src) {
        Ok(Value::String(s)) => s.to_string_lossy(),
        other => panic!("{src}: {other:?}"),
    };
    assert_eq!(string("return require('./a')"), "a");
    assert_eq!(string("return require('./b')"), "b");
    assert_eq!(string("return require('./dir')"), "dir:inner");
    assert_eq!(string("return require('./lib/deep')"), "a+sib");
    // Cached per resolved file: `a` ran once although required twice.
    assert_eq!(string("require('./a') return tostring(count)"), "1");
    let message = |src: &str| entry(src).expect_err(src).message();
    assert!(
        message("return require('./amb')").contains("ambiguous"),
        "{}",
        message("return require('./amb')")
    );
    // Leaving the package, lexically or through a symlink, is "not found".
    assert!(
        message("return require('../outside')")
            .contains("could not get parent of requiring context")
    );
    #[cfg(unix)]
    assert!(
        message("return require('./link')").contains("could not resolve child component \"link\"")
    );
    assert!(
        message("return require('./nope')").contains("could not resolve child component \"nope\"")
    );
    // `.luaurc` aliases are not read.
    assert!(message("return require('@x')").contains("@x is not a valid alias"));
}

/// Spins until the budget trips; records that `pcall` returned first.
const SPIN_WITH_PCALL: &str = "
    local ok, err = pcall(function() while true do end end)
    caught = err
    while true do end
";

#[test]
fn s5_formatter_budget_trips_at_4ms_and_disables_only_that_hook() {
    let fx = Fixture::new(&[]);
    let r = &fx.root;
    let vm = fx.vm(Context::Window);
    let started = Instant::now();
    let err = run::<()>(&vm, r, SPIN_WITH_PCALL, "status", Budget::Formatter).expect_err("trips");
    let took = started.elapsed();
    assert!(
        matches!(&err, CallError::Exceeded { hook, budget_ms: 4 } if hook == "status"),
        "{err:?}"
    );
    assert_eq!(err.to_string(), "tern: status exceeded 4 ms");
    assert!(
        took >= Duration::from_millis(4) && took < Duration::from_millis(40),
        "{took:?}"
    );
    // pcall saw the trip, and the loop after it was stopped too.
    assert_eq!(
        eval::<String>(&vm, r, "return tostring(caught)"),
        "tern: status exceeded 4 ms"
    );
    // The hook is disabled until reload: skipped without running.
    let err =
        run::<()>(&vm, r, "skipped = true", "status", Budget::Formatter).expect_err("disabled");
    assert!(matches!(&err, CallError::Disabled { hook } if hook == "status"));
    assert_eq!(
        err.to_string(),
        "tern: status is disabled after exceeding its budget"
    );
    assert!(eval::<bool>(&vm, r, "return skipped == nil"));
    // Other hooks keep running, with the same budget.
    assert_eq!(
        run::<i64>(&vm, r, "return 7", "tab_title", Budget::Formatter).expect("other hook"),
        7
    );
    // A rebuilt VM (a reload) has every hook enabled again.
    let vm = fx.vm(Context::Window);
    assert_eq!(
        run::<i64>(&vm, r, "return 1", "status", Budget::Formatter).expect("re-enabled"),
        1
    );
    println!("formatter trip after {took:?}");
}

#[test]
fn s5_window_budget_trips_at_50ms() {
    let fx = Fixture::new(&[]);
    let vm = fx.vm(Context::Window);
    let started = Instant::now();
    let err = run::<()>(
        &vm,
        &fx.root,
        SPIN_WITH_PCALL,
        "command run",
        Budget::Window,
    )
    .expect_err("trips");
    let took = started.elapsed();
    assert_eq!(err.to_string(), "tern: command run exceeded 50 ms");
    assert!(
        took >= Duration::from_millis(50) && took < Duration::from_millis(100),
        "{took:?}"
    );
    assert!(vm.is_disabled("command run"));
    assert!(!vm.is_disabled("command other"));
    println!("window trip after {took:?}");
}

#[test]
fn s5_host_budget_trips_at_2s_in_native_code() {
    let fx = Fixture::new(&[]);
    let vm = fx.vm(Context::Host);
    let src = "
        @native local function spin(n) local x = 0 while true do x = (x + n) % 7 end end
        local ok = pcall(spin, 3)
        caught = not ok
        spin(1)
    ";
    let started = Instant::now();
    let err = run::<()>(&vm, &fx.root, src, "command_started", Budget::Host).expect_err("trips");
    let took = started.elapsed();
    assert_eq!(err.to_string(), "tern: command_started exceeded 2000 ms");
    assert!(
        took >= Duration::from_secs(2) && took < Duration::from_millis(2100),
        "{took:?}"
    );
    assert!(eval::<bool>(&vm, &fx.root, "return caught"));
    assert!(vm.is_disabled("command_started"));
    println!("host trip after {took:?}");
}

#[test]
fn s5_swallowed_trip_still_counts() {
    // The trip raises inside pcall and nothing checks again before the call returns; the call
    // still counts as tripped and its result is discarded.
    let fx = Fixture::new(&[]);
    let vm = fx.vm(Context::Window);
    let src = "local ok = pcall(function() while true do end end) return ok";
    let err = run::<bool>(
        &vm,
        &fx.root,
        src,
        "available plugin.test.x",
        Budget::Formatter,
    )
    .expect_err("tripped");
    assert!(matches!(err, CallError::Exceeded { budget_ms: 4, .. }));
}

#[test]
fn s5_nested_call_keeps_the_earlier_deadline() {
    let fx = Fixture::new(&[]);
    let vm = Rc::new(fx.vm(Context::Window));
    let weak: Weak<Vm> = Rc::downgrade(&vm);
    let spin = vm
        .compile("while true do end", &fx.root.join("spin.luau"))
        .expect("compile");
    let nested = vm
        .lua()
        .create_function(move |_, ()| {
            let vm = weak.upgrade().expect("vm");
            let inner = vm.call::<()>("inner", Budget::Host, &spin, ());
            Ok(inner.err().map(|e| e.to_string()))
        })
        .expect("function");
    vm.lua().globals().set("nested", nested).expect("set");
    let started = Instant::now();
    let err = run::<()>(
        &vm,
        &fx.root,
        "inner_result = nested() while true do end",
        "status",
        Budget::Formatter,
    )
    .expect_err("trips");
    let took = started.elapsed();
    assert!(
        took < Duration::from_millis(40),
        "the inner 2 s call ended at the outer 4 ms deadline: {took:?}"
    );
    assert_eq!(err.to_string(), "tern: status exceeded 4 ms");
    assert!(vm.is_disabled("inner") && vm.is_disabled("status"));
    let inner: String = eval(&vm, &fx.root, "return inner_result");
    assert_eq!(inner, "tern: inner exceeded 2000 ms");
}

#[test]
fn s5_calls_within_budget_never_trip() {
    let fx = Fixture::new(&[]);
    let vm = fx.vm(Context::Window);
    let f = vm
        .compile(
            "local s = 0 for i = 1, 1000 do s += i end return s",
            &fx.root.join("f.luau"),
        )
        .expect("compile");
    let started = Instant::now();
    for _ in 0..20_000 {
        assert_eq!(
            vm.call::<i64>("tab_title", Budget::Formatter, &f, ())
                .expect("fast call"),
            500_500
        );
    }
    println!("20000 budgeted calls in {:?}", started.elapsed());
    // A long call that stays under its budget isn't tripped by an earlier call's deadline.
    let sleepy = vm
        .compile(
            "local t = os.clock() while os.clock() - t < 0.03 do end return 1",
            &fx.root.join("s.luau"),
        )
        .expect("compile");
    assert_eq!(
        vm.call::<i64>("status", Budget::Window, &sleepy, ())
            .expect("30 ms < 50 ms"),
        1
    );
}

#[test]
fn s5_memory_cap_holds() {
    let fx = Fixture::new(&[]);
    let vm = fx.vm(Context::Host);
    let r = &fx.root;
    // One allocation past the cap fails cleanly, and pcall catches it.
    let (ok, message): (bool, String) = eval(
        &vm,
        r,
        "local ok, err = pcall(string.rep, 'x', 300 * 1024 * 1024) return ok, tostring(err)",
    );
    assert!(!ok);
    assert!(message.contains("memory"), "{message}");
    // Growing a table until the cap: an ordinary error in the allocating call.
    let err = run::<()>(
        &vm,
        r,
        "local t = {} for i = 1, 1e9 do t[i] = string.rep('y', 1024 * 1024) .. i end",
        "grow",
        Budget::Host,
    )
    .expect_err("cap");
    assert!(
        matches!(err, CallError::Lua(mlua::Error::MemoryError(_))),
        "{err:?}"
    );
    assert!(
        vm.used_memory() <= MEMORY_LIMIT,
        "{} bytes",
        vm.used_memory()
    );
    // The hook stays enabled and the VM keeps working once the garbage is gone.
    assert!(!vm.is_disabled("grow"));
    assert_eq!(
        run::<i64>(&vm, r, "return 1 + 1", "grow", Budget::Host).expect("still works"),
        2
    );
    let (ok, len): (bool, i64) = eval(
        &vm,
        r,
        "local ok, s = pcall(string.rep, 'z', 64 * 1024 * 1024) return ok, #s",
    );
    assert!(ok && len == 64 * 1024 * 1024);
}

fn best_of(f: &Function, runs: usize) -> Duration {
    (0..runs)
        .map(|_| {
            let t = Instant::now();
            let _: f64 = f.call(5_000_000).expect("call");
            t.elapsed()
        })
        .min()
        .expect("runs")
}

#[test]
fn s5_native_codegen_is_on() {
    assert!(
        codegen_supported(),
        "Luau codegen should support this CPU ({})",
        std::env::consts::ARCH
    );
    let fx = Fixture::new(&[]);
    let src = "
        @native local function sum(n) local s = 0 for i = 1, n do s += (i * 3) % 7 end return s end
        return sum
    ";
    let jit = fx.vm(Context::Host);
    assert!(jit.jit());
    assert!(eval::<bool>(&jit, &fx.root, "return tern.runtime.jit"));
    let mut options = fx.options(Context::Host);
    options.jit = false;
    let interp = Vm::new(options).expect("vm");
    assert!(!eval::<bool>(&interp, &fx.root, "return tern.runtime.jit"));

    let native_fn: Function = eval(&jit, &fx.root, src);
    let interp_fn: Function = eval(&interp, &fx.root, src);
    assert_eq!(
        native_fn.call::<f64>(1000).expect("call"),
        interp_fn.call::<f64>(1000).expect("call")
    );
    let native = best_of(&native_fn, 5);
    let interpreted = best_of(&interp_fn, 5);
    let ratio = interpreted.as_secs_f64() / native.as_secs_f64();
    println!("@native loop: native {native:?}, interpreted {interpreted:?}, {ratio:.2}x");
    assert!(
        ratio > 1.5,
        "native code should run the @native loop well ahead of the interpreter: {ratio:.2}x"
    );
}
