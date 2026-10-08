//! The pane socket (§11.2), `AUK_PANE_SOCKET`: a Unix socket for programs running in the pane.
//! Frames are a u32 little-endian length plus one JSON object. The greeting `{"hello":{}}` is
//! answered by `{"welcome":{"ops":[]}}`. v1 offers no ops, so programs can feature-detect:
//! every request `{"id":N, KIND: req}` is answered `{"id":N, KIND: {"error":{…}}}`, and
//! messages without an id are skipped.

use std::io;
use std::path::Path;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

/// The largest frame accepted.
const MAX_FRAME: usize = 256 << 20;

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
    let kind = msg.keys().find(|k| *k != "id")?;
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
        task.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
