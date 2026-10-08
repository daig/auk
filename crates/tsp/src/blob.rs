//! Blobs (`b`, §7.12): content-addressed by the SHA-256 of the decoded bytes, shared by a pane's
//! surfaces. Storage sits behind [`BlobStore`]; the daemon backs it with the on-disk cache.

use std::collections::BTreeMap;
use std::fmt::Debug;

use base64::Engine as _;
use sha2::{Digest as _, Sha256};

/// The largest blob, decoded (§7.14).
pub const MAX_BLOB: usize = 16 << 20;
/// Unreferenced blobs kept per pane before LRU eviction (§7.14: 256 MiB per screen).
pub const BLOBS_PER_PANE: usize = 256 << 20;

/// A stored blob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blob {
    /// Lowercase hex SHA-256 of `bytes`.
    pub id: String,
    pub mime: Option<String>,
    pub bytes: Vec<u8>,
}

/// The lowercase hex SHA-256 of `bytes`.
pub fn blob_id(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Decodes and checks a `b` body: standard base64, at most [`MAX_BLOB`] bytes, and an `id`, when
/// given, equal to the hash of the bytes.
pub fn decode(id: Option<&str>, mime: Option<&str>, body: &[u8]) -> Result<Blob, String> {
    // Base64 is 4 bytes per 3; reject an over-size body before decoding it.
    if body.len() / 4 * 3 > MAX_BLOB + 2 {
        return Err(format!("blob over {MAX_BLOB} bytes"));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(body)
        .map_err(|e| format!("malformed b body: {e}"))?;
    if bytes.len() > MAX_BLOB {
        return Err(format!("blob over {MAX_BLOB} bytes"));
    }
    let hash = blob_id(&bytes);
    if let Some(id) = id
        && !id.eq_ignore_ascii_case(&hash)
    {
        return Err(format!("blob id {id} doesn't match its bytes ({hash})"));
    }
    Ok(Blob {
        id: hash,
        mime: mime.map(str::to_owned),
        bytes,
    })
}

/// Where a pane keeps its blobs.
pub trait BlobStore: Debug + Send {
    /// Stores a blob. `referenced` tells which ids documents still use, for eviction.
    fn put(&mut self, blob: Blob, referenced: &dyn Fn(&str) -> bool);
    /// Whether the blob is held.
    fn has(&self, id: &str) -> bool;
    /// Counts a use for LRU eviction (a `blobs` query or a draw).
    fn touch(&mut self, id: &str);
    /// The blob, if held.
    fn get(&self, id: &str) -> Option<&Blob>;
}

/// An in-memory store with LRU eviction of unreferenced blobs past a byte limit.
#[derive(Debug)]
pub struct MemoryBlobs {
    limit: usize,
    bytes: usize,
    clock: u64,
    blobs: BTreeMap<String, (u64, Blob)>,
}

impl Default for MemoryBlobs {
    fn default() -> Self {
        Self::new(BLOBS_PER_PANE)
    }
}

impl MemoryBlobs {
    pub fn new(limit: usize) -> Self {
        MemoryBlobs {
            limit,
            bytes: 0,
            clock: 0,
            blobs: BTreeMap::new(),
        }
    }

    /// Bytes held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.blobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blobs.is_empty()
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }
}

impl BlobStore for MemoryBlobs {
    fn put(&mut self, blob: Blob, referenced: &dyn Fn(&str) -> bool) {
        let now = self.tick();
        let keep = blob.id.clone();
        if let Some((used, _)) = self.blobs.get_mut(&blob.id) {
            *used = now;
        } else {
            self.bytes += blob.bytes.len();
            self.blobs.insert(blob.id.clone(), (now, blob));
        }
        while self.bytes > self.limit {
            let victim = self
                .blobs
                .iter()
                .filter(|(id, _)| **id != keep && !referenced(id))
                .min_by_key(|(_, (used, _))| *used)
                .map(|(id, _)| id.clone());
            let Some(victim) = victim else { break };
            if let Some((_, blob)) = self.blobs.remove(&victim) {
                self.bytes -= blob.bytes.len();
            }
        }
    }

    fn has(&self, id: &str) -> bool {
        self.blobs.contains_key(id)
    }

    fn touch(&mut self, id: &str) {
        let now = self.tick();
        if let Some((used, _)) = self.blobs.get_mut(id) {
            *used = now;
        }
    }

    fn get(&self, id: &str) -> Option<&Blob> {
        self.blobs.get(id).map(|(_, b)| b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_checks_id_and_base64() {
        let id = blob_id(b"hello");
        assert_eq!(
            id,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(
            decode(None, Some("text/plain"), b"aGVsbG8=").unwrap().id,
            id
        );
        assert!(decode(Some(&id.to_uppercase()), None, b"aGVsbG8=").is_ok());
        assert!(
            decode(Some("00"), None, b"aGVsbG8=")
                .unwrap_err()
                .contains("doesn't match")
        );
        assert!(decode(None, None, b"aGVsbG8").is_err());
        assert!(decode(None, None, b"a*b=").is_err());
    }

    #[test]
    fn lru_evicts_unreferenced() {
        let mut store = MemoryBlobs::new(10);
        let blob = |s: &str| Blob {
            id: blob_id(s.as_bytes()),
            mime: None,
            bytes: s.as_bytes().to_vec(),
        };
        let (a, b, c) = (blob("aaaa"), blob("bbbb"), blob("cccc"));
        store.put(a.clone(), &|_| false);
        store.put(b.clone(), &|_| false);
        store.touch(&a.id);
        store.put(c.clone(), &|_| false);
        assert!(store.has(&a.id) && !store.has(&b.id) && store.has(&c.id));
        let pinned = a.id.clone();
        store.put(b.clone(), &|id| id == pinned);
        assert!(store.has(&a.id) && store.has(&b.id) && !store.has(&c.id));
    }
}
