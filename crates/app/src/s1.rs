//! Spike S1's reference side (§23): the bytes Ghostty sends its program for the same physical
//! keystrokes. Ghostty runs a recorder (`cat` on a raw tty into a file); each stroke is posted
//! to its process as a `CGEvent`, which needs the Accessibility permission. Used by the
//! `keyprobe` example and the app suite's S1 test.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use auk_platform_macos::synth::{Stroke, can_post_events, post_to_pid};

/// Why the reference couldn't be recorded.
pub const PERMISSION: &str = "posting key events to another app needs the Accessibility \
    permission for the process running this (System Settings > Privacy & Security > \
    Accessibility)";

/// How Ghostty should treat the keys.
#[derive(Clone, Debug, Default)]
pub struct GhosttyOptions {
    /// Push kitty keyboard flag 1 before recording.
    pub kitty: bool,
    /// Ghostty's `macos-option-as-alt`: `false`, `left`, `right` or `true`.
    pub option_as_alt: Option<String>,
}

/// Records what Ghostty sends for each stroke: one byte string per stroke.
pub fn record_ghostty(strokes: &[Stroke], opts: &GhosttyOptions) -> Result<Vec<Vec<u8>>, String> {
    if !can_post_events() {
        return Err(PERMISSION.into());
    }
    let dir = std::env::temp_dir().join(format!("auk-s1-{}-{}", std::process::id(), nanos()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("temp dir: {e}"))?;
    let recorded = record_in(&dir, strokes, opts);
    let _ = std::fs::remove_dir_all(&dir);
    recorded
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

fn record_in(dir: &Path, strokes: &[Stroke], opts: &GhosttyOptions) -> Result<Vec<Vec<u8>>, String> {
    let out = dir.join("bytes");
    let ready = dir.join("ready");
    let script = dir.join("record.sh");
    let push = if opts.kitty { "printf '\\033[>1u'; " } else { "" };
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n{push}stty raw -echo; : > '{}'; exec cat > '{}'\n",
            ready.display(),
            out.display()
        ),
    )
    .map_err(|e| format!("write the recorder: {e}"))?;
    Command::new("chmod")
        .arg("+x")
        .arg(&script)
        .status()
        .map_err(|e| format!("chmod: {e}"))?;
    let before = ghostty_pids();
    let mut cmd = Command::new("open");
    cmd.args(["-na", "Ghostty", "--args"]);
    if let Some(alt) = &opts.option_as_alt {
        cmd.arg(format!("--macos-option-as-alt={alt}"));
    }
    let status = cmd
        .arg("-e")
        .arg(&script)
        .status()
        .map_err(|e| format!("open Ghostty: {e}"))?;
    if !status.success() {
        return Err("Ghostty isn't installed (brew install --cask ghostty)".into());
    }
    let started = Instant::now();
    while !ready.exists() {
        if started.elapsed() > Duration::from_secs(15) {
            return Err("Ghostty didn't run the recorder; open Ghostty once from Finder to clear \
                 Gatekeeper's first-launch prompt"
                .into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // The recorder has set the tty raw; give the window a moment to become key.
    std::thread::sleep(Duration::from_millis(700));
    let Some(pid) = ghostty_pids().into_iter().find(|p| !before.contains(p)) else {
        return Err("no new Ghostty process".into());
    };
    let mut recorded = Vec::new();
    let mut seen = 0;
    for stroke in strokes {
        post_to_pid(pid, *stroke);
        std::thread::sleep(Duration::from_millis(150));
        let all = std::fs::read(&out).unwrap_or_default();
        recorded.push(all[seen.min(all.len())..].to_vec());
        seen = all.len();
    }
    let _ = Command::new("kill").arg(pid.to_string()).status();
    Ok(recorded)
}

fn ghostty_pids() -> Vec<i32> {
    let Ok(out) = Command::new("pgrep").arg("-x").arg("ghostty").output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}
