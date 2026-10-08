//! `surface-play` (§7.15): feeds a TSP recording's program-side messages into a pane, as if its
//! program wrote them, so recordings become a regression corpus.
//!
//! `surface-play [--pane TARGET] FILE [from N] [until N] [paced [TIMES]]`
//!
//! - The recording is Tern's JSONL format, `{"t","dir","verb","params","body"}`. Its `out`
//!   messages `o t s f x b` are encoded as the program would send them (chunked past `apc`), and
//!   hand-written `osc` and `pty` lines become an OSC string and raw output. `q` and `in` lines are
//!   skipped: the terminal answers for itself.
//! - The first play in a pane restarts it as a stand-in program ([`stand_in`]): a raw-mode reader
//!   that swallows whatever the terminal answers, so the answers pass the mode gate and never
//!   reach a shell.
//! - A frame is named by its `s` (`3`) or by surface and `s` (`chat#3`), as in Tern (book:11581,
//!   16730). `from F` starts after frame F, so a second play continues the first; it needs an
//!   earlier play in the pane. `until F` stops after frame F. `paced` keeps the recorded gaps,
//!   divided by `TIMES` (default 1); without it the messages go in one after another.

use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::pane::Pane;
use crate::server::{Daemon, lock};

/// The longest pause `paced` keeps between two messages.
const MAX_GAP: Duration = Duration::from_secs(2);

/// A frame of a recording: its `s`, and optionally its surface (`chat#3`).
#[derive(Clone, Debug, PartialEq)]
pub struct FrameRef {
    pub sf: Option<String>,
    pub s: u64,
}

impl FrameRef {
    fn parse(word: &str) -> Option<FrameRef> {
        let (sf, s) = match word.rsplit_once('#') {
            Some((sf, s)) if !sf.is_empty() => (Some(sf.to_owned()), s),
            Some(_) => return None,
            None => (None, word),
        };
        Some(FrameRef {
            sf,
            s: s.parse().ok()?,
        })
    }

    /// Whether a frame `(sf, s)` is this one.
    fn names(&self, frame: &(String, u64)) -> bool {
        self.s == frame.1 && self.sf.as_ref().is_none_or(|sf| *sf == frame.0)
    }
}

impl std::fmt::Display for FrameRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.sf {
            Some(sf) => write!(f, "{sf}#{}", self.s),
            None => write!(f, "{}", self.s),
        }
    }
}

/// How a recording plays.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayOptions {
    pub from: Option<FrameRef>,
    pub until: Option<FrameRef>,
    /// `paced`, with its speed-up.
    pub paced: Option<f64>,
}

impl PlayOptions {
    /// Parses `[from N] [until N] [paced [TIMES]]`.
    pub fn parse(words: &[String]) -> Result<Self, String> {
        let mut opts = PlayOptions {
            from: None,
            until: None,
            paced: None,
        };
        let mut it = words.iter().peekable();
        while let Some(word) = it.next() {
            let mut number = |what: &str| -> Result<FrameRef, String> {
                it.next().and_then(|w| FrameRef::parse(w)).ok_or_else(|| {
                    format!("surface-play: `{what}` needs a frame: `s` or `<sf>#<s>`")
                })
            };
            match word.as_str() {
                "from" => opts.from = Some(number("from")?),
                "until" => opts.until = Some(number("until")?),
                "paced" => {
                    let times = match it.peek().and_then(|w| w.parse::<f64>().ok()) {
                        Some(t) if t > 0.0 => {
                            it.next();
                            t
                        }
                        _ => 1.0,
                    };
                    opts.paced = Some(times);
                }
                other => return Err(format!("surface-play: unexpected `{other}`")),
            }
        }
        Ok(opts)
    }
}

/// One message to feed, with its recorded time.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub t: Option<f64>,
    pub bytes: Vec<u8>,
    /// For an `f`: its surface and `s`.
    pub frame: Option<(String, u64)>,
}

