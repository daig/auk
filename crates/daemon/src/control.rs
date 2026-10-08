//! The control endpoint (spec §11.3): one command per line in, one JSON line out.
//!
//! Replies take Tern's shape: `{"ok":true}`, `{"ok":true,"value":…}` or
//! `{"ok":false,"error":"…"}`. M0 knows two commands, `ping` and `version`. The headless
//! harness and the scenario commands arrive with the daemon in M1.

use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::thread;

use serde_json::{Value, json};

/// Answers one command line.
pub fn handle(line: &str) -> Value {
    let mut words = line.split_whitespace();
    match words.next() {
        None => json!({"ok": false, "error": "empty command"}),
        Some("ping") => json!({"ok": true, "value": "pong"}),
        Some("version") => json!({"ok": true, "value": env!("CARGO_PKG_VERSION")}),
        Some(other) => json!({"ok": false, "error": format!("unknown command `{other}`")}),
    }
}

/// Serves one client until it disconnects.
pub fn serve_client(stream: UnixStream) -> io::Result<()> {
    let mut writer = stream.try_clone()?;
    for line in BufReader::new(stream).lines() {
        let reply = handle(line?.trim());
        writeln!(writer, "{reply}")?;
    }
    Ok(())
}

/// Binds `path`, replacing a stale socket but never another kind of file.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => fs::remove_file(path)?,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a socket", path.display()),
            ));
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    UnixListener::bind(path)
}

/// Serves clients on `listener`, each on its own thread, until accepting fails.
pub fn serve_listener(listener: UnixListener) -> io::Result<()> {
    for stream in listener.incoming() {
        let stream = stream?;
        thread::spawn(move || {
            if let Err(err) = serve_client(stream) {
                eprintln!("control client: {err}");
            }
        });
    }
    Ok(())
}

/// Listens on `path` and serves clients until accepting fails.
pub fn serve(path: &Path) -> io::Result<()> {
    serve_listener(bind(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    #[test]
    fn answers_known_and_unknown_commands() {
        assert_eq!(handle("ping"), json!({"ok": true, "value": "pong"}));
        assert_eq!(handle(""), json!({"ok": false, "error": "empty command"}));
        assert_eq!(handle("nope 1"), json!({"ok": false, "error": "unknown command `nope`"}));
    }

    #[test]
    fn serves_a_client_over_a_socket() {
        let dir = std::env::temp_dir().join(format!("auk-control-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("control.sock");
        let listener = bind(&path).unwrap();
        thread::spawn(move || serve_listener(listener));

        let mut stream = UnixStream::connect(&path).unwrap();
        stream.write_all(b"ping\nversion\n").unwrap();
        let mut lines = BufReader::new(stream).lines();
        let first: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        let second: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert_eq!(first, json!({"ok": true, "value": "pong"}));
        assert_eq!(second["ok"], json!(true));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_to_replace_a_regular_file() {
        let dir = std::env::temp_dir().join(format!("auk-control-file-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("not-a-socket");
        fs::write(&path, b"keep me").unwrap();
        assert!(bind(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"keep me");
        let _ = fs::remove_dir_all(&dir);
    }
}
