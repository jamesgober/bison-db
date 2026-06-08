# bison-db -- Roadmap

> Path from scaffold to a stable 1.0. Hard parts are front-loaded; each phase has hard exit criteria.
>
> **Anti-deferral rule:** no listed hard task moves to a later phase unless this file records the move and the reason.

---

## v0.1.0 -- Scaffold (DONE)

Compiles, CI green, structure correct, no domain logic.

- [x] Manifest, README, CHANGELOG, REPS, dual license, CI, deny, clippy, rustfmt, FUNDING.
- [x] API surface sketched in `docs/API.md`.

---

## v0.2.0 -- document model + single-file store + insert/get/delete (DONE)

Document model (`Value`/`Document`), versioned CRC-checked log-structured
single-file store, and `insert`/`get`/`update`/`delete`/`flush` with replay-based
crash recovery. Optional `serde` for the document model.

Exit criteria:
- [x] Every public item has rustdoc + a runnable example.
- [x] Core invariants property-tested (lossless round-trip; index matches file
  after reopen).

---

## v0.3.0 -- secondary indexes + field and range queries (DONE)

Ordered secondary indexes over document fields (any number per store), with
equality (`find`) and range (`range`) queries. Both queries also work without an
index via a full scan, so the index is a pure speedup. Indexes are in-memory and
rebuilt per session (`create_index`), keeping the on-disk format unfrozen.

Exit criteria:
- [x] New surface tested (index/scan parity property-tested); hot paths
  benchmarked (indexed point lookup vs full scan).

---

## v0.4.0 -- durability policy + crash recovery + on-disk format freeze (DONE)

bison-db is log-structured, so the data file already provides write-ahead
durability; rather than bolt on a redundant second log, v0.4.0 makes durability
*configurable* (`SyncPolicy::Always` for per-write `fsync`, `Manual` for
flush-controlled), hardens recovery (parent-directory `fsync` on create,
best-effort sync on drop), and **freezes the on-disk format** (version 1, spec in
`docs/FORMAT.md`). Files from 0.2.0 onward stay readable by every later release.

Exit criteria:
- [x] No `todo!`/`unimplemented!`. On-disk format frozen and specified.

---

## v0.5.0 -- concurrency + compaction + API freeze (DONE)

`Db::compact` reclaims the space left by overwrites and deletes via a crash-safe
temp-file-plus-atomic-rename swap (verified on Windows and Linux). Concurrency
follows a single-writer, multi-reader model: `Db` is `Send + Sync` (asserted at
compile time) and shares across threads behind `Arc<RwLock<Db>>` — many readers
or one writer, enforced by `&self`/`&mut self`. A single append-only file has one
tail, so single-writer is inherent and intended, as in an embedded SQL engine.

### Frozen public API (as of v0.5.0)

The surface below is **stable**: additive changes only until 1.0, and no breaking
change before then. The authoritative item-level reference is `docs/API.md`.

- Types: `Db`, `DbOptions`, `SyncPolicy`, `DocId`, `Stats`, `Document`, `Value`,
  `Error`, `Result`, and the constant `MAX_RECORD_BYTES`.
- `Db`: `open`, `open_with`, `insert`, `get`, `update`, `delete`, `contains`,
  `len`, `is_empty`, `ids`, `flush`, `compact`, `path`, `stats`, `sync_policy`,
  `create_index`, `drop_index`, `indexes`, `find`, `range`.
- `DbOptions`: `new`, `sync`, `build_sync_policy`, `open`.
- `Document`: `new`, `with_capacity`, `set`, `get`, `get_mut`, `contains_key`,
  `remove`, `len`, `is_empty`, `clear`, `iter`, `keys`, `values`, plus
  `FromIterator`/`IntoIterator`.
- `Value`: variants and accessors (`is_null`, `as_*`, `type_name`) plus the
  `From` conversions.

Exit criteria:
- [x] Public API frozen (recorded here). `cargo audit` + `cargo deny` clean.

---

## v0.6.0 -- Alpha: hardening (DONE)

API and format are already frozen (v0.4.0 / v0.5.0), so this phase is about
confidence, not features.

- In-tree fuzzing of the parse/recovery paths: property tests proving the decoder
  and `Db::open` never panic on arbitrary, corrupted, or truncated input.
- Captured benchmarks with method and environment recorded in `docs/PERFORMANCE.md`.
- A realistic example (`session_store`) exercising indexed lookups, per-write
  durability, and compaction together.

---

## v0.7.0 -> v1.0.0 -- Beta / RC / Stable

Integrate against real consumers, broaden testing further, capture final
head-to-head benchmarks, then publish 1.0 with the API and format frozen here.

---

## Out of scope for 1.0

- Networked/server mode - bison-db is embedded only; a server is a separate product.
- Graph and vector models - those are other Bison / AnimusDB family members.
- Distributed storage / replication - out of scope for the embedded core.
