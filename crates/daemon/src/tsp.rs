//! The daemon's side of TSP (§7, §12.1, §14.2): the engine its authoritative replicas run, with
//! the UI engine's CSS checker and the on-disk blob store; ages rewritten to snapshot time; view
//! events from clients; and the JSON view of a pane's TSP state that the control endpoints share.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use auk_replica::{Settings, TspChanges, TspPane, ViewEvent, tsp_config};
use auk_tsp::blob::{BLOBS_PER_PANE, Blob, BlobStore, MemoryBlobs};
use auk_tsp::{
    AGE_PROPS, AGENT_AGE_OBJECTS, Gate, Mods, NodeRef, SheetChecker, State, Surface, SurfaceKey,
    TitleSource,
};
use auk_ui::css::properties::Restrictions;
use auk_ui::css::sheet::Stylesheet;
use serde_json::{Map, Value, json};

/// Blobs kept on disk (§7.14): past this, the least recently shown go at daemon start.
pub const BLOB_DISK_LIMIT: u64 = 1 << 30;

/// The daemon's TSP engine for a new pane.
pub fn engine(settings: &Settings, blob_dir: &Path) -> TspPane {
    TspPane::with(
        tsp_config(settings),
        Box::new(ProgramCss),
        Box::new(DiskBlobs::new(blob_dir.to_owned())),
    )
}

/// Program stylesheets checked by the UI engine's parser with the program restrictions (§7.12):
/// `url()` and `position: fixed` dropped and reported, `@import` reported, parse errors.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProgramCss;

impl SheetChecker for ProgramCss {
    fn check(&self, css: &str) -> Vec<String> {
        Stylesheet::parse(css, Restrictions::PROGRAM).errors
    }
}

/// A pane's blobs (§7.12, §7.14): in memory per pane (256 MiB, LRU of unreferenced blobs) and
/// written through to `<state>/blobs/<sha256>`, which outlives the pane and the daemon.
#[derive(Debug)]
pub struct DiskBlobs {
    dir: PathBuf,
    memory: MemoryBlobs,
}

impl DiskBlobs {
    pub fn new(dir: PathBuf) -> Self {
        DiskBlobs {
            dir,
            memory: MemoryBlobs::new(BLOBS_PER_PANE),
        }
    }

    fn path(&self, id: &str) -> Option<PathBuf> {
        // Ids are lowercase hex SHA-256; anything else never names a file.
        (id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .then(|| self.dir.join(id))
    }

    fn write(&self, blob: &Blob) -> io::Result<()> {
        let Some(path) = self.path(&blob.id) else {
            return Ok(());
        };
        if path.exists() {
            return touch(&path);
        }
        fs::create_dir_all(&self.dir)?;
        let tmp = self
            .dir
            .join(format!(".{}.{}", blob.id, std::process::id()));
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&blob.bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, &path)
    }
}

/// Marks a blob file as just shown: its mtime orders eviction.
fn touch(path: &Path) -> io::Result<()> {
    fs::File::options()
        .append(true)
        .open(path)?
        .set_modified(SystemTime::now())
}

impl BlobStore for DiskBlobs {
    fn put(&mut self, blob: Blob, referenced: &dyn Fn(&str) -> bool) {
        if let Err(err) = self.write(&blob) {
            eprintln!("auk daemon: blob {}: {err}", blob.id);
        }
        self.memory.put(blob, referenced);
    }

    fn has(&self, id: &str) -> bool {
        self.memory.has(id) || self.path(id).is_some_and(|p| p.is_file())
    }

    fn touch(&mut self, id: &str) {
        self.memory.touch(id);
        if let Some(path) = self.path(id) {
            let _ = touch(&path);
        }
    }

    fn get(&self, id: &str) -> Option<&Blob> {
        self.memory.get(id)
    }

    fn load(&self, id: &str) -> Option<Blob> {
        if let Some(blob) = self.memory.get(id) {
            return Some(blob.clone());
        }
        // Restored panes (§16) find their blobs on disk; the file name is the hash, checked again.
        let bytes = fs::read(self.path(id)?).ok()?;
        (auk_tsp::blob::blob_id(&bytes) == id).then(|| Blob {
            id: id.to_owned(),
            mime: None,
            bytes,
        })
    }
}

/// Evicts the least recently shown blobs until the store fits `limit` bytes (at daemon start).
/// Returns the bytes left.
pub fn evict_blobs(dir: &Path, limit: u64) -> io::Result<u64> {
    let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_file() {
            files.push((meta.modified()?, meta.len(), entry.path()));
        }
    }
    files.sort();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    for (_, len, path) in files {
        if total <= limit {
            break;
        }
        fs::remove_file(&path)?;
        total -= len;
    }
    Ok(total)
}

/// When each age prop arrived (§7.13): a snapshot rewrites ages to snapshot time, as
/// `age + (now − received)`. Keyed by surface, node and the prop's path.
#[derive(Debug, Default)]
pub struct Ages {
    seen: BTreeMap<(SurfaceKey, String, String), (f64, Instant)>,
}

