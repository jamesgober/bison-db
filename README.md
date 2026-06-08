<h1 align="center">
    <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <br>
    <b>bison-db</b>
    <br>
    <sub><sup>EMBEDDED DOCUMENT DATABASE</sup></sub>
</h1>

<div align="center">
    <a href="https://crates.io/crates/bison-db"><img alt="Crates.io" src="https://img.shields.io/crates/v/bison-db"></a>
    <a href="https://crates.io/crates/bison-db" alt="Download bison-db"><img alt="Crates.io Downloads" src="https://img.shields.io/crates/d/bison-db?color=%230099ff"></a>
    <a href="https://docs.rs/bison-db" title="bison-db Documentation"><img alt="docs.rs" src="https://img.shields.io/docsrs/bison-db"></a>
    <a href="https://github.com/jamesgober/bison-db/actions"><img alt="GitHub CI" src="https://github.com/jamesgober/bison-db/actions/workflows/ci.yml/badge.svg"></a>
    <a href="https://github.com/rust-lang/rfcs/blob/master/text/2495-min-rust-version.md" title="MSRV"><img alt="MSRV" src="https://img.shields.io/badge/MSRV-1.85%2B-blue"></a>
</div>

<br>

<div align="left">
    <p>
        <strong>bison-db</strong> is an <b>embedded, document-oriented database</b> for Rust: store, index, and query <b>schemaless documents</b> entirely in-process, with no server, no network, and no external services. It is the document-store counterpart to an embedded key-value store, giving you rich nested records and secondary indexes linked directly into your binary.
    </p>
    <p>
        It is built for <b>durability</b> and <b>speed</b>: <b>ACID</b> writes through a write-ahead log, <b>single-file storage</b> that is trivial to ship and back up, and <b>secondary indexes</b> for fast queries over document fields. <code>bison-db</code> is the first member of the <b>Bison</b> family of embedded databases.
    </p>
    <br>
    <hr>
    <p>
        <strong>MSRV is 1.85+</strong> (Rust 2024 edition). Schemaless documents. Single-file storage. Crash-safe, embedded, zero-network.
    </p>
    <blockquote>
        <strong>Status: pre-1.0, in active development.</strong> As of <code>v0.4.0</code> the document model, the single-file store, secondary indexes with field and range queries, and a configurable durability policy are all implemented, and the <a href="./docs/FORMAT.md">on-disk format is frozen</a> (version 1). The remaining 0.x work is hardening and benchmarking toward a stable <code>1.0.0</code>, per <a href="./dev/ROADMAP.md"><code>dev/ROADMAP.md</code></a>. The public API is frozen at <code>1.0.0</code>.
    </blockquote>
</div>

<hr>
<br>

<h2>What it does</h2>

Available now (`v0.4.0`):

