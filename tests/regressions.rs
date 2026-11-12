//! Regression tests for failure paths: a second handle on a live store,
//! damaged records before the end of the log, a failed compaction swap, id
//! reuse after compaction, hostile documents, and write-path ordering.
//!
//! Each test builds the faulty condition directly (raw store bytes with valid
//! checksums, a second open, an injected rename failure) and checks that the
//! store refuses it or recovers without losing acknowledged data.

#![cfg(feature = "std")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use bison_db::{Db, DbOptions, DocId, Document, Error, SyncPolicy, Value};

const TAG_NULL: u8 = 0;
const TAG_INT: u8 = 2;
const TAG_ARRAY: u8 = 6;

fn crc32c(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x82F6_3B78
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn header() -> Vec<u8> {
    let mut h = b"BISONDB1".to_vec();
    h.extend_from_slice(&1u16.to_le_bytes());
    h.extend_from_slice(&[0u8; 6]);
    h
}

/// One framed PUT record carrying an already-encoded document body.
fn put_record(id: u64, body: &[u8]) -> Vec<u8> {
    let mut payload = vec![1u8];
    payload.extend_from_slice(&id.to_le_bytes());
    payload.extend_from_slice(body);
    let mut rec = Vec::with_capacity(8 + payload.len());
    rec.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    rec.extend_from_slice(&crc32c(&payload).to_le_bytes());
    rec.extend_from_slice(&payload);
    rec
}

/// Body of a document with one field `key` holding an Int.
fn int_doc_body(key: &str, n: i64) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&(key.len() as u32).to_le_bytes());
    b.extend_from_slice(key.as_bytes());
    b.push(TAG_INT);
    b.extend_from_slice(&n.to_le_bytes());
    b
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn temp_path(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let p = std::env::temp_dir().join(format!(
        "bison_db_regress_{tag}_{}_{}.bison",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    cleanup(&p);
    p
}

fn cleanup(p: &Path) {
    let _ = std::fs::remove_file(p);
    for suffix in [".compacting", ".lock", ".damaged"] {
        let _ = std::fs::remove_file(sidecar(p, suffix));
    }
}

fn doc(k: &str, v: impl Into<Value>) -> Document {
    let mut d = Document::new();
    d.set(k, v);
    d
}

// ---------------------------------------------------------------------------
// A second handle on a live store
// ---------------------------------------------------------------------------

#[test]
fn second_open_in_same_process_is_refused() {
    let p = temp_path("twohandles");
    let mut a = DbOptions::new().sync(SyncPolicy::Always).open(&p).unwrap();
    let second = DbOptions::new().sync(SyncPolicy::Always).open(&p);
    assert!(
        matches!(second, Err(Error::Locked)),
        "a second handle on a live store must be refused, got {second:?}"
    );
    let id = a.insert(doc("owner", "A")).unwrap();
    drop(a);

    // Once the first handle is gone the store opens again and kept A's write.
    let db = Db::open(&p).unwrap();
    assert_eq!(
        db.get(id).unwrap().unwrap().get("owner").unwrap().as_str(),
        Some("A")
    );
    drop(db);
    cleanup(&p);
}

/// Child half of `second_process_cannot_open_a_live_store`: does nothing unless
/// the parent test sets the probe variable.
#[test]
fn lock_probe_child() {
    let Ok(path) = std::env::var("BISON_DB_LOCK_PROBE") else {
        return;
    };
    match Db::open(&path) {
        Err(Error::Locked) => println!("PROBE:locked"),
        Err(e) => println!("PROBE:error:{e}"),
        Ok(_) => println!("PROBE:opened"),
    }
}

fn run_probe(path: &Path) -> String {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "lock_probe_child",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("BISON_DB_LOCK_PROBE", path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn second_process_cannot_open_a_live_store() {
    let p = temp_path("xproc");
    let db = Db::open(&p).unwrap();
    let seen = run_probe(&p);
    assert!(seen.contains("PROBE:locked"), "child output: {seen}");
    drop(db);
    let seen = run_probe(&p);
    assert!(seen.contains("PROBE:opened"), "child output: {seen}");
    cleanup(&p);
}

#[test]
fn lock_left_by_a_dead_process_does_not_block_open() {
    let p = temp_path("stale");
    {
        let mut db = Db::open(&p).unwrap();
        db.insert(doc("n", 1_i64)).unwrap();
    }
    // A crashed holder leaves its lock file behind. On Windows the lock is an
    // open handle, so a file nobody holds is free; on Linux the recorded holder
    // is a process that has already exited.
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--list"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let dead_pid = child.id();
    child.wait().unwrap();
    let identity = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
    let ns = std::fs::read_link("/proc/self/ns/pid")
        .map(|l| l.to_string_lossy().into_owned())
        .unwrap_or_default();
    let content = format!(
        "bison-db lock v1\npid {dead_pid}\nstart 1\nhost {}\npidns {ns}\n",
        identity.trim()
    );
    std::fs::write(sidecar(&p, ".lock"), content).unwrap();

    if cfg!(any(windows, target_os = "linux", target_os = "android")) {
        let db = Db::open(&p).expect("a lock left by a dead process is cleared");
        assert_eq!(db.len(), 1);
    } else {
        // Without procfs the holder cannot be checked, so the lock is honoured.
        assert!(matches!(Db::open(&p), Err(Error::Locked)));
    }
    cleanup(&p);
}

// ---------------------------------------------------------------------------
// Damage before the end of the log
// ---------------------------------------------------------------------------

fn thousand_doc_store(tag: &str) -> PathBuf {
    let p = temp_path(tag);
    let mut db = DbOptions::new().sync(SyncPolicy::Manual).open(&p).unwrap();
    for i in 0..1000_i64 {
        db.insert(doc("n", i)).unwrap();
    }
    db.flush().unwrap();
    drop(db);
    p
}

#[test]
fn length_bitflip_mid_file_is_corrupt_and_file_untouched() {
    for (byte, mask) in [(3_usize, 0x80_u8), (2, 0x10)] {
        let p = thousand_doc_store("lenflip");
        let mut bytes = std::fs::read(&p).unwrap();
        let before = bytes.len() as u64;
        bytes[16 + byte] ^= mask;
        std::fs::write(&p, &bytes).unwrap();

        let r = Db::open(&p);
        assert!(
            matches!(r, Err(Error::Corrupt(_))),
            "damage before the tail must be reported, got {:?}",
            r.map(|d| d.len())
        );
        assert_eq!(
            std::fs::metadata(&p).unwrap().len(),
            before,
            "the file must not shrink"
        );
        cleanup(&p);
    }
}

#[test]
fn crc_flip_mid_file_is_corrupt() {
    let p = thousand_doc_store("crcflip");
    let mut bytes = std::fs::read(&p).unwrap();
    bytes[16 + 7] ^= 0x80;
    std::fs::write(&p, &bytes).unwrap();
    assert!(matches!(Db::open(&p), Err(Error::Corrupt(_))));
    cleanup(&p);
}

#[test]
fn torn_tail_is_still_truncated_and_reported() {
    let p = thousand_doc_store("torn");
    let full = std::fs::read(&p).unwrap();
    // Cut the last record in half: a torn final write.
    let cut = full.len() - 10;
    std::fs::write(&p, &full[..cut]).unwrap();
    let db = Db::open(&p).unwrap();
    assert_eq!(db.len(), 999);
    assert_eq!(db.discarded_bytes(), (cut - (full.len() - 35)) as u64);
    drop(db);
    assert_eq!(
        std::fs::metadata(&p).unwrap().len() as usize,
        full.len() - 35
    );
    cleanup(&p);
}

#[test]
fn salvage_keeps_records_after_the_damage_and_saves_a_copy() {
    use bison_db::RecoveryMode;
    let p = thousand_doc_store("salvage");
    let mut bytes = std::fs::read(&p).unwrap();
    // Damage the length of record 10 (each record is 35 bytes here).
    let rec10 = 16 + 9 * 35;
    bytes[rec10 + 3] ^= 0x80;
    std::fs::write(&p, &bytes).unwrap();

    assert!(matches!(Db::open(&p), Err(Error::Corrupt(_))));

    let db = DbOptions::new()
        .recovery(RecoveryMode::Salvage)
        .open(&p)
        .unwrap();
    assert_eq!(db.len(), 999, "only the damaged record is lost");
    assert!(db.get(DocId::from(10)).unwrap().is_none());
    assert_eq!(
        db.get(DocId::from(11))
            .unwrap()
            .unwrap()
            .get("n")
            .unwrap()
            .as_int(),
        Some(10)
    );
    assert!(db.discarded_bytes() >= 35);
    drop(db);

    // The damaged original is kept beside the store, byte for byte.
    assert_eq!(std::fs::read(sidecar(&p, ".damaged")).unwrap(), bytes);
    // The rewritten store opens cleanly in strict mode.
    let db = Db::open(&p).unwrap();
    assert_eq!(db.len(), 999);
    assert_eq!(db.discarded_bytes(), 0);
    drop(db);
    cleanup(&p);
}

// ---------------------------------------------------------------------------
// Compaction
// ---------------------------------------------------------------------------

/// Windows: a process holding the store without share-delete makes the rename
/// fail. The handle must stay on the original file, so later writes survive.
#[cfg(windows)]
#[test]
fn failed_compaction_rename_keeps_writing_to_the_original() {
    use std::os::windows::fs::OpenOptionsExt;
    let p = temp_path("compactfail");
    let mut db = DbOptions::new().sync(SyncPolicy::Always).open(&p).unwrap();
    let first = db.insert(doc("v", "first")).unwrap();
    let blocker = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2) // read | write, no delete
        .open(&p)
        .unwrap();
    assert!(db.compact().is_err());
    drop(blocker);
    assert!(!sidecar(&p, ".compacting").exists(), "temp file is removed");
    let second = db.insert(doc("v", "second")).unwrap();
    assert!(db.get(second).unwrap().is_some());
    drop(db);
    let db = Db::open(&p).unwrap();
    assert!(db.get(first).unwrap().is_some());
    assert!(
        db.get(second).unwrap().is_some(),
        "a write acknowledged after a failed compaction survives reopen"
    );
    drop(db);
    cleanup(&p);
}

#[test]
fn compaction_does_not_reissue_deleted_ids_after_reopen() {
    let p = temp_path("idreuse");
    let deleted;
    {
        let mut db = Db::open(&p).unwrap();
        db.insert(doc("user", "alice")).unwrap();
        db.insert(doc("user", "bob")).unwrap();
        deleted = db.insert(doc("user", "mallory-session")).unwrap();
        assert!(db.delete(deleted).unwrap());
        db.compact().unwrap();
    }
    let mut db = Db::open(&p).unwrap();
    let fresh = db.insert(doc("user", "carol")).unwrap();
    assert!(
        fresh.get() > deleted.get(),
        "deleted id {deleted} was reissued as {fresh}"
    );
    // A second compaction of the reopened store keeps the mark as well.
    db.delete(fresh).unwrap();
    db.compact().unwrap();
    drop(db);
    let mut db = Db::open(&p).unwrap();
    assert!(db.insert(doc("user", "dave")).unwrap().get() > fresh.get());
    drop(db);
    cleanup(&p);
}

#[test]
fn compaction_after_deleting_everything_keeps_the_id_counter() {
    let p = temp_path("idreuse-all");
    {
        let mut db = Db::open(&p).unwrap();
        for _ in 0..5 {
            let id = db.insert(doc("x", 1_i64)).unwrap();
            db.delete(id).unwrap();
        }
        db.compact().unwrap();
    }
    let mut db = Db::open(&p).unwrap();
    assert_eq!(db.insert(doc("x", 2_i64)).unwrap(), DocId::from(6));
    drop(db);
    cleanup(&p);
}

// ---------------------------------------------------------------------------
// Hostile documents
// ---------------------------------------------------------------------------

#[test]
fn wide_document_decodes_in_linear_time() {
    let p = temp_path("wide");
    let big: Document = (0..40_000_i64).map(|i| (format!("k{i}"), i)).collect();
    let mut db = Db::open(&p).unwrap();
    let id = db.insert(big).unwrap();
    let start = std::time::Instant::now();
    let back = db.get(id).unwrap().unwrap();
    let took = start.elapsed();
    assert_eq!(back.len(), 40_000);
    // The quadratic decoder took seconds even in release; linear takes
    // milliseconds. The bound is loose so a loaded debug run passes.
    assert!(
        took < std::time::Duration::from_secs(3),
        "decoding 40,000 fields took {took:?}"
    );
    drop(db);
    cleanup(&p);
}

#[test]
fn document_with_too_many_fields_is_refused_on_write() {
    let p = temp_path("toomany");
    let wide: Document = (0..=bison_db::MAX_DOCUMENT_FIELDS)
        .map(|i| (format!("k{i}"), Value::Null))
        .collect();
    assert_eq!(wide.len(), bison_db::MAX_DOCUMENT_FIELDS + 1);
    let mut db = Db::open(&p).unwrap();
    assert!(matches!(db.insert(wide), Err(Error::ValueTooLarge)));
    assert!(db.is_empty());
    drop(db);
    cleanup(&p);
}

fn nested_arrays_body(depth: usize) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&1u32.to_le_bytes());
    body.extend_from_slice(&1u32.to_le_bytes());
    body.push(b'k');
    for _ in 0..depth {
        body.push(TAG_ARRAY);
        body.extend_from_slice(&1u32.to_le_bytes());
    }
    body.push(TAG_NULL);
    body
}

#[test]
fn deeply_nested_record_is_corrupt_not_a_stack_overflow() {
    let p = temp_path("deep");
    let mut bytes = header();
    bytes.extend(put_record(1, &nested_arrays_body(200_000)));
    std::fs::write(&p, &bytes).unwrap();
    let db = Db::open(&p).unwrap();
    assert!(matches!(db.get(DocId::from(1)), Err(Error::Corrupt(_))));
    assert!(matches!(
        db.find("other", &Value::Null),
        Err(Error::Corrupt(_))
    ));
    drop(db);
    cleanup(&p);
}

#[test]
fn nesting_at_the_limit_roundtrips_and_past_it_is_refused() {
    let p = temp_path("depthlimit");
    let nest = |depth: usize| {
        let mut v = Value::Null;
        for _ in 0..depth {
            v = Value::Array(vec![v]);
        }
        doc("k", v)
    };
    let mut db = Db::open(&p).unwrap();
    let ok = nest(bison_db::MAX_NESTING_DEPTH);
    let id = db.insert(ok.clone()).unwrap();
    assert_eq!(db.get(id).unwrap().unwrap(), ok);
    assert!(matches!(
        db.insert(nest(bison_db::MAX_NESTING_DEPTH + 1)),
        Err(Error::ValueTooLarge)
    ));
    drop(db);
    cleanup(&p);
}

#[test]
fn record_id_u64_max_is_corrupt_not_a_panic() {
    let p = temp_path("idmax");
    let mut bytes = header();
    bytes.extend(put_record(1, &int_doc_body("n", 111)));
    bytes.extend(put_record(u64::MAX, &int_doc_body("n", 222)));
    std::fs::write(&p, &bytes).unwrap();
    let r = std::panic::catch_unwind(|| Db::open(&p).map(|db| db.len()));
    assert!(
        matches!(r, Ok(Err(Error::Corrupt(_)))),
        "expected Corrupt, got {r:?}"
    );
    cleanup(&p);
}

// ---------------------------------------------------------------------------
// Write-path ordering and read verification
// ---------------------------------------------------------------------------

#[test]
fn failed_update_leaves_secondary_index_in_sync() {
    let p = temp_path("idxdesync");
    let mut db = Db::open(&p).unwrap();
    db.create_index("f").unwrap();
    let id = db.insert(doc("f", 1_i64)).unwrap();
    let mut big = Document::new();
    big.set("f", 2_i64)
        .set("blob", "x".repeat(bison_db::MAX_RECORD_BYTES + 1));
    assert!(matches!(db.update(id, big), Err(Error::ValueTooLarge)));
    assert_eq!(db.find("f", &Value::from(1_i64)).unwrap(), vec![id]);
    assert!(db.find("f", &Value::from(2_i64)).unwrap().is_empty());
    drop(db);
    cleanup(&p);
}

#[test]
fn get_verifies_the_record_checksum() {
    let p = temp_path("nocrc");
    let mut db = Db::open(&p).unwrap();
    let id = db.insert(doc("n", 12345_i64)).unwrap();
    db.insert(doc("n", 1_i64)).unwrap();
    db.flush().unwrap();
    // Body of record 1 starts at 16 + 8 + 9 = 33; the Int payload at 43.
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.seek(SeekFrom::Start(43)).unwrap();
        f.write_all(&[0x00, 0x00, 0x01]).unwrap();
        f.sync_all().unwrap();
    }
    assert!(matches!(db.get(id), Err(Error::Corrupt(_))));
    drop(db);
    cleanup(&p);
}

