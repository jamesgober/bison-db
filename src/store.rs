//! The single-file document store: [`Db`].
//!
//! `bison-db` persists documents to one append-only file. Every write — insert,
//! overwrite, or delete — appends a self-describing record to the tail; the file
//! is never edited in place. An in-memory index maps each live document id to
//! the byte offset of its most recent record, so a read is one hash lookup and
//! one positional read. This log-structured design makes writes sequential
//! (the pattern disks and SSDs serve fastest) and keeps a crash from corrupting
//! data already on disk: a half-written record at the tail is detected by its
//! length and checksum and dropped on the next open.
//!
//! ## Record framing
//!
//! The file opens with a fixed header (magic plus a format version), then a run
//! of records. Each record is an 8-byte frame (`u32` payload length, `u32`
//! CRC-32C of the payload) followed by the payload itself: a one-byte operation
//! tag, the 8-byte document id, and — for an insert or overwrite — the encoded
//! document body. A delete writes a tombstone with no body.
//!
//! ## Durability
//!
//! A record reaches the OS page cache as soon as it is written, so it is visible
//! to later reads in the same process immediately. It is durable against a power
//! loss only after [`Db::flush`] returns, which issues an `fsync`. A crash
//! before `flush` may lose the most recent unsynced records but never tears an
//! earlier one. Full write-ahead-log durability and group commit arrive in a
//! later release; this module's contract is "no corruption, last-unsynced-writes
//! may be lost".

use std::collections::HashMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use crate::codec::{crc32c, decode_document, encode_document_into};
use crate::error::{Error, Result};
use crate::sys::{read_exact_at, write_all_at};
use crate::value::Document;

/// The largest record payload the store will write or accept while reading.
///
/// A document encodes to at most this many bytes; a larger one is rejected with
/// [`Error::ValueTooLarge`] on write. On read, any framed length above this cap
/// is treated as corruption, which bounds the allocation the recovery path can
/// be asked to make from a damaged file.
pub const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;

/// Magic bytes at the start of every store file. The trailing digit tracks the
/// header layout, distinct from the format version that follows it.
const HEADER_MAGIC: [u8; 8] = *b"BISONDB1";

/// On-disk format version. Bumped only on an incompatible record-layout change.
const FORMAT_VERSION: u16 = 1;

/// Length of the file header: 8 magic bytes, a `u16` version, 6 reserved bytes.
const HEADER_LEN: u64 = 16;

/// Size of a record frame: a `u32` length followed by a `u32` checksum.
const FRAME_LEN: usize = 8;

/// Smallest legal payload: a one-byte op tag plus an 8-byte id, with no body
/// (the shape of a delete tombstone).
const MIN_PAYLOAD: usize = 1 + 8;

/// Operation tag for an insert or overwrite: the payload carries a document body.
const OP_PUT: u8 = 1;

/// Operation tag for a delete: the payload is the op tag and id only.
const OP_DELETE: u8 = 2;

/// A document's primary key within a [`Db`].
///
/// Ids are assigned by [`Db::insert`] as a dense, monotonically increasing
/// sequence starting at 1; `0` is never assigned and can be used as a sentinel.
/// The id is stable for the life of the document and survives reopening the
/// file. Reconstruct one with [`DocId::from`] when you have stored it elsewhere.
///
/// # Examples
///
/// ```
/// use bison_db::DocId;
/// let id = DocId::from(7);
/// assert_eq!(id.get(), 7);
/// assert_eq!(id.to_string(), "7");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocId(u64);

