//! Controlled head-to-head: bison-db versus redb on the same workload.
//!
//! redb is a pure-Rust, actively-maintained embedded key/value store — the
//! closest single peer to bison-db's storage engine. This harness runs both
//! through the same two phases, with matched durability (one device sync per
//! engine, no per-operation fsync), and reports the medians.
//!
//! Run from this directory:
//!
//! ```bash
//! cargo run --release
//! ```
//!
//! ## What is measured
//!
//! - **Bulk insert** of `N` records, each engine using its natural batched path
//!   with a single durability sync at the end (bison-db: `Manual` + one `flush`;
//!   redb: one write transaction committed with default durability). Reported as
//!   records/second.
//! - **Random point read** of `R` records by key after reopening. Reported as
//!   nanoseconds per read.
//!
//! ## Fairness notes
//!
//! - Same key space (`1..=N`) and the same 64-byte payload for both engines.
//! - bison-db stores a *typed document* (`{ "v": i64, "data": bytes }`); redb
//!   stores opaque bytes keyed by an integer. bison-db therefore does strictly
//!   more per record (encode a document, frame it, checksum it) — so parity or a
//!   win on reads is despite a richer data model, not because of a thinner one.
//! - Both skip per-operation `fsync`; the single end-of-phase sync keeps the
//!   comparison about engine overhead, not disk latency.

use std::error::Error;
use std::hint::black_box;
use std::time::Instant;

use bison_db::{Db, DocId, Document, Value};
use redb::{Database, TableDefinition};

/// Records inserted and reads performed per trial.
const N: u64 = 100_000;
/// Number of trials; the median is reported.
const TRIALS: usize = 3;
/// Fixed payload size, in bytes, attached to every record.
const PAYLOAD: usize = 64;

const REDB_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("records");

/// Deterministic xorshift64 PRNG, so the read keys are identical across engines.
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

fn temp_path(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    dir.join(format!("bison_cmp_{tag}_{pid}.db"))
}

fn median(mut xs: Vec<f64>) -> f64 {
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    xs[xs.len() / 2]
}

/// One bison-db trial: returns (insert records/sec, read ns/op, file bytes).
fn bison_trial(payload: &[u8]) -> Result<(f64, f64, u64), Box<dyn Error>> {
    let path = temp_path("bison");
    let _ = std::fs::remove_file(&path);

    // Insert phase.
    let mut db = Db::open(&path)?;
    let start = Instant::now();
    for _ in 0..N {
        let mut d = Document::new();
        d.set("v", 1_i64).set("data", Value::Bytes(payload.to_vec()));
        db.insert(d)?;
    }
    db.flush()?;
    let insert_secs = start.elapsed().as_secs_f64();
    let file_bytes = std::fs::metadata(&path)?.len();
    drop(db);

    // Read phase (reopened cold).
    let db = Db::open(&path)?;
    let mut rng = Rng::new(0xA5A5_1234);
    let start = Instant::now();
    for _ in 0..N {
        let key = (rng.next_u64() % N) + 1; // ids are 1..=N
        let doc = db.get(DocId::from(key))?;
        black_box(doc);
    }
    let read_ns = start.elapsed().as_nanos() as f64 / N as f64;

    let _ = std::fs::remove_file(&path);
    Ok((N as f64 / insert_secs, read_ns, file_bytes))
}

/// One redb trial: returns (insert records/sec, read ns/op, file bytes).
fn redb_trial(payload: &[u8]) -> Result<(f64, f64, u64), Box<dyn Error>> {
    let path = temp_path("redb");
    let _ = std::fs::remove_file(&path);

    // Insert phase: one write transaction, committed once (one fsync).
    let db = Database::create(&path)?;
    let start = Instant::now();
    let wtxn = db.begin_write()?;
    {
        let mut table = wtxn.open_table(REDB_TABLE)?;
        for key in 1..=N {
            table.insert(key, payload)?;
        }
    }
    wtxn.commit()?;
    let insert_secs = start.elapsed().as_secs_f64();
    let file_bytes = std::fs::metadata(&path)?.len();
    drop(db);

    // Read phase (reopened cold).
    let db = Database::create(&path)?;
    let rtxn = db.begin_read()?;
    let table = rtxn.open_table(REDB_TABLE)?;
    let mut rng = Rng::new(0xA5A5_1234);
    let start = Instant::now();
    for _ in 0..N {
        let key = (rng.next_u64() % N) + 1;
        let value = table.get(key)?;
        black_box(value.map(|g| g.value().len()));
    }
    let read_ns = start.elapsed().as_nanos() as f64 / N as f64;

    drop(table);
    drop(rtxn);
    drop(db);
    let _ = std::fs::remove_file(&path);
    Ok((N as f64 / insert_secs, read_ns, file_bytes))
}

fn main() -> Result<(), Box<dyn Error>> {
    let payload = vec![0xCD_u8; PAYLOAD];

    println!("head-to-head: bison-db vs redb");
    println!("  records per trial: {N}, payload: {PAYLOAD} bytes, trials: {TRIALS}");
    println!("  durability: one sync per engine per phase (no per-op fsync)\n");

    let mut bison = (Vec::new(), Vec::new(), 0u64);
    let mut redb = (Vec::new(), Vec::new(), 0u64);

    // One warm-up trial (discarded) to prime the filesystem and allocator.
    let _ = bison_trial(&payload)?;
    let _ = redb_trial(&payload)?;

    for _ in 0..TRIALS {
        let (ins, rd, sz) = bison_trial(&payload)?;
        bison.0.push(ins);
        bison.1.push(rd);
        bison.2 = sz;

        let (ins, rd, sz) = redb_trial(&payload)?;
        redb.0.push(ins);
        redb.1.push(rd);
        redb.2 = sz;
    }

    let b_ins = median(bison.0);
    let b_rd = median(bison.1);
    let r_ins = median(redb.0);
    let r_rd = median(redb.1);

    println!("{:<22} {:>16} {:>16}", "metric", "bison-db", "redb");
    println!("{:-<22} {:->16} {:->16}", "", "", "");
    println!("{:<22} {:>14.2} M {:>14.2} M", "insert (records/s)", b_ins / 1e6, r_ins / 1e6);
    println!("{:<22} {:>14.0} ns {:>13.0} ns", "read (per op)", b_rd, r_rd);
    println!("{:<22} {:>13} KB {:>13} KB", "file size", bison.2 / 1024, redb.2 / 1024);

    println!("\nread latency: bison-db is {:.2}x redb", r_rd / b_rd);
    println!("insert rate:  bison-db is {:.2}x redb", b_ins / r_ins);

    Ok(())
}
