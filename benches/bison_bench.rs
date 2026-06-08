//! Criterion benchmarks for the hot paths: insert, point read, overwrite.
//!
//! Each benchmark runs against a real on-disk store in the system temp
//! directory so the measured cost includes encoding, framing, the CRC, and the
//! write/read syscalls — the work that actually happens in production, not a
//! synthetic in-memory stand-in.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use bison_db::{Db, DbOptions, Document, SyncPolicy, Value};
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

/// Point lookup through a secondary index versus the full-scan fallback, over
/// the same 10k-document store. Shows what the index buys.
fn bench_find(c: &mut Criterion) {
    const N: i64 = 10_000;
    let path = temp_path("find");
    let mut db = Db::open(&path).expect("open");
    for i in 0..N {
        let mut d = Document::new();
        d.set("key", i);
        db.insert(d).expect("insert");
    }

    let mut group = c.benchmark_group("find_one_of_10k");

    // Unindexed: scans all 10k documents.
    let mut needle = 0_i64;
    group.bench_function("scan", |b| {
        b.iter(|| {
            let hits = db
                .find("key", &Value::from(black_box(needle % N)))
                .expect("find");
            needle += 1;
            black_box(hits);
        });
    });

    // Indexed: a B-tree point lookup.
    db.create_index("key").expect("create_index");
    let mut needle = 0_i64;
    group.bench_function("indexed", |b| {
        b.iter(|| {
            let hits = db
                .find("key", &Value::from(black_box(needle % N)))
                .expect("find");
            needle += 1;
            black_box(hits);
        });
    });

    group.finish();
    let _ = std::fs::remove_file(&path);
}

/// Insert cost under each durability policy: `Manual` (sync on flush) versus
/// `Always` (fsync every write). The gap is the price of per-operation
/// durability, and it is dominated by the device, not by bison-db.
fn bench_durability(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert_by_sync_policy");

    let manual_path = temp_path("sync_manual");
    let mut manual = Db::open(&manual_path).expect("open");
    let mut i = 0_i64;
    group.bench_function("manual", |b| {
        b.iter(|| {
            let id = manual.insert(small_doc(black_box(i))).expect("insert");
            i += 1;
            black_box(id);
        });
    });
    drop(manual);
    let _ = std::fs::remove_file(&manual_path);

    let always_path = temp_path("sync_always");
    let mut always =
        Db::open_with(&always_path, DbOptions::new().sync(SyncPolicy::Always)).expect("open");
    let mut i = 0_i64;
    group.bench_function("always", |b| {
        b.iter(|| {
            let id = always.insert(small_doc(black_box(i))).expect("insert");
            i += 1;
            black_box(id);
        });
    });
    drop(always);
    let _ = std::fs::remove_file(&always_path);

    group.finish();
}

criterion_group!(
    benches,
    bench_insert,
    bench_get,
    bench_update,
    bench_find,
    bench_durability
);
criterion_main!(benches);
