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
//! to later reads in the same process immediately. When it becomes durable
//! against a power loss is governed by the store's [`SyncPolicy`]:
//!
//! - [`SyncPolicy::Always`] forces an `fsync` after every write, so each
//!   operation is durable the moment it returns.
//! - [`SyncPolicy::Manual`] (the default) syncs only on [`Db::flush`] and once,
//!   best-effort, on drop. It is faster, and writes remain crash-*safe* — a torn
//!   write is never misread — but the most recent unsynced writes can be lost on
//!   power loss.
//!
//! Either way the on-disk invariant holds: a crash never tears a record that was
//! already durable. On a newly created file, the parent directory is `fsync`ed
//! so the file's existence is itself durable.
//!
//! ## One handle per file
//!
//! Each handle caches the tail offset and the id counter, so only one may have
//! a file open at a time. [`Db::open`] takes an exclusive lock (see
//! `crate::lock`) and a second open fails with [`Error::Locked`].

use std::collections::HashMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
use std::ops::RangeBounds;
use std::path::{Path, PathBuf};

use crate::codec::{crc32c, decode_document, encode_document_into};
use crate::error::{Error, Result};
use crate::index::{SecondaryIndex, in_bounds, total_cmp_value};
use crate::lock::StoreLock;
use crate::sys::{read_exact_at, write_all_at};
use crate::value::{Document, Value};

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

/// On-disk format version. Frozen at `1` as of v0.4.0: the layout described in
/// `docs/FORMAT.md` is stable, and files written by 0.2.0 onward are readable by
/// every later release. Bumped only on an incompatible record-layout change,
/// which would be a major-version event.
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

/// Bytes before a record's document body: the frame, the op tag and the id.
const RECORD_PREFIX: usize = FRAME_LEN + MIN_PAYLOAD;

/// Bytes read per refill while replaying or scanning the log sequentially.
const READ_CHUNK: usize = 1 << 20;

/// Most payload bytes a search for the next intact record may checksum before
/// it gives up and reports the damaged region as unclassifiable.
const SCAN_BUDGET: usize = 256 << 20;

/// The write buffer is trimmed back to [`SCRATCH_KEEP`] bytes after a write
/// that grew it past this, so one large document does not pin its size.
const SCRATCH_SHRINK_ABOVE: usize = 1 << 20;

/// Capacity the write buffer keeps after being trimmed.
const SCRATCH_KEEP: usize = 64 * 1024;

/// A document's primary key within a [`Db`].
///
/// Ids are assigned by [`Db::insert`] as a monotonically increasing sequence
/// starting at 1; `0` is never assigned and can be used as a sentinel. An id is
/// never handed out twice, even after the document is deleted, the store is
/// compacted, and the file is reopened. The id is stable for the life of the
/// document. Reconstruct one with [`DocId::from`] when you have stored it
/// elsewhere.
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

/// When a write is made durable on disk.
///
/// bison-db never holds writes in a userspace buffer — every write reaches the
/// operating system immediately and is visible to later reads. This policy
/// controls only when the store forces those bytes through the OS cache to the
/// physical device with `fsync`, which is what protects them from a power loss.
///
/// # Examples
///
/// ```
/// # fn main() -> bison_db::Result<()> {
/// use bison_db::{DbOptions, SyncPolicy};
/// # let path = std::env::temp_dir().join("bison_db_syncpolicy_doc.bison");
/// # let _ = std::fs::remove_file(&path);
/// // Durable per write, at the cost of an fsync on every insert/update/delete.
/// let db = DbOptions::new().sync(SyncPolicy::Always).open(&path)?;
/// # drop(db);
/// # let _ = std::fs::remove_file(&path);
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SyncPolicy {
    /// `fsync` after every write before it returns. Each insert, update, and
    /// delete is durable the moment the call completes, at the cost of one
    /// device sync per operation.
    Always,
    /// `fsync` only when [`Db::flush`] is called (and once, best-effort, when the
    /// store is dropped). Writes are still crash-*safe* — a torn write is never
    /// misread — but the most recent unsynced writes can be lost on power loss.
    /// This is the default, and the fastest policy.
    #[default]
    Manual,
}

/// How [`Db::open`] treats a damaged record found before the end of the log.
///
/// A record cut short at the very end of the file is what a crash during a
/// write leaves behind; every mode removes it and reports the bytes through
/// [`Db::discarded_bytes`]. The modes differ only for damage *before* the end,
/// which a crash does not cause.
///
/// # Examples
///
/// ```
/// # fn main() -> bison_db::Result<()> {
/// use bison_db::{DbOptions, RecoveryMode};
/// # let path = std::env::temp_dir().join("bison_db_recovery_doc.bison");
/// # let _ = std::fs::remove_file(&path);
/// let db = DbOptions::new().recovery(RecoveryMode::Salvage).open(&path)?;
/// assert_eq!(db.discarded_bytes(), 0); // a clean file needs no repair
/// # drop(db);
/// # let _ = std::fs::remove_file(&path);
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum RecoveryMode {
    /// Refuse to open a damaged file: [`Db::open`] returns [`Error::Corrupt`]
    /// and leaves the file exactly as it was. This is the default.
    #[default]
    Strict,
    /// Open a damaged file by keeping every record that still verifies.
    ///
    /// Each damaged region is skipped up to the next intact record. Before the
    /// store is changed, a byte-for-byte copy of the damaged file is saved
    /// beside it as `<path>.damaged` (or `<path>.damaged.1`, and so on, if that
    /// name is taken); the store is then rewritten, as by [`Db::compact`], so
    /// it opens cleanly afterwards. Documents whose latest record was damaged
    /// come back at an earlier version or not at all, so check the result.
    Salvage,
}

/// Options for opening a [`Db`], built fluently and finished with
/// [`open`](DbOptions::open).
///
/// Use this when the default [`Db::open`] is not enough: to choose a
/// [`SyncPolicy`] or a [`RecoveryMode`]. The set of options is intentionally
/// small and will only grow additively.
///
/// # Examples
///
/// ```
/// # fn main() -> bison_db::Result<()> {
/// use bison_db::{DbOptions, SyncPolicy};
/// # let path = std::env::temp_dir().join("bison_db_dboptions_doc.bison");
/// # let _ = std::fs::remove_file(&path);
/// let db = DbOptions::new().sync(SyncPolicy::Always).open(&path)?;
/// assert_eq!(db.sync_policy(), SyncPolicy::Always);
/// # drop(db);
/// # let _ = std::fs::remove_file(&path);
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct DbOptions {
    sync: SyncPolicy,
    recovery: RecoveryMode,
}