impl Ages {
    /// Records the ages that changed, from the replica's drained changes.
    pub fn observe(&mut self, pane: &TspPane, changes: &TspChanges) {
        let now = Instant::now();
        for key in &changes.removed {
            self.seen.retain(|(k, _, _), _| k != key);
        }
        for sc in &changes.surfaces {
            for id in &sc.doc.removed {
                self.seen
                    .retain(|(k, node, _), _| !(*k == sc.key && node == id));
            }
            let Some(surface) = pane.surface(sc.key) else {
                continue;
            };
            let mut visit = |node: NodeRef<'_>| {
                for (path, age) in ages_of(node) {
                    let entry = (sc.key, node.id().to_owned(), path);
                    match self.seen.get(&entry) {
                        Some((old, _)) if *old == age => {}
                        _ => {
                            self.seen.insert(entry, (age, now));
                        }
                    }
                }
            };
            if sc.opened || !sc.doc.children.is_empty() {
                // New subtrees: walk the whole document; known ages keep their stamps.
                walk(surface.document().root(), &mut visit);
            } else {
                for id in &sc.doc.nodes {
                    if let Some(node) = surface.document().get(id) {
                        visit(node);
                    }
                }
            }
        }
    }

    /// Rewrites a node's ages to now (the snapshot rewrite).
    pub fn rewrite(&self, surface: &Surface, node: NodeRef<'_>, props: &mut Map<String, Value>) {
        let current: BTreeMap<String, f64> = ages_of(node).into_iter().collect();
        for (path, age) in current {
            let key = (surface.key(), node.id().to_owned(), path.clone());
            let Some((seen, at)) = self.seen.get(&key) else {
                continue;
            };
            if *seen != age {
                continue;
            }
            let rewritten = age + at.elapsed().as_secs_f64() * 1000.0;
            let rewritten = json!(rewritten.round() as i64);
            match path.split_once('.') {
                None => {
                    props.insert(path, rewritten);
                }
                Some((obj, field)) => {
                    if let Some(Value::Object(o)) = props.get_mut(obj) {
                        o.insert(field.to_owned(), rewritten);
                    }
                }
            }
        }
    }
}

fn walk(node: NodeRef<'_>, f: &mut dyn FnMut(NodeRef<'_>)) {
    f(node);
    for child in node.children() {
        walk(child, f);
    }
}

/// A node's age props as `(path, ms)`: `age` on the kinds of [`AGE_PROPS`], and `<obj>.age` in
/// `agent`'s nested objects.
fn ages_of(node: NodeRef<'_>) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for (kind, prop) in AGE_PROPS {
        if node.kind() == kind
            && let Some(age) = node.prop(prop).and_then(Value::as_f64)
        {
            out.push((prop.to_owned(), age));
        }
    }
    if node.kind() == "agent" {
        for obj in AGENT_AGE_OBJECTS {
            if let Some(age) = node
                .prop(obj)
                .and_then(|o| o.get("age"))
                .and_then(Value::as_f64)
            {
                out.push((format!("{obj}.age"), age));
            }
        }
    }
    out
}

/// Reads a view event a client sent (§15.4): the body the program will get, as JSON, without
/// `sf` (the message carries it).
pub fn parse_view_event(v: &Value) -> Result<ViewEvent, String> {
    let ev = v.get("ev").and_then(Value::as_str).ok_or("missing `ev`")?;
    let str_of = |k: &str| -> Result<String, String> {
        v.get(k)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("{ev}: missing `{k}`"))
    };
    let u64_of = |k: &str| -> Result<u64, String> {
        v.get(k)
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("{ev}: missing `{k}`"))
    };
    let values = || v.get("values").and_then(Value::as_object).cloned();
    let id = str_of("id")?;
    Ok(match ev {
        "toggle" => ViewEvent::Toggle {
            id,
            collapsed: v.get("collapsed").and_then(Value::as_bool).unwrap_or(false),
            key: v.get("key").cloned(),
        },
        "select" => ViewEvent::Select {
            id,
            item: str_of("item")?,
            values: values(),
        },
        "activate" => ViewEvent::Activate {
            id,
            item: str_of("item")?,
            values: values(),
        },
        "action" => {
            let act = str_of("act")?;
            let action = match v.get("value").and_then(Value::as_str) {
                Some(value) => format!("{act}={value}"),
                None => act,
            };
            let mut mods = Mods::NONE;
            for m in v
                .get("mods")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                match m.as_str() {
                    Some("shift") => mods.shift = true,
                    Some("ctrl") => mods.ctrl = true,
                    Some("alt") => mods.alt = true,
                    Some("meta") => mods.meta = true,
                    _ => return Err(format!("action: unknown modifier {m}")),
                }
            }
            ViewEvent::Action {
                id,
                action,
                mods,
                values: values(),
            }
        }
        "change" if v.get("item").is_some() => ViewEvent::PrefsChange {
            id,
            item: str_of("item")?,
            value: v.get("value").cloned().unwrap_or(Value::Null),
        },
        "change" => ViewEvent::Change {
            id,
            value: v.get("value").cloned().unwrap_or(Value::Null),
            checked: v.get("checked").and_then(Value::as_bool).unwrap_or(false),
            name: v.get("name").and_then(Value::as_str).map(str::to_owned),
            values: values(),
        },
        "focus" => ViewEvent::Focus { id },
        "edit" => ViewEvent::Edit {
            id,
            from: u64_of("from")?,
            to: u64_of("to")?,
            text: str_of("text")?,
            cursor: u64_of("cursor")?,
            len: u64_of("len")?,
        },
        "undo" => ViewEvent::Undo { id },
        "send" => ViewEvent::Send {
            id,
            text: str_of("text")?,
        },
        other => return Err(format!("`{other}` isn't a view event")),
    })
}

