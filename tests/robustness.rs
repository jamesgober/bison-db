//! Robustness tests: `Db::open` must survive arbitrary and corrupted file
//! contents without panicking. The on-disk bytes are untrusted once a file has
//! been moved, truncated, or damaged, so opening one is a parse of hostile input.
//!
//! These are property-based, in-tree fuzz tests of the open/replay path: they do
//! not prove a *specific* outcome, only that every input yields `Ok` or `Err`
//! (never a panic, hang, or over-read) and that a returned store is self
//! consistent.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use bison_db::{Db, Document};
use proptest::prelude::*;

fn temp_path() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    std::env::temp_dir().join(format!("bison_db_robust_{pid}_{n}.bison"))
}

/// Builds a small valid store file and returns its raw bytes.
fn valid_store_bytes(doc_count: usize) -> Vec<u8> {
    let path = temp_path();
    let _ = std::fs::remove_file(&path);
    {
        let mut db = Db::open(&path).unwrap();
        for i in 0..doc_count {
            let mut d = Document::new();
            d.set("i", i as i64).set("name", "robustness");
            db.insert(d).unwrap();
        }
        db.flush().unwrap();
    }
    let bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    bytes
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// Opening a file of arbitrary bytes never panics; it returns `Ok` or `Err`.
    /// If it opens, the store answers `len()`/`stats()` consistently.
    #[test]
    fn prop_open_arbitrary_bytes_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..1024)) {
        let path = temp_path();
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, &bytes).unwrap();

        if let Ok(db) = Db::open(&path) {
            prop_assert_eq!(db.len(), db.stats().live_documents);
        }
        let _ = std::fs::remove_file(&path);
    }

    /// Flipping arbitrary bytes in a valid store never makes `open` panic. The
    /// result is `Ok` (with a self-consistent store) or a clean `Err`.
    #[test]
    fn prop_corrupted_store_open_never_panics(
        doc_count in 1usize..16,
        mutations in proptest::collection::vec((any::<u16>(), any::<u8>()), 1..16),
    ) {
        let mut bytes = valid_store_bytes(doc_count);
        if bytes.is_empty() {
            return Ok(());
        }
        for (pos, val) in mutations {
            let idx = (pos as usize) % bytes.len();
            bytes[idx] = val;
        }

        let path = temp_path();
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, &bytes).unwrap();

        if let Ok(db) = Db::open(&path) {
            // Whatever survived recovery must be internally consistent and
            // re-readable without panicking.
            prop_assert!(db.len() <= doc_count + 1);
            for id in db.ids() {
                let _ = db.get(id);
            }
        }
        let _ = std::fs::remove_file(&path);
    }

    /// Truncating a valid store at any length never makes `open` panic.
    #[test]
    fn prop_truncated_store_open_never_panics(doc_count in 1usize..16, cut in any::<u16>()) {
        let bytes = valid_store_bytes(doc_count);
        let keep = (cut as usize) % (bytes.len() + 1);

        let path = temp_path();
        let _ = std::fs::remove_file(&path);
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(&bytes[..keep]).unwrap();
        }

        if let Ok(db) = Db::open(&path) {
            prop_assert_eq!(db.len(), db.stats().live_documents);
        }
        let _ = std::fs::remove_file(&path);
    }
}