impl DbOptions {
    /// Creates options with the defaults ([`SyncPolicy::Manual`],
    /// [`RecoveryMode::Strict`]).
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::{DbOptions, RecoveryMode, SyncPolicy};
    /// let opts = DbOptions::new();
    /// assert_eq!(opts.sync_policy(), SyncPolicy::Manual);
    /// assert_eq!(opts.recovery_mode(), RecoveryMode::Strict);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        DbOptions::default()
    }

    /// Sets the [`SyncPolicy`] for the store.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::{DbOptions, SyncPolicy};
    /// let opts = DbOptions::new().sync(SyncPolicy::Always);
    /// assert_eq!(opts.sync_policy(), SyncPolicy::Always);
    /// ```
    #[must_use]
    pub fn sync(mut self, policy: SyncPolicy) -> Self {
        self.sync = policy;
        self
    }

    /// Sets the [`RecoveryMode`] used when the file is damaged.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::{DbOptions, RecoveryMode};
    /// let opts = DbOptions::new().recovery(RecoveryMode::Salvage);
    /// assert_eq!(opts.recovery_mode(), RecoveryMode::Salvage);
    /// ```
    #[must_use]
    pub fn recovery(mut self, mode: RecoveryMode) -> Self {
        self.recovery = mode;
        self
    }

    /// Returns the [`SyncPolicy`] these options currently carry.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::{DbOptions, SyncPolicy};
    /// assert_eq!(DbOptions::new().sync_policy(), SyncPolicy::Manual);
    /// ```
    #[must_use]
    pub fn sync_policy(&self) -> SyncPolicy {
        self.sync
    }

    /// Returns the [`RecoveryMode`] these options currently carry.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::{DbOptions, RecoveryMode};
    /// assert_eq!(DbOptions::new().recovery_mode(), RecoveryMode::Strict);
    /// ```
    #[must_use]
    pub fn recovery_mode(&self) -> RecoveryMode {
        self.recovery
    }

    /// Returns the [`SyncPolicy`] these options currently carry.
    ///
    /// Kept for compatibility; [`sync_policy`](DbOptions::sync_policy) is the
    /// same getter under a clearer name.
    #[deprecated(since = "1.1.0", note = "use `DbOptions::sync_policy`")]
    #[must_use]
    pub fn build_sync_policy(&self) -> SyncPolicy {
        self.sync
    }

    /// Opens (or creates) the store at `path` with these options.
    ///
    /// Equivalent to [`Db::open`] when the options are the defaults.
    ///
    /// # Errors
    ///
    /// Same as [`Db::open`].
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// use bison_db::{DbOptions, SyncPolicy};
    /// # let path = std::env::temp_dir().join("bison_db_dboptions_open_doc.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// let db = DbOptions::new().sync(SyncPolicy::Always).open(&path)?;
    /// # drop(db);
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    pub fn open<P: AsRef<Path>>(self, path: P) -> Result<Db> {
        Db::open_inner(path.as_ref().to_path_buf(), self)
    }
}

/// An embedded document store backed by a single append-only file.
///
/// Open one with [`Db::open`], then [`insert`](Db::insert),
/// [`get`](Db::get), [`update`](Db::update), and [`delete`](Db::delete)
/// documents by id. Call [`flush`](Db::flush) to make recent writes durable, and
/// [`compact`](Db::compact) to reclaim space left by overwrites and deletes.
///
/// # Concurrency
///
/// `Db` follows a single-writer, multi-reader model, like an embedded SQL
/// engine: reads ([`get`](Db::get), [`find`](Db::find), [`range`](Db::range))
/// take `&self`, while writes take `&mut self`. The compiler therefore enforces
/// that writes are exclusive. `Db` is [`Send`] and [`Sync`], so the idiomatic
/// way to share one across threads is an [`Arc`](std::sync::Arc)`<`[`RwLock`](std::sync::RwLock)`<Db>>`:
/// many threads can read concurrently, and a writer takes the lock exclusively.
/// The single-writer design is inherent to a single append-only file — there is
/// one tail — and is the right fit for an embedded store.
///
/// The model extends across processes: only one `Db` may have a given file
/// open at a time. [`Db::open`] takes an exclusive lock on the store, held
/// until the handle is dropped, and a second open, from this process or any
/// other, fails with [`Error::Locked`]. Run one writer process per store and
/// share its handle.
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
    /// Secondary indexes by field name, built on demand and maintained on every
    /// write. Not persisted: rebuilt via [`Db::create_index`] each session.
    indexes: HashMap<String, SecondaryIndex>,
    /// When to force writes to disk with `fsync`.
    sync: SyncPolicy,
    /// Bytes the open that produced this handle cut or skipped.
    discarded: u64,
    /// Set after a failed `fsync` (whose outcome on disk is unknown) or a
    /// compaction that could not restore its handle; writes are then refused
    /// until the store is reopened.
    poisoned: bool,
    /// The exclusive open lock, released when the handle drops. Declared last
    /// so it is released after the file is closed.
    _lock: StoreLock,
}

