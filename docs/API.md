<h1 align="center">
        <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <br><b>bison-db</b><br>
    <sub><sup>API REFERENCE</sup></sub>
</h1>
<div align="center">
    <sup>
        <a href="../README.md" title="Project Home"><b>HOME</b></a>
        <span>&nbsp;│&nbsp;</span>
        <span>API</span>
        <span>&nbsp;│&nbsp;</span>
        <a href="../CHANGELOG.md" title="Changelog"><b>CHANGELOG</b></a>
        <span>&nbsp;│&nbsp;</span>
        <a href="../dev/ROADMAP.md" title="Roadmap"><b>ROADMAP</b></a>
    </sup>
</div>
<br>

> Complete reference for every public item in `bison-db` as of `v0.3.0`, with runnable examples.
> The crate is pre-1.0: the surface grows across the 0.x series (see [`dev/ROADMAP.md`](../dev/ROADMAP.md)) and is frozen at `1.0.0`. Items marked _(planned)_ are not yet implemented.

## Table of Contents

- [Installation](#installation)
- [Quick Start](#quick-start)
- [The document model](#the-document-model)
- [Error handling](#error-handling)
- [Public APIs](#public-apis)
  - [`Db`](#db)
  - [Secondary indexes and queries](#secondary-indexes-and-queries)
  - [`DocId`](#docid)
  - [`Stats`](#stats)
  - [`Document`](#document)
  - [`Value`](#value)
  - [`Error` and `Result`](#error)
  - [`MAX_RECORD_BYTES`](#max-record-bytes)
- [Durability and recovery](#durability-and-recovery)
- [Feature flags](#feature-flags)
- [Roadmap surface](#roadmap-surface)

---

## Installation

```toml
[dependencies]
bison-db = "0.3"

# Enable serde for the document model:
bison-db = { version = "0.3", features = ["serde"] }
```

The default `std` feature provides the file-backed [`Db`]. Disabling it
(`default-features = false`) drops the store and leaves a `no_std` build of the
in-memory document model ([`Document`], [`Value`]).

MSRV: Rust 1.85 (2024 edition).

---

## Quick Start

```rust
use bison_db::{Db, Document};

fn main() -> bison_db::Result<()> {
    # let path = std::env::temp_dir().join("bison_db_api_quickstart.bison");
    # let _ = std::fs::remove_file(&path);
    let mut db = Db::open(&path)?;

    let mut doc = Document::new();
    doc.set("title", "Take Five").set("year", 1959_i64);
    let id = db.insert(doc)?;

    let stored = db.get(id)?.expect("just inserted");
    assert_eq!(stored.get("year").and_then(|v| v.as_int()), Some(1959));

    db.flush()?;
    # let _ = std::fs::remove_file(&path);
    Ok(())
}
```

---

## The document model

A **document** is an ordered set of named fields — the unit the store holds.
Each field's content is a **value**: one of null, boolean, signed 64-bit
integer, 64-bit float, UTF-8 string, raw bytes, array, or a nested document.
Documents preserve field insertion order, so an encode/decode round trip
compares equal to the original.

```rust
use bison_db::{Document, Value};

let mut doc = Document::new();
doc.set("name", "ada")                                  // &str   -> Value::Str
   .set("age", 36_i64)                                  // i64    -> Value::Int
   .set("admin", true)                                  // bool   -> Value::Bool
   .set("scores", Value::Array(vec![Value::from(9_i64)])); // explicit array

assert_eq!(doc.get("name").and_then(|v| v.as_str()), Some("ada"));
```

---

## Error handling

Every fallible operation returns [`Result<T>`](#error), an alias for
`Result<T, bison_db::Error>`. [`Error`](#error) is a small, closed enum; match on
it to distinguish a missing file from a corrupt one from an oversized value.

```rust
use bison_db::{Db, Error};

fn main() {
    # let path = std::env::temp_dir().join("bison_db_api_err.bison");
    # std::fs::write(&path, b"not a bison-db file").unwrap();
    match Db::open(&path) {
        Ok(_db) => { /* opened */ }
        Err(Error::BadMagic) => eprintln!("that file is not a bison-db store"),
        Err(Error::UnsupportedVersion(v)) => eprintln!("written by a newer format: {v}"),
        Err(Error::Corrupt(what)) => eprintln!("damaged record: {what}"),
        Err(e) => eprintln!("other failure: {e}"),
    }
    # let _ = std::fs::remove_file(&path);
}
```

No public method panics on invalid input or hostile data; failures are values.

---

## Public APIs

### `Db`

The single-file document store. Open one with [`Db::open`], then operate on
documents by [`DocId`]. Reads take `&self` and writes take `&mut self`, so the
compiler enforces single-writer access; to share a store across threads, place
it behind your own lock.

Requires the `std` feature (on by default).

#### `Db::open`

```rust,ignore
pub fn open<P: AsRef<Path>>(path: P) -> Result<Db>
```

Opens the store at `path`, creating an empty one if the file does not exist, and
replaying any existing records to rebuild the in-memory index.

- **`path`** — the file to open or create. Parent directories must already exist.

On open the whole log is scanned: each record's CRC-32C is verified and the
index is rebuilt from the surviving inserts and deletes. A record left
half-written by a crash — detectable because it runs past the end of the file or
fails its checksum at the tail — is truncated away. A checksum failure on a
record that is **not** at the tail is reported as [`Error::Corrupt`].

**Errors:** [`Error::Io`] (cannot open/read), [`Error::BadMagic`] (not a bison-db
file), [`Error::UnsupportedVersion`] (newer format), [`Error::Corrupt`] (a
non-tail record failed verification).

```rust
use bison_db::Db;

fn main() -> bison_db::Result<()> {
    # let path = std::env::temp_dir().join("bison_db_api_open1.bison");
    # let _ = std::fs::remove_file(&path);
    let db = Db::open(&path)?;       // creates an empty store
    assert!(db.is_empty());
    drop(db);

    let db = Db::open(&path)?;       // reopens and recovers it
    assert_eq!(db.len(), 0);
    # let _ = std::fs::remove_file(&path);
    Ok(())
}
```

#### `Db::insert`

```rust,ignore
pub fn insert(&mut self, doc: Document) -> Result<DocId>
```

Appends `doc` to the log and returns a freshly assigned [`DocId`]. The document
is readable immediately and durable after the next [`flush`](#dbflush).

- **`doc`** — the document to store; consumed by the call.

**Errors:** [`Error::ValueTooLarge`] if the encoded document exceeds
[`MAX_RECORD_BYTES`](#max-record-bytes); [`Error::Io`] if the append fails.

```rust
use bison_db::{Db, Document};

fn main() -> bison_db::Result<()> {
    # let path = std::env::temp_dir().join("bison_db_api_insert.bison");
    # let _ = std::fs::remove_file(&path);
    let mut db = Db::open(&path)?;

    let mut a = Document::new(); a.set("n", 1_i64);
    let mut b = Document::new(); b.set("n", 2_i64);
    let id_a = db.insert(a)?;
    let id_b = db.insert(b)?;
    assert_ne!(id_a, id_b);            // ids are unique and increasing
    assert_eq!(db.len(), 2);
    # let _ = std::fs::remove_file(&path);
    Ok(())
}
```

#### `Db::get`

```rust,ignore
pub fn get(&self, id: DocId) -> Result<Option<Document>>
```

Reads the document stored under `id`, or `Ok(None)` if no live document has that
id (because it was never inserted, or it was deleted).

- **`id`** — the key returned by [`insert`](#dbinsert).

**Errors:** [`Error::Io`] (read failed); [`Error::Corrupt`] (stored bytes failed
to decode — unexpected given the passing checksum).

```rust
use bison_db::{Db, Document, DocId};

fn main() -> bison_db::Result<()> {
    # let path = std::env::temp_dir().join("bison_db_api_get.bison");
    # let _ = std::fs::remove_file(&path);
    let mut db = Db::open(&path)?;
    let mut doc = Document::new(); doc.set("city", "Oslo");
    let id = db.insert(doc)?;

    let found = db.get(id)?.expect("present");
    assert_eq!(found.get("city").and_then(|v| v.as_str()), Some("Oslo"));

    assert!(db.get(DocId::from(99_999))?.is_none());   // absent id
    # let _ = std::fs::remove_file(&path);
    Ok(())
}
```

#### `Db::update`

```rust,ignore
pub fn update(&mut self, id: DocId, doc: Document) -> Result<bool>
```

Overwrites the document under `id` with `doc`. Returns `Ok(true)` if a document
was present to overwrite, `Ok(false)` if `id` is unknown (in which case nothing
is written).

- **`id`** — the document to replace.
- **`doc`** — the new contents.

A successful update appends a new record and repoints the index; the previous
body stays in the file as dead space until compaction.

**Errors:** as [`insert`](#dbinsert).

```rust
use bison_db::{Db, Document, DocId};

fn main() -> bison_db::Result<()> {
    # let path = std::env::temp_dir().join("bison_db_api_update.bison");
    # let _ = std::fs::remove_file(&path);
    let mut db = Db::open(&path)?;
    let mut v1 = Document::new(); v1.set("v", 1_i64);
    let id = db.insert(v1)?;

    let mut v2 = Document::new(); v2.set("v", 2_i64);
    assert!(db.update(id, v2)?);                        // overwrote
    assert_eq!(db.get(id)?.unwrap().get("v").and_then(|v| v.as_int()), Some(2));

    assert!(!db.update(DocId::from(404), Document::new())?); // nothing to update
    # let _ = std::fs::remove_file(&path);
    Ok(())
}
```

#### `Db::delete`

```rust,ignore
pub fn delete(&mut self, id: DocId) -> Result<bool>
```

Deletes the document under `id`, returning `Ok(true)` if one was present and
`Ok(false)` otherwise. A tombstone is appended so the deletion survives
reopening; the document is unreadable as soon as this returns.

- **`id`** — the document to remove.

**Errors:** [`Error::Io`] if the tombstone cannot be appended.

```rust
use bison_db::{Db, Document};

fn main() -> bison_db::Result<()> {
    # let path = std::env::temp_dir().join("bison_db_api_delete.bison");
    # let _ = std::fs::remove_file(&path);
    let mut db = Db::open(&path)?;
    let id = db.insert(Document::new())?;

    assert!(db.delete(id)?);          // removed
    assert!(db.get(id)?.is_none());
    assert!(!db.delete(id)?);         // already gone
    # let _ = std::fs::remove_file(&path);
    Ok(())
}
```

#### Other `Db` methods

| Method | Signature | Description |
|--------|-----------|-------------|
| `contains` | `fn contains(&self, id: DocId) -> bool` | `true` if a live document has this id (in-memory lookup, no file access). |
| `len` | `fn len(&self) -> usize` | Number of live documents. |
| `is_empty` | `fn is_empty(&self) -> bool` | `true` if the store holds no live documents. |
| `ids` | `fn ids(&self) -> impl Iterator<Item = DocId>` | Iterator over all live document ids. Order is unspecified. |
| `flush` | `fn flush(&mut self) -> Result<()>` | `fsync`s the file, making preceding writes durable against power loss. |
| `path` | `fn path(&self) -> &Path` | The path the store was opened from. |
| `stats` | `fn stats(&self) -> Stats` | A [`Stats`](#stats) snapshot of size and live contents. |

```rust
use bison_db::{Db, Document};

fn main() -> bison_db::Result<()> {
    # let path = std::env::temp_dir().join("bison_db_api_misc.bison");
    # let _ = std::fs::remove_file(&path);
    let mut db = Db::open(&path)?;
    let id = db.insert(Document::new())?;
    db.insert(Document::new())?;

    assert!(db.contains(id));
    assert_eq!(db.len(), 2);
    assert!(!db.is_empty());

    let mut all: Vec<u64> = db.ids().map(|i| i.get()).collect();
    all.sort();
    assert_eq!(all, vec![1, 2]);

    db.flush()?;
    assert_eq!(db.path(), path.as_path());
    # let _ = std::fs::remove_file(&path);
    Ok(())
}
```

<br>

<a id="secondary-indexes-and-queries"></a>

### Secondary indexes and queries

`find` and `range` answer queries over document fields. They work whether or not
a field is indexed: with an index they are a B-tree lookup, without one they fall
back to scanning every live document. So an index never changes a result — only
its speed. You may index **any number of fields**; call `create_index` once per
field. Indexes live in memory and are rebuilt per session (they are not in the
file), so re-declare them after reopening a store.

Both query methods compare values with a single total order over [`Value`]
(`null < bool < int < float < string < bytes < array < object`, then natural
order within a variant, floats via `f64::total_cmp`). One consequence: integers
and floats sort in separate bands, so index a numeric field with a consistent
type.

#### `Db::create_index`

```rust,ignore
pub fn create_index(&mut self, field: &str) -> Result<()>
```

Builds an ordered index over `field` by reading every live document once;
documents without the field are skipped. The index is then maintained
automatically on every insert, update, and delete. Idempotent — indexing an
already-indexed field is a no-op.

- **`field`** — the field name to index.

**Errors:** [`Error::Io`] / [`Error::Corrupt`] if a document cannot be read while
building the index.

#### `Db::find`

```rust,ignore
pub fn find(&self, field: &str, value: &Value) -> Result<Vec<DocId>>
```

Returns the ids of all live documents whose `field` equals `value`.

- **`field`** — the field to match on.
- **`value`** — the exact value to match.

**Errors:** [`Error::Io`] / [`Error::Corrupt`] on the unindexed (scan) path if a
document cannot be read.

```rust
use bison_db::{Db, Document, Value};

fn main() -> bison_db::Result<()> {
    # let path = std::env::temp_dir().join("bison_db_api_find2.bison");
    # let _ = std::fs::remove_file(&path);
    let mut db = Db::open(&path)?;
    db.insert({ let mut d = Document::new(); d.set("role", "admin"); d })?;
    db.insert({ let mut d = Document::new(); d.set("role", "user"); d })?;

    // Works before indexing (full scan)…
    assert_eq!(db.find("role", &Value::from("admin"))?.len(), 1);
    // …and faster after (point lookup), with the same result.
    db.create_index("role")?;
    assert_eq!(db.find("role", &Value::from("admin"))?.len(), 1);
    # let _ = std::fs::remove_file(&path);
    Ok(())
}
```

#### `Db::range`

```rust,ignore
pub fn range<R: RangeBounds<Value>>(&self, field: &str, range: R) -> Result<Vec<DocId>>
```

Returns the ids of all live documents whose `field` falls within `range`. Any
[`RangeBounds`](https://doc.rust-lang.org/std/ops/trait.RangeBounds.html) form
works: `a..b`, `a..=b`, `..b`, `a..`, `..`. When the field is indexed, matches
come back ordered by field value (then id).

- **`field`** — the field to range over.
- **`range`** — inclusive/exclusive bounds as [`Value`]s.

**Errors:** as `find`.

```rust
use bison_db::{Db, Document, Value};

fn main() -> bison_db::Result<()> {
    # let path = std::env::temp_dir().join("bison_db_api_range2.bison");
    # let _ = std::fs::remove_file(&path);
    let mut db = Db::open(&path)?;
    for age in [17_i64, 25, 40, 70] {
        db.insert({ let mut d = Document::new(); d.set("age", age); d })?;
    }
    db.create_index("age")?;

    let working_age = db.range("age", Value::from(18_i64)..=Value::from(65_i64))?;
    assert_eq!(working_age.len(), 2);            // 25 and 40
    let over_60 = db.range("age", Value::from(60_i64)..)?;
    assert_eq!(over_60.len(), 1);                // 70
    # let _ = std::fs::remove_file(&path);
    Ok(())
}
```

#### `Db::drop_index` and `Db::indexes`

| Method | Signature | Description |
|--------|-----------|-------------|
| `drop_index` | `fn drop_index(&mut self, field: &str) -> bool` | Removes a field's index; `true` if one existed. |
| `indexes` | `fn indexes(&self) -> impl Iterator<Item = &str>` | Names of the currently indexed fields (order unspecified). |

```rust
use bison_db::Db;

fn main() -> bison_db::Result<()> {
    # let path = std::env::temp_dir().join("bison_db_api_dropidx.bison");
    # let _ = std::fs::remove_file(&path);
    let mut db = Db::open(&path)?;
    db.create_index("a")?;
    db.create_index("b")?;
    assert_eq!(db.indexes().count(), 2);
    assert!(db.drop_index("a"));
    assert!(!db.drop_index("a"));
    # let _ = std::fs::remove_file(&path);
    Ok(())
}
```

<br>

### `DocId`

A document's primary key within a [`Db`].

```rust,ignore
pub struct DocId(/* private */);
```

Ids are assigned by [`Db::insert`] as a dense, monotonically increasing sequence
starting at `1`; `0` is never assigned and can serve as a sentinel. An id is
stable for the life of the document and survives reopening the file. `DocId` is
`Copy`, `Ord`, and `Hash`, so it works as a map key or in a sorted collection.

| Item | Signature | Description |
|------|-----------|-------------|
| `get` | `const fn get(self) -> u64` | The underlying integer. |
| `From<u64>` | `DocId::from(raw)` | Reconstruct an id you stored elsewhere. |
| `From<DocId> for u64` | `u64::from(id)` / `id.into()` | Extract the integer. |
| `Display` | `id.to_string()` | Renders the integer. |

```rust
use bison_db::DocId;

let id = DocId::from(7);
assert_eq!(id.get(), 7);
assert_eq!(u64::from(id), 7);
assert_eq!(id.to_string(), "7");
```

<br>

### `Stats`

A point-in-time summary returned by [`Db::stats`].

```rust,ignore
pub struct Stats {
    pub live_documents: usize, // documents currently readable
    pub file_bytes: u64,       // total size of the file on disk
    pub live_bytes: u64,       // bytes held by live document bodies (no framing)
}
```

The gap between `file_bytes` and `live_bytes` (plus per-record framing) is space
held by superseded and deleted records — the slack a future compaction step will
reclaim.

```rust
use bison_db::{Db, Document};

fn main() -> bison_db::Result<()> {
    # let path = std::env::temp_dir().join("bison_db_api_stats.bison");
    # let _ = std::fs::remove_file(&path);
    let mut db = Db::open(&path)?;
    let id = db.insert(Document::new())?;
    db.insert(Document::new())?;
    db.delete(id)?;

    let s = db.stats();
    assert_eq!(s.live_documents, 1);    // two inserted, one deleted
    assert!(s.file_bytes > 0);
    # let _ = std::fs::remove_file(&path);
    Ok(())
}
```

<br>

### `Document`

An ordered collection of named fields — the record a store holds. Lookups are a
linear scan, which is the fastest strategy for the small field counts typical of
documents.

| Method | Signature | Description |
|--------|-----------|-------------|
| `new` | `const fn new() -> Document` | An empty document. |
| `with_capacity` | `fn with_capacity(n: usize) -> Document` | Empty, with room for `n` fields before reallocating. |
| `set` | `fn set<K: Into<String>, V: Into<Value>>(&mut self, k, v) -> &mut Self` | Sets a field, replacing in place if the key exists; returns `&mut self` for chaining. |
| `get` | `fn get(&self, key: &str) -> Option<&Value>` | The value for `key`, if present. |
| `get_mut` | `fn get_mut(&mut self, key: &str) -> Option<&mut Value>` | Mutable access to a field's value. |
| `contains_key` | `fn contains_key(&self, key: &str) -> bool` | Whether `key` is present. |
| `remove` | `fn remove(&mut self, key: &str) -> Option<Value>` | Removes and returns a field; order of the rest is kept. |
| `len` / `is_empty` | `fn len(&self) -> usize` / `fn is_empty(&self) -> bool` | Field count / emptiness. |
| `clear` | `fn clear(&mut self)` | Removes all fields, keeping capacity. |
| `iter` | `fn iter(&self) -> impl Iterator<Item = (&str, &Value)>` | Fields in order. |
| `keys` / `values` | iterators | Field keys / values in order. |

`Document` also implements `FromIterator<(K, V)>` and `IntoIterator` for
`&Document`.

**Building and reading:**

```rust
use bison_db::{Document, Value};

let mut doc = Document::new();
doc.set("a", 1_i64).set("b", "two").set("a", 3_i64); // "a" replaced in place

assert_eq!(doc.keys().collect::<Vec<_>>(), ["a", "b"]);
assert_eq!(doc.get("a").and_then(Value::as_int), Some(3));
assert!(doc.contains_key("b"));
assert_eq!(doc.len(), 2);
```

**Mutating a field in place:**

```rust
use bison_db::{Document, Value};

let mut doc = Document::new();
doc.set("count", 41_i64);
if let Some(Value::Int(n)) = doc.get_mut("count") {
    *n += 1;
}
assert_eq!(doc.get("count").and_then(Value::as_int), Some(42));
```

**From an iterator of pairs:**

```rust
use bison_db::Document;

let doc: Document = [("x", 1_i64), ("y", 2_i64)].into_iter().collect();
assert_eq!(doc.len(), 2);
```

<br>

### `Value`

The content of a document field.

```rust,ignore
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Array(Vec<Value>),
    Object(Document),
}
```

**Construction.** `Value` implements `From` for the common Rust types, so
`Document::set` accepts them directly:

| From | Produces |
|------|----------|
| `bool` | `Value::Bool` |
| `i32`, `i64`, `u32` | `Value::Int` |
| `f64` | `Value::Float` |
| `&str`, `String` | `Value::Str` |
| `Vec<u8>` | `Value::Bytes` |
| `Vec<Value>` | `Value::Array` |
| `Document` | `Value::Object` |
| `Option<T>` where `T: Into<Value>` | the inner value, or `Value::Null` for `None` |

**Inspection.** Each accessor returns `Some` only for the matching variant; no
coercion is performed (an `Int` is not read as a `Float`).

| Method | Returns |
|--------|---------|
| `is_null` | `bool` |
| `as_bool` | `Option<bool>` |
| `as_int` | `Option<i64>` |
| `as_float` | `Option<f64>` |
| `as_str` | `Option<&str>` |
| `as_bytes` | `Option<&[u8]>` |
| `as_array` | `Option<&[Value]>` |
| `as_object` | `Option<&Document>` |
| `type_name` | `&'static str` (for diagnostics) |

```rust
use bison_db::Value;

assert_eq!(Value::from("bison").as_str(), Some("bison"));
assert_eq!(Value::from(42_i64).as_int(), Some(42));
assert_eq!(Value::from(42_i64).as_float(), None);   // no coercion
assert!(Value::Null.is_null());
assert_eq!(Value::from(1_i64).type_name(), "int");

let from_none = Value::from(None::<i64>);
assert!(from_none.is_null());
```

**Nested values:**

```rust
use bison_db::{Document, Value};

let mut address = Document::new();
address.set("city", "Kyoto");

let mut user = Document::new();
user.set("name", "tomoko")
    .set("tags", Value::Array(vec![Value::from("a"), Value::from("b")]))
    .set("address", Value::Object(address));

let city = user.get("address")
    .and_then(Value::as_object)
    .and_then(|a| a.get("city"))
    .and_then(Value::as_str);
assert_eq!(city, Some("Kyoto"));
```

**With `serde`** (feature `serde`), both `Value` and `Document` implement
`Serialize`/`Deserialize`, mapping onto the serde data model like a dynamic JSON
value:

```rust,ignore
let doc: bison_db::Document = serde_json::from_str(r#"{ "n": 1, "ok": true }"#)?;
let json = serde_json::to_string(&doc)?;
```

<br>

<a id="error"></a>

### `Error` and `Result`

```rust,ignore
pub type Result<T> = core::result::Result<T, Error>;

#[non_exhaustive]
pub enum Error {
    Io(std::io::Error),          // (std feature) an underlying filesystem op failed
    BadMagic,                    // the file is not a bison-db store
    UnsupportedVersion(u16),     // the file's format is newer than this build
    Corrupt(&'static str),       // a record failed its checksum or structure check
    ValueTooLarge,               // a value exceeded MAX_RECORD_BYTES
}
```

`Error` is `#[non_exhaustive]`; always include a catch-all arm when matching.
`Error::Io` carries the originating `std::io::Error`, reachable through the
standard `source()` chain. A `Corrupt` error always means the bytes on disk did
not match what the writer produced — never that an in-memory argument was wrong.
A clean torn write at the very end of the log is recovered silently, not
reported as `Corrupt` (see [Durability and recovery](#durability-and-recovery)).

```rust
use bison_db::Error;

let err = Error::UnsupportedVersion(9);
assert!(err.to_string().contains('9'));
```

<br>

<a id="max-record-bytes"></a>

### `MAX_RECORD_BYTES`

```rust,ignore
pub const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024; // 64 MiB
```

The largest record payload the store will write or accept while reading. A
document encoding to more than this is rejected on write with
[`Error::ValueTooLarge`]; on read, any framed length above the cap is treated as
corruption, which bounds the allocation the recovery path can be asked to make
from a damaged file. Requires the `std` feature.

```rust
assert_eq!(bison_db::MAX_RECORD_BYTES, 64 * 1024 * 1024);
```

---

## Durability and recovery

- **Visibility.** A write is visible to later reads in the same process as soon
  as the call returns.
- **Durability.** A write is durable against power loss only after
  [`Db::flush`] returns (it issues an `fsync`). A crash before `flush` may lose
  the most recent unsynced writes.
- **No corruption.** A crash never tears a record that was already durable.
  Every record is length-framed and CRC-32C checked. On open, the log is
  replayed: a partial record at the tail (short read or failing checksum at the
  end of the file) is truncated; a checksum failure earlier in the file is
  surfaced as [`Error::Corrupt`] rather than silently misread.
- **Versioned format.** The file header carries a format version; a file written
  by a newer release is refused with [`Error::UnsupportedVersion`] instead of
  being misinterpreted.

Full write-ahead-log durability, group commit, and a frozen on-disk format are
scheduled for `v0.4.0`.

---

## Feature flags

| Feature | Default | Description |
|---------|---------|-------------|
| `std` | yes | Enables the file-backed [`Db`], [`DocId`], [`Stats`], and [`MAX_RECORD_BYTES`]. Without it, the crate is `no_std` (with `alloc`) and exposes only the document model. |
| `serde` | no | Derives `serde::Serialize`/`Deserialize` for [`Value`] and [`Document`]. |

---

## Roadmap surface

These are **not yet implemented** and are listed so integrators can see the
intended shape. Track them in [`dev/ROADMAP.md`](../dev/ROADMAP.md).

- **Write-ahead log** _(planned, v0.4.0)_ — group-commit durability and a frozen
  on-disk format.
- **Persistent / lazily-rebuilt indexes** _(planned, v0.4.0+)_ — avoid
  re-declaring indexes after reopening a store.
- **Compaction** _(planned, v0.5.0)_ — reclaim space held by superseded and
  deleted records.

---

<sub>Copyright &copy; 2026 <strong>James Gober</strong>.</sub>