- **Schemaless documents** &mdash; store nested, JSON-like documents with no fixed schema, built from a small, typed [`Value`](./docs/API.md#value) model
- **Single-file storage** &mdash; the whole database is one file: trivial to ship, copy, and back up
- **Crash-safe writes** &mdash; every record is length-framed and CRC-32C checked; a write torn by a crash is detected and dropped on the next open, never silently misread
- **Configurable durability** &mdash; `fsync` on every write, or batch and sync on `flush`; either way the file is never left corrupt
- **Frozen on-disk format** &mdash; the [format](./docs/FORMAT.md) is stable (version 1); files written by 0.2.0 onward stay readable
- **Embedded, zero-network** &mdash; runs in-process; no server, no daemon, no external services
- **Point operations** &mdash; `insert`, `get`, `update`, and `delete` documents by id, plus `flush` for durability
- **Secondary indexes** &mdash; index any number of document fields; queries also work without an index, so an index is a pure speedup
- **Field and range queries** &mdash; `find` by an exact field value, `range` over an ordered field
- **Optional `serde`** &mdash; move documents in and out of JSON, MessagePack, or any serde format

On the roadmap (`v0.5.0`+, see [`dev/ROADMAP.md`](./dev/ROADMAP.md)):

- **Concurrency** &mdash; safe shared access patterns for multi-threaded workloads
- **Compaction** &mdash; reclaim space held by superseded and deleted records

<br>
<hr>
<br>

## Installation

```toml
[dependencies]
bison-db = "0.4"

# With serde support for the document model:
bison-db = { version = "0.4", features = ["serde"] }
```

<br>

## Quick Start

```rust
use bison_db::{Db, Document};

fn main() -> bison_db::Result<()> {
    // The whole database is a single file.
    let mut db = Db::open("library.bison")?;

    // Schemaless: set whatever fields you like, of mixed types.
    let mut album = Document::new();
    album.set("artist", "Miles Davis").set("title", "Kind of Blue").set("year", 1959_i64);

    // Insert returns a stable id; read, overwrite, and delete by it.
    let id = db.insert(album)?;
    let stored = db.get(id)?.expect("just inserted");
    assert_eq!(stored.get("title").and_then(|v| v.as_str()), Some("Kind of Blue"));

    db.update(id, { let mut d = Document::new(); d.set("title", "So What"); d })?;
    assert!(db.delete(id)?);

    db.flush()?; // make recent writes durable
    Ok(())
}
```

More runnable programs live in [`examples/`](./examples): `quick_start`, `user_profiles` (CRUD with nested documents), `secondary_indexes`, `durability`, `crash_recovery`, and `json_interop`.

```bash
cargo run --example user_profiles
cargo run --example secondary_indexes
cargo run --example durability
cargo run --example crash_recovery
cargo run --example json_interop --features serde
```

<br>

## Querying

Index any number of fields, then query by exact value or by range. Queries work
with or without an index — declaring one only makes them faster.

```rust
use bison_db::{Db, Document, Value};

fn main() -> bison_db::Result<()> {
    let mut db = Db::open("people.bison")?;

    for (name, age) in [("ada", 36_i64), ("grace", 45), ("alan", 29)] {
        let mut d = Document::new();
        d.set("name", name).set("age", age);
        db.insert(d)?;
    }

    // Build indexes — there is no cap on how many fields you index.
    db.create_index("name")?;
    db.create_index("age")?;

    // Equality: who is named "ada"?
    let ada = db.find("name", &Value::from("ada"))?;        // Vec<DocId>

    // Range: everyone aged 30..=44 (results ordered by age).
    let thirties_forties = db.range("age", Value::from(30_i64)..=Value::from(44_i64))?;

    assert_eq!(ada.len(), 1);
    assert_eq!(thirties_forties.len(), 1); // ada (36)
    Ok(())
}
```

<br>

## Durability

Choose how durable writes are when you open the store. The default,
`SyncPolicy::Manual`, is fastest: writes are crash-safe (a torn write is never
misread), but a power loss can lose the most recent writes that were never
flushed. `SyncPolicy::Always` `fsync`s after every write, so each one is durable
the moment it returns.

```rust
use bison_db::{Db, DbOptions, Document, SyncPolicy};

fn main() -> bison_db::Result<()> {
    // Durable per write — every insert/update/delete fsyncs before returning.
    let mut db = Db::open_with("ledger.bison", DbOptions::new().sync(SyncPolicy::Always))?;
    db.insert({ let mut d = Document::new(); d.set("entry", "debit 100"); d })?;
    // No explicit flush needed under Always.
    Ok(())
}
```

Either way the file is never left corrupt: every record is CRC-checked, a crash
torn write at the tail is truncated on the next open, and the [on-disk
format](./docs/FORMAT.md) is frozen and versioned. See [`docs/FORMAT.md`](./docs/FORMAT.md) for the byte-level layout.

<br>

## API Overview

For the complete reference, see [`docs/API.md`](./docs/API.md).

- [`Db`](./docs/API.md#db) / [`DbOptions`](./docs/API.md#dboptions) &mdash; open the store (with a durability policy); `insert` / `get` / `update` / `delete` / `flush`; `create_index` / `find` / `range`
- [`Document`](./docs/API.md#document) &mdash; the ordered, schemaless record you store
- [`Value`](./docs/API.md#value) &mdash; a field's content: null, bool, int, float, string, bytes, array, or nested document
- [`DocId`](./docs/API.md#docid) &mdash; a document's stable primary key
- [`Error`](./docs/API.md#error) &mdash; the closed set of failures an operation can return

<br>

## Performance

`bison-db` is an in-process store: there is no network hop and no client/server serialization. Writes are appended sequentially to one file (the access pattern storage hardware serves fastest), and a read is a single hash-index lookup followed by one positional read. Indicative single-threaded figures from `cargo bench` on a developer laptop (Linux, x86_64, Rust 1.95):

| Operation | Time |
|-----------|------|
| `insert` a small document | ~0.8 µs |
| `get` a small document | ~0.3 µs |
| `update` a small document | ~0.8 µs |
| `find` by indexed field (in a 10k-doc store) | ~60 ns |
| `find` by full scan (in a 10k-doc store) | ~1.6 ms |

The last two rows are the same query with and without an index — the index turns a full scan into a B-tree point lookup. Numbers are produced by [`benches/bison_bench.rs`](./benches/bison_bench.rs) against a real on-disk store; reproduce them with `cargo bench`. A populated head-to-head comparison against other embedded and document stores is planned for the 1.0 cycle.

<br>
<hr>
<br>

## Where It Fits

`bison-db` composes the storage primitives into a document store. It builds on:

- [`wal-db`](https://github.com/jamesgober/wal-db) &mdash; durable write-ahead logging and crash recovery
- [`index-db`](https://github.com/jamesgober/index-db) &mdash; B+tree secondary indexes over document fields
- [`page-db`](https://github.com/jamesgober/page-db) &mdash; fixed-size paged storage substrate
- applications &mdash; any Rust app needing a local document store with no server

It is the first crate in the Bison embedded-database family.

<br>

## Cross-Platform Support

Linux (x86_64, aarch64), macOS (x86_64, Apple Silicon), and Windows (x86_64) are first-class and verified by the CI matrix.

<br>

## Contributing

See [`CONTRIBUTING.md`](./CONTRIBUTING.md) and [`dev/DIRECTIVES.md`](./dev/DIRECTIVES.md). Before a PR: `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all-features` must be clean.

<br>

<div id="license">
    <h2>License</h2>
    <p>Licensed under either of</p>
    <ul>
        <li><b>Apache License, Version 2.0</b> &mdash; <a href="./LICENSE-APACHE">LICENSE-APACHE</a></li>
        <li><b>MIT License</b> &mdash; <a href="./LICENSE-MIT">LICENSE-MIT</a></li>
    </ul>
    <p>at your option.</p>
</div>

<div align="center">
  <h2></h2>
  <sup>COPYRIGHT <small>&copy;</small> 2026 <strong>JAMES GOBER.</strong></sup>
</div>