impl Db {
    /// Opens the store at `path`, creating an empty one if the file does not
    /// exist, and replaying any existing records to rebuild the index.
    ///
    /// On open the whole log is scanned: each record's checksum is verified and
    /// the in-memory index is reconstructed from the surviving inserts and
    /// deletes. A record left half-written by a crash, at the very end of the
    /// file, is cut off, restoring the file to its last consistent state; the
    /// number of bytes removed is reported by [`Db::discarded_bytes`]. Damage
    /// anywhere before the end (a checksum mismatch, or a corrupted length
    /// with intact records after it) is in-place damage rather than a torn
    /// write: it is reported as [`Error::Corrupt`] and the file is left exactly
    /// as it was. [`RecoveryMode::Salvage`], set through [`DbOptions`], opens
    /// such a file by keeping every record that still verifies.
    ///
    /// Opening takes an exclusive lock on the store for the life of the
    /// handle, so a store is open in at most one handle at a time. The lock
    /// lives in a sibling file, `<path>.lock`. On Windows it is an unshared
    /// handle the OS releases when the process exits. On Unix the file records
    /// the holder's process id and start time; on Linux a lock whose holder no
    /// longer exists is replaced automatically, while on other Unix systems a
    /// lock left by a crashed process must be deleted by hand once no process
    /// is using the store.
    ///
    /// Uses [`SyncPolicy::Manual`] and [`RecoveryMode::Strict`]; for other
    /// settings, open through [`DbOptions`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Locked`] if the store is open in another handle,
    /// [`Error::Io`] if the file cannot be opened or read,
    /// [`Error::BadMagic`] if an existing file is not a bison-db store,
    /// [`Error::UnsupportedVersion`] if it was written by a newer format, and
    /// [`Error::Corrupt`] if a record before the end of the log fails
    /// verification.
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
        DbOptions::new().open(path)
    }

    /// Opens (or creates) the store at `path` with the given [`DbOptions`].
    ///
    /// A shorthand for [`DbOptions::open`]; see [`Db::open`] for the open and
    /// recovery contract.
    ///
    /// # Errors
    ///
    /// Same as [`Db::open`].
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// use bison_db::{Db, DbOptions, SyncPolicy};
    /// # let path = std::env::temp_dir().join("bison_db_open_with_example.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// let db = Db::open_with(&path, DbOptions::new().sync(SyncPolicy::Always))?;
    /// assert_eq!(db.sync_policy(), SyncPolicy::Always);
    /// # drop(db);
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    pub fn open_with<P: AsRef<Path>>(path: P, options: DbOptions) -> Result<Self> {
        options.open(path)
    }

    /// The shared open path used by [`Db::open`] and [`DbOptions::open`].
    fn open_inner(path: PathBuf, options: DbOptions) -> Result<Self> {
        // The lock comes first: everything below, including removing a
        // compaction temp file, belongs to the one live handle.
        let lock = StoreLock::acquire(&path)?;

        // Remove any temporary file left behind by an interrupted compaction;
        // the original store file is the authoritative copy.
        let _ = std::fs::remove_file(compacting_path(&path));

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
            indexes: HashMap::new(),
            sync: options.sync,
            discarded: 0,
            poisoned: false,
            _lock: lock,
        };

        if file_len == 0 {
            db.write_header()?;
            // Make the newly created file's directory entry durable, so the file
            // is guaranteed to exist after a crash that follows creation.
            sync_parent_dir(&db.path)?;
        } else {
            db.verify_header(file_len)?;
            if db.replay(file_len, options.recovery)? {
                db.salvage_rewrite()?;
            }
        }
        Ok(db)
    }

    /// Returns how many bytes the open that produced this handle removed from
    /// the file: a record torn by a crash at the end of the log, plus, under
    /// [`RecoveryMode::Salvage`], every damaged region it skipped. Zero after a
    /// clean open.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// # let path = std::env::temp_dir().join("bison_db_discarded_example.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// let db = bison_db::Db::open(&path)?;
    /// assert_eq!(db.discarded_bytes(), 0);
    /// # drop(db);
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn discarded_bytes(&self) -> u64 {
        self.discarded
    }

    /// Returns the store's [`SyncPolicy`].
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// use bison_db::{Db, SyncPolicy};
    /// # let path = std::env::temp_dir().join("bison_db_syncpolicy_getter.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// let db = Db::open(&path)?;
    /// assert_eq!(db.sync_policy(), SyncPolicy::Manual);
    /// # drop(db);
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn sync_policy(&self) -> SyncPolicy {
        self.sync
    }

    /// Inserts `doc`, assigning and returning a fresh [`DocId`].
    ///
    /// The document is appended to the log and indexed; it is readable
    /// immediately and durable after the next [`flush`](Db::flush).
    ///
    /// # Errors
    ///
    /// Returns [`Error::ValueTooLarge`] if the document exceeds
    /// [`MAX_RECORD_BYTES`] encoded, [`MAX_DOCUMENT_FIELDS`] fields in one
    /// object, or [`MAX_NESTING_DEPTH`] levels of nesting, or [`Error::Io`] if
    /// the append fails. A failed insert assigns no id and changes nothing.
    /// Under [`SyncPolicy::Always`] a failed `fsync` leaves the store refusing
    /// further writes until it is reopened, because the state of the disk is
    /// then unknown.
    ///
    /// [`MAX_DOCUMENT_FIELDS`]: crate::MAX_DOCUMENT_FIELDS
    /// [`MAX_NESTING_DEPTH`]: crate::MAX_NESTING_DEPTH
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
        self.check_writable()?;
        let id = self.next_id;
        if id == u64::MAX {
            return Err(Error::Io(io::Error::other("document id space exhausted")));
        }
        let payload_len = self.encode_record(OP_PUT, id, Some(&doc))?;
        let start = self.write_record(payload_len)?;
        // Only now, with the record written (and synced under `Always`), does
        // the in-memory state change.
        let _ = self.index.insert(id, body_loc(start, payload_len));
        self.next_id = id + 1;
        self.index_add(id, &doc);
        Ok(DocId(id))
    }

    /// Reads the document stored under `id`, or `None` if no live document has
    /// that id.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the body cannot be read, or [`Error::Corrupt`]
    /// if the stored record fails its checksum (which is checked on every
    /// read) or does not decode.
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
        match self.index.get(&id.0).copied() {
            Some(loc) => self.read_body(id.0, loc).map(Some),
            None => Ok(None),
        }
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
    /// conditions as [`insert`](Db::insert); a failed update leaves the stored
    /// document and every index as they were.
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
        self.check_writable()?;
        let Some(loc) = self.index.get(&id.0).copied() else {
            return Ok(false);
        };
        // Every fallible step comes before any in-memory change.
        let payload_len = self.encode_record(OP_PUT, id.0, Some(&doc))?;
        let old = if self.indexes.is_empty() {
            None
        } else {
            Some(self.read_body(id.0, loc)?)
        };
        let start = self.write_record(payload_len)?;
        let _ = self.index.insert(id.0, body_loc(start, payload_len));
        if let Some(old) = old {
            self.index_remove(id.0, &old);
        }
        self.index_add(id.0, &doc);
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
        self.check_writable()?;
        let Some(loc) = self.index.get(&id.0).copied() else {
            return Ok(false);
        };
        let payload_len = self.encode_record(OP_DELETE, id.0, None)?;
        let old = if self.indexes.is_empty() {
            None
        } else {
            Some(self.read_body(id.0, loc)?)
        };
        let _ = self.write_record(payload_len)?;
        let _ = self.index.remove(&id.0);
        if let Some(old) = old {
            self.index_remove(id.0, &old);
        }
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
    /// Returns [`Error::Io`] if the sync fails. After a failed sync the store
    /// refuses further writes until it is reopened: the operating system may
    /// already have dropped the unsynced data, so a later sync that succeeds
    /// would not prove it reached the disk.
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
        self.check_writable()?;
        if let Err(e) = self.file.sync_all() {
            self.poisoned = true;
            return Err(Error::Io(e));
        }
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

    /// Rewrites the file to contain only live documents, reclaiming the space
    /// held by superseded and deleted records.
    ///
    /// Every overwrite and delete leaves dead bytes behind in the append-only
    /// log; over time the file grows past the size of its live data. Compaction
    /// writes a fresh copy containing one current record per live document, then
    /// atomically swaps it in. Document ids are preserved, so existing
    /// [`DocId`]s and secondary indexes remain valid; only the on-disk layout
    /// changes.
    ///
    /// The compacted copy is built in a sibling temporary file and put in place
    /// with an atomic rename, so a crash at any point leaves either the original
    /// file or the fully compacted one — never a partial result. A leftover
    /// temporary from an interrupted compaction is cleaned up on the next
    /// [`open`](Db::open). Each record is copied only after its checksum
    /// verifies, and the id counter is kept, so a deleted id is never reissued
    /// after a compaction.
    ///
    /// The new file is `fsync`ed before the swap regardless of [`SyncPolicy`].
    /// On Unix the directory is `fsync`ed after it, so the compaction is
    /// durable on return. On Windows the rename is atomic, but a power loss
    /// right after it can still bring back the complete pre-compaction file.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the temporary file cannot be written or swapped
    /// in, or [`Error::Corrupt`] if a live record fails its checksum. If the
    /// swap fails, the handle keeps using the original file, so later writes
    /// are safe; the temporary file is removed.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// # let path = std::env::temp_dir().join("bison_db_compact_example.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// use bison_db::{Db, Document};
    /// let mut db = Db::open(&path)?;
    /// let id = db.insert({ let mut d = Document::new(); d.set("v", 1_i64); d })?;
    /// for n in 2..1000 {
    ///     db.update(id, { let mut d = Document::new(); d.set("v", n); d })?;
    /// }
    /// let before = db.stats().file_bytes;
    ///
    /// db.compact()?; // collapse 999 versions down to one live record
    ///
    /// assert!(db.stats().file_bytes < before);
    /// assert_eq!(db.get(id)?.unwrap().get("v").and_then(|v| v.as_int()), Some(999));
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    pub fn compact(&mut self) -> Result<()> {
        self.check_writable()?;
        let temp_path = compacting_path(&self.path);
        let _ = std::fs::remove_file(&temp_path);

        let temp = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        let (new_index, tail) = match self.write_compacted(&temp) {
            Ok(built) => built,
            Err(e) => {
                drop(temp);
                let _ = std::fs::remove_file(&temp_path);
                return Err(e);
            }
        };
        self.swap_in(temp, &temp_path, new_index, tail)
    }

    /// Writes the compacted log into `temp` and syncs it, returning the index
    /// over the new layout and its tail. The original file is not touched.
    fn write_compacted(&self, temp: &File) -> Result<(HashMap<u64, BodyLoc>, u64)> {
        write_header_to(temp)?;
        let mut new_index = HashMap::with_capacity(self.index.len());
        let mut tail = HEADER_LEN;
        let mut record = Vec::new();
        for (&id, &loc) in &self.index {
            // The live record is copied byte for byte once its checksum
            // verifies, so damage is reported here rather than re-stamped with
            // a fresh checksum.
            self.read_record_into(id, loc, &mut record)?;
            write_all_at(temp, &record, tail)?;
            let _ = new_index.insert(
                id,
                BodyLoc {
                    offset: tail + RECORD_PREFIX as u64,
                    len: loc.len,
                },
            );
            tail += record.len() as u64;
        }
        // Replay restores the id counter from the highest id in any record.
        // When that id is no longer live, compaction would drop every record
        // naming it, so a tombstone for it is kept to hold the counter.
        let high = self.next_id - 1;
        if high >= 1 && !self.index.contains_key(&high) {
            let tombstone = tombstone_record(high);
            write_all_at(temp, &tombstone, tail)?;
            tail += tombstone.len() as u64;
        }
        temp.sync_all()?;
        Ok((new_index, tail))
    }

    /// Puts the compacted file in place of the original. The handle switches
    /// to the new file only once the rename has succeeded; until then it keeps
    /// the original, so a failed swap loses nothing.
    fn swap_in(
        &mut self,
        temp: File,
        temp_path: &Path,
        new_index: HashMap<u64, BodyLoc>,
        tail: u64,
    ) -> Result<()> {
        if let Err(first) = rename_file(temp_path, &self.path) {
            #[cfg(windows)]
            {
                return self.swap_in_closed(temp, temp_path, new_index, tail, first);
            }
            #[cfg(not(windows))]
            {
                drop(temp);
                let _ = std::fs::remove_file(temp_path);
                return Err(Error::Io(first));
            }
        }
        // The path now names the compacted file, which `temp` has open.
        self.adopt(temp, new_index, tail);
        sync_parent_dir(&self.path)
    }

    /// Windows retry for [`Db::swap_in`]: some filesystems (FAT, exFAT, many
    /// network shares) refuse to replace a file that is still open, so the
    /// original handle is closed and the rename tried again. The store lock
    /// keeps other handles out while the original is closed.
    #[cfg(windows)]
    fn swap_in_closed(
        &mut self,
        temp: File,
        temp_path: &Path,
        new_index: HashMap<u64, BodyLoc>,
        tail: u64,
        first: io::Error,
    ) -> Result<()> {
        let Ok(stand_in) = temp.try_clone() else {
            drop(temp);
            let _ = std::fs::remove_file(temp_path);
            return Err(Error::Io(first));
        };
        drop(std::mem::replace(&mut self.file, stand_in));
        match rename_file(temp_path, &self.path) {
            Ok(()) => {
                self.adopt(temp, new_index, tail);
                sync_parent_dir(&self.path)
            }
            Err(second) => match OpenOptions::new().read(true).write(true).open(&self.path) {
                Ok(original) => {
                    self.file = original;
                    drop(temp);
                    let _ = std::fs::remove_file(temp_path);
                    Err(Error::Io(second))
                }
                Err(reopen) => {
                    // The original cannot be reopened. The compacted copy holds
                    // the same live documents, so reads are served from it, but
                    // writes are refused: the next open discards that copy and
                    // uses the original, which is complete.
                    self.adopt(temp, new_index, tail);
                    self.poisoned = true;
                    Err(Error::Io(reopen))
                }
            },
        }
    }

    /// Switches the handle to a compacted file and its index.
    fn adopt(&mut self, file: File, new_index: HashMap<u64, BodyLoc>, tail: u64) {
        self.file = file;
        self.index = new_index;
        self.tail = tail;
    }

    /// Under [`RecoveryMode::Salvage`], after replay skipped damaged regions:
    /// keeps a copy of the damaged file, then rewrites the store from the
    /// records that verified, so it opens cleanly from now on.
    fn salvage_rewrite(&mut self) -> Result<()> {
        let copy = free_sidecar(&self.path, ".damaged")?;
        let _ = std::fs::copy(&self.path, &copy)?;
        OpenOptions::new().write(true).open(&copy)?.sync_all()?;
        sync_parent_dir(&copy)?;
        self.compact()
    }

    /// Builds a secondary index over `field`, making [`find`](Db::find) and
    /// [`range`](Db::range) on that field fast point and range lookups instead of
    /// full scans.
    ///
    /// The index is built by reading every live document once and recording its
    /// value for `field`; documents without the field are skipped. From then on,
    /// it is maintained automatically on every insert, update, and delete. Any
    /// number of fields may be indexed — call this once per field.
    ///
    /// Indexes live in memory only and are **not** persisted: after reopening a
    /// store, call this again for each field you want indexed. Calling it for a
    /// field that is already indexed is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] or [`Error::Corrupt`] if a document cannot be read
    /// while building the index.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// # let path = std::env::temp_dir().join("bison_db_createindex_example.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// use bison_db::{Db, Document, Value};
    /// let mut db = Db::open(&path)?;
    /// db.insert({ let mut d = Document::new(); d.set("city", "Oslo"); d })?;
    ///
    /// db.create_index("city")?;
    /// let hits = db.find("city", &Value::from("Oslo"))?;
    /// assert_eq!(hits.len(), 1);
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    pub fn create_index(&mut self, field: &str) -> Result<()> {
        if self.indexes.contains_key(field) {
            return Ok(());
        }
        let mut index = SecondaryIndex::new();
        let entries: Vec<(u64, BodyLoc)> = self.index.iter().map(|(id, loc)| (*id, *loc)).collect();
        for (id, loc) in entries {
            let doc = self.read_body(id, loc)?;
            if let Some(value) = doc.get(field) {
                index.add(value, id);
            }
        }
        let _ = self.indexes.insert(field.to_string(), index);
        Ok(())
    }

    /// Drops the secondary index over `field`, returning `true` if one existed.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// # let path = std::env::temp_dir().join("bison_db_dropindex_example.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// let mut db = bison_db::Db::open(&path)?;
    /// db.create_index("name")?;
    /// assert!(db.drop_index("name"));
    /// assert!(!db.drop_index("name"));
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    pub fn drop_index(&mut self, field: &str) -> bool {
        self.indexes.remove(field).is_some()
    }

    /// Returns an iterator over the names of the currently indexed fields.
    ///
    /// The order is unspecified.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// # let path = std::env::temp_dir().join("bison_db_indexes_example.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// let mut db = bison_db::Db::open(&path)?;
    /// db.create_index("a")?;
    /// db.create_index("b")?;
    /// assert_eq!(db.indexes().count(), 2);
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    pub fn indexes(&self) -> impl Iterator<Item = &str> {
        self.indexes.keys().map(String::as_str)
    }

    /// Returns the ids of all live documents whose `field` equals `value`.
    ///
    /// If `field` is indexed (see [`create_index`](Db::create_index)) this is a
    /// point lookup; otherwise it falls back to scanning every live document, so
    /// the result is correct either way — the index only changes the speed.
    /// The ids come back in ascending order either way. Equality follows the
    /// same total order the indexes use, so a `Float` field distinguishes `0.0`
    /// from `-0.0`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] or [`Error::Corrupt`] if a document must be read
    /// (the unindexed path) and cannot be.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// # let path = std::env::temp_dir().join("bison_db_find_example.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// use bison_db::{Db, Document, Value};
    /// let mut db = Db::open(&path)?;
    /// db.insert({ let mut d = Document::new(); d.set("role", "admin"); d })?;
    /// db.insert({ let mut d = Document::new(); d.set("role", "user"); d })?;
    /// db.create_index("role")?;
    ///
    /// assert_eq!(db.find("role", &Value::from("admin"))?.len(), 1);
    /// assert!(db.find("role", &Value::from("ghost"))?.is_empty());
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    pub fn find(&self, field: &str, value: &Value) -> Result<Vec<DocId>> {
        if let Some(index) = self.indexes.get(field) {
            return Ok(index.equal(value).into_iter().map(DocId).collect());
        }
        let mut out = Vec::new();
        for (id, loc) in &self.index {
            let doc = self.read_body(*id, *loc)?;
            if doc
                .get(field)
                .is_some_and(|v| total_cmp_value(v, value) == core::cmp::Ordering::Equal)
            {
                out.push(DocId(*id));
            }
        }
        // The index yields ascending ids; the scan matches it.
        out.sort_unstable();
        Ok(out)
    }

    /// Returns the ids of all live documents whose `field` falls within `range`.
    ///
    /// Bounds are [`Value`]s compared with the same total order the indexes use;
    /// any [`RangeBounds`] form works (`a..b`, `a..=b`, `..b`, `a..`, `..`).
    /// The matches come back ordered by field value, then id. If `field` is
    /// indexed this is a range walk over the index; otherwise the store scans
    /// every live document and sorts the matches. As with [`find`](Db::find),
    /// the index changes only the speed, not the result or its order.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] or [`Error::Corrupt`] if a document must be read
    /// (the unindexed path) and cannot be.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> bison_db::Result<()> {
    /// # let path = std::env::temp_dir().join("bison_db_range_example.bison");
    /// # let _ = std::fs::remove_file(&path);
    /// use bison_db::{Db, Document, Value};
    /// let mut db = Db::open(&path)?;
    /// for age in [17_i64, 25, 40, 70] {
    ///     db.insert({ let mut d = Document::new(); d.set("age", age); d })?;
    /// }
    /// db.create_index("age")?;
    ///
    /// // Working-age adults: 18..=65.
    /// let hits = db.range("age", Value::from(18_i64)..=Value::from(65_i64))?;
    /// assert_eq!(hits.len(), 2); // 25 and 40
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(())
    /// # }
    /// ```
    pub fn range<R: RangeBounds<Value>>(&self, field: &str, range: R) -> Result<Vec<DocId>> {
        let lo = range.start_bound();
        let hi = range.end_bound();
        if let Some(index) = self.indexes.get(field) {
            return Ok(index.range(lo, hi).into_iter().map(DocId).collect());
        }
        let mut hits = Vec::new();
        for (id, loc) in &self.index {
            let mut doc = self.read_body(*id, *loc)?;
            if doc.get(field).is_some_and(|v| in_bounds(v, lo, hi)) {
                if let Some(value) = doc.remove(field) {
                    hits.push((value, *id));
                }
            }
        }
        // Same order as the index: by value in the index's total order, then
        // by id.
        hits.sort_unstable_by(|a, b| total_cmp_value(&a.0, &b.0).then(a.1.cmp(&b.1)));
        Ok(hits.into_iter().map(|(_, id)| DocId(id)).collect())
    }

    /// Reads document `id`'s record at `loc`, verifies its checksum, and
    /// decodes the body.
    fn read_body(&self, id: u64, loc: BodyLoc) -> Result<Document> {
        let mut record = Vec::new();
        self.read_record_into(id, loc, &mut record)?;
        decode_document(&record[RECORD_PREFIX..])
    }

    /// Reads the whole record (frame, op, id and body) of document `id` at
    /// `loc` into `out` and verifies it: the length, the checksum, and that it
    /// is a PUT of that id.
    fn read_record_into(&self, id: u64, loc: BodyLoc, out: &mut Vec<u8>) -> Result<()> {
        let start = loc.offset - RECORD_PREFIX as u64;
        let payload_len = MIN_PAYLOAD + loc.len as usize;
        out.clear();
        out.resize(FRAME_LEN + payload_len, 0);
        read_exact_at(&self.file, out, start)?;
        let stored_len = u32::from_le_bytes([out[0], out[1], out[2], out[3]]) as usize;
        let stored_crc = u32::from_le_bytes([out[4], out[5], out[6], out[7]]);
        if stored_len != payload_len || crc32c(&out[FRAME_LEN..]) != stored_crc {
            return Err(Error::Corrupt("crc mismatch"));
        }
        if out[FRAME_LEN] != OP_PUT || read_id(&out[FRAME_LEN..]) != id {
            return Err(Error::Corrupt("record does not match the index"));
        }
        Ok(())
    }

    /// Refuses writes once the store is poisoned.
    fn check_writable(&self) -> Result<()> {
        if self.poisoned {
            return Err(Error::Io(io::Error::other(
                "a previous sync or compaction failed; reopen the store before writing",
            )));
        }
        Ok(())
    }

    /// Adds document `id`'s indexed field values to every secondary index.
    fn index_add(&mut self, id: u64, doc: &Document) {
        for (field, index) in &mut self.indexes {
            if let Some(value) = doc.get(field) {
                index.add(value, id);
            }
        }
    }

    /// Removes document `id`'s indexed field values from every secondary index.
    fn index_remove(&mut self, id: u64, doc: &Document) {
        for (field, index) in &mut self.indexes {
            if let Some(value) = doc.get(field) {
                index.remove(value, id);
            }
        }
    }

    /// Frames one record in `scratch` and returns its payload length. This is
    /// every fallible step of a write that does not touch the file: encoding
    /// and the size check. The frame is built in `scratch` so the steady-state
    /// write path performs no per-record allocation.
    fn encode_record(&mut self, op: u8, id: u64, doc: Option<&Document>) -> Result<usize> {
        self.scratch.clear();
        // Reserve the frame header; the length and checksum are backfilled once
        // the payload is known.
        self.scratch.extend_from_slice(&[0u8; FRAME_LEN]);
        self.scratch.push(op);
        self.scratch.extend_from_slice(&id.to_le_bytes());
        let encoded = match doc {
            Some(doc) => encode_document_into(&mut self.scratch, doc),
            None => Ok(()),
        };
        let payload_len = self.scratch.len() - FRAME_LEN;
        if encoded.is_err() || payload_len > MAX_RECORD_BYTES {
            self.trim_scratch();
            return Err(encoded.err().unwrap_or(Error::ValueTooLarge));
        }
        let crc = crc32c(&self.scratch[FRAME_LEN..]);
        self.scratch[0..4].copy_from_slice(&(payload_len as u32).to_le_bytes());
        self.scratch[4..8].copy_from_slice(&crc.to_le_bytes());
        Ok(payload_len)
    }

    /// Appends the record framed in `scratch` at the tail, syncing it under
    /// [`SyncPolicy::Always`], and returns its start offset. The tail advances
    /// only on success; on failure the file is cut back to the old tail (best
    /// effort) so no partial record is left behind, and a failed sync poisons
    /// the store.
    fn write_record(&mut self, payload_len: usize) -> Result<u64> {
        self.check_writable()?;
        let start = self.tail;
        if let Err(e) = write_all_at(&self.file, &self.scratch, start) {
            let _ = self.file.set_len(start);
            self.trim_scratch();
            return Err(Error::Io(e));
        }
        if self.sync == SyncPolicy::Always {
            if let Err(e) = self.file.sync_all() {
                self.poisoned = true;
                let _ = self.file.set_len(start);
                self.trim_scratch();
                return Err(Error::Io(e));
            }
        }
        self.tail = start + (FRAME_LEN + payload_len) as u64;
        self.trim_scratch();
        Ok(start)
    }

    /// Releases the write buffer's memory after an unusually large record.
    fn trim_scratch(&mut self) {
        if self.scratch.capacity() > SCRATCH_SHRINK_ABOVE {
            self.scratch.clear();
            self.scratch.shrink_to(SCRATCH_KEEP);
        }
    }

    /// Writes the 16-byte file header at offset 0 and syncs it, establishing a
    /// valid empty store.
    fn write_header(&mut self) -> Result<()> {
        write_header_to(&self.file)?;
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

    /// Scans every record after the header and rebuilds the index.
    ///
    /// A record cut short at the very end is a torn write and is truncated
    /// away. Damage before the end is [`Error::Corrupt`] under
    /// [`RecoveryMode::Strict`], with the file left alone; under
    /// [`RecoveryMode::Salvage`] each damaged region is skipped up to the next
    /// intact record and `true` is returned, so the caller rewrites the store.
    /// The log is read sequentially through a buffer, a few syscalls per MiB.
    fn replay(&mut self, file_len: u64, mode: RecoveryMode) -> Result<bool> {
        let salvage = mode == RecoveryMode::Salvage;
        let mut window = Window::new();
        let mut offset = HEADER_LEN;
        let mut damaged = false;
        let mut torn_from = None;

        while offset < file_len {
            let resume = match check_record(&self.file, &mut window, offset, file_len)? {
                Frame::Good {
                    op,
                    id,
                    payload_len,
                    end,
                } => {
                    self.apply(op, id, offset, payload_len);
                    offset = end;
                    continue;
                }
                // Too few bytes for a frame: only a torn write ends like this.
                Frame::Short => None,
                Frame::BadLength => {
                    // A torn write leaves nothing intact after it. An intact
                    // record further on means the length was damaged in place.
                    let next = find_next_record(&self.file, &mut window, offset + 1, file_len)?;
                    if next.is_some() && !salvage {
                        return Err(Error::Corrupt(
                            "bad record length before the end of the log",
                        ));
                    }
                    next
                }
                Frame::BadChecksum { end } => {
                    if end == file_len {
                        // The final record failed its checksum: a torn write.
                        None
                    } else if !salvage {
                        return Err(Error::Corrupt("crc mismatch"));
                    } else {
                        find_next_record(&self.file, &mut window, offset + 1, file_len)?
                    }
                }
                Frame::Invalid { end, what } => {
                    if !salvage {
                        return Err(Error::Corrupt(what));
                    }
                    Some(end)
                }
            };
            match resume {
                Some(next) => {
                    damaged = true;
                    self.discarded += next - offset;
                    offset = next;
                }
                None => {
                    torn_from = Some(offset);
                    break;
                }
            }
        }

        if let Some(from) = torn_from {
            self.discarded += file_len - from;
            // With damage skipped, the salvage rewrite drops the torn tail along
            // with everything else, and the damaged copy keeps it.
            if !damaged {
                self.file.set_len(from)?;
            }
            offset = from;
        }
        self.tail = offset;
        Ok(damaged)
    }

    /// Applies one verified record from replay to the in-memory index.
    fn apply(&mut self, op: u8, id: u64, record_start: u64, payload_len: usize) {
        if op == OP_PUT {
            let _ = self.index.insert(id, body_loc(record_start, payload_len));
        } else {
            let _ = self.index.remove(&id);
        }
        // `check_record` rejects `u64::MAX`, so this cannot overflow.
        if id >= self.next_id {
            self.next_id = id + 1;
        }
    }
}