#[test]
fn query_order_does_not_depend_on_the_index() {
    let p = temp_path("order");
    let mut db = Db::open(&p).unwrap();
    for v in [5_i64, 3, 9, 1, 7, 3, 3, 8, 2, 6, 4, 0] {
        db.insert(doc("x", v)).unwrap();
    }
    let scan_range = db.range("x", Value::from(0_i64)..).unwrap();
    let scan_find = db.find("x", &Value::from(3_i64)).unwrap();
    db.create_index("x").unwrap();
    assert_eq!(db.range("x", Value::from(0_i64)..).unwrap(), scan_range);
    assert_eq!(db.find("x", &Value::from(3_i64)).unwrap(), scan_find);
    drop(db);
    cleanup(&p);
}

#[test]
fn torn_tail_inside_a_large_record_is_still_a_torn_tail() {
    let p = temp_path("tornbig");
    {
        let mut db = Db::open(&p).unwrap();
        db.insert(doc("n", 1_i64)).unwrap();
        // A 3 MiB body of small integers, so the scan over the torn tail sees
        // many plausible-looking lengths.
        let items: Vec<Value> = (0..300_000_i64).map(|i| Value::from(i % 40)).collect();
        db.insert(doc("blob", Value::Array(items))).unwrap();
    }
    let full = std::fs::read(&p).unwrap();
    let first_end = 16 + 35;
    // Cut the large record at several points: every one is a torn tail.
    for cut in [
        first_end + 5,
        first_end + 100,
        full.len() / 2,
        full.len() - 1,
    ] {
        std::fs::write(&p, &full[..cut]).unwrap();
        let db = Db::open(&p).unwrap();
        assert_eq!(db.len(), 1, "cut at {cut}");
        assert_eq!(db.discarded_bytes(), (cut - first_end) as u64);
        drop(db);
        assert_eq!(std::fs::metadata(&p).unwrap().len() as usize, first_end);
    }
    cleanup(&p);
}

