//! A small CRUD workflow: store user profiles, update one, delete another, and
//! scan the survivors. Shows nested documents and arrays as field values.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example user_profiles
//! ```

use bison_db::{Db, Document, Value};

/// Builds a profile document with a nested address and a list of roles.
fn profile(name: &str, city: &str, roles: &[&str]) -> Document {
    let mut address = Document::new();
    address.set("city", city);

    let roles = roles.iter().map(|r| Value::from(*r)).collect::<Vec<_>>();

    let mut doc = Document::new();
    doc.set("name", name)
        .set("address", Value::Object(address))
        .set("roles", Value::Array(roles));
    doc
}

fn main() -> bison_db::Result<()> {
    let path = std::env::temp_dir().join("bison_db_user_profiles.bison");
    let _ = std::fs::remove_file(&path);
    let mut db = Db::open(&path)?;

    // Create.
    let ada = db.insert(profile("Ada", "London", &["admin", "author"]))?;
    let grace = db.insert(profile("Grace", "New York", &["author"]))?;
    let alan = db.insert(profile("Alan", "Manchester", &["reviewer"]))?;
    println!("created {} profiles", db.len());

    // Update: give Grace an extra role.
    db.update(grace, profile("Grace", "New York", &["author", "admin"]))?;

    // Delete: remove Alan.
    db.delete(alan)?;

    // Read one back, reaching into the nested document.
    if let Some(doc) = db.get(ada)? {
        let city = doc
            .get("address")
            .and_then(Value::as_object)
            .and_then(|a| a.get("city"))
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        println!("Ada lives in {city}");
    }

    // Scan every surviving document.
    let mut ids: Vec<_> = db.ids().collect();
    ids.sort();
    for id in ids {
        if let Some(doc) = db.get(id)? {
            let name = doc.get("name").and_then(Value::as_str).unwrap_or("<none>");
            let role_count = doc
                .get("roles")
                .and_then(Value::as_array)
                .map_or(0, <[_]>::len);
            println!("  {id}: {name} ({role_count} roles)");
        }
    }

    db.flush()?;
    let _ = std::fs::remove_file(&path);
    Ok(())
}