impl DocId {
    /// Returns the underlying `u64`.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::DocId;
    /// assert_eq!(DocId::from(42).get(), 42);
    /// ```
    #[inline]
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for DocId {
    #[inline]
    fn from(raw: u64) -> Self {
        DocId(raw)
    }
}

impl From<DocId> for u64 {
    #[inline]
    fn from(id: DocId) -> Self {
        id.0
    }
}

impl fmt::Display for DocId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Where a live document's body sits in the file.
#[derive(Clone, Copy)]
struct BodyLoc {
    /// Byte offset of the encoded document body.
    offset: u64,
    /// Length of the encoded document body in bytes.
    len: u32,
}

/// A point-in-time summary of a store's size and contents.
///
/// Returned by [`Db::stats`]. The gap between `file_bytes` and `live_bytes`
/// (plus framing) is space held by superseded and deleted records — the slack a
/// future compaction step will reclaim.
///
/// # Examples
///
/// ```no_run
/// # fn main() -> bison_db::Result<()> {
/// let db = bison_db::Db::open("data.bison")?;
/// let stats = db.stats();
/// println!("{} live documents in {} bytes", stats.live_documents, stats.file_bytes);
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stats {
    /// Number of documents currently readable.
    pub live_documents: usize,
    /// Total size of the file on disk, in bytes.
    pub file_bytes: u64,
    /// Bytes occupied by the bodies of live documents, excluding framing.
    pub live_bytes: u64,
}

/// An embedded document store backed by a single append-only file.
///
/// Open one with [`Db::open`], then [`insert`](Db::insert),
/// [`get`](Db::get), [`update`](Db::update), and [`delete`](Db::delete)
/// documents by id. Reads take `&self` and writes take `&mut self`, so the
/// compiler enforces single-writer access; share a `Db` across threads by
/// placing it behind your own lock. Call [`flush`](Db::flush) to make recent
/// writes durable.
///
/// # Examples
///
/// ```
/// # fn main() -> bison_db::Result<()> {
/// use bison_db::{Db, Document};
///
/// let dir = std::env::temp_dir().join("bison_db_doc_example");
/// let _ = std::fs::remove_file(&dir);
/// let mut db = Db::open(&dir)?;
///
/// let mut user = Document::new();
/// user.set("name", "grace").set("born", 1906_i64);
/// let id = db.insert(user)?;
///
/// let fetched = db.get(id)?.expect("just inserted");
/// assert_eq!(fetched.get("name").and_then(|v| v.as_str()), Some("grace"));
///
/// db.flush()?;
/// # let _ = std::fs::remove_file(&dir);
/// # Ok(())
/// # }
/// ```
pub struct Db {
    /// The open store file, used for both positional reads and tail appends.
    file: File,
    /// Path the store was opened from, returned by [`Db::path`].
    path: PathBuf,
    /// Live document id to the location of its most recent body.
    index: HashMap<u64, BodyLoc>,
    /// Offset at which the next record will be appended.
    tail: u64,
    /// Id that the next [`Db::insert`] will assign.
    next_id: u64,
    /// Reusable buffer for framing a record, so writes do not allocate.
    scratch: Vec<u8>,
}

impl Db {
    /// Opens the store at `path`, creating an empty one if the file does not
    /// exist, and replaying any existing records to rebuild the index.
    ///
    /// On open the whole log is scanned: each record's checksum is verified and
    /// the in-memory index is reconstructed from the surviving inserts and
    /// deletes. A record left half-written by a crash — detectable because it
    /// runs past the end of the file or fails its checksum at the tail — is
    /// truncated away, restoring the file to its last consistent state. A
    /// checksum failure on a record that is *not* at the tail is reported as
    /// [`Error::Corrupt`], because that indicates in-place damage rather than a
    /// torn write.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the file cannot be opened or read,
    /// [`Error::BadMagic`] if an existing file is not a bison-db store,
    /// [`Error::UnsupportedVersion`] if it was written by a newer format, and
    /// [`Error::Corrupt`] if a non-tail record fails verification.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// let path = std::env::temp_dir().join("bison_db_open_example.bison");
    /// let _ = std::fs::remove_file(&path);
    /// let db = bison_db::Db::open(&path)?;
    /// assert!(db.is_empty());
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        let file_len = file.metadata()?.len();

        let mut db = Db {
            file,
            path,
            index: HashMap::new(),
            tail: HEADER_LEN,
            next_id: 1,
            scratch: Vec::with_capacity(256),
        };