/// The location of the body of a PUT record starting at `record_start`.
fn body_loc(record_start: u64, payload_len: usize) -> BodyLoc {
    BodyLoc {
        offset: record_start + RECORD_PREFIX as u64,
        len: (payload_len - MIN_PAYLOAD) as u32,
    }
}

/// Reads the little-endian id that follows the op tag in a payload.
fn read_id(payload: &[u8]) -> u64 {
    let mut id = [0u8; 8];
    id.copy_from_slice(&payload[1..MIN_PAYLOAD]);
    u64::from_le_bytes(id)
}

/// A framed DELETE record for `id`.
fn tombstone_record(id: u64) -> [u8; FRAME_LEN + MIN_PAYLOAD] {
    let mut record = [0u8; FRAME_LEN + MIN_PAYLOAD];
    record[FRAME_LEN] = OP_DELETE;
    record[FRAME_LEN + 1..].copy_from_slice(&id.to_le_bytes());
    let crc = crc32c(&record[FRAME_LEN..]);
    record[0..4].copy_from_slice(&(MIN_PAYLOAD as u32).to_le_bytes());
    record[4..8].copy_from_slice(&crc.to_le_bytes());
    record
}

/// A read buffer over the log for sequential replay and scanning: one
/// positional read fills up to [`READ_CHUNK`] bytes (or a whole record, if it
/// is larger), so replay costs a few syscalls per MiB rather than two per
/// record.
struct Window {
    buf: Vec<u8>,
    start: u64,
    len: usize,
}

