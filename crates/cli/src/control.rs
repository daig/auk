//! A control-endpoint client: one command line out, one JSON line back.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use serde_json::Value;

/// A connection to a control endpoint (the daemon's or a headless window's).
#[derive(Debug)]
pub struct Control {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

/// A command the endpoint refused.
#[derive(Debug)]
pub enum ControlError {
    Io(io::Error),
    /// `{"ok":false,…}`: the message, and whether it was a `wait` timeout.
    Refused {
        message: String,
        timeout: bool,
    },
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlError::Io(err) => write!(f, "{err}"),
            ControlError::Refused { message, .. } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ControlError {}

impl From<io::Error> for ControlError {
    fn from(err: io::Error) -> Self {
        ControlError::Io(err)
    }
}

impl Control {
    pub fn connect(path: &Path) -> io::Result<Control> {
        let writer = UnixStream::connect(path)?;
        Ok(Control {
            reader: BufReader::new(writer.try_clone()?),
            writer,
        })
    }

    /// Gives up on a reply after `timeout` (`None` waits forever).
    pub fn set_timeout(&self, timeout: Option<std::time::Duration>) -> io::Result<()> {
        self.writer.set_read_timeout(timeout)
    }

    /// Sends a raw command line and returns the reply object.
    pub fn send_line(&mut self, line: &str) -> io::Result<Value> {
        if line.contains(['\n', '\r']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a command line can't hold a line break; send keys for Enter",
            ));
        }
        self.writer.write_all(format!("{line}\n").as_bytes())?;
        let mut reply = String::new();
        if self.reader.read_line(&mut reply)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the endpoint closed",
            ));
        }
        serde_json::from_str(&reply).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Reads one more line (an event stream's); `None` when the endpoint closed.
    pub fn read_line(&mut self) -> io::Result<Option<String>> {
        let mut line = String::new();
        Ok((self.reader.read_line(&mut line)? > 0).then_some(line))
    }

    /// Sends a command built from words (quoted as a shell would) and returns its `value`
    /// (`null` for a bare `{"ok":true}`).
    pub fn request<S: AsRef<str>>(&mut self, words: &[S]) -> Result<Value, ControlError> {
        let line = quote(words);
        let reply = self.send_line(&line)?;
        if reply["ok"] == Value::Bool(true) {
            Ok(reply.get("value").cloned().unwrap_or(Value::Null))
        } else {
            Err(ControlError::Refused {
                message: reply["error"]
                    .as_str()
                    .unwrap_or("the command failed")
                    .to_owned(),
                timeout: reply["timeout"] == Value::Bool(true),
            })
        }
    }
}

/// Joins words into one command line, quoting as needed. Newlines and other control characters
/// can't travel on the line protocol; use `send … keys` for those.
pub fn quote<S: AsRef<str>>(words: &[S]) -> String {
    words
        .iter()
        .map(|w| match shlex::try_quote(w.as_ref()) {
            Ok(q) => q.into_owned(),
            Err(_) => format!("'{}'", w.as_ref().replace('\0', "")),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_words() {
        assert_eq!(quote(&["send", "1", "text", "a b"]), "send 1 text 'a b'");
        assert_eq!(
            shlex::split(&quote(&["x", "it's", ""])).unwrap(),
            ["x", "it's", ""]
        );
    }
}