        if file_len == 0 {
            db.write_header()?;
        } else {
            db.verify_header(file_len)?;
            db.replay(file_len)?;
        }
        Ok(db)
    }

    /// Inserts `doc`, assigning and returning a fresh [`DocId`].
    ///
    /// The document is appended to the log and indexed; it is readable
    /// immediately and durable after the next [`flush`](Db::flush).
    ///
    /// # Errors
    ///
    /// Returns [`Error::ValueTooLarge`] if the encoded document exceeds
    /// [`MAX_RECORD_BYTES`], or [`Error::Io`] if the append fails.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// # let path = std::env::temp_dir().join("bison_db_insert_example.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// use bison_db::{Db, Document};
    /// let mut db = Db::open(&path)?;
    /// let mut doc = Document::new();
    /// doc.set("k", "v");
    /// let id = db.insert(doc)?;
    /// assert!(db.contains(id));
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    pub fn insert(&mut self, doc: Document) -> Result<DocId> {
        let id = self.next_id;
        self.append(OP_PUT, id, Some(&doc))?;
        self.next_id = id + 1;
        Ok(DocId(id))
    }

    /// Reads the document stored under `id`, or `None` if no live document has
    /// that id.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the body cannot be read, or [`Error::Corrupt`]
    /// if the stored bytes fail to decode (which a passing checksum makes
    /// unexpected in practice).
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// # let path = std::env::temp_dir().join("bison_db_get_example.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// use bison_db::{Db, Document, DocId};
    /// let mut db = Db::open(&path)?;
    /// let id = db.insert({ let mut d = Document::new(); d.set("n", 1_i64); d })?;
    /// assert!(db.get(id)?.is_some());
    /// assert!(db.get(DocId::from(9999))?.is_none());
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get(&self, id: DocId) -> Result<Option<Document>> {
        let Some(loc) = self.index.get(&id.0).copied() else {
            return Ok(None);
        };
        let mut buf = vec![0u8; loc.len as usize];
        read_exact_at(&self.file, &mut buf, loc.offset)?;
        decode_document(&buf).map(Some)
    }

    /// Overwrites the document stored under `id` with `doc`, returning `true` if
    /// a document was present to overwrite and `false` otherwise.
    ///
    /// A successful update appends a new record and repoints the index; the
    /// previous body remains in the file as dead space until compaction.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ValueTooLarge`] or [`Error::Io`] under the same
    /// conditions as [`insert`](Db::insert).
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// # let path = std::env::temp_dir().join("bison_db_update_example.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// use bison_db::{Db, Document, DocId};
    /// let mut db = Db::open(&path)?;
    /// let id = db.insert({ let mut d = Document::new(); d.set("v", 1_i64); d })?;
    ///
    /// let mut next = Document::new();
    /// next.set("v", 2_i64);
    /// assert!(db.update(id, next)?);
    /// assert!(!db.update(DocId::from(404), Document::new())?);
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    pub fn update(&mut self, id: DocId, doc: Document) -> Result<bool> {
        if !self.index.contains_key(&id.0) {
            return Ok(false);
        }
        self.append(OP_PUT, id.0, Some(&doc))?;
        Ok(true)
    }

    /// Deletes the document stored under `id`, returning `true` if one was
    /// present and `false` otherwise.
    ///
    /// A tombstone is appended so the deletion survives reopening; the document
    /// is unreadable as soon as this returns.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the tombstone cannot be appended.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// # let path = std::env::temp_dir().join("bison_db_delete_example.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// use bison_db::{Db, Document};
    /// let mut db = Db::open(&path)?;
    /// let id = db.insert({ let mut d = Document::new(); d.set("x", 1_i64); d })?;
    /// assert!(db.delete(id)?);
    /// assert!(db.get(id)?.is_none());
    /// assert!(!db.delete(id)?);
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    pub fn delete(&mut self, id: DocId) -> Result<bool> {
        if !self.index.contains_key(&id.0) {
            return Ok(false);
        }
        self.append(OP_DELETE, id.0, None)?;
        Ok(true)
    }

