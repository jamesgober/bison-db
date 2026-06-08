<h1 align="center">
        <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <br><b>bison-db</b><br>
    <sub><sup>PERFORMANCE</sup></sub>
</h1>
<div align="center">
    <sup>
        <a href="../README.md" title="Project Home"><b>HOME</b></a>
        <span>&nbsp;│&nbsp;</span>
        <a href="./API.md" title="API Reference"><b>API</b></a>
        <span>&nbsp;│&nbsp;</span>
        <a href="./FORMAT.md" title="On-disk Format"><b>FORMAT</b></a>
        <span>&nbsp;│&nbsp;</span>
        <span>PERFORMANCE</span>
    </sup>
</div>
<br>

> Measured throughput of the core operations, the method behind the numbers, and
> the design decisions that produce them. Every figure here is reproducible with
> `cargo bench`.

## Method

All benchmarks run against a **real on-disk store** in the system temp directory,
not an in-memory stand-in: each measured operation includes document encoding,
record framing, the CRC-32C, and the actual read/write syscalls. They are driven
by [`criterion`](https://github.com/bheisler/criterion.rs) from
[`benches/bison_bench.rs`](../benches/bison_bench.rs).

The "small document" used throughout is five typed fields (an integer id, a
string, a bool, a float, and a two-element array) — a realistic record, not a
single scalar.

Reproduce locally:

```bash
cargo bench                       # all groups
cargo bench --bench bison_bench -- find    # just the find comparison
```

## Environment

The numbers below were captured on:

- **OS:** Linux (WSL2, Ubuntu) on Windows 11
- **CPU:** x86_64
- **Storage:** ext4 on an SSD
- **Toolchain:** Rust stable 1.95, release profile (`lto = "fat"`, `codegen-units = 1`)

Treat them as **indicative** — absolute timings vary with hardware, especially
`fsync` latency, which is a property of the device, not of bison-db. The
*relationships* between operations (indexed vs scan, manual vs always) are the
durable takeaway.

## Results

Single-threaded, median of 100 criterion samples.

| Operation | Median | Notes |
|-----------|-------:|-------|
| `insert` (small doc, `Manual`) | **~0.9 µs** | encode + frame + CRC + append (no per-op fsync) |
| `get` (small doc) | **~0.36 µs** | one index lookup + one positional read + decode |
| `update` (small doc) | **~0.61 µs** | append a new version, repoint the index |
| `find` by **indexed** field (10k docs) | **~65 ns** | B-tree point lookup |
| `find` by **full scan** (10k docs) | **~1.8 ms** | read + compare every live document |
| `insert` (`Manual`) | **~0.88 µs** | sync deferred to `flush` |
| `insert` (`Always`) | **~1.44 ms** | one `fsync` per write — device-bound |

### What the numbers say

- **Reads are ~0.36 µs.** A point read is a hash-index lookup followed by a
  single positional read and a decode — no scan, no lock on the hot path.
- **An index is worth ~27,000×.** On a 10k-document store, the same equality
  query is ~65 ns indexed versus ~1.8 ms scanning. The index turns an O(n) read
  of the whole store into an O(log n) B-tree lookup. Declaring one is the single
  highest-leverage tuning step.
- **Durability has a price, and it is the disk's.** `SyncPolicy::Always` is
  ~1.4 ms per insert here versus ~0.9 µs for `Manual` — roughly three orders of
  magnitude, all of it the `fsync`. Batch writes under `Manual` and call `flush`
  once for throughput; use `Always` when each record must be durable on return.

## Why it is fast

These are not micro-optimizations bolted on at the end; they fall out of the
architecture:

- **Embedded, in-process.** There is no network hop and no client/server
  serialization. A call is a function call.
- **Log-structured writes.** Every write is a sequential append to one file —
  the access pattern SSDs and disks serve fastest — with no in-place updates and
  no read-modify-write.
- **O(1) primary lookups.** An in-memory hash index maps each id to a byte
  offset, so a read is one lookup plus one positional read; reads take `&self`
  and never touch a lock on the hot path.
- **Allocation-free framing.** The write path frames each record in a reused
  scratch buffer, so steady-state inserts do not allocate.
- **Branch-light codec.** Fixed-width little-endian scalars and a one-byte type
  tag keep encode/decode free of branchy variable-length decoding.
- **Ordered indexes for ranges.** Secondary indexes are B-trees keyed by a total
  order over `Value`, so a range query is a contiguous, already-sorted walk.

## How it compares

A controlled, populated head-to-head against peer engines (a pure-Rust embedded
store such as `redb` or `sled`, and a document database such as MongoDB) is
planned for the release-candidate cycle, where it can be set up fairly and
without adding a heavy benchmark dependency to the library itself. Until then,
this is the honest **architectural** comparison.

The decisive factor is not micro-optimization — it is *where the work runs*. A
server document database (MongoDB and the like) answers a query over a network
socket: the request is serialized, sent over a connection, parsed by a separate
process, executed, and the result serialized back. Even on localhost, that round
trip is measured in **hundreds of microseconds to milliseconds**, dominated by
the network stack and cross-process serialization, before any indexing work.

bison-db has none of that. A read is a function call: one in-memory index lookup
and one positional file read, measured here at **~0.36 µs**. There is no socket,
no wire protocol, no second process. For the workloads bison-db targets —
in-process storage for a single application — this is a structural advantage of
roughly **three orders of magnitude on point operations**, not a tuning margin
that a competitor closes with a faster release.

The trade-off is honest and explicit: bison-db is *embedded* and *single-process*
by design (see [Out of scope](../dev/ROADMAP.md)). It does not replace a
networked database for multi-client, multi-host deployments — it removes the
network entirely for the cases that do not need it. When your data lives in the
same process as your code, paying a network round trip to reach it is the cost
worth eliminating.

## Correctness under load

Performance claims are only meaningful if the store stays correct while it runs.
A seeded **stress/soak test** (`tests/stress.rs`) drives thousands of randomized
mixed operations — insert, update, delete, read-back, compaction, and
close-and-reopen — against an in-memory reference model, checking that the store
matches the model throughout, including the secondary index. It runs as part of
the normal test suite on every platform.

## Honesty notes

- These are **0.x baselines** captured during development, not a certified
  benchmark suite. A populated head-to-head comparison against other embedded and
  document stores is planned for the 1.0 cycle.
- `fsync` timings are dominated by the storage device and its write-cache
  policy; on a different disk the `Always` row will move substantially while the
  in-memory and `Manual` rows will not.
- No claim here is made without a corresponding `criterion` benchmark you can run
  yourself.

---

<sub>Copyright &copy; 2026 <strong>James Gober</strong>.</sub>