impl Window {
    fn new() -> Self {
        Window {
            buf: Vec::new(),
            start: 0,
            len: 0,
        }
    }

    /// Returns the `n` bytes at `at`, refilling from `file` when they are not
    /// buffered. The caller guarantees `at + n <= file_len`.
    fn get(&mut self, file: &File, file_len: u64, at: u64, n: usize) -> io::Result<&[u8]> {
        let end = at + n as u64;
        if at < self.start || end > self.start + self.len as u64 {
            let len = (n.max(READ_CHUNK) as u64).min(file_len - at) as usize;
            if self.buf.len() < len {
                self.buf.resize(len, 0);
            }
            self.len = 0;
            read_exact_at(file, &mut self.buf[..len], at)?;
            self.start = at;
            self.len = len;
        }
        let from = (at - self.start) as usize;
        Ok(&self.buf[from..from + n])
    }
}

/// What replay found at one offset.
enum Frame {
    /// An intact record.
    Good {
        op: u8,
        id: u64,
        payload_len: usize,
        end: u64,
    },
    /// Fewer bytes than a frame remain.
    Short,
    /// The length is out of range or runs past the end of the file.
    BadLength,
    /// The length fits but the checksum does not match.
    BadChecksum { end: u64 },
    /// The checksum matches but the op or id is not valid.
    Invalid { end: u64, what: &'static str },
}

/// Classifies the record starting at `offset`.
fn check_record(file: &File, window: &mut Window, offset: u64, file_len: u64) -> io::Result<Frame> {
    if file_len - offset < FRAME_LEN as u64 {
        return Ok(Frame::Short);
    }
    let head = window.get(file, file_len, offset, FRAME_LEN)?;
    let payload_len = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
    let expected_crc = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
    if !(MIN_PAYLOAD..=MAX_RECORD_BYTES).contains(&payload_len) {
        return Ok(Frame::BadLength);
    }
    let end = offset + (FRAME_LEN + payload_len) as u64;
    if end > file_len {
        return Ok(Frame::BadLength);
    }
    let payload = window.get(file, file_len, offset + FRAME_LEN as u64, payload_len)?;
    if crc32c(payload) != expected_crc {
        return Ok(Frame::BadChecksum { end });
    }
    let op = payload[0];
    let id = read_id(payload);
    if op != OP_PUT && op != OP_DELETE {
        return Ok(Frame::Invalid {
            end,
            what: "unknown record op",
        });
    }
    // Ids start at 1, and `u64::MAX` would overflow the id counter.
    if id == 0 || id == u64::MAX {
        return Ok(Frame::Invalid {
            end,
            what: "invalid document id",
        });
    }
    Ok(Frame::Good {
        op,
        id,
        payload_len,
        end,
    })
}

/// Searches forward from `from` for the start of an intact record: a frame
/// whose length fits, whose op and id are valid, and whose checksum verifies.
/// Used to tell a torn tail (nothing intact follows) from damage in place.
fn find_next_record(
    file: &File,
    window: &mut Window,
    from: u64,
    file_len: u64,
) -> Result<Option<u64>> {
    let mut budget = SCAN_BUDGET;
    let mut at = from;
    while at + (FRAME_LEN + MIN_PAYLOAD) as u64 <= file_len {
        let head = window.get(file, file_len, at, FRAME_LEN + MIN_PAYLOAD)?;
        let payload_len = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let expected_crc = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
        let op = head[FRAME_LEN];
        let id = read_id(&head[FRAME_LEN..]);
        let plausible = (MIN_PAYLOAD..=MAX_RECORD_BYTES).contains(&payload_len)
            && at + (FRAME_LEN + payload_len) as u64 <= file_len
            && ((op == OP_PUT && payload_len >= MIN_PAYLOAD + 4)
                || (op == OP_DELETE && payload_len == MIN_PAYLOAD))
            && id != 0
            && id != u64::MAX;
        if plausible {
            budget = budget
                .checked_sub(payload_len)
                .ok_or(Error::Corrupt("damaged region too large to classify"))?;
            let payload = window.get(file, file_len, at + FRAME_LEN as u64, payload_len)?;
            if crc32c(payload) == expected_crc {
                return Ok(Some(at));
            }
        }
        at += 1;
    }
    Ok(None)
}

/// The first of `<path><suffix>`, `<path><suffix>.1`, `<path><suffix>.2`, ...
/// that does not exist yet.
fn free_sidecar(path: &Path, suffix: &str) -> Result<PathBuf> {
    for n in 0..1000u32 {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        if n > 0 {
            name.push(format!(".{n}"));
        }
        let candidate = PathBuf::from(name);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(Error::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free name for the copy of the damaged store",
    )))
}

