//! `auk doctor`: prints facts about this build and process as JSON: the version, where the
//! executable is, its code signature, the web client's assets, and whether Luau's native code
//! really runs here (§19, §20; spike S5).

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use serde_json::json;

pub fn main(args: Vec<OsString>) -> ExitCode {
    if args.len() > 2 {
        let help = args.get(2).is_some_and(|a| a == "--help" || a == "-h");
        eprintln!(
            "Usage: auk doctor\n\nPrints this build's version, executable, code signature, web \
             assets and whether Luau's native code runs, as JSON."
        );
        return if help {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(2)
        };
    }
    let exe = std::env::current_exe().unwrap_or_default();
    let assets = auk_web::serve::default_assets();
    let assets_present = ["auk_web.js", "auk_web_bg.wasm"]
        .iter()
        .all(|f| assets.join(f).is_file());

    let scratch = std::env::temp_dir().join(format!("auk-doctor-{}", std::process::id()));
    let jit = match std::fs::create_dir_all(&scratch) {
        Ok(()) => {
            let probe = auk_plugins::jit::probe(&scratch);
            let _ = std::fs::remove_dir_all(&scratch);
            json!({
                "codegen_supported": probe.codegen_supported,
                "native_functions": probe.native_functions,
                "native_code_bytes": probe.native_code_bytes,
                "interpreted_functions": probe.interpreted_functions,
                "native_ms": ms(probe.native_time),
                "interpreted_ms": ms(probe.interpreted_time),
                "speedup": (probe.speedup() * 100.0).round() / 100.0,
                "results_match": probe.results_match,
                "native_ran": probe.native_ran(),
                "error": probe.error,
            })
        }
        Err(e) => json!({
            "native_ran": false,
            "error": format!("can't make {}: {e}", scratch.display()),
        }),
    };

    let report = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "exe": exe.display().to_string(),
        "bundle": bundle_dir(&exe),
        "os": os_version(),
        "arch": std::env::consts::ARCH,
        "code_signature": code_signature(),
        "web_assets": { "dir": assets.display().to_string(), "present": assets_present },
        "jit": jit,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&report).unwrap_or_default()
    );
    ExitCode::SUCCESS
}

fn ms(d: std::time::Duration) -> f64 {
    (d.as_secs_f64() * 1e5).round() / 100.0
}

/// The app bundle the executable runs from (`…/Auk.app/Contents/MacOS/auk`), if any.
fn bundle_dir(exe: &Path) -> Option<String> {
    let app = exe.parent()?.parent()?.parent()?;
    let is_bundle =
        exe.parent()?.ends_with("Contents/MacOS") && app.extension().is_some_and(|e| e == "app");
    is_bundle.then(|| app.display().to_string())
}

#[cfg(target_os = "macos")]
fn os_version() -> Option<String> {
    use std::ffi::{c_char, c_int, c_void};
    unsafe extern "C" {
        fn sysctlbyname(
            name: *const c_char,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *mut c_void,
            newlen: usize,
        ) -> c_int;
    }
    let mut buf = [0u8; 64];
    let mut len = buf.len();
    // SAFETY: a read-only query into a buffer of the length given.
    let rc = unsafe {
        sysctlbyname(
            c"kern.osproductversion".as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0).then(|| {
        let s = &buf[..len.min(buf.len())];
        String::from_utf8_lossy(s.split(|&b| b == 0).next().unwrap_or(s)).into_owned()
    })
}

#[cfg(not(target_os = "macos"))]
fn os_version() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|s| s.trim().to_owned())
}

/// The kernel's view of this process's code signature (`csops(CS_OPS_STATUS)`).
#[cfg(target_os = "macos")]
fn code_signature() -> serde_json::Value {
    use std::ffi::{c_int, c_void};
    unsafe extern "C" {
        fn csops(pid: c_int, ops: u32, useraddr: *mut c_void, usersize: usize) -> c_int;
    }
    const CS_OPS_STATUS: u32 = 0;
    const CS_VALID: u32 = 0x1;
    const CS_RUNTIME: u32 = 0x1_0000;
    const CS_LINKER_SIGNED: u32 = 0x2_0000;
    let mut flags: u32 = 0;
    // SAFETY: CS_OPS_STATUS writes one u32 into the buffer given.
    let rc = unsafe {
        csops(
            std::process::id() as c_int,
            CS_OPS_STATUS,
            (&mut flags as *mut u32).cast(),
            std::mem::size_of::<u32>(),
        )
    };
    if rc != 0 {
        return json!({ "error": std::io::Error::last_os_error().to_string() });
    }
    json!({
        "flags": format!("{flags:#x}"),
        "valid": flags & CS_VALID != 0,
        "hardened_runtime": flags & CS_RUNTIME != 0,
        "linker_signed": flags & CS_LINKER_SIGNED != 0,
    })
}

#[cfg(not(target_os = "macos"))]
fn code_signature() -> serde_json::Value {
    serde_json::Value::Null
}
