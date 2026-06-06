//! The shortest end-to-end use of bison-db: open a store, insert a document,
//! read it back, and make it durable.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example quick_start
//! ```

use bison_db::{Db, Document};

fn main() -> bison_db::Result<()> {
    // The whole database is a single file. Opening a path that does not exist
    // creates an empty store there.
    let path = std::env::temp_dir().join("bison_db_quick_start.bison");
    let _ = std::fs::remove_file(&path);
    let mut db = Db::open(&path)?;

    // Documents are schemaless: set whatever fields you like, of mixed types.
    let mut album = Document::new();
    album
        .set("artist", "Miles Davis")
        .set("title", "Kind of Blue")
        .set("year", 1959_i64);

    let id = db.insert(album)?;
    println!("inserted document {id}");

    // Read it back by id.
    if let Some(doc) = db.get(id)? {
        let title = doc
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("<none>");
        let year = doc.get("year").and_then(|v| v.as_int()).unwrap_or_default();
        println!("read back: {title} ({year})");
    }

    // Persist to disk so the write survives a power loss.
    db.flush()?;

    let _ = std::fs::remove_file(&path);
    Ok(())
}
