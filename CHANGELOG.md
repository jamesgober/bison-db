<h1 align="center">
    <img width="90px" height="auto" src="https://raw.githubusercontent.com/jamesgober/jamesgober/main/media/icons/hexagon-3.svg" alt="Triple Hexagon">
    <br>
    <b>CHANGELOG</b>
</h1>
<p>
  All notable changes to <code>bison-db</code> will be documented in this file. The format is based on <a href="https://keepachangelog.com/en/1.1.0/">Keep a Changelog</a>,
  and this project adheres to <a href="https://semver.org/spec/v2.0.0.html/">Semantic Versioning</a>.
</p>

## [Unreleased]

## [1.0.0] - 2026-06-08

The stable release. The public API and the on-disk format (version 1) are now a
**stability commitment**: no breaking change until 2.0, per semantic versioning.
Files written by 0.2.0 onward remain readable. No code changed from 0.9.0 — this
release declares the surface stable.

### Summary of the 1.0 surface

- **Document model** — `Value` (null, bool, i64, f64, string, bytes, array,
  nested object) and an insertion-ordered `Document`.
- **Single-file store** — `Db` with `open`/`insert`/`get`/`update`/`delete`/
  `flush`/`compact`, log-structured and crash-safe (CRC-32C framing,
  torn-tail recovery).
- **Secondary indexes** — `create_index`/`find`/`range` over any number of
  fields; queries also work without an index.
- **Durability** — `SyncPolicy` (`Always`/`Manual`) via `DbOptions`.
- **Compaction** — `Db::compact` reclaims dead space via a crash-safe atomic swap.
- **Concurrency** — single-writer, multi-reader; `Db: Send + Sync`, shared behind
  `Arc<RwLock<Db>>`.
- **Optional `serde`** and a `no_std` document-model build.

### Stability guarantee

- The public API will not change incompatibly before 2.0.
- The on-disk format is version 1 and frozen; a future incompatible format would
  bump both the format version and the crate's major version.
- Future work (read cache, persistent indexes) is additive and does not alter the
  1.0 surface.

## [0.9.0] - 2026-06-08

Final soak before 1.0: concurrency under sustained load. The API and on-disk
format remain frozen and unchanged.

### Added

- `tests/concurrency.rs`: a sustained-load soak test. Four reader threads
  continuously read and run indexed queries against a shared `Arc<RwLock<Db>>`
  while a writer performs 3,000 mutations (insert, update, delete, and
  compaction). Reads never panic as the store changes under them, and the store
  matches the writer's reference model exactly once the writer finishes — proving
  the single-writer, multi-reader model holds under contention, including across
  compaction.

## [0.8.0] - 2026-06-08

Release candidate: a controlled head-to-head benchmark against a peer engine. No
code changes to the library — the API and on-disk format remain frozen.

### Added

- `benchmarks/`: a standalone, package-detached crate that benchmarks bison-db
  head-to-head against `redb` (a pure-Rust ACID embedded key/value store) on a
  matched workload. Because it is its own workspace, redb never enters bison-db's
  dependency tree, `cargo audit`/`cargo deny` surface, or CI; the main package
  excludes it from publication.
- `docs/PERFORMANCE.md`: a measured "Head-to-head: bison-db vs redb" section
  reporting the result (100k records, matched durability) — bison-db ~1.85×
  faster on bulk insert and ~35% smaller on disk; redb ~1.3× faster on point
  reads — with method, environment, and an honest read of the split. The prior
  "deferred to the RC" note is replaced with these numbers.

## [0.7.0] - 2026-06-08

Beta soak testing. More confidence on top of the frozen API and format: a
randomized stress test, error-path coverage, and a written competitive analysis.

### Added

- `tests/stress.rs`: a seeded stress/soak test driving thousands of randomized
  mixed operations (insert, update, delete, read-back, compaction, and
  close-and-reopen) against an in-memory reference model, verifying the store —
  and its secondary index — match the model throughout.
- Error-path tests: an oversized value is rejected with `ValueTooLarge` and
  leaves the store unchanged (no consumed id); compaction preserves the store's
  `SyncPolicy`.
