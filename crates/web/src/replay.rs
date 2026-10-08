//! Replicas driven through streams of output and records (§15.2), for the determinism check
//! (AC2, S4): the same streams through the same code, natively and compiled to wasm, must give
//! the same state hashes.
//!
//! A run feeds each [`Case`] to an authoritative replica (which numbers the items), two
//! followers, and a follower rebuilt from a snapshot where the case says so. Its result is one
//! line per case: the stream position, then each replica's state hash, then the hash of a
//! replica rebuilt from the final snapshot (which can differ from the live state when the stream
//! ends inside an escape sequence: snapshots don't carry the parser's partial state).

use auk_replica::{Item, Record, Replica, Role, Settings};

/// One step of a case's stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Output(Vec<u8>),
    Record(Record),
    /// Rebuild the late follower from the authoritative replica's snapshot here.
    Snapshot,
}

/// A stream for a pane of `cols`×`rows`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Case {
    pub cols: u16,
    pub rows: u16,
    pub steps: Vec<Step>,
}

/// Encodes cases: `C cols:u16 rows:u16`, then `O len:u32 bytes`, `R len:u32 json` or `S` per
/// step, then `E`. Integers are little-endian.
pub fn encode(cases: &[Case]) -> Vec<u8> {
    let mut out = Vec::new();
    for case in cases {
        out.push(b'C');
        out.extend(case.cols.to_le_bytes());
        out.extend(case.rows.to_le_bytes());
        for step in &case.steps {
            match step {
                Step::Output(bytes) => {
                    out.push(b'O');
                    out.extend((bytes.len() as u32).to_le_bytes());
                    out.extend(bytes);
                }
                Step::Record(record) => {
                    let json = serde_json::to_vec(record).expect("records serialize");
                    out.push(b'R');
                    out.extend((json.len() as u32).to_le_bytes());
                    out.extend(json);
                }
                Step::Snapshot => out.push(b'S'),
            }
        }
        out.push(b'E');
    }
    out
}

/// Decodes [`encode`]'s format.
pub fn decode(mut bytes: &[u8]) -> Result<Vec<Case>, String> {
    fn take<'a>(bytes: &mut &'a [u8], n: usize) -> Result<&'a [u8], String> {
        if bytes.len() < n {
            return Err("truncated stream".into());
        }
        let (head, rest) = bytes.split_at(n);
        *bytes = rest;
        Ok(head)
    }
    fn u16_le(bytes: &mut &[u8]) -> Result<u16, String> {
        Ok(u16::from_le_bytes(
            take(bytes, 2)?.try_into().expect("two bytes"),
        ))
    }
    fn u32_le(bytes: &mut &[u8]) -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            take(bytes, 4)?.try_into().expect("four bytes"),
        ))
    }
    let mut cases = Vec::new();
    while !bytes.is_empty() {
        if take(&mut bytes, 1)? != b"C" {
            return Err("expected a case".into());
        }
        let cols = u16_le(&mut bytes)?;
        let rows = u16_le(&mut bytes)?;
        let mut steps = Vec::new();
        loop {
            match take(&mut bytes, 1)?[0] {
                b'O' => {
                    let n = u32_le(&mut bytes)? as usize;
                    steps.push(Step::Output(take(&mut bytes, n)?.to_vec()));
                }
                b'R' => {
                    let n = u32_le(&mut bytes)? as usize;
                    let record = serde_json::from_slice(take(&mut bytes, n)?)
                        .map_err(|e| format!("bad record: {e}"))?;
                    steps.push(Step::Record(record));
                }
                b'S' => steps.push(Step::Snapshot),
                b'E' => break,
                other => return Err(format!("unknown step {other:#04x}")),
            }
        }
        cases.push(Case { cols, rows, steps });
    }
    Ok(cases)
}

fn hex(hash: [u8; 32]) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

/// Runs each case; one line per case: `seq daemon follower follower late rebuilt`, the late
/// follower's hash being `-` when the case took no snapshot.
pub fn run(cases: &[Case]) -> Vec<String> {
    cases.iter().map(run_case).collect()
}

fn run_case(case: &Case) -> String {
    let settings = Settings::default();
    let mut daemon = Replica::new(case.cols, case.rows, settings.clone(), Role::Authoritative);
    let mut followers = [
        Replica::new(case.cols, case.rows, settings.clone(), Role::Follower),
        Replica::new(case.cols, case.rows, settings, Role::Follower),
    ];
    let mut late: Option<Replica> = None;
    let mut errors = Vec::new();
    for step in &case.steps {
        let item = match step {
            Step::Output(bytes) => Item::Output(bytes),
            Step::Record(record) => Item::Record(record),
            Step::Snapshot => {
                late = Some(Replica::from_snapshot(
                    &daemon.snapshot(),
                    Role::Follower,
                    None,
                ));
                continue;
            }
        };
        let (seq, _) = daemon.push(item);
        let _ = daemon.take_replies();
        for r in followers.iter_mut().chain(late.as_mut()) {
            if let Err(err) = r.apply(seq, item) {
                errors.push(err.to_string());
            }
        }
    }
    let rebuilt = Replica::from_snapshot(&daemon.snapshot(), Role::Follower, None);
    let mut line = format!("{}", daemon.seq());
    for r in std::iter::once(&daemon).chain(&followers) {
        line.push(' ');
        line.push_str(&hex(r.state_hash()));
    }
    line.push(' ');
    line.push_str(
        &late
            .as_ref()
            .map_or_else(|| "-".to_owned(), |r| hex(r.state_hash())),
    );
    line.push(' ');
    line.push_str(&hex(rebuilt.state_hash()));
    for err in errors {
        line.push_str(" error:");
        line.push_str(&err.replace(' ', "_"));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_round_trips_and_replicas_agree() {
        let cases = vec![Case {
            cols: 20,
            rows: 4,
            steps: vec![
                Step::Output(b"hello\r\n\x1b[31mred".to_vec()),
                Step::Snapshot,
                Step::Record(Record::Resize { cols: 10, rows: 3 }),
                Step::Output("\u{4e2d}\u{6587} wraps around".as_bytes().to_vec()),
            ],
        }];
        let bytes = encode(&cases);
        assert_eq!(decode(&bytes).unwrap(), cases);
        let lines = run(&cases);
        let words: Vec<&str> = lines[0].split(' ').collect();
        assert_eq!(words[0], "3");
        assert_eq!(words.len(), 6, "{lines:?}");
        assert!(words[1..].iter().all(|h| *h == words[1]), "{lines:?}");
    }
}
