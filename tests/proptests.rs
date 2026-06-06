//! Property-based tests for the project invariants.
//!
//! Two invariants from `dev/DIRECTIVES.md` are checked over a wide input space:
//!
//! - **Encode/decode is lossless.** Any document inserted and read back compares
//!   equal — the on-disk encoding never silently alters a value.
//! - **The index agrees with the file.** After an arbitrary sequence of inserts
//!   and deletes, closing and reopening the store yields exactly the live set
//!   the operations imply; nothing is resurrected and nothing goes missing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use bison_db::{Db, Document, Value};
use proptest::prelude::*;

fn temp_path() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let path = std::env::temp_dir().join(format!("bison_db_pt_{pid}_{n}.bison"));
    let _ = std::fs::remove_file(&path);
    path
}

/// A strategy for arbitrary values, including nested arrays and objects.
///
/// Floats are restricted to finite values so equality is reflexive (`NaN` never
/// equals itself, which would make any round-trip assertion meaningless).
fn arb_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::Int),
        any::<f64>()
            .prop_filter("finite only", |f| f.is_finite())
            .prop_map(Value::Float),
        ".*".prop_map(Value::Str),
        proptest::collection::vec(any::<u8>(), 0..32).prop_map(Value::Bytes),
    ];
    leaf.prop_recursive(3, 24, 5, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..5).prop_map(Value::Array),
            proptest::collection::vec(("[a-z]{1,8}", inner), 0..5).prop_map(|pairs| {
                let mut doc = Document::new();
                for (k, v) in pairs {
                    doc.set(k, v);
                }
                Value::Object(doc)
            }),
        ]
    })
}

/// A strategy for arbitrary documents.
fn arb_document() -> impl Strategy<Value = Document> {
    proptest::collection::vec(("[a-z]{1,8}", arb_value()), 0..8).prop_map(|pairs| {
        let mut doc = Document::new();
        for (k, v) in pairs {
            doc.set(k, v);
        }
        doc
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Inserting a document and reading it back yields an equal document.
    #[test]
    fn prop_insert_get_is_lossless(doc in arb_document()) {
        let path = temp_path();
        let mut db = Db::open(&path).unwrap();
        let id = db.insert(doc.clone()).unwrap();
        let got = db.get(id).unwrap().unwrap();
        prop_assert_eq!(got, doc);
        let _ = std::fs::remove_file(&path);
    }

    /// After a mix of inserts and deletes, a reopened store reflects exactly the
    /// surviving documents.
    #[test]
    fn prop_index_matches_file_after_reopen(
        docs in proptest::collection::vec(arb_document(), 1..32),
        delete_mask in proptest::collection::vec(any::<bool>(), 1..32),
    ) {
        let path = temp_path();
        let mut model: HashMap<u64, Document> = HashMap::new();

        {
            let mut db = Db::open(&path).unwrap();
            for (i, doc) in docs.iter().enumerate() {
                let id = db.insert(doc.clone()).unwrap();
                let _ = model.insert(id.get(), doc.clone());
                if *delete_mask.get(i % delete_mask.len()).unwrap_or(&false) {
                    db.delete(id).unwrap();
                    let _ = model.remove(&id.get());
                }
            }
            db.flush().unwrap();
        }

        let db = Db::open(&path).unwrap();
        prop_assert_eq!(db.len(), model.len());
        for (raw, expected) in &model {
            let got = db.get((*raw).into()).unwrap();
            prop_assert_eq!(got.as_ref(), Some(expected));
        }
        let _ = std::fs::remove_file(&path);
    }
}