- `docs/PERFORMANCE.md`: a "How it compares" section laying out the architectural
  case against networked document databases, and a "Correctness under load"
  note documenting the soak test.

### Changed

- Dropped the `html_reports` feature from the `criterion` dev-dependency, which
  removed the `plotters` -> `web-sys` chain (~19 fewer crates resolved). This
  shrinks the dev dependency tree and the `cargo audit`/`cargo deny` surface, and
  makes CI lockfile resolution faster and less prone to transient download
  failures. Benchmarks report to stdout, unchanged.

## [0.6.0] - 2026-06-08

Alpha hardening. The API and on-disk format were frozen in 0.4.0/0.5.0, so this
release adds confidence rather than features: fuzz-tested parse/recovery paths,
captured benchmarks, and a realistic example.

### Added

- In-tree fuzzing of the parse path: property tests proving the document decoder
  and `Db::open` never panic, over-read, or hang on arbitrary, corrupted, or
  truncated input (`tests/robustness.rs` and codec property tests).
- `docs/PERFORMANCE.md`: benchmark results with the method, environment, and the
  durability cost, all reproducible with `cargo bench`.
- `session_store` example: an embedded web-session store using an indexed
  `user_id` for "log out everywhere", per-write durability, and compaction.

### Changed

- Refreshed the README performance table with measured medians and linked the
  full performance write-up.

## [0.5.0] - 2026-06-08

Space-reclaiming compaction, a defined concurrency model, and the **public API
freeze**. From here, changes are additive only until 1.0.

### Added

- `Db::compact`: rewrites the file with one record per live document, reclaiming
  the space left by overwrites and deletes. The compacted copy is built in a
  sibling temporary file and swapped in with an atomic rename, so a crash leaves
  either the original or the fully compacted file — never a partial result.
  Document ids and secondary indexes are preserved. Verified on Windows and Linux.
- Interrupted-compaction recovery: a leftover `.compacting` temporary is removed
  on `open`.
- Compile-time `Db: Send + Sync` assertion, a concurrency integration test using
  `Arc<RwLock<Db>>`, and a `compaction` example.

### Changed

- **Public API frozen** (recorded in `dev/ROADMAP.md`): additive-only changes
  until 1.0, no breaking change before then.
- Documented the single-writer, multi-reader concurrency model across the crate
  docs, README, and `docs/API.md`.

## [0.4.0] - 2026-06-08

Durability you can tune, recovery hardening, and a frozen on-disk format. The
store is log-structured, so the data file already provides write-ahead
durability; rather than add a redundant second log, this release makes durability
*configurable* and declares the file layout stable.

### Added

- `SyncPolicy` (`Always` / `Manual`) and `DbOptions`, a small builder for opening
  a store with a chosen durability policy.
- `Db::open_with` and `Db::sync_policy` to open with options and read the active
  policy. `Db::open` is unchanged and uses `SyncPolicy::Manual`.
- `SyncPolicy::Always` issues an `fsync` after every write, so each insert,
  update, and delete is durable the moment it returns.
- Best-effort `fsync` on `Db` drop under `Manual`, so a clean shutdown does not
  lose unflushed writes.
- Parent-directory `fsync` on file creation (Unix), making a new store's
  existence durable; a documented no-op on Windows, where file-level `fsync`
  already persists the entry.
- `docs/FORMAT.md`: the byte-level on-disk format specification.
- `durability` example and a Criterion benchmark comparing `Manual` and `Always`
  insert costs.

### Changed

- **On-disk format frozen at version 1.** The layout in `docs/FORMAT.md` is
  stable; files written by 0.2.0 onward remain readable by every later release.
- Documented the durability contract in terms of `SyncPolicy` across the crate
  docs.

## [0.3.0] - 2026-06-07

Secondary indexes and field queries. Index any number of document fields, then
query by exact value or by range. Queries also work without an index via a full
scan, so an index is a pure speedup, never a correctness requirement.

### Added

- `Db::create_index` / `Db::drop_index` / `Db::indexes` — declare, remove, and
  list ordered secondary indexes over document fields. Any number of fields may
  be indexed.