    /// Returns `true` if a live document has this `id`.
    ///
    /// This is an in-memory index lookup with no file access.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// # let path = std::env::temp_dir().join("bison_db_contains_example.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// use bison_db::{Db, Document};
    /// let mut db = Db::open(&path)?;
    /// let id = db.insert(Document::new())?;
    /// assert!(db.contains(id));
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn contains(&self, id: DocId) -> bool {
        self.index.contains_key(&id.0)
    }

    /// Returns the number of live documents.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// # let path = std::env::temp_dir().join("bison_db_len_example.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// use bison_db::{Db, Document};
    /// let mut db = Db::open(&path)?;
    /// db.insert(Document::new())?;
    /// assert_eq!(db.len(), 1);
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Returns `true` if the store holds no live documents.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// # let path = std::env::temp_dir().join("bison_db_isempty_example.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// let db = bison_db::Db::open(&path)?;
    /// assert!(db.is_empty());
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Returns an iterator over the ids of all live documents.
    ///
    /// The order is unspecified and may change between runs; collect and sort if
    /// you need a stable order.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// # let path = std::env::temp_dir().join("bison_db_ids_example.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// use bison_db::{Db, Document};
    /// let mut db = Db::open(&path)?;
    /// db.insert(Document::new())?;
    /// db.insert(Document::new())?;
    /// assert_eq!(db.ids().count(), 2);
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    pub fn ids(&self) -> impl Iterator<Item = DocId> + '_ {
        self.index.keys().copied().map(DocId)
    }

    /// Flushes buffered writes and `fsync`s the file, making every preceding
    /// write durable against power loss.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the sync fails.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// # let path = std::env::temp_dir().join("bison_db_flush_example.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// use bison_db::{Db, Document};
    /// let mut db = Db::open(&path)?;
    /// db.insert(Document::new())?;
    /// db.flush()?;
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    pub fn flush(&mut self) -> Result<()> {
        self.file.sync_all()?;
        Ok(())
    }

    /// Returns the path the store was opened from.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// # let path = std::env::temp_dir().join("bison_db_path_example.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// let db = bison_db::Db::open(&path)?;
    /// assert_eq!(db.path(), path.as_path());
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns a [`Stats`] snapshot of the store's size and live contents.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// # let path = std::env::temp_dir().join("bison_db_stats_example.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// use bison_db::{Db, Document};
    /// let mut db = Db::open(&path)?;
    /// db.insert(Document::new())?;
    /// assert_eq!(db.stats().live_documents, 1);
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn stats(&self) -> Stats {
        let live_bytes = self.index.values().map(|loc| u64::from(loc.len)).sum();
        Stats {
            live_documents: self.index.len(),
            file_bytes: self.tail,
            live_bytes,
        }
    }

    /// Appends one framed record and updates the index accordingly.
    ///
    /// For [`OP_PUT`] the body is encoded and the index repointed at it; for
    /// [`OP_DELETE`] the index entry is removed. The frame is built in `scratch`
    /// so the steady-state write path performs no per-record allocation.
    fn append(&mut self, op: u8, id: u64, doc: Option<&Document>) -> Result<()> {
        self.scratch.clear();
        // Reserve the frame header; the length and checksum are backfilled once
        // the payload is known.
        self.scratch.extend_from_slice(&[0u8; FRAME_LEN]);
        self.scratch.push(op);
        self.scratch.extend_from_slice(&id.to_le_bytes());
        if let Some(doc) = doc {
            encode_document_into(&mut self.scratch, doc)?;
        }

        let payload_len = self.scratch.len() - FRAME_LEN;
        if payload_len > MAX_RECORD_BYTES {
            return Err(Error::ValueTooLarge);
        }
        let crc = crc32c(&self.scratch[FRAME_LEN..]);
        self.scratch[0..4].copy_from_slice(&(payload_len as u32).to_le_bytes());
        self.scratch[4..8].copy_from_slice(&crc.to_le_bytes());

        write_all_at(&self.file, &self.scratch, self.tail)?;

        let record_start = self.tail;
        self.tail += (FRAME_LEN + payload_len) as u64;

        match op {
            OP_PUT => {
                let offset = record_start + FRAME_LEN as u64 + MIN_PAYLOAD as u64;
                let len = (payload_len - MIN_PAYLOAD) as u32;
                let _ = self.index.insert(id, BodyLoc { offset, len });
            }
            OP_DELETE => {
                let _ = self.index.remove(&id);
            }
            _ => {}
        }
        Ok(())
    }

    /// Writes the 16-byte file header at offset 0 and syncs it, establishing a
    /// valid empty store.
    fn write_header(&mut self) -> Result<()> {
        let mut header = [0u8; HEADER_LEN as usize];
        header[0..8].copy_from_slice(&HEADER_MAGIC);
        header[8..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        write_all_at(&self.file, &header, 0)?;
        self.file.sync_all()?;
        Ok(())
    }

    /// Validates the header of an existing file: length, magic, and version.
    fn verify_header(&self, file_len: u64) -> Result<()> {
        if file_len < HEADER_LEN {
            return Err(Error::BadMagic);
        }
        let mut header = [0u8; HEADER_LEN as usize];
        read_exact_at(&self.file, &mut header, 0)?;
        if header[0..8] != HEADER_MAGIC {
            return Err(Error::BadMagic);
        }
        let version = u16::from_le_bytes([header[8], header[9]]);
        if version > FORMAT_VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        Ok(())
    }

    /// Scans every record after the header, rebuilding the index and truncating
    /// a torn record at the tail if one is found.
    fn replay(&mut self, file_len: u64) -> Result<()> {
        let mut offset = HEADER_LEN;
        let mut frame = [0u8; FRAME_LEN];

        loop {
            if offset + FRAME_LEN as u64 > file_len {
                break;
            }
            read_exact_at(&self.file, &mut frame, offset)?;
            let payload_len = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
            let expected_crc = u32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]);

            if !(MIN_PAYLOAD..=MAX_RECORD_BYTES).contains(&payload_len) {
                // A length this size at the tail is an incomplete write; mid-file
                // it is corruption. Either way the run of valid records ends here.
                break;
            }
            let record_end = offset + FRAME_LEN as u64 + payload_len as u64;
            if record_end > file_len {
                break;
            }

            let mut payload = vec![0u8; payload_len];
            read_exact_at(&self.file, &mut payload, offset + FRAME_LEN as u64)?;
            if crc32c(&payload) != expected_crc {
                if record_end == file_len {
                    // Torn final record: drop it and stop.
                    break;
                }
                return Err(Error::Corrupt("crc mismatch"));
            }

            let op = payload[0];
            let id = u64::from_le_bytes([
                payload[1], payload[2], payload[3], payload[4], payload[5], payload[6], payload[7],
                payload[8],
            ]);

            match op {
                OP_PUT => {
                    let offset = offset + FRAME_LEN as u64 + MIN_PAYLOAD as u64;
                    let len = (payload_len - MIN_PAYLOAD) as u32;
                    let _ = self.index.insert(id, BodyLoc { offset, len });
                }
                OP_DELETE => {
                    let _ = self.index.remove(&id);
                }
                _ => return Err(Error::Corrupt("unknown record op")),
            }
            if id >= self.next_id {
                self.next_id = id + 1;
            }
            offset = record_end;
        }

        if offset < file_len {
            // Trailing torn bytes: cut the file back to the last good record.
            self.file.set_len(offset)?;
        }
        self.tail = offset;
        Ok(())
    }
}

