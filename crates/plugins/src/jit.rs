//! Whether Luau's native code generator really runs in this process (§20, spike S5; `auk
//! doctor`).
//!
//! Luau compiles `@native` functions into memory it maps with `MAP_JIT`. When the process may not
//! map such memory (the hardened runtime without `com.apple.security.cs.allow-jit`), compiling
//! quietly falls back to the interpreter, so neither `tern.runtime.jit` nor a successful call
//! says whether native code ran. The probe asks Luau itself: its perf-log hook reports each
//! function only once the function's code sits in executable memory and is bound to the
//! function, which from then on enters it natively. The speedup over the interpreter is reported
//! beside it as corroboration.

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_void};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mlua::Function;

use crate::vm::{Context, MEMORY_LIMIT, PluginInfo, Vm, VmOptions, codegen_supported};

unsafe extern "C" {
    /// `void Luau::CodeGen::setPerfLog(void* context, PerfLogFn logFn)`, where `PerfLogFn` is
    /// `void (*)(void* context, uintptr_t addr, unsigned size, const char* symbol)`. Luau has no C
    /// entry point for it; the mangled name pins the signature, so a Luau that changes it fails
    /// to link rather than misbehaving.
    #[link_name = "_ZN4Luau7CodeGen10setPerfLogEPvPFvS1_mjPKcE"]
    fn luau_set_perf_log(context: *mut c_void, log: Option<PerfLogFn>);
}

type PerfLogFn = unsafe extern "C" fn(*mut c_void, usize, u32, *const c_char);

thread_local! {
    /// The native functions (name, code bytes) Luau has bound on this thread while the hook is
    /// set.
    static EMITTED: RefCell<Vec<(String, u32)>> = const { RefCell::new(Vec::new()) };
}

/// Records the functions Luau reports. Its own entries (`<luau gate>`, `<luau helpers>`) are not
/// functions; a function's entry is `<luau> <source>:<line> <name>`.
unsafe extern "C" fn record_native(_: *mut c_void, _addr: usize, size: u32, symbol: *const c_char) {
    // SAFETY: Luau passes a NUL-terminated name it formatted for this call.
    let name = unsafe { CStr::from_ptr(symbol) }.to_string_lossy();
    if let Some(function) = name.strip_prefix("<luau> ") {
        EMITTED.with(|e| e.borrow_mut().push((function.to_owned(), size)));
    }
}

/// Luau keeps the hook in a plain global, so one probe runs at a time.
static PROBE: Mutex<()> = Mutex::new(());

/// Sets the hook for its lifetime.
struct PerfLog;

impl PerfLog {
    fn start() -> PerfLog {
        // SAFETY: the callback is a plain function that lives for the whole program, and the
        // PROBE lock keeps other probes from setting the global at the same time.
        unsafe { luau_set_perf_log(std::ptr::null_mut(), Some(record_native)) };
        PerfLog
    }

    /// The functions (count, code bytes) from `source` the hook saw on this thread since the
    /// last call. Other code (the API's own Luau, for one) compiles in the same VMs.
    fn take(&self, source: &str) -> (u64, u64) {
        let prefix = format!("{source}:");
        EMITTED
            .with(|e| std::mem::take(&mut *e.borrow_mut()))
            .iter()
            .filter(|(name, _)| name.starts_with(&prefix))
            .fold((0, 0), |(n, bytes), (_, size)| {
                (n + 1, bytes + u64::from(*size))
            })
    }
}

impl Drop for PerfLog {
    fn drop(&mut self) {
        // SAFETY: clearing the hook; Luau checks it for null before each use.
        unsafe { luau_set_perf_log(std::ptr::null_mut(), None) };
    }
}

