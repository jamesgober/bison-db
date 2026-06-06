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
        <strong>MSRV is 1.85+</strong> (Rust 2024 edition). Schemaless documents. Secondary indexes. ACID, single-file, embedded.
    </p>
    <blockquote>
        <strong>Status: pre-1.0, in active development.</strong> This is the <code>v0.1.0</code> scaffold &mdash; structure, tooling, and CI gates are in place; the implementation lands across the 0.x series per <a href="./dev/ROADMAP.md"><code>dev/ROADMAP.md</code></a>. The public API is frozen at <code>1.0.0</code>.
    </blockquote>
</div>

<hr>
<br>

<h2>What it does</h2>

- **Schemaless documents** &mdash; store nested, JSON-like documents with no fixed schema
- **Secondary indexes** &mdash; index document fields for fast equality and range queries
- **ACID writes** &mdash; atomic, durable writes via a write-ahead log with crash recovery
- **Single-file storage** &mdash; the whole database is one file: trivial to ship, copy, and back up
- **Embedded, zero-network** &mdash; runs in-process; no server, no daemon, no external services
- **Query API** &mdash; fetch by id, by field predicate, or by range over a secondary index

<br>
<hr>
<br>

## Installation

```toml
[dependencies]
bison-db = "0.1"
```

<br>

## API Overview

For the complete reference, see [`docs/API.md`](./docs/API.md).

- [`Schemaless documents`](./docs/API.md)
- [`Secondary indexes`](./docs/API.md)
- [`ACID writes`](./docs/API.md)
- [`Single-file storage`](./docs/API.md)

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
