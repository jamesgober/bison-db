//! Index document fields, then query by equality and by range.
//!
//! A `Db` answers `find` and `range` whether or not a field is indexed — the
//! index only changes the speed. Here we build a small catalogue, index two
//! fields, and run both kinds of query. There is no limit on how many fields you
//! index; call `create_index` once per field.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example secondary_indexes
//! ```

use bison_db::{Db, Document, Value};

fn product(name: &str, category: &str, price: i64) -> Document {
    let mut d = Document::new();
    d.set("name", name)
        .set("category", category)
        .set("price", price);
    d
}

fn main() -> bison_db::Result<()> {
    let path = std::env::temp_dir().join("bison_db_secondary_indexes.bison");
    let _ = std::fs::remove_file(&path);
    let mut db = Db::open(&path)?;

    db.insert(product("Notebook", "stationery", 6))?;
    db.insert(product("Fountain pen", "stationery", 45))?;
    db.insert(product("Desk lamp", "lighting", 30))?;
    db.insert(product("Floor lamp", "lighting", 120))?;
    db.insert(product("Pencil", "stationery", 2))?;

    // Index two fields — equality on `category`, ranges on `price`.
    db.create_index("category")?;
    db.create_index("price")?;
    println!("indexed fields: {:?}", db.indexes().collect::<Vec<_>>());

    // Equality query: everything in the stationery aisle.
    let stationery = db.find("category", &Value::from("stationery"))?;
    println!("\n{} stationery items:", stationery.len());
    for id in &stationery {
        if let Some(doc) = db.get(*id)? {
            let name = doc.get("name").and_then(Value::as_str).unwrap_or("?");
            println!("  - {name}");
        }
    }

    // Range query: mid-priced items, 10..=50. Indexed ranges come back ordered
    // by the field value.
    let mid = db.range("price", Value::from(10_i64)..=Value::from(50_i64))?;
    println!("\n{} items priced 10..=50 (cheapest first):", mid.len());
    for id in &mid {
        if let Some(doc) = db.get(*id)? {
            let name = doc.get("name").and_then(Value::as_str).unwrap_or("?");
            let price = doc.get("price").and_then(Value::as_int).unwrap_or_default();
            println!("  - {name}: {price}");
        }
    }

    db.flush()?;
    let _ = std::fs::remove_file(&path);
    Ok(())
}
