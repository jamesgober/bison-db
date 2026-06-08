//! Reclaim disk space with `compact`.
//!
//! bison-db is append-only: every overwrite and delete leaves the previous
//! record behind as dead space, so an actively-updated store grows past the size
//! of its live data. `compact` rewrites the file with one record per live
//! document and atomically swaps it in — ids and indexes are preserved.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example compaction
//! ```

use bison_db::{Db, Document};

fn counter(value: i64) -> Document {
    let mut d = Document::new();
    d.set("value", value);
    d
}

fn main() -> bison_db::Result<()> {
    let path = std::env::temp_dir().join("bison_db_compaction.bison");
    let _ = std::fs::remove_file(&path);
    let mut db = Db::open(&path)?;

    // One document, updated many times — each update appends a new record and
    // orphans the previous one.
    let id = db.insert(counter(0))?;
    for v in 1..=10_000 {
        db.update(id, counter(v))?;
    }

    let before = db.stats();
    println!("before compaction:");
    println!("  live documents: {}", before.live_documents);
    println!("  file bytes:     {}", before.file_bytes);
    println!("  live bytes:     {}", before.live_bytes);

    db.compact()?;

    let after = db.stats();
    println!("\nafter compaction:");
    println!("  live documents: {}", after.live_documents);
    println!("  file bytes:     {}", after.file_bytes);
    println!("  live bytes:     {}", after.live_bytes);

    // The data is intact: id preserved, latest value readable.
    let value = db
        .get(id)?
        .and_then(|d| d.get("value").and_then(|v| v.as_int()));
    println!(
        "\nreclaimed {} bytes; document {id} still reads {value:?}",
        before.file_bytes - after.file_bytes
    );
    assert_eq!(value, Some(10_000));

    let _ = std::fs::remove_file(&path);
    Ok(())
}
