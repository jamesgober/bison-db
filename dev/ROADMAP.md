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

## v0.2.0 -- document model + single-file store + insert/get/delete (THE HARD PART, NOT DEFERRED)

Exit criteria:
- [ ] Every public item has rustdoc + a runnable example.
- [ ] Core invariants property-tested.

---

## v0.3.0 -- secondary indexes + field and range queries

Exit criteria:
- [ ] New surface tested; hot paths benchmarked.

---

## v0.4.0 -- WAL + crash recovery + on-disk format freeze

Exit criteria:
- [ ] No `todo!`/`unimplemented!`. Feature freeze declared.

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
