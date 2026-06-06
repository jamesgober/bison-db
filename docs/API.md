# bison-db &mdash; API Reference

> Complete reference for every public item in `bison-db`, with examples.
> **Status: pre-1.0.** Sections below describe the intended surface as it lands across the 0.x series (see [`dev/ROADMAP.md`](../dev/ROADMAP.md)).

## Table of Contents

- [Overview](#overview)
- [Schemaless documents](#schemaless-documents) _(planned)_
- [Secondary indexes](#secondary-indexes) _(planned)_
- [ACID writes](#acid-writes) _(planned)_
- [Single-file storage](#single-file-storage) _(planned)_
- [Embedded, zero-network](#embedded,-zero-network) _(planned)_
- [Query API](#query-api) _(planned)_
- [Feature flags](#feature-flags)

---

## Overview

bison-db is an embedded, document-oriented database for Rust: store, index, and query schemaless documents entirely in-process, with no server, no network, and no external services. It is the document-store counterpart to an embedded key-value store, giving you rich nested records and secondary indexes linked directly into your binary.

---

### Schemaless documents

_store nested, JSON-like documents with no fixed schema. Documented as this lands across the 0.x series._

### Secondary indexes

_index document fields for fast equality and range queries. Documented as this lands across the 0.x series._

### ACID writes

_atomic, durable writes via a write-ahead log with crash recovery. Documented as this lands across the 0.x series._

### Single-file storage

_the whole database is one file: trivial to ship, copy, and back up. Documented as this lands across the 0.x series._

### Embedded, zero-network

_runs in-process; no server, no daemon, no external services. Documented as this lands across the 0.x series._

### Query API

_fetch by id, by field predicate, or by range over a secondary index. Documented as this lands across the 0.x series._

---

## Feature flags

| Feature | Default | Description |
|---------|---------|-------------|
| `std` | yes | Standard library. |
| `serde` | no | Serialization for public types. |

---

<sub>Copyright &copy; 2026 <strong>James Gober</strong>.</sub>
