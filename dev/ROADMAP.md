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

## v0.5.0 -- concurrency + compaction + API freeze

Exit criteria:
- [ ] Public API frozen (recorded here). `cargo audit` + `cargo deny` clean.

---

## v0.6.0 -> v1.0.0 -- Alpha / Beta / RC / Stable

Integrate against real consumers, broaden testing, capture final benchmarks, then freeze the public API until 2.0 and publish.

---

## Out of scope for 1.0

- Networked/server mode - bison-db is embedded only; a server is a separate product.
- Graph and vector models - those are other Bison / AnimusDB family members.
- Distributed storage / replication - out of scope for the embedded core.