impl fmt::Debug for Db {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Db")
            .field("path", &self.path)
            .field("live_documents", &self.index.len())
            .field("file_bytes", &self.tail)
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::value::Value;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Returns a unique temp path and removes any stale file at it.
    fn temp_path() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("bison_db_test_{pid}_{n}.bison"));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn doc(pairs: &[(&str, i64)]) -> Document {
        let mut d = Document::new();
        for (k, v) in pairs {
            d.set(*k, *v);
        }
        d
    }

    #[test]
    fn test_insert_get_roundtrip() {
        let path = temp_path();
        let mut db = Db::open(&path).unwrap();
        let id = db.insert(doc(&[("a", 1), ("b", 2)])).unwrap();
        let got = db.get(id).unwrap().unwrap();
        assert_eq!(got.get("a").and_then(Value::as_int), Some(1));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_get_missing_returns_none() {
        let path = temp_path();
        let db = Db::open(&path).unwrap();
        assert!(db.get(DocId::from(1)).unwrap().is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_delete_removes_document() {
        let path = temp_path();
        let mut db = Db::open(&path).unwrap();
        let id = db.insert(doc(&[("x", 9)])).unwrap();
        assert!(db.delete(id).unwrap());
        assert!(db.get(id).unwrap().is_none());
        assert!(!db.delete(id).unwrap());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_update_changes_value() {
        let path = temp_path();
        let mut db = Db::open(&path).unwrap();
        let id = db.insert(doc(&[("v", 1)])).unwrap();
        assert!(db.update(id, doc(&[("v", 2)])).unwrap());
        assert_eq!(
            db.get(id)
                .unwrap()
                .unwrap()
                .get("v")
                .and_then(Value::as_int),
            Some(2)
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_update_absent_id_is_false() {
        let path = temp_path();
        let mut db = Db::open(&path).unwrap();
        assert!(!db.update(DocId::from(7), Document::new()).unwrap());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_reopen_recovers_state() {
        let path = temp_path();
        let (a, b);
        {
            let mut db = Db::open(&path).unwrap();
            a = db.insert(doc(&[("n", 10)])).unwrap();
            b = db.insert(doc(&[("n", 20)])).unwrap();
            db.delete(a).unwrap();
            db.flush().unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert!(db.get(a).unwrap().is_none());
        assert_eq!(
            db.get(b).unwrap().unwrap().get("n").and_then(Value::as_int),
            Some(20)
        );
        assert_eq!(db.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_reopen_continues_id_sequence() {
        let path = temp_path();
        let first;
        {
            let mut db = Db::open(&path).unwrap();
            first = db.insert(Document::new()).unwrap();
        }
        let mut db = Db::open(&path).unwrap();
        let second = db.insert(Document::new()).unwrap();
        assert!(second.get() > first.get());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_open_rejects_foreign_file() {
        let path = temp_path();
        std::fs::write(&path, b"this is definitely not a bison-db file at all").unwrap();
        assert!(matches!(Db::open(&path), Err(Error::BadMagic)));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_torn_tail_is_truncated_on_open() {
        let path = temp_path();
        let keep;
        {
            let mut db = Db::open(&path).unwrap();
            keep = db.insert(doc(&[("ok", 1)])).unwrap();
            db.flush().unwrap();
        }
        // Append a bogus frame claiming a payload longer than what follows.
        {
            use std::io::Write;
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            let mut frame = Vec::new();
            frame.extend_from_slice(&999u32.to_le_bytes());
            frame.extend_from_slice(&0u32.to_le_bytes());
            frame.extend_from_slice(b"short");
            f.write_all(&frame).unwrap();
            f.flush().unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert!(db.get(keep).unwrap().is_some());
        assert_eq!(db.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_stats_reflect_live_documents() {
        let path = temp_path();
        let mut db = Db::open(&path).unwrap();
        db.insert(doc(&[("a", 1)])).unwrap();
        let id = db.insert(doc(&[("b", 2)])).unwrap();
        db.delete(id).unwrap();
        let stats = db.stats();
        assert_eq!(stats.live_documents, 1);
        assert!(stats.file_bytes > HEADER_LEN);
        let _ = std::fs::remove_file(&path);
    }
}
