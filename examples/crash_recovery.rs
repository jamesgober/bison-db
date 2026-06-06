//! Demonstrates that the store recovers cleanly after a simulated crash.
//!
//! We write and flush some documents, then append garbage to the tail of the
//! file to imitate a half-finished write that a power loss might leave behind.
//! On the next open, bison-db detects the torn record by its length and checksum
//! and truncates it, leaving the previously durable data intact.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example crash_recovery
//! ```

use std::io::Write;

use bison_db::{Db, Document};

fn record(n: i64) -> Document {
    let mut d = Document::new();
    d.set("n", n).set("note", "durable record");
    d
}

fn main() -> bison_db::Result<()> {
    let path = std::env::temp_dir().join("bison_db_crash_recovery.bison");
    let _ = std::fs::remove_file(&path);

    // Phase 1: write three records and flush them to disk.
    let ids = {
        let mut db = Db::open(&path)?;
        let ids = [
            db.insert(record(1))?,
            db.insert(record(2))?,
            db.insert(record(3))?,
        ];
        db.flush()?;
        println!("wrote and flushed {} records", db.len());
        ids
    };

    // Phase 2: simulate a crash mid-write by appending bytes that do not form a
    // complete, checksummed record.
    {
        let mut f = std::fs::OpenOptions::new().append(true).open(&path)?;
        f.write_all(b"\xff\xff\xff\xff garbage from an interrupted write")?;
        f.flush()?;
        println!("appended a torn partial write to the tail");
    }

    // Phase 3: reopen. The torn tail is dropped; the flushed records remain.
    let db = Db::open(&path)?;
    println!("recovered {} records after reopen", db.len());
    for id in ids {
        let present = db.get(id)?.is_some();
        println!("  {id}: {}", if present { "intact" } else { "lost" });
    }
    assert_eq!(db.len(), 3, "all flushed records must survive recovery");

    let _ = std::fs::remove_file(&path);
    Ok(())
}
