# bison-db -- Engineering Directives

> Engineering standards and the definition of done for this project. Read alongside `REPS.md` (root, authoritative) and `dev/ROADMAP.md` (current phase). If anything here conflicts with `REPS.md`, `REPS.md` wins.

---

## 0. Philosophy

This library is built and maintained to a production standard and treated as a flagship piece of work. Plan the full path, then build one verified step at a time. "Good enough" is treated as a defect.

---

## 1. What this is

bison-db is an embedded, document-oriented database for Rust: store, index, and query schemaless documents entirely in-process, with no server, no network, and no external services. It is the document-store counterpart to an embedded key-value store, giving you rich nested records and secondary indexes linked directly into your binary. It is built for durability and speed: ACID writes through a write-ahead log, single-file storage that is trivial to ship and back up, and secondary indexes for fast queries over document fields. `bison-db` is the first member of the Bison family of embedded databases.

---

## 2. Engineering law (non-negotiable)

- **Performance** -- peak is the baseline; borrow over clone; no steady-state hot-path allocation; no "faster" claim without `criterion` numbers.
- **Concurrency** -- correctness under contention is proven with `loom`, not assumed.
- **Correctness** -- the invariants in section 4 are covered by property tests.
- **Security** -- all untrusted input validated; every allocation bounded; library code never panics on hostile input; parse/recovery paths fuzzed.
- **Architecture** -- SOLID, KISS, YAGNI; one responsibility; trait seams are the extension points.
- **Cross-platform** -- Linux/macOS/Windows first-class, verified by CI.
- **Error handling** -- every fallible path returns `Result`; errors are never silently swallowed.
- **Production-ready** -- no commented-out code, no stray `println!`/`dbg!`; every public item has rustdoc with a runnable example.

---

## 3. Definition of done

1. Compiles clean on Linux/macOS/Windows, stable and MSRV.
2. `fmt`, `clippy -D warnings`, `test --all-features`, `cargo doc -D warnings` clean.
3. `cargo audit` + `cargo deny check` pass.
4. No `unwrap`/`expect`/`todo!`/`dbg!` in shipping code; `unsafe` only with `// SAFETY:`.
5. A Tier-1 API exists and headlines the docs.
6. Property tests cover every section-4 invariant; `loom` covers every concurrent path.
7. Hot-path changes carry benchmarks; no regression over 5%.
8. Docs and `CHANGELOG.md` updated.

---

## 4. Project-specific invariants

- Writes are atomic and durable: a crash never leaves a partially-applied document, enforced by WAL replay on open.
- Index contents always agree with document contents - no index entry without its document, property-tested.
- The on-disk format is versioned and CRC-checked; a corrupt or truncated file is detected, never silently misread.

Per-phase exit criteria in `dev/ROADMAP.md` encode these.

---

## 5. Integration points

- `wal-db`: durable write-ahead logging and crash recovery
- `index-db`: B+tree secondary indexes over document fields
- `page-db`: fixed-size paged storage substrate
- applications: any Rust app needing a local document store with no server

<sub>Copyright &copy; 2026 <strong>James Gober</strong>.</sub>
