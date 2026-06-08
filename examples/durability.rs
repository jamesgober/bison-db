//! Choose how durable writes are: sync on every write, or batch and sync when
//! you decide.
//!
//! `SyncPolicy::Manual` (the default) is fastest — writes are crash-safe, but a
//! power loss can lose the most recent writes that were never flushed.
//! `SyncPolicy::Always` fsyncs after every write, so each one is durable the
//! moment it returns, at the cost of a device sync per operation. Both policies
//! guarantee the file is never left corrupt.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example durability
//! ```

use std::time::Instant;

use bison_db::{Db, DbOptions, Document, SyncPolicy};

fn record(n: i64) -> Document {
    let mut d = Document::new();
    d.set("n", n);
    d
}

fn write_batch(db: &mut Db, count: i64) -> bison_db::Result<()> {
    for n in 0..count {
        db.insert(record(n))?;
    }
    Ok(())
}

fn main() -> bison_db::Result<()> {
    const COUNT: i64 = 2_000;

    // Manual: one fsync at the end (via flush). Fast.
    let manual_path = std::env::temp_dir().join("bison_db_durability_manual.bison");
    let _ = std::fs::remove_file(&manual_path);
    let mut manual = Db::open(&manual_path)?; // default SyncPolicy::Manual
    let t0 = Instant::now();
    write_batch(&mut manual, COUNT)?;
    manual.flush()?; // single sync for the whole batch
    let manual_elapsed = t0.elapsed();

    // Always: fsync after every insert. Durable per write, slower.
    let always_path = std::env::temp_dir().join("bison_db_durability_always.bison");
    let _ = std::fs::remove_file(&always_path);
    let mut always = Db::open_with(&always_path, DbOptions::new().sync(SyncPolicy::Always))?;
    let t0 = Instant::now();
    write_batch(&mut always, COUNT)?;
    let always_elapsed = t0.elapsed();

    println!("inserted {COUNT} documents under each policy:");
    println!("  Manual (sync once on flush): {manual_elapsed:?}");
    println!("  Always (fsync every write):  {always_elapsed:?}");
    println!("\nboth files reopen cleanly with all {COUNT} records:");
    println!("  manual -> {}", Db::open(&manual_path)?.len());
    println!("  always -> {}", Db::open(&always_path)?.len());

    let _ = std::fs::remove_file(&manual_path);
    let _ = std::fs::remove_file(&always_path);
    Ok(())
}
