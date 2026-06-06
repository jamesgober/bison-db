//! Integration tests for the file-backed store.
//!
//! These exercise the public API across the persistence boundary: durability,
//! reopen-and-recover, large and nested documents, and the interaction between
//! inserts, updates, and deletes.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use bison_db::{Db, DocId, Document, Value};

/// A unique temp path per call, with any stale file removed first. The file is
/// the caller's to clean up; each test removes it at the end.
fn temp_path() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let path = std::env::temp_dir().join(format!("bison_db_it_{pid}_{n}.bison"));
    let _ = std::fs::remove_file(&path);
    path
}

fn person(name: &str, age: i64) -> Document {
    let mut d = Document::new();
    d.set("name", name).set("age", age);
    d
}

#[test]
fn test_many_documents_roundtrip_after_reopen() {
    let path = temp_path();
    let mut ids = Vec::new();
    {
        let mut db = Db::open(&path).unwrap();
        for i in 0..1_000 {
            ids.push(db.insert(person(&format!("user{i}"), i)).unwrap());
        }
        db.flush().unwrap();
    }

    let db = Db::open(&path).unwrap();
    assert_eq!(db.len(), 1_000);
    for (i, id) in ids.iter().enumerate() {
        let doc = db.get(*id).unwrap().unwrap();
        assert_eq!(doc.get("age").and_then(Value::as_int), Some(i as i64));
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_update_then_reopen_sees_latest_value() {
    let path = temp_path();
    let id;
    {
        let mut db = Db::open(&path).unwrap();
        id = db.insert(person("ada", 1)).unwrap();
        db.update(id, person("ada", 2)).unwrap();
        db.update(id, person("ada", 3)).unwrap();
        db.flush().unwrap();
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(
        db.get(id)
            .unwrap()
            .unwrap()
            .get("age")
            .and_then(Value::as_int),
        Some(3)
    );
    assert_eq!(db.len(), 1);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_delete_survives_reopen() {
    let path = temp_path();
    let (a, b);
    {
        let mut db = Db::open(&path).unwrap();
        a = db.insert(person("keep", 1)).unwrap();
        b = db.insert(person("drop", 2)).unwrap();
        db.delete(b).unwrap();
        db.flush().unwrap();
    }
    let db = Db::open(&path).unwrap();
    assert!(db.get(a).unwrap().is_some());
    assert!(db.get(b).unwrap().is_none());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_nested_document_roundtrip() {
    let path = temp_path();
    let mut db = Db::open(&path).unwrap();

    let mut address = Document::new();
    address.set("city", "Paris").set("zip", 75001_i64);
    let mut user = Document::new();
    user.set("name", "colette")
        .set(
            "tags",
            Value::Array(vec![Value::from("a"), Value::from("b")]),
        )
        .set("address", Value::Object(address));

    let id = db.insert(user.clone()).unwrap();
    assert_eq!(db.get(id).unwrap().unwrap(), user);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_large_value_under_limit_roundtrips() {
    let path = temp_path();
    let mut db = Db::open(&path).unwrap();
    let blob = vec![0xAB_u8; 4 * 1024 * 1024];
    let mut doc = Document::new();
    doc.set("blob", Value::Bytes(blob.clone()));
    let id = db.insert(doc).unwrap();
    assert_eq!(
        db.get(id)
            .unwrap()
            .unwrap()
            .get("blob")
            .and_then(Value::as_bytes),
        Some(&blob[..])
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_ids_are_unique_and_monotonic() {
    let path = temp_path();
    let mut db = Db::open(&path).unwrap();
    let mut prev = 0;
    for _ in 0..100 {
        let id = db.insert(Document::new()).unwrap().get();
        assert!(id > prev, "ids must increase: {id} after {prev}");
        prev = id;
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_get_after_delete_then_insert_uses_new_id() {
    let path = temp_path();
    let mut db = Db::open(&path).unwrap();
    let first = db.insert(person("x", 1)).unwrap();
    db.delete(first).unwrap();
    let second = db.insert(person("y", 2)).unwrap();
    assert_ne!(first, second);
    assert!(db.get(first).unwrap().is_none());
    assert!(db.get(second).unwrap().is_some());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_empty_value_kinds_roundtrip() {
    let path = temp_path();
    let mut db = Db::open(&path).unwrap();
    let mut doc = Document::new();
    doc.set("empty_str", "")
        .set("empty_bytes", Value::Bytes(Vec::new()))
        .set("empty_array", Value::Array(Vec::new()))
        .set("empty_obj", Value::Object(Document::new()))
        .set("null", Value::Null);
    let id = db.insert(doc.clone()).unwrap();
    assert_eq!(db.get(id).unwrap().unwrap(), doc);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_reconstructed_docid_addresses_same_document() {
    let path = temp_path();
    let mut db = Db::open(&path).unwrap();
    let id = db.insert(person("reload", 7)).unwrap();
    let raw: u64 = id.into();
    assert!(db.get(DocId::from(raw)).unwrap().is_some());
    let _ = std::fs::remove_file(&path);
}
