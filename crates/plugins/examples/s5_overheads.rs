//! Spike S5 measurements: interrupt and per-call budget overheads, trip latency, native codegen
//! speedup, and worker round trips. Run in release:
//!
//! ```text
//! cargo run --release -p auk-plugins --example s5_overheads
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use auk_plugins::mlua::{Function, Lua};
use auk_plugins::{
    Budget, Context, HostConfig, HostEvent, MEMORY_LIMIT, PluginHost, PluginInfo, PluginSettings,
    SpawnSpec, Vm, VmOptions,
};

const LOOPS: &str = "
    @native local function native_loop(n) local s = 0 for i = 1, n do s += i % 7 end return s end
    local function calls(n) local function k(x) return x + 1 end local s = 0 for i = 1, n do s = k(s) end return s end
    return native_loop, calls
";
const LOOP_N: f64 = 20_000_000.0;

fn best_ns_per_iter(f: &Function) -> f64 {
    (0..5)
        .map(|_| {
            let t = Instant::now();
            let _: f64 = f.call(LOOP_N).expect("loop");
            t.elapsed().as_secs_f64() * 1e9 / LOOP_N
        })
        .fold(f64::MAX, f64::min)
}

fn options(dir: &std::path::Path, context: Context, jit: bool) -> VmOptions {
    VmOptions {
        context,
        plugin: PluginInfo {
            id: "bench".into(),
            name: "Bench".into(),
            dir: dir.into(),
            data: dir.into(),
        },
        declared_blocks: vec![],
        declared_lenses: vec![],
        memory_limit: MEMORY_LIMIT,
        jit,
        log: Arc::new(|_| {}),
    }
}

fn percentile(samples: &mut [Duration], p: f64) -> Duration {
    samples.sort();
    samples[((samples.len() - 1) as f64 * p).round() as usize]
}

fn main() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical");
    let file = root.join("bench.luau");

    println!("== safepoint cost (ns per loop iteration, best of 5) ==");
    let bare = Lua::new();
    let (bare_native, bare_calls): (Function, Function) = bare.load(LOOPS).eval().expect("bare");
    let vm = Vm::new(options(&root, Context::Host, true)).expect("vm");
    let (vm_native, vm_calls): (Function, Function) = vm
        .compile(LOOPS, &file)
        .expect("compile")
        .call(())
        .expect("eval");
    let interp = Vm::new(options(&root, Context::Host, false)).expect("vm");
    let (interp_native, interp_calls): (Function, Function) = interp
        .compile(LOOPS, &file)
        .expect("compile")
        .call(())
        .expect("eval");
    for (name, bare, ours, interp) in [
        ("@native loop", &bare_native, &vm_native, &interp_native),
        ("call loop", &bare_calls, &vm_calls, &interp_calls),
    ] {
        let (b, o, i) = (
            best_ns_per_iter(bare),
            best_ns_per_iter(ours),
            best_ns_per_iter(interp),
        );
        println!(
            "{name:14} no interrupt {b:6.2}   with budget interrupt {o:6.2} (+{:4.1}%)   interpreter {i:6.2} ({:.2}x slower than native)",
            (o / b - 1.0) * 100.0,
            i / o
        );
    }

    println!("\n== per-call budget cost ==");
    let empty = vm.compile("return 1", &file).expect("compile");
    let n = 200_000;
    let t = Instant::now();
    for _ in 0..n {
        let _: i64 = empty.call(()).expect("call");
    }
    let direct = t.elapsed().as_secs_f64() * 1e9 / n as f64;
    let t = Instant::now();
    for _ in 0..n {
        let _: i64 = vm
            .call("bench", Budget::Formatter, &empty, ())
            .expect("call");
    }
    let budgeted = t.elapsed().as_secs_f64() * 1e9 / n as f64;
    println!(
        "empty call: direct {direct:.0} ns, through Vm::call {budgeted:.0} ns (+{:.0} ns)",
        budgeted - direct
    );

    println!("\n== trip latency (time past the budget until the call returns) ==");
    let spin = vm.compile("while true do end", &file).expect("compile");
    for (budget, runs) in [
        (Budget::Formatter, 50),
        (Budget::Window, 20),
        (Budget::Host, 3),
    ] {
        let mut late: Vec<Duration> = (0..runs)
            .map(|i| {
                let vm = Vm::new(options(&root, Context::Window, true)).expect("vm");
                let spin = vm.compile("while true do end", &file).expect("compile");
                let t = Instant::now();
                let _ = vm.call::<()>(&format!("hook{i}"), budget, &spin, ());
                t.elapsed().saturating_sub(budget.duration())
            })
            .collect();
        println!(
            "{:>4} ms budget: overshoot p50 {:?}, p95 {:?}, max {:?} ({runs} runs)",
            budget.millis(),
            percentile(&mut late, 0.5),
            percentile(&mut late, 0.95),
            late.last().copied().unwrap_or_default()
        );
    }
    drop(spin);

    println!("\n== worker round trips ==");
    let plugins = root.join("plugins");
    let pkg = plugins.join("echo");
    std::fs::create_dir_all(&pkg).expect("mkdir");
    std::fs::write(
        pkg.join("plugin.toml"),
        "schema = 1\nid = \"echo\"\nname = \"Echo\"\nversion = \"1\"\nhost = \"host.luau\"\n",
    )
    .expect("write");
    std::fs::write(
        pkg.join("host.luau"),
        "tern.on('cwd', function(ev, cx) cx:copy(ev.path) end) tern.on('spawn', function(spec) spec.env.X = '1' return spec end)",
    )
    .expect("write");
    let started = Instant::now();
    let host = PluginHost::start(HostConfig {
        plugins_dir: plugins,
        state_dir: root.join("state"),
        settings: PluginSettings::default(),
        waker: None,
        log: Arc::new(|_| {}),
    });
    println!("host start with one plugin: {:?}", started.elapsed());
    let mut events: Vec<Duration> = (0..2000)
        .map(|_| {
            let t = Instant::now();
            host.dispatch(HostEvent::Cwd {
                pane: 1,
                path: "/".into(),
            });
            host.outputs().recv().expect("effect");
            t.elapsed()
        })
        .collect();
    println!(
        "event -> effect: p50 {:?}, p95 {:?}",
        percentile(&mut events, 0.5),
        percentile(&mut events, 0.95)
    );
    let spec = SpawnSpec {
        program: "/bin/zsh".into(),
        args: vec!["-l".into()],
        cwd: "/".into(),
        env: vec![],
    };
    let mut spawns: Vec<Duration> = (0..2000)
        .map(|_| {
            let t = Instant::now();
            let out = host.filter_spawn(spec.clone());
            assert_eq!(out.env.len(), 1);
            t.elapsed()
        })
        .collect();
    println!(
        "spawn filter: p50 {:?}, p95 {:?}",
        percentile(&mut spawns, 0.5),
        percentile(&mut spawns, 0.95)
    );
}
