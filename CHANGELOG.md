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
[Unreleased]: https://github.com/jamesgober/bison-db/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/jamesgober/bison-db/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jamesgober/bison-db/releases/tag/v0.1.0
