//! # bison-db
//!
//! An embedded, document-oriented database for Rust: store, index, and query
//! schemaless documents entirely in-process, with no server, no network, and no
//! external services. The whole database is one file — trivial to ship, copy,
//! and back up — and every write is appended to a checksummed log, so a crash
//! never leaves a half-written record behind.
//!
//! `bison-db` is the first member of the Bison family of embedded databases.
//!
//! ## The shape of the API
//!
//! Three types carry the everyday workflow:
//!
//! - [`Document`] — an ordered set of named fields, the record you store.
//! - [`Value`] — the content of a field: null, bool, integer, float, string,
//!   bytes, array, or a nested document.
//! - [`Db`] — the single-file store. Insert a document to get a [`DocId`], then
//!   read, overwrite, or delete it by that id.
//!
//! ## Quick start
//!
//! ```
//! # fn main() -> bison_db::Result<()> {
//! use bison_db::{Db, Document};
//!
//! # let path = std::env::temp_dir().join("bison_db_lib_quickstart.bison");
//! # let _ = std::fs::remove_file(&path);
//! // Open (or create) a store backed by a single file.
//! let mut db = Db::open(&path)?;
//!
//! // Build a schemaless document and insert it.
//! let mut song = Document::new();
//! song.set("title", "Take Five").set("year", 1959_i64).set("live", false);
//! let id = db.insert(song)?;
//!
//! // Read it back by id.
//! let stored = db.get(id)?.expect("just inserted");
//! assert_eq!(stored.get("title").and_then(|v| v.as_str()), Some("Take Five"));
//!
//! // Overwrite, delete, and make it durable.
//! db.update(id, { let mut d = Document::new(); d.set("title", "So What"); d })?;
//! assert!(db.delete(id)?);
//! db.flush()?;
//! # let _ = std::fs::remove_file(&path);
//! # Ok(())
//! # }
//! ```
//!
//! ## Feature flags
//!
//! - `std` *(default)* — enables the file-backed [`Db`]. Without it, the crate
//!   is `no_std` and exposes only the in-memory document model ([`Value`],
//!   [`Document`]).
//! - `serde` — derives `serde::Serialize`/`Deserialize` for [`Value`] and
//!   [`Document`], so documents move in and out of any serde data format.
//!
//! ## Durability and recovery
//!
//! Writes are visible to later reads immediately and become durable against
//! power loss after [`Db::flush`]. On open, the log is replayed and verified;
//! a record torn by a crash is detected by its length and CRC-32C checksum and
//! truncated, never silently misread. See [`Db::open`] for the full contract.

#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(missing_docs)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![forbid(unsafe_code)]

extern crate alloc;

mod error;
mod value;

#[cfg(feature = "serde")]
mod serde_support;

// The binary codec, secondary indexes, and file store are the persistence
// layer; they require `std` for filesystem access, so the `no_std` build exposes
// only the document model.
#[cfg(feature = "std")]
mod codec;
#[cfg(feature = "std")]
mod index;
#[cfg(feature = "std")]
mod store;
#[cfg(feature = "std")]
mod sys;

pub use error::{Error, Result};
pub use value::{Document, Value};

#[cfg(feature = "std")]
pub use store::{Db, DbOptions, DocId, MAX_RECORD_BYTES, Stats, SyncPolicy};
