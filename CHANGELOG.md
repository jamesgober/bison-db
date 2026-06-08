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
[Unreleased]: https://github.com/jamesgober/bison-db/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/jamesgober/bison-db/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/jamesgober/bison-db/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/jamesgober/bison-db/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jamesgober/bison-db/releases/tag/v0.1.0
