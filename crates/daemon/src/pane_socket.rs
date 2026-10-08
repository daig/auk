//! The pane socket (§11.2), `AUK_PANE_SOCKET`: a Unix socket for programs running in the pane.
//! Frames are a u32 little-endian length plus one JSON object. The greeting `{"hello":{}}` is
//! answered by `{"welcome":{"ops":[]}}`. v1 offers no ops, so programs can feature-detect.
//!
//! A request is `{"id":N, KIND: req}`. Unknown members and kinds are skipped: the kind is the
//! member naming a kind of Tern's vocabulary ([`KINDS`]), wherever it sits among the others, and
//! the answer is keyed by it, `{"id":N, KIND: {"error":{"kind":"unsupported",…}}}` since v1 runs
//! none. A request naming no known kind, and a message without an id, get no answer. A frame
//! longer than 256 MiB closes the connection.

use std::io;
use std::path::Path;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

/// The largest frame accepted.
pub const MAX_FRAME: usize = 256 << 20;

/// The request kinds of Tern's pane-socket vocabulary (omp uses `browser` and `fork`). v1
/// offers none of them, and answers each with `unsupported`.
pub const KINDS: [&str; 2] = ["browser", "fork"];

/// Binds the pane socket at `path` and serves it until the task is aborted.
pub fn spawn(path: &Path) -> io::Result<tokio::task::AbortHandle> {
    let std_listener = crate::control::bind(path)?;
    std_listener.set_nonblocking(true)?;
    let listener = UnixListener::from_std(std_listener)?;
    let path = path.to_owned();
    let task = tokio::spawn(async move {
        let _cleanup = RemoveOnDrop(path);
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let _ = serve(stream).await;
            });
        }
    });
    Ok(task.abort_handle())
}

struct RemoveOnDrop(std::path::PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn serve(mut stream: UnixStream) -> io::Result<()> {
    loop {
        let len = match stream.read_u32_le().await {
            Ok(len) => len as usize,
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(err) => return Err(err),
        };
        if len > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pane socket frame too large",
            ));
        }
        let mut buf = vec![0; len];
        stream.read_exact(&mut buf).await?;
        let Ok(Value::Object(msg)) = serde_json::from_slice::<Value>(&buf) else {
            continue;
        };
        if let Some(reply) = answer(&msg) {
            let bytes = serde_json::to_vec(&reply).expect("replies serialize");
            stream.write_u32_le(bytes.len() as u32).await?;
            stream.write_all(&bytes).await?;
        }
    }
}

fn answer(msg: &Map<String, Value>) -> Option<Value> {
    if msg.contains_key("hello") {
        return Some(json!({"welcome": {"ops": []}}));
    }
    let id = msg.get("id")?;
    let kind = msg.keys().find(|k| KINDS.contains(&k.as_str()))?;
    Some(json!({
        "id": id,
        kind: {"error": {"kind": "unsupported", "message": format!("unknown op `{kind}`")}},
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn roundtrip(stream: &mut UnixStream, msg: Value) -> Value {
        let bytes = serde_json::to_vec(&msg).unwrap();
        stream.write_u32_le(bytes.len() as u32).await.unwrap();
        stream.write_all(&bytes).await.unwrap();
        let len = stream.read_u32_le().await.unwrap() as usize;
        let mut buf = vec![0; len];
        stream.read_exact(&mut buf).await.unwrap();
        serde_json::from_slice(&buf).unwrap()
    }

    #[tokio::test]
    async fn greets_and_refuses_ops() {
        let dir = std::env::temp_dir().join(format!("auk-pane-sock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("1.sock");
        let task = spawn(&path).unwrap();
        let mut s = UnixStream::connect(&path).await.unwrap();
        assert_eq!(
            roundtrip(&mut s, json!({"hello": {}})).await,
            json!({"welcome": {"ops": []}})
        );
        // Messages without an id are skipped; the next answer is the request's.
        let bytes = serde_json::to_vec(&json!({"noise": 1})).unwrap();
        s.write_u32_le(bytes.len() as u32).await.unwrap();
        s.write_all(&bytes).await.unwrap();
        let reply = roundtrip(&mut s, json!({"id": 4, "fork": {}})).await;
        assert_eq!(reply["id"], 4);
        assert_eq!(reply["fork"]["error"]["kind"], "unsupported");
        // The answer is keyed by the kind, not by an unknown member sorted before it.
        let reply = roundtrip(&mut s, json!({"aaa": 1, "browser": {"op": "x"}, "id": 5})).await;
        assert_eq!(reply, json!({"id": 5, "browser": {"error": {"kind": "unsupported",
            "message": "unknown op `browser`"}}}));
        // A request of an unknown kind is skipped: the next answer is the next request's.
        let bytes = serde_json::to_vec(&json!({"id": 6, "teleport": {}})).unwrap();
        s.write_u32_le(bytes.len() as u32).await.unwrap();
        s.write_all(&bytes).await.unwrap();
        assert_eq!(roundtrip(&mut s, json!({"id": 7, "fork": {}})).await["id"], 7);
        // A frame over 256 MiB closes the connection.
        s.write_u32_le((MAX_FRAME + 1) as u32).await.unwrap();
        let mut rest = Vec::new();
        let n = tokio::time::timeout(std::time::Duration::from_secs(10), s.read_to_end(&mut rest))
            .await
            .expect("the daemon closes the connection")
            .unwrap_or(0);
        assert_eq!(n, 0);
        task.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
