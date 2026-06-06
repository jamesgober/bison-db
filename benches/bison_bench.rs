//! Criterion benchmarks for the hot paths: insert, point read, overwrite.
//!
//! Each benchmark runs against a real on-disk store in the system temp
//! directory so the measured cost includes encoding, framing, the CRC, and the
//! write/read syscalls — the work that actually happens in production, not a
//! synthetic in-memory stand-in.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use bison_db::{Db, Document, Value};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn temp_path(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let path = std::env::temp_dir().join(format!("bison_db_bench_{tag}_{pid}_{n}.bison"));
    let _ = std::fs::remove_file(&path);
    path
}

/// A representative small record: a handful of typed fields.
fn small_doc(i: i64) -> Document {
    let mut d = Document::new();
    d.set("id", i)
        .set("name", "benchmark user")
        .set("active", true)
        .set("score", 87.5_f64)
        .set(
            "tags",
            Value::Array(vec![Value::from("a"), Value::from("b")]),
        );
    d
}

fn bench_insert(c: &mut Criterion) {
    let path = temp_path("insert");
    let mut db = Db::open(&path).expect("open");
    let mut i = 0_i64;
    c.bench_function("insert_small_doc", |b| {
        b.iter(|| {
            let id = db.insert(small_doc(black_box(i))).expect("insert");
            i += 1;
            black_box(id);
        });
    });
    let _ = std::fs::remove_file(&path);
}

fn bench_get(c: &mut Criterion) {
    let path = temp_path("get");
    let mut db = Db::open(&path).expect("open");
    let mut ids = Vec::new();
    for i in 0..10_000 {
        ids.push(db.insert(small_doc(i)).expect("insert"));
    }
    let mut cursor = 0_usize;
    c.bench_function("get_small_doc", |b| {
        b.iter(|| {
            let id = ids[cursor % ids.len()];
            cursor += 1;
            let doc = db.get(black_box(id)).expect("get");
            black_box(doc);
        });
    });
    let _ = std::fs::remove_file(&path);
}

fn bench_update(c: &mut Criterion) {
    let path = temp_path("update");
    let mut db = Db::open(&path).expect("open");
    let id = db.insert(small_doc(0)).expect("insert");
    let mut i = 0_i64;
    c.bench_function("update_small_doc", |b| {
        b.iter(|| {
            let ok = db.update(black_box(id), small_doc(i)).expect("update");
            i += 1;
            black_box(ok);
        });
    });
    let _ = std::fs::remove_file(&path);
}

criterion_group!(benches, bench_insert, bench_get, bench_update);
criterion_main!(benches);