/// Renames `from` over `to`. Tests can make it fail on demand.
fn rename_file(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(test)]
    if tests::take_injected_rename_failure() {
        return Err(io::Error::other("injected rename failure"));
    }
    std::fs::rename(from, to)
}

impl fmt::Debug for Db {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Db")
            .field("path", &self.path)
            .field("live_documents", &self.index.len())
            .field("file_bytes", &self.tail)
            .field("sync", &self.sync)
            .finish()
    }
}

/// Compile-time proof that [`Db`] is `Send + Sync`, so it can be shared across
/// threads behind a lock as the concurrency docs describe. If a future field
/// broke this, the crate would fail to compile here rather than silently.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Db>();
};

impl Drop for Db {
    /// Makes a best-effort `fsync` on a clean shutdown under
    /// [`SyncPolicy::Manual`], so a normal program exit does not lose writes that
    /// were never explicitly flushed. Under [`SyncPolicy::Always`] every write is
    /// already durable, so nothing is done. Any error here is ignored because a
    /// destructor cannot return one; call [`Db::flush`] before dropping when you
    /// need to observe a sync failure.
    fn drop(&mut self) {
        if self.sync == SyncPolicy::Manual && !self.poisoned {
            let _ = self.file.sync_all();
        }
    }
}

/// Writes the 16-byte file header at offset 0 of `file` (without syncing).
fn write_header_to(file: &File) -> Result<()> {
    let mut header = [0u8; HEADER_LEN as usize];
    header[0..8].copy_from_slice(&HEADER_MAGIC);
    header[8..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    write_all_at(file, &header, 0)?;
    Ok(())
}

/// The sibling path a compaction writes its temporary file to: the store's path
/// with a `.compacting` suffix. A leftover one is removed on [`Db::open`].
fn compacting_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".compacting");
    PathBuf::from(name)
}

