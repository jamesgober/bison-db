//! Concurrency tests: `Db` is `Send + Sync` and works under the documented
//! single-writer, multi-reader pattern of `Arc<RwLock<Db>>`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;

use bison_db::{Db, DocId, Document, Value};

/// Deterministic xorshift64 PRNG, owned per thread (so it stays `Send`).
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
}

fn temp_path() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let path = std::env::temp_dir().join(format!("bison_db_conc_{pid}_{n}.bison"));
    let _ = std::fs::remove_file(&path);
    path
}

fn numbered(n: i64) -> Document {
    let mut d = Document::new();
    d.set("n", n);
    d
}

#[test]
fn test_db_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Db>();
}

#[test]
fn test_concurrent_readers_with_one_writer() {
    let path = temp_path();
    let db = Arc::new(RwLock::new(Db::open(&path).unwrap()));

    // Seed a batch of documents.
    let seeded: Vec<_> = {
        let mut guard = db.write().unwrap();
        (0..200)
            .map(|n| guard.insert(numbered(n)).unwrap())
            .collect()
    };

    let mut handles = Vec::new();

    // Several reader threads repeatedly read the seeded documents.
    for _ in 0..4 {
        let db = Arc::clone(&db);
        let ids = seeded.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..50 {
                for id in &ids {
                    let guard = db.read().unwrap();
                    let doc = guard.get(*id).unwrap().expect("seeded doc present");
                    assert!(doc.get("n").and_then(Value::as_int).is_some());
                }
            }
        }));
    }

    // One writer thread inserts more documents concurrently.
    {
        let db = Arc::clone(&db);
        handles.push(thread::spawn(move || {
            for n in 200..400 {
                let mut guard = db.write().unwrap();
                let _ = guard.insert(numbered(n)).unwrap();
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    let guard = db.read().unwrap();
    assert_eq!(guard.len(), 400);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_shared_db_survives_writer_then_readers() {
    let path = temp_path();
    let db = Arc::new(RwLock::new(Db::open(&path).unwrap()));

    let id = db.write().unwrap().insert(numbered(42)).unwrap();

    let readers: Vec<_> = (0..8)
        .map(|_| {
            let db = Arc::clone(&db);
            thread::spawn(move || {
                let guard = db.read().unwrap();
                guard
                    .get(id)
                    .unwrap()
                    .and_then(|d| d.get("n").and_then(Value::as_int))
            })
        })
        .collect();

    for r in readers {
        assert_eq!(r.join().unwrap(), Some(42));
    }
    let _ = std::fs::remove_file(&path);
}

/// Number of groups for the indexed field, matching the soak writer.
const SOAK_GROUPS: i64 = 8;

fn grouped(group: i64, payload: i64) -> Document {
    let mut d = Document::new();
    d.set("group", group).set("payload", payload);
    d
}

/// Sustained-load soak: while a writer mutates the store (insert, update,
/// delete, and compaction) under the write lock, several reader threads
/// continuously read and query under read locks. Reads must never panic despite
/// the store changing under them, and once the writer finishes, the store must
/// match the writer's model exactly.
#[test]
fn test_concurrent_soak() {
    let path = temp_path();
    let db = Arc::new(RwLock::new(Db::open(&path).unwrap()));
    db.write().unwrap().create_index("group").unwrap();

    let stop = Arc::new(AtomicBool::new(false));

    // Reader threads: spin reading and querying until the writer signals stop.
    let mut readers = Vec::new();
    for r in 0..4u64 {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        readers.push(thread::spawn(move || {
            let mut rng = Rng::new(0xC0FFEE ^ (r + 1));
            let mut reads = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let guard = db.read().unwrap();
                let len = guard.len() as u64;
                if len > 0 {
                    // A possibly-live, possibly-deleted id: `get` must return a
                    // clean `Option`, never panic.
                    let id = DocId::from((rng.next_u64() % (len * 2 + 1)) + 1);
                    let _ = guard.get(id).unwrap();
                    let group = (rng.next_u64() % SOAK_GROUPS as u64) as i64;
                    let _ = guard.find("group", &Value::from(group)).unwrap();
                }
                drop(guard);
                reads += 1;
            }
            reads
        }));
    }

    // Writer thread: a long sequence of mutations, returning its final model.
    let writer = {
        let db = Arc::clone(&db);
        thread::spawn(move || {
            let mut rng = Rng::new(0x5EED_F00D);
            let mut model: HashMap<u64, Document> = HashMap::new();
            let mut live: Vec<u64> = Vec::new();

            for _ in 0..3_000u64 {
                let mut guard = db.write().unwrap();
                match rng.next_u64() % 100 {
                    0..=54 => {
                        let group = (rng.next_u64() % SOAK_GROUPS as u64) as i64;
                        let doc = grouped(group, rng.next_u64() as i64);
                        let id = guard.insert(doc.clone()).unwrap();
                        let _ = model.insert(id.get(), doc);
                        live.push(id.get());
                    }
                    55..=74 if !live.is_empty() => {
                        let raw = live[(rng.next_u64() % live.len() as u64) as usize];
                        let group = (rng.next_u64() % SOAK_GROUPS as u64) as i64;
                        let doc = grouped(group, rng.next_u64() as i64);
                        assert!(guard.update(DocId::from(raw), doc.clone()).unwrap());
                        let _ = model.insert(raw, doc);
                    }
                    75..=89 if !live.is_empty() => {
                        let idx = (rng.next_u64() % live.len() as u64) as usize;
                        let raw = live.swap_remove(idx);
                        assert!(guard.delete(DocId::from(raw)).unwrap());
                        let _ = model.remove(&raw);
                    }
                    90..=93 => guard.compact().unwrap(),
                    _ => {}
                }
            }
            model
        })
    };

    let model = writer.join().unwrap();
    stop.store(true, Ordering::Relaxed);

    let mut total_reads = 0u64;
    for reader in readers {
        total_reads += reader.join().unwrap();
    }
    assert!(
        total_reads > 0,
        "readers should have observed the store under load"
    );

    // Final consistency: the store matches the writer's model exactly.
    let guard = db.read().unwrap();
    assert_eq!(guard.len(), model.len());
    for (raw, expected) in &model {
        assert_eq!(
            guard.get(DocId::from(*raw)).unwrap().as_ref(),
            Some(expected)
        );
    }
    drop(guard);
    let _ = std::fs::remove_file(&path);
}
