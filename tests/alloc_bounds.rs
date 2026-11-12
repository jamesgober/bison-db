//! Length fields in a stored record must not drive allocations far larger than
//! the record itself. A counting allocator measures the peak extra heap a
//! `get` uses while decoding a record whose nested arrays each claim
//! `u32::MAX` elements.

#![cfg(feature = "std")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use bison_db::{Db, DocId, Error};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// Refuse any single request above this, so a regression fails the test with
/// an allocation error instead of committing gigabytes on the test machine.
const REFUSE_ABOVE: usize = 1 << 30;

// SAFETY: forwards to the system allocator; only adds bookkeeping.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.size() > REFUSE_ABOVE {
            return std::ptr::null_mut();
        }
        let now = LIVE.fetch_add(layout.size(), Relaxed) + layout.size();
        PEAK.fetch_max(now, Relaxed);
        // SAFETY: same layout contract as the caller's.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Relaxed);
        // SAFETY: `ptr` came from `System.alloc` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

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

fn store_with_body(tag: &str, body: &[u8]) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("bison_db_alloc_{tag}_{}.bison", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let mut bytes = b"BISONDB1".to_vec();
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&[0u8; 6]);
    let mut payload = vec![1u8];
    payload.extend_from_slice(&1u64.to_le_bytes());
    payload.extend_from_slice(body);
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&crc32c(&payload).to_le_bytes());
    bytes.extend_from_slice(&payload);
    std::fs::write(&p, &bytes).unwrap();
    p
}

fn remove(p: &std::path::Path) {
    let _ = std::fs::remove_file(p);
    let mut lock = p.as_os_str().to_os_string();
    lock.push(".lock");
    let _ = std::fs::remove_file(lock);
}

/// One test function, so the two measurements never overlap in time.
#[test]
fn hostile_lengths_do_not_amplify_allocations() {
    // 100 nested arrays, each claiming u32::MAX elements, in a 64 KiB record.
    let mut body = Vec::new();
    body.extend_from_slice(&1u32.to_le_bytes());
    body.extend_from_slice(&0u32.to_le_bytes());
    for _ in 0..100 {
        body.push(6);
        body.extend_from_slice(&u32::MAX.to_le_bytes());
    }
    body.resize(64 * 1024, 0xEE); // 0xEE is an unknown tag: decoding fails here
    let p = store_with_body("amp", &body);
    let db = Db::open(&p).unwrap();
    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    let r = db.get(DocId::from(1));
    let peak = PEAK.load(Relaxed).saturating_sub(base);
    assert!(matches!(r, Err(Error::Corrupt(_))));
    assert!(
        peak < 16 * 1024 * 1024,
        "decoding a 64 KiB record held {peak} extra bytes"
    );
    drop(db);
    remove(&p);

    // A field count of u32::MAX in a small record.
    let mut body = Vec::new();
    body.extend_from_slice(&u32::MAX.to_le_bytes());
    body.resize(1024 * 1024, 0xEE); // the first key length overruns the record
    let p = store_with_body("fields", &body);
    let db = Db::open(&p).unwrap();
    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    let r = db.get(DocId::from(1));
    let peak = PEAK.load(Relaxed).saturating_sub(base);
    assert!(matches!(r, Err(Error::Corrupt(_))));
    assert!(
        peak < 4 * 1024 * 1024,
        "decoding a 1 MiB record held {peak} extra bytes"
    );
    drop(db);
    remove(&p);
}