/// A pane's TSP state as JSON, for the `tsp` control commands of the daemon and headless
/// windows (and the tests that read them).
pub fn state_json(pane: &TspPane, term: &auk_term::Terminal) -> Value {
    let gate = match pane.gate() {
        Gate::Open => "open",
        Gate::Gone => "gone",
        Gate::Muted => "muted",
    };
    let title = match pane.title() {
        TitleSource::Grid => json!({"source": "grid", "title": term.title()}),
        TitleSource::Surface(t) => json!({"source": "surface", "title": t}),
    };
    let hello = pane
        .hello()
        .map(|h| json!({"app": h.app, "features": h.features}));
    let surfaces: Vec<Value> = pane
        .surfaces()
        .iter()
        .map(|s| {
            let anchor = s
                .anchor()
                .map(|k| json!({"key": k.0, "line": term.anchor_line(k)}));
            json!({
                "key": s.key().0,
                "id": s.id(),
                "mode": s.mode().as_str(),
                "buffer": s.buffer().as_str(),
                "state": match s.state() { State::Live => "live", State::Closed => "closed" },
                "listen": s.listens(),
                "interactive": s.interactive(),
                "restored": s.restored(),
                "covered": pane.covered(s.key()),
                "title": s.title(),
                "role": s.role(),
                "anchor": anchor,
                "suspended": s.suspended(),
                "last_s": s.last_s(),
                "palette": s.palette().map(|p| p.to_json(s.id())),
                "sheets": s.sheets().iter().map(|(n, c)| json!([n, c])).collect::<Vec<_>>(),
                "focus": s.document().focus(),
                "settled": s.document().settled(),
                "doc": s.document().root().to_json(),
            })
        })
        .collect();
    json!({
        "enabled": pane.config().enabled,
        "kinds": pane.config().kinds,
        "features": pane.config().features,
        "retention": pane.config().retention,
        "cost": pane.retention_cost(),
        "gate": gate,
        "hello": hello,
        "app": pane.app(),
        "title": title,
        "chunk_pending": pane.chunk_pending(),
        "surfaces": surfaces,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_events_parse_as_programs_read_them() {
        let ev = parse_view_event(
            &json!({"ev": "action", "id": "b", "act": "open", "value": "x=y", "mods": ["shift"]}),
        )
        .unwrap();
        assert_eq!(
            ev,
            ViewEvent::Action {
                id: "b".into(),
                action: "open=x=y".into(),
                mods: Mods {
                    shift: true,
                    ..Mods::NONE
                },
                values: None
            }
        );
        assert!(parse_view_event(&json!({"ev": "ack", "id": "x"})).is_err());
        assert!(parse_view_event(&json!({"ev": "edit", "id": "x"})).is_err());
    }

    #[test]
    fn program_css_reports_dropped_constructs() {
        let msgs = ProgramCss.check(".a { background: url(x.png); position: fixed } @import 'b';");
        assert!(msgs.len() >= 2, "{msgs:?}");
        assert!(ProgramCss.check(".a { color: red }").is_empty());
    }

    #[test]
    fn disk_blobs_write_through_and_evict_oldest() {
        let dir = std::env::temp_dir().join(format!("auk-blobs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut store = DiskBlobs::new(dir.clone());
        let blob = |bytes: &[u8]| Blob {
            id: auk_tsp::blob::blob_id(bytes),
            mime: None,
            bytes: bytes.to_vec(),
        };
        let (a, b) = (blob(b"aaaa"), blob(b"bbbbbb"));
        store.put(a.clone(), &|_| false);
        std::thread::sleep(std::time::Duration::from_millis(20));
        store.put(b.clone(), &|_| false);
        assert!(dir.join(&a.id).is_file() && dir.join(&b.id).is_file());
        let fresh = DiskBlobs::new(dir.clone());
        assert!(fresh.has(&a.id) && !fresh.has("00"));
        assert_eq!(evict_blobs(&dir, 6).unwrap(), 6);
        assert!(!dir.join(&a.id).exists() && dir.join(&b.id).exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
