//! Concurrency tests: `Db` is `Send + Sync` and works under the documented
//! single-writer, multi-reader pattern of `Arc<RwLock<Db>>`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;

use bison_db::{Db, Document, Value};

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
