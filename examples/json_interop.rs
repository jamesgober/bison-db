//! Move documents in and out of JSON using the `serde` feature.
//!
//! With `serde` enabled, [`Document`] and [`Value`] implement
//! `Serialize`/`Deserialize`, so any serde data format works. Here we parse a
//! JSON object into a document, store it, read it back, and serialise it again.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example json_interop --features serde
//! ```

use bison_db::{Db, Document};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join("bison_db_json_interop.bison");
    let _ = std::fs::remove_file(&path);
    let mut db = Db::open(&path)?;

    // Parse external JSON straight into a document.
    let json = r#"{ "name": "bison", "legs": 4, "wild": true, "weight_kg": 900.5 }"#;
    let doc: Document = serde_json::from_str(json)?;

    let id = db.insert(doc)?;
    db.flush()?;

    // Read it back and serialise it again — the round trip is lossless.
    let stored = db.get(id)?.ok_or("document vanished")?;
    let out = serde_json::to_string_pretty(&stored)?;
    println!("{out}");

    let _ = std::fs::remove_file(&path);
    Ok(())
}