#[test]
fn intact_record_with_an_unknown_op_is_corrupt_unless_salvaged() {
    use bison_db::RecoveryMode;
    let p = temp_path("badop");
    let mut bytes = header();
    bytes.extend(put_record(1, &int_doc_body("n", 1)));
    let mut bad = put_record(2, &int_doc_body("n", 2));
    bad[8] = 9; // unknown op tag, then restamp the checksum so the frame verifies
    let crc = crc32c(&bad[8..]);
    bad[4..8].copy_from_slice(&crc.to_le_bytes());
    bytes.extend(bad);
    bytes.extend(put_record(3, &int_doc_body("n", 3)));
    std::fs::write(&p, &bytes).unwrap();

    assert!(matches!(Db::open(&p), Err(Error::Corrupt(_))));
    let mut db = DbOptions::new()
        .recovery(RecoveryMode::Salvage)
        .open(&p)
        .unwrap();
    assert_eq!(db.len(), 2);
    assert!(db.get(DocId::from(3)).unwrap().is_some());
    assert_eq!(db.insert(doc("n", 4_i64)).unwrap(), DocId::from(4));
    drop(db);
    assert!(Db::open(&p).is_ok());
    cleanup(&p);
}

#[test]
fn files_written_by_1_0_still_open() {
    // A byte-exact 1.0 layout: two PUTs, an overwrite and a DELETE.
    let p = temp_path("compat");
    let mut bytes = header();
    bytes.extend(put_record(1, &int_doc_body("n", 1)));
    bytes.extend(put_record(2, &int_doc_body("n", 2)));
    bytes.extend(put_record(1, &int_doc_body("n", 10)));
    let mut tomb = vec![2u8];
    tomb.extend_from_slice(&2u64.to_le_bytes());
    bytes.extend_from_slice(&(tomb.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&crc32c(&tomb).to_le_bytes());
    bytes.extend(tomb);
    std::fs::write(&p, &bytes).unwrap();

    let mut db = Db::open(&p).unwrap();
    assert_eq!(db.len(), 1);
    assert_eq!(db.discarded_bytes(), 0);
    assert_eq!(
        db.get(DocId::from(1))
            .unwrap()
            .unwrap()
            .get("n")
            .unwrap()
            .as_int(),
        Some(10)
    );
    assert_eq!(db.insert(doc("n", 3_i64)).unwrap(), DocId::from(3));
    drop(db);
    assert_eq!(std::fs::read(&p).unwrap()[..bytes.len()], bytes[..]);
    cleanup(&p);
}
