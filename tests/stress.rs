//! Stress / soak test: a long, deterministic sequence of mixed operations
//! validated against an in-memory model.
//!
//! A reference `HashMap<u64, Document>` mirrors what the store should contain.
//! Each step performs a random insert, update, delete, read-back, compaction, or
//! close-and-reopen, and the store is checked against the model throughout. This
//! exercises the whole stack end to end — encoding, framing, recovery,
//! compaction, and index maintenance — catching cross-cutting bugs that
//! single-operation unit tests miss. The sequence is driven by a seeded PRNG, so
//! a failure reproduces exactly.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use bison_db::{Db, DocId, Document, Value};

/// A tiny deterministic xorshift64 PRNG — no external crate, fully reproducible.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

fn temp_path() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let path = std::env::temp_dir().join(format!("bison_db_stress_{pid}_{n}.bison"));
    let _ = std::fs::remove_file(&path);
    path
}

/// The indexed field's value is `id % GROUPS`, so index queries are easy to
/// check against the model.
const GROUPS: i64 = 8;

fn make_doc(rng: &mut Rng, group: i64) -> Document {
    let mut d = Document::new();
    d.set("group", group)
        .set("payload", rng.next_u64() as i64)
        .set("tag", "stress");
    d
}

/// Verifies the store matches the model exactly: same length, every live id maps
/// to its expected document, and a few absent ids read back as `None`.
fn verify(db: &Db, model: &HashMap<u64, Document>) {
    assert_eq!(db.len(), model.len(), "live count diverged from model");
    for (raw, expected) in model {
        let got = db.get(DocId::from(*raw)).unwrap();
        assert_eq!(
            got.as_ref(),
            Some(expected),
            "doc {raw} diverged from model"
        );
    }
    // Spot-check that ids well past the assigned range are absent.
    for raw in [u64::MAX, u64::MAX - 1, 10_000_000] {
        if !model.contains_key(&raw) {
            assert!(db.get(DocId::from(raw)).unwrap().is_none());
        }
    }
}

/// Verifies the secondary index over `group` matches the model for every group.
fn verify_index(db: &Db, model: &HashMap<u64, Document>) {
    for group in 0..GROUPS {
        let mut expected: Vec<u64> = model
            .iter()
            .filter(|(_, doc)| doc.get("group").and_then(Value::as_int) == Some(group))
            .map(|(raw, _)| *raw)
            .collect();
        expected.sort_unstable();

        let mut got: Vec<u64> = db
            .find("group", &Value::from(group))
            .unwrap()
            .into_iter()
            .map(DocId::get)
            .collect();
        got.sort_unstable();

        assert_eq!(got, expected, "index for group {group} diverged from model");
    }
}

#[test]
fn test_stress_mixed_operations_match_model() {
    let path = temp_path();
    let mut rng = Rng::new(0x5EED_1234_ABCD_0001);

    let mut db = Db::open(&path).unwrap();
    db.create_index("group").unwrap();
    let mut model: HashMap<u64, Document> = HashMap::new();
    let mut live: Vec<u64> = Vec::new();

    for step in 0..4_000_u64 {
        match rng.below(100) {
            // ~45% inserts
            0..=44 => {
                let group = (rng.below(GROUPS as u64)) as i64;
                let doc = make_doc(&mut rng, group);
                let id = db.insert(doc.clone()).unwrap();
                let _ = model.insert(id.get(), doc);
                live.push(id.get());
            }
            // ~25% updates
            45..=69 if !live.is_empty() => {
                let idx = (rng.below(live.len() as u64)) as usize;
                let raw = live[idx];
                let group = (rng.below(GROUPS as u64)) as i64;
                let doc = make_doc(&mut rng, group);
                assert!(db.update(DocId::from(raw), doc.clone()).unwrap());
                let _ = model.insert(raw, doc);
            }
            // ~20% deletes
            70..=89 if !live.is_empty() => {
                let idx = (rng.below(live.len() as u64)) as usize;
                let raw = live.swap_remove(idx);
                assert!(db.delete(DocId::from(raw)).unwrap());
                let _ = model.remove(&raw);
            }
            // occasional compaction
            90..=92 => {
                db.compact().unwrap();
                verify(&db, &model);
                verify_index(&db, &model);
            }
            // occasional close-and-reopen (indexes are rebuilt by design)
            93..=95 => {
                db.flush().unwrap();
                drop(db);
                db = Db::open(&path).unwrap();
                db.create_index("group").unwrap();
                verify(&db, &model);
            }
            // remaining steps: read-back checks
            _ => {
                if !live.is_empty() {
                    let raw = live[(rng.below(live.len() as u64)) as usize];
                    let got = db.get(DocId::from(raw)).unwrap();
                    assert_eq!(got.as_ref(), model.get(&raw));
                }
            }
        }

        // Periodic full check keeps a failure close to the step that caused it.
        if step % 500 == 0 {
            verify(&db, &model);
            verify_index(&db, &model);
        }
    }

    // Final reopen-and-verify: durability and recovery of the whole sequence.
    db.flush().unwrap();
    drop(db);
    let mut db = Db::open(&path).unwrap();
    db.create_index("group").unwrap();
    verify(&db, &model);
    verify_index(&db, &model);

    let _ = std::fs::remove_file(&path);
}
