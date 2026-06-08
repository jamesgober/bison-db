//! A realistic use case: an embedded web-session store.
//!
//! Sessions are documents keyed by their [`DocId`] (the session id handed to the
//! client). They are indexed by `user_id` so "log out everywhere" — find and
//! revoke every session for a user — is a fast indexed lookup rather than a scan.
//! Writes use `SyncPolicy::Always` so a session is durable the moment it is
//! created, and `compact` reclaims the space left by expired sessions.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example session_store
//! ```

use bison_db::{Db, DbOptions, DocId, Document, SyncPolicy, Value};

/// Creates a session document for `user_id` issued at `created_at` (epoch secs).
fn session(user_id: i64, created_at: i64, agent: &str) -> Document {
    let mut d = Document::new();
    d.set("user_id", user_id)
        .set("created_at", created_at)
        .set("agent", agent);
    d
}

/// Opens a new session and returns the id to hand to the client.
fn login(db: &mut Db, user_id: i64, created_at: i64, agent: &str) -> bison_db::Result<DocId> {
    db.insert(session(user_id, created_at, agent))
}

/// Revokes every session belonging to `user_id` ("log out everywhere").
fn logout_everywhere(db: &mut Db, user_id: i64) -> bison_db::Result<usize> {
    let ids = db.find("user_id", &Value::from(user_id))?;
    for id in &ids {
        db.delete(*id)?;
    }
    Ok(ids.len())
}

fn main() -> bison_db::Result<()> {
    let path = std::env::temp_dir().join("bison_db_session_store.bison");
    let _ = std::fs::remove_file(&path);

    // Each session must be durable on return, so a crash never forgets a login.
    let mut db = Db::open_with(&path, DbOptions::new().sync(SyncPolicy::Always))?;
    db.create_index("user_id")?;

    // Two users sign in from a few devices.
    let alice = 1001;
    let bob = 1002;
    let a1 = login(&mut db, alice, 1_700_000_000, "Firefox/Linux")?;
    let _a2 = login(&mut db, alice, 1_700_000_300, "Safari/iOS")?;
    let b1 = login(&mut db, bob, 1_700_000_500, "Chrome/Windows")?;
    println!("opened {} sessions", db.len());

    // Validate a single session by its id (what a request cookie carries).
    if let Some(s) = db.get(a1)? {
        let agent = s.get("agent").and_then(Value::as_str).unwrap_or("?");
        println!(
            "session {a1} belongs to user {} via {agent}",
            s.get("user_id").and_then(Value::as_int).unwrap_or(0)
        );
    }

    // Alice taps "log out everywhere".
    let revoked = logout_everywhere(&mut db, alice)?;
    println!("revoked {revoked} sessions for user {alice}");
    assert!(db.get(a1)?.is_none());
    assert!(db.get(b1)?.is_some()); // Bob is unaffected

    // The deletes left tombstones behind; reclaim that space.
    let before = db.stats().file_bytes;
    db.compact()?;
    println!(
        "compacted {} -> {} bytes, {} live session(s)",
        before,
        db.stats().file_bytes,
        db.len()
    );

    let _ = std::fs::remove_file(&path);
    Ok(())
}