- `Db::find` — return the ids of documents whose field equals a given value.
- `Db::range` — return the ids of documents whose field falls within a
  `RangeBounds<Value>` (`a..b`, `a..=b`, `..b`, `a..`, `..`); indexed results are
  ordered by field value.
- A total order over `Value` (`null < bool < int < float < string < bytes <
  array < object`, floats via `f64::total_cmp`) backing both index ordering and
  query equality, so the indexed and scan paths always agree.
- `secondary_indexes` example and a Criterion benchmark comparing an indexed
  point lookup against the full-scan fallback.
- Property test asserting indexed `find`/`range` return the same set as the
  equivalent scan.

### Changed

- Indexes are maintained automatically on every insert, update, and delete.
  `update` and `delete` now read the previous document when indexes exist, to
  keep index entries in step with document contents.
- Indexes are in-memory and rebuilt per session (`create_index`); they are not
  part of the on-disk format, which stays unfrozen until v0.4.0.

## [0.2.0] - 2026-06-06

The first working release: the document model and a durable single-file store.
Documents are inserted, read, overwritten, and deleted by id; every record is
checksummed, and a crash-torn write is detected and dropped on the next open.

### Added

- Document model: `Value` (null, bool, int, float, string, bytes, array, nested
  object) and `Document`, an insertion-ordered field map with `set`/`get`/
  `get_mut`/`remove`/`contains_key`/`iter`/`keys`/`values` and `From`/
  `FromIterator` conversions.
- `Db`: a single-file, append-only document store with `open`, `insert`, `get`,
  `update`, `delete`, `contains`, `len`, `is_empty`, `ids`, `flush`, `path`, and
  `stats`.
- `DocId` primary key (monotonic, stable across reopen) and a `Stats` snapshot
  type (`live_documents`, `file_bytes`, `live_bytes`).
- Versioned, CRC-32C-framed on-disk log with replay-based crash recovery:
  torn-tail truncation on open and `Error::Corrupt` for in-place damage.
- `Error`/`Result`: a closed error enum (`Io`, `BadMagic`, `UnsupportedVersion`,
  `Corrupt`, `ValueTooLarge`).
- `MAX_RECORD_BYTES` cap bounding per-record allocation on the recovery path.
- Working `serde` feature: `Serialize`/`Deserialize` for `Value` and `Document`.
- Cross-platform positional file I/O (Unix `pread`/`pwrite`, Windows
  `seek_read`/`seek_write`), verified on Linux and Windows.
- Property tests (lossless round-trip; index matches file after reopen),
  integration tests, four runnable examples, and Criterion benchmarks for the
  insert/get/update hot paths.

### Changed

- `no_std` builds (default features off) now expose the in-memory document model
  only; the file store requires `std`.

## [0.1.0] - 2026-06-05

Initial scaffold and repository bootstrap. No domain logic yet &mdash; this release establishes the structure, tooling, and quality gates the implementation will be built on.

### Added

- `Cargo.toml` with crate metadata, Rust 2024 edition, MSRV 1.85, dual `Apache-2.0 OR MIT` license.
- `README.md`, `docs/API.md`, `CONTRIBUTING.md`, and a documentation skeleton.
- `dev/DIRECTIVES.md` and `dev/ROADMAP.md` (committed engineering standards + plan).
- `REPS.md` compliance baseline; `deny.toml`, `clippy.toml`, `rustfmt.toml`.
- `.github/workflows/ci.yml` (Node 24 actions; fmt, clippy, test, doc, audit, deny) and `.github/FUNDING.yml`.

<!-- LINKS -->
[Unreleased]: https://github.com/jamesgober/bison-db/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/jamesgober/bison-db/compare/v0.9.0...v1.0.0
[0.9.0]: https://github.com/jamesgober/bison-db/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/jamesgober/bison-db/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/jamesgober/bison-db/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/jamesgober/bison-db/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/jamesgober/bison-db/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/jamesgober/bison-db/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/jamesgober/bison-db/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/jamesgober/bison-db/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jamesgober/bison-db/releases/tag/v0.1.0