/// Reads a recording into the bytes its program side would write, in order.
pub fn load(path: &Path, apc: usize) -> Result<Vec<Step>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut steps = Vec::new();
    let mut chunk_ids = 0u64;
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let rec: Value =
            serde_json::from_str(line).map_err(|e| format!("{}:{}: {e}", path.display(), n + 1))?;
        if rec.get("dir").and_then(Value::as_str) != Some("out") {
            continue;
        }
        let verb = rec.get("verb").and_then(Value::as_str).unwrap_or_default();
        let t = rec.get("t").and_then(Value::as_f64);
        let body = rec.get("body").cloned().unwrap_or(Value::Null);
        let params: Vec<(String, String)> = rec
            .get("params")
            .and_then(Value::as_object)
            .map(|p| {
                p.iter()
                    .filter(|(k, _)| !matches!(k.as_str(), "c" | "m"))
                    .map(|(k, v)| {
                        let v = match v {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        (k.clone(), v)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut bytes = Vec::new();
        match verb {
            "o" | "t" | "s" | "f" | "x" | "b" => {
                let body = match &body {
                    Value::String(s) => s.clone().into_bytes(),
                    other => other.to_string().into_bytes(),
                };
                let params: Vec<(&str, &str)> = params
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();
                let chunk = if body.len() > apc {
                    chunk_ids += 1;
                    format!("play{chunk_ids}")
                } else {
                    String::new()
                };
                auk_tsp::wire::encode_chunked(&mut bytes, verb, &params, &body, apc, &chunk);
            }
            "osc" => {
                let text = body.as_str().unwrap_or_default();
                bytes.extend_from_slice(b"\x1b]");
                bytes.extend_from_slice(text.as_bytes());
                bytes.extend_from_slice(b"\x1b\\");
            }
            "pty" => bytes.extend_from_slice(body.as_str().unwrap_or_default().as_bytes()),
            _ => continue,
        }
        let frame = (verb == "f").then(|| {
            let sf = body.get("sf").and_then(Value::as_str).unwrap_or_default();
            let s = body.get("s").and_then(Value::as_u64).unwrap_or_default();
            (sf.to_owned(), s)
        });
        steps.push(Step { t, bytes, frame });
    }
    Ok(steps)
}

/// The stand-in program `surface-play` restarts a pane as: puts its tty in raw mode and reads
/// (and drops) everything the terminal writes, until the tty closes.
pub fn stand_in() -> io::Result<()> {
    use rustix::termios::{self, InputModes, LocalModes, OptionalActions};
    let stdin = rustix::stdio::stdin();
    let mut attrs = termios::tcgetattr(stdin)?;
    attrs.local_modes &=
        !(LocalModes::ICANON | LocalModes::ECHO | LocalModes::ISIG | LocalModes::IEXTEN);
    attrs.input_modes &= !(InputModes::ICRNL | InputModes::IXON);
    termios::tcsetattr(stdin, OptionalActions::Now, &attrs)?;
    let mut buf = [0u8; 4096];
    loop {
        match rustix::io::read(stdin, &mut buf) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => {}
            Err(e) => return Err(e.into()),
        }
    }
}

impl Daemon {
    /// Plays `steps` into a pane, restarting it as the stand-in first if needed. Returns how many
    /// messages and frames went in.
    pub(crate) async fn play(
        self: &Arc<Self>,
        pane: Arc<Mutex<Pane>>,
        steps: Vec<Step>,
        opts: PlayOptions,
    ) -> Result<Value, String> {
        let (id, stand_in) = {
            let p = lock(&pane);
            (p.id, p.stand_in)
        };
        // `from` continues an earlier play: it starts after the named frame.
        let start = match &opts.from {
            None => 0,
            Some(_) if !stand_in => {
                return Err(format!(
                    "surface-play: `from` continues an earlier play, and pane {id} has none"
                ));
            }
            Some(from) => {
                steps
                    .iter()
                    .position(|st| st.frame.as_ref().is_some_and(|f| from.names(f)))
                    .ok_or_else(|| format!("surface-play: the recording has no frame {from}"))?
                    + 1
            }
        };
        if !stand_in {
            self.restart_as_stand_in(&pane, id).await?;
        }
        let mut frames = 0u64;
        let mut played = 0u64;
        let mut last_t: Option<f64> = None;
        for step in steps.into_iter().skip(start) {
            if step.frame.is_some() {
                frames += 1;
            }
            if let (Some(times), Some(t), Some(last)) = (opts.paced, step.t, last_t) {
                let gap = Duration::from_secs_f64(((t - last) / times).max(0.0) / 1000.0);
                tokio::time::sleep(gap.min(MAX_GAP)).await;
            }
            last_t = step.t.or(last_t);
            {
                let mut p = lock(&pane);
                if p.exit.is_some() {
                    return Err(format!("pane {id} exited during the play"));
                }
                p.on_output(&step.bytes);
            }
            played += 1;
            if let (Some(until), Some(frame)) = (&opts.until, &step.frame)
                && until.names(frame)
            {
                break;
            }
        }
        Ok(json!({"pane": id, "played": played, "frames": frames}))
    }

    async fn restart_as_stand_in(
        self: &Arc<Self>,
        pane: &Arc<Mutex<Pane>>,
        id: auk_mirror::PaneId,
    ) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| format!("the auk binary: {e}"))?;
        let (old_pid, spec) = {
            let p = lock(pane);
            let term = p.replica.terminal();
            let mut spec = self.spawn_spec(
                id,
                &p.kind,
                &p.window_key,
                &p.cwd(),
                term.cols(),
                term.rows(),
            );
            spec.program = exe.into_os_string();
            spec.args = vec!["stand-in".into()];
            (p.pid.filter(|_| p.exit.is_none()), spec)
        };
        if !spec.cwd.is_dir() {
            return Err(format!("{} is not a directory", spec.cwd.display()));
        }
        self.start_process(pane, id, &spec)?;
        lock(pane).stand_in = true;
        if let Some(pid) = old_pid {
            crate::server::hang_up(pid);
        }
        // Answers pass the gate once the stand-in is in raw mode.
        for _ in 0..200 {
            if lock(pane).tty_mode().is_some_and(|m| m.accepts_tsp_input()) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Err("the stand-in program didn't switch its tty to raw mode".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_parse() {
        let words = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        assert_eq!(
            PlayOptions::parse(&words("from 3 until chat#9 paced 4")).unwrap(),
            PlayOptions {
                from: Some(FrameRef { sf: None, s: 3 }),
                until: Some(FrameRef {
                    sf: Some("chat".into()),
                    s: 9
                }),
                paced: Some(4.0)
            }
        );
        assert!(PlayOptions::parse(&words("from #3")).is_err());
        assert_eq!(
            PlayOptions::parse(&words("paced")).unwrap().paced,
            Some(1.0)
        );
        assert!(PlayOptions::parse(&words("from x")).is_err());
        assert!(PlayOptions::parse(&words("faster")).is_err());
    }

    #[test]
    fn recordings_encode_as_the_program_wrote_them() {
        let dir = std::env::temp_dir().join(format!("auk-play-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("r.jsonl");
        let big = "x".repeat(100);
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n{}\n{}\n",
                r#"{"t":1,"dir":"out","verb":"q","params":{},"body":{"q":"hello","v":[1]}}"#,
                r#"{"t":2,"dir":"out","verb":"o","params":{},"body":{"id":"s1","mode":"flow"}}"#,
                format_args!(
                    r#"{{"t":3,"dir":"out","verb":"f","params":{{}},"body":{{"sf":"s1","s":1,"ops":[["add","t","s1",null,{{"id":"t","k":"text","p":{{"text":"{big}"}}}}]]}}}}"#
                ),
                r#"{"t":4,"dir":"in","verb":"e","params":{},"body":{"ev":"ack","sf":"s1","s":1}}"#,
                r#"{"dir":"out","verb":"osc","body":"133;A"}"#,
            ),
        )
        .unwrap();
        let steps = load(&path, 64).unwrap();
        assert_eq!(steps.len(), 3, "q and `in` lines are skipped");
        assert_eq!(
            steps[0].bytes,
            b"\x1b_tsp;o;{\"id\":\"s1\",\"mode\":\"flow\"}\x1b\\"
        );
        assert_eq!(steps[1].frame, Some(("s1".to_owned(), 1)));
        assert!(
            steps[1]
                .bytes
                .windows(4)
                .filter(|w| *w == b"\x1b_ts")
                .count()
                > 1,
            "chunked past apc"
        );
        assert_eq!(steps[2].bytes, b"\x1b]133;A\x1b\\");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