/// Forces the directory containing `path` to disk, so the file's creation is
/// durable. On Unix this is a real `fsync` of the parent directory; on Windows,
/// directory handles do not support this and file-level `fsync` already persists
/// the entry, so this is a documented no-op.
#[cfg(unix)]
fn sync_parent_dir(path: &Path) -> Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = parent.unwrap_or_else(|| Path::new("."));
    let handle = File::open(dir)?;
    handle.sync_all()?;
    Ok(())
}

/// Windows counterpart to [`sync_parent_dir`]: a no-op, because the file-level
/// `fsync` already makes the directory entry durable on this platform.
#[cfg(windows)]
fn sync_parent_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::value::Value;
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU64, Ordering};

    std::thread_local! {
        /// How many upcoming renames on this thread should fail.
        static FAIL_RENAMES: Cell<u32> = const { Cell::new(0) };
    }

    /// Consumes one injected rename failure, if any are pending.
    pub(super) fn take_injected_rename_failure() -> bool {
        FAIL_RENAMES.with(|n| {
            let left = n.get();
            if left > 0 {
                n.set(left - 1);
            }
            left > 0
        })
    }

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

    fn sorted(mut ids: Vec<DocId>) -> Vec<u64> {
        ids.sort();
        ids.into_iter().map(DocId::get).collect()
    }

    #[test]
    fn test_create_index_then_find() {
        let path = temp_path();
        let mut db = Db::open(&path).unwrap();
        let a = db.insert(doc(&[("g", 1)])).unwrap();
        let b = db.insert(doc(&[("g", 2)])).unwrap();
        let c = db.insert(doc(&[("g", 1)])).unwrap();

        db.create_index("g").unwrap();
        assert_eq!(
            sorted(db.find("g", &Value::from(1_i64)).unwrap()),
            sorted(vec![a, c])
        );
        assert_eq!(
            sorted(db.find("g", &Value::from(2_i64)).unwrap()),
            vec![b.get()]
        );
        assert!(db.find("g", &Value::from(9_i64)).unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_find_indexed_matches_scan() {
        let path = temp_path();
        let mut db = Db::open(&path).unwrap();
        for n in [1, 2, 2, 3, 2] {
            db.insert(doc(&[("k", n)])).unwrap();
        }
        let scan = sorted(db.find("k", &Value::from(2_i64)).unwrap()); // no index yet
        db.create_index("k").unwrap();
        let indexed = sorted(db.find("k", &Value::from(2_i64)).unwrap());
        assert_eq!(scan, indexed);
        assert_eq!(scan.len(), 3);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_range_query_inclusive_and_exclusive() {
        let path = temp_path();
        let mut db = Db::open(&path).unwrap();
        for n in [10, 20, 30, 40] {
            db.insert(doc(&[("age", n)])).unwrap();
        }
        db.create_index("age").unwrap();
        assert_eq!(
            db.range("age", Value::from(20_i64)..=Value::from(30_i64))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            db.range("age", Value::from(20_i64)..Value::from(40_i64))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(db.range("age", Value::from(25_i64)..).unwrap().len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_index_maintained_on_update_and_delete() {
        let path = temp_path();
        let mut db = Db::open(&path).unwrap();
        let id = db.insert(doc(&[("status", 1)])).unwrap();
        db.create_index("status").unwrap();
        assert_eq!(db.find("status", &Value::from(1_i64)).unwrap(), vec![id]);

        db.update(id, doc(&[("status", 2)])).unwrap();
        assert!(db.find("status", &Value::from(1_i64)).unwrap().is_empty());
        assert_eq!(db.find("status", &Value::from(2_i64)).unwrap(), vec![id]);

        db.delete(id).unwrap();
        assert!(db.find("status", &Value::from(2_i64)).unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_indexes_listed_and_dropped() {
        let path = temp_path();
        let mut db = Db::open(&path).unwrap();
        db.create_index("a").unwrap();
        db.create_index("b").unwrap();
        db.create_index("a").unwrap(); // idempotent
        let mut names: Vec<&str> = db.indexes().collect();
        names.sort_unstable();
        assert_eq!(names, ["a", "b"]);
        assert!(db.drop_index("a"));
        assert!(!db.drop_index("a"));
        assert_eq!(db.indexes().count(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_index_not_persisted_but_rebuildable_after_reopen() {
        let path = temp_path();
        let id;
        {
            let mut db = Db::open(&path).unwrap();
            id = db.insert(doc(&[("city", 7)])).unwrap();
            db.create_index("city").unwrap();
            db.flush().unwrap();
        }
        let mut db = Db::open(&path).unwrap();
        assert_eq!(db.indexes().count(), 0); // indexes are not on disk
        db.create_index("city").unwrap(); // rebuild from the log
        assert_eq!(db.find("city", &Value::from(7_i64)).unwrap(), vec![id]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_default_sync_policy_is_manual() {
        let path = temp_path();
        let db = Db::open(&path).unwrap();
        assert_eq!(db.sync_policy(), SyncPolicy::Manual);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_options_set_always_sync_policy() {
        let path = temp_path();
        let mut db = Db::open_with(&path, DbOptions::new().sync(SyncPolicy::Always)).unwrap();
        assert_eq!(db.sync_policy(), SyncPolicy::Always);
        // Every write fsyncs; data is present and reopen recovers it.
        let id = db.insert(doc(&[("v", 1)])).unwrap();
        assert!(db.get(id).unwrap().is_some());
        drop(db);
        let db = Db::open(&path).unwrap();
        assert_eq!(db.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_always_sync_persists_without_explicit_flush() {
        let path = temp_path();
        let id;
        {
            let mut db = Db::open_with(&path, DbOptions::new().sync(SyncPolicy::Always)).unwrap();
            id = db.insert(doc(&[("durable", 1)])).unwrap();
            // No flush() call: Always already synced each write.
        }
        let db = Db::open(&path).unwrap();
        assert!(db.get(id).unwrap().is_some());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_dboptions_open_matches_db_open() {
        let path = temp_path();
        let db = DbOptions::new().open(&path).unwrap();
        assert_eq!(db.sync_policy(), SyncPolicy::Manual);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_compact_reclaims_space_and_preserves_data() {
        let path = temp_path();
        let mut db = Db::open(&path).unwrap();
        let id = db.insert(doc(&[("v", 1)])).unwrap();
        for n in 2..500 {
            db.update(id, doc(&[("v", n)])).unwrap();
        }
        let before = db.stats().file_bytes;

        db.compact().unwrap();

        let after = db.stats().file_bytes;
        assert!(
            after < before,
            "compaction should shrink the file: {after} !< {before}"
        );
        assert_eq!(db.len(), 1);
        assert_eq!(
            db.get(id)
                .unwrap()
                .unwrap()
                .get("v")
                .and_then(Value::as_int),
            Some(499)
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_compact_drops_deleted_records() {
        let path = temp_path();
        let mut db = Db::open(&path).unwrap();
        let keep = db.insert(doc(&[("k", 1)])).unwrap();
        let gone = db.insert(doc(&[("k", 2)])).unwrap();
        db.delete(gone).unwrap();

        db.compact().unwrap();

        assert_eq!(db.len(), 1);
        assert!(db.get(keep).unwrap().is_some());
        assert!(db.get(gone).unwrap().is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_compact_preserves_ids_and_indexes() {
        let path = temp_path();
        let mut db = Db::open(&path).unwrap();
        let a = db.insert(doc(&[("city", 1)])).unwrap();
        let b = db.insert(doc(&[("city", 2)])).unwrap();
        db.create_index("city").unwrap();

        db.compact().unwrap();

        // Ids are unchanged and the secondary index still resolves them.
        assert_eq!(db.find("city", &Value::from(1_i64)).unwrap(), vec![a]);
        assert_eq!(db.find("city", &Value::from(2_i64)).unwrap(), vec![b]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_compact_then_reopen_recovers() {
        let path = temp_path();
        let id;
        {
            let mut db = Db::open(&path).unwrap();
            id = db.insert(doc(&[("v", 7)])).unwrap();
            db.update(id, doc(&[("v", 8)])).unwrap();
            db.compact().unwrap();
            db.flush().unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.len(), 1);
        assert_eq!(
            db.get(id)
                .unwrap()
                .unwrap()
                .get("v")
                .and_then(Value::as_int),
            Some(8)
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_compact_empty_db_is_ok() {
        let path = temp_path();
        let mut db = Db::open(&path).unwrap();
        db.compact().unwrap();
        assert!(db.is_empty());
        // Still writable afterwards.
        let id = db.insert(doc(&[("x", 1)])).unwrap();
        assert!(db.get(id).unwrap().is_some());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_compact_preserves_sync_policy_and_data() {
        let path = temp_path();
        let mut db = Db::open_with(&path, DbOptions::new().sync(SyncPolicy::Always)).unwrap();
        let id = db.insert(doc(&[("v", 1)])).unwrap();
        for n in 2..50 {
            db.update(id, doc(&[("v", n)])).unwrap();
        }
        db.compact().unwrap();
        assert_eq!(db.sync_policy(), SyncPolicy::Always);
        assert_eq!(
            db.get(id)
                .unwrap()
                .unwrap()
                .get("v")
                .and_then(Value::as_int),
            Some(49)
        );
        // Still writable, still durable-per-write, after the swap.
        let id2 = db.insert(doc(&[("w", 7)])).unwrap();
        assert!(db.get(id2).unwrap().is_some());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_value_too_large_is_rejected_and_leaves_db_unchanged() {
        let path = temp_path();
        let mut db = Db::open(&path).unwrap();
        let mut big = Document::new();
        big.set("blob", Value::Bytes(vec![0u8; MAX_RECORD_BYTES + 1]));
        assert!(matches!(db.insert(big), Err(Error::ValueTooLarge)));
        // The failed insert must not have advanced state.
        assert_eq!(db.len(), 0);
        let id = db.insert(doc(&[("ok", 1)])).unwrap();
        assert_eq!(id.get(), 1, "a rejected insert must not consume an id");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_failed_compaction_rename_keeps_the_original_live() {
        let path = temp_path();
        let first;
        let second;
        {
            let mut db = Db::open_with(&path, DbOptions::new().sync(SyncPolicy::Always)).unwrap();
            first = db.insert(doc(&[("v", 1)])).unwrap();
            db.update(first, doc(&[("v", 2)])).unwrap();
            // Fail the rename and, on Windows, the retry after closing.
            FAIL_RENAMES.with(|n| n.set(2));
            assert!(db.compact().is_err());
            FAIL_RENAMES.with(|n| n.set(0));
            assert!(!compacting_path(&path).exists(), "temp file removed");
            second = db.insert(doc(&[("v", 3)])).unwrap();
            assert_eq!(
                db.get(first)
                    .unwrap()
                    .unwrap()
                    .get("v")
                    .and_then(Value::as_int),
                Some(2)
            );
        }
        let db = Db::open(&path).unwrap();
        assert!(db.get(first).unwrap().is_some());
        assert!(
            db.get(second).unwrap().is_some(),
            "a write acknowledged after a failed compaction survives reopen"
        );
        drop(db);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_poisoned_store_refuses_writes_but_reads() {
        let path = temp_path();
        let mut db = Db::open(&path).unwrap();
        let id = db.insert(doc(&[("v", 1)])).unwrap();
        db.poisoned = true;
        assert!(matches!(db.insert(doc(&[("v", 2)])), Err(Error::Io(_))));
        assert!(matches!(db.update(id, doc(&[("v", 2)])), Err(Error::Io(_))));
        assert!(matches!(db.delete(id), Err(Error::Io(_))));
        assert!(matches!(db.flush(), Err(Error::Io(_))));
        assert!(matches!(db.compact(), Err(Error::Io(_))));
        assert!(db.get(id).unwrap().is_some());
        drop(db);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_large_write_does_not_pin_the_scratch_buffer() {
        let path = temp_path();
        let mut db = Db::open(&path).unwrap();
        let mut big = Document::new();
        big.set("blob", Value::Bytes(vec![7u8; 4 * 1024 * 1024]));
        db.insert(big).unwrap();
        assert!(db.scratch.capacity() <= SCRATCH_SHRINK_ABOVE);
        drop(db);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_open_removes_stale_compacting_temp() {
        let path = temp_path();
        {
            let mut db = Db::open(&path).unwrap();
            db.insert(doc(&[("v", 1)])).unwrap();
            db.flush().unwrap();
        }
        // Simulate a compaction that died before swapping in its temp file.
        let stale = compacting_path(&path);
        std::fs::write(&stale, b"garbage from an interrupted compaction").unwrap();
        assert!(stale.exists());

        let db = Db::open(&path).unwrap();
        assert!(!stale.exists(), "open should remove the stale temp");
        assert_eq!(db.len(), 1);
        let _ = std::fs::remove_file(&path);
    }
}