/// What [`probe`] found.
#[derive(Debug, Clone, PartialEq)]
pub struct JitProbe {
    /// Whether Luau's code generator supports this CPU.
    pub codegen_supported: bool,
    /// Functions Luau compiled to native code and bound, with codegen on (the chunk has one).
    pub native_functions: u64,
    /// The size of their machine code.
    pub native_code_bytes: u64,
    /// Functions bound to native code with codegen off: 0 unless the switch is broken.
    pub interpreted_functions: u64,
    /// Best of five runs of the `@native` loop, with codegen on and off.
    pub native_time: Duration,
    pub interpreted_time: Duration,
    /// Whether both VMs computed the same result.
    pub results_match: bool,
    /// What went wrong building or running the loop, if anything did.
    pub error: Option<String>,
}

impl JitProbe {
    /// The interpreter's time over native code's.
    pub fn speedup(&self) -> f64 {
        self.interpreted_time.as_secs_f64() / self.native_time.as_secs_f64().max(1e-9)
    }

    /// Whether native code ran: Luau bound the loop to native code with codegen on, bound
    /// nothing with it off, and both computed the same result.
    pub fn native_ran(&self) -> bool {
        self.codegen_supported
            && self.error.is_none()
            && self.native_functions >= 1
            && self.interpreted_functions == 0
            && self.results_match
    }
}

const LOOP: &str = "
    @native local function sum(n) local s = 0 for i = 1, n do s += (i * 3) % 7 end return s end
    return sum
";
const LOOP_N: f64 = 5_000_000.0;

/// Compiles and times a `@native` loop in two plugin VMs, one with native codegen on and one
/// with it off. `scratch` is an existing directory the VMs treat as their package.
pub fn probe(scratch: &Path) -> JitProbe {
    let _one = PROBE.lock().unwrap_or_else(|e| e.into_inner());
    let log = PerfLog::start();
    let file = scratch.join("probe.luau");
    // Luau names a chunk's functions by its chunk name minus the leading `@`.
    let source = crate::require::chunk_name(&file);
    let source = source.strip_prefix('@').unwrap_or(&source).to_owned();
    log.take(&source);
    let mut probe = JitProbe {
        codegen_supported: codegen_supported(),
        native_functions: 0,
        native_code_bytes: 0,
        interpreted_functions: 0,
        native_time: Duration::ZERO,
        interpreted_time: Duration::ZERO,
        results_match: false,
        error: None,
    };
    let run = |jit: bool| -> Result<(Function, Vm), String> {
        let vm = Vm::new(options(scratch, jit)).map_err(|e| format!("VM: {e}"))?;
        let f: Function = vm
            .compile(LOOP, &file)
            .and_then(|chunk| chunk.call(()))
            .map_err(|e| format!("compile: {e}"))?;
        Ok((f, vm))
    };
    let result = (|| -> Result<(), String> {
        let (native_fn, _native_vm) = run(true)?;
        (probe.native_functions, probe.native_code_bytes) = log.take(&source);
        let (interp_fn, _interp_vm) = run(false)?;
        probe.interpreted_functions = log.take(&source).0;
        let (native_sum, native_time) = best_of_five(&native_fn)?;
        let (interp_sum, interp_time) = best_of_five(&interp_fn)?;
        probe.results_match = native_sum == interp_sum;
        probe.native_time = native_time;
        probe.interpreted_time = interp_time;
        Ok(())
    })();
    probe.error = result.err();
    probe
}

fn best_of_five(f: &Function) -> Result<(f64, Duration), String> {
    let mut best = Duration::MAX;
    let mut sum = 0.0;
    for _ in 0..5 {
        let t = Instant::now();
        sum = f.call::<f64>(LOOP_N).map_err(|e| format!("call: {e}"))?;
        best = best.min(t.elapsed());
    }
    Ok((sum, best))
}

fn options(dir: &Path, jit: bool) -> VmOptions {
    VmOptions {
        context: Context::Host,
        plugin: PluginInfo {
            id: "auk.jit-probe".into(),
            name: "JIT probe".into(),
            dir: dir.into(),
            data: dir.into(),
        },
        declared_blocks: vec![],
        declared_lenses: vec![],
        memory_limit: MEMORY_LIMIT,
        jit,
        log: Arc::new(|_| {}),
        panes: None,
        builtin_commands: None,
    }
}
