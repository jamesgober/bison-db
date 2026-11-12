//! The exclusive open lock: one live [`Db`](crate::Db) handle per store file.
//!
//! Every handle caches the tail offset and the id counter from its own replay,
//! so two handles on one file would append over each other. [`StoreLock`]
//! makes the second open fail with [`Error::Locked`] instead, whether the
//! first handle lives in the same process or another one.
//!
//! The lock is a sibling file, `<store>.lock`. File locking proper
//! (`File::try_lock`) entered `std` in Rust 1.89, above this crate's MSRV, and
//! the crate adds no dependency and no `unsafe` to reach `flock` or
//! `LockFileEx` directly. So the mechanism is what `std` offers on each
//! platform:
//!
//! - **Windows**: the lock file is opened with a share mode of zero, which the
//!   OS enforces against every other open of it. The OS drops the handle when
//!   the process exits for any reason, so a crash never leaves a stale lock.
//! - **Unix**: the lock file is created with `create_new` (atomic
//!   `O_CREAT | O_EXCL`) and records the holder: process id, process start
//!   time, host name and PID namespace. A later open that finds the file checks
//!   the holder. On Linux, `/proc` shows whether that process still exists with
//!   the same start time; if it does not, the lock is stale and is replaced. A
//!   holder on another host or in another PID namespace cannot be checked, so
//!   its lock is honoured. Unix systems without `/proc` (macOS, the BSDs)
//!   cannot check the holder either: a lock left by a crash there must be
//!   removed by hand once no process is using the store. Clearing a stale
//!   lock is a read followed by a delete, not one atomic step, so two
//!   processes that both start in the same instant after a crash could both
//!   clear it; outside that window the lock holds.
//!
//! The lock is advisory: it stops other bison-db handles, not other programs
//! that write to the file directly.

use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// The sibling path of the lock file for `store`: `<store>.lock`.
pub(crate) fn lock_path(store: &Path) -> PathBuf {
    let mut name = store.as_os_str().to_os_string();
    name.push(".lock");
    PathBuf::from(name)
}

/// An acquired store lock, released on drop.
pub(crate) struct StoreLock {
    path: PathBuf,
    /// Windows: the open, unshared handle that is the lock itself.
    #[cfg(windows)]
    handle: Option<File>,
    /// Unix: the identity written into the lock file, compared on release so a
    /// handle never removes a lock it does not own.
    #[cfg(unix)]
    token: String,
}

impl StoreLock {
    /// Takes the lock for the store at `store`, or fails with
    /// [`Error::Locked`] if another live handle holds it.
    pub(crate) fn acquire(store: &Path) -> Result<StoreLock> {
        acquire_impl(lock_path(store))
    }
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

/// `ERROR_SHARING_VIOLATION`: another handle has the file open unshared.
#[cfg(windows)]
const ERROR_SHARING_VIOLATION: i32 = 32;
/// `ERROR_LOCK_VIOLATION`: a region of the file is locked by another handle.
#[cfg(windows)]
const ERROR_LOCK_VIOLATION: i32 = 33;

#[cfg(windows)]
fn acquire_impl(path: PathBuf) -> Result<StoreLock> {
    use std::os::windows::fs::OpenOptionsExt;
    let opened = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(&path);
    match opened {
        Ok(mut file) => {
            // The holder's id is informational only; the handle is the lock.
            let _ = file.set_len(0);
            let _ = writeln!(file, "bison-db lock v1\npid {}", std::process::id());
            Ok(StoreLock {
                path,
                handle: Some(file),
            })
        }
        Err(e)
            if matches!(
                e.raw_os_error(),
                Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
            ) =>
        {
            Err(Error::Locked)
        }
        Err(e) => Err(Error::Io(e)),
    }
}

#[cfg(windows)]
impl Drop for StoreLock {
    fn drop(&mut self) {
        drop(self.handle.take());
        // Best effort: if another opener already holds the file again, the
        // removal fails with a sharing violation and its lock stays intact.
        let _ = std::fs::remove_file(&self.path);
    }
}

// ---------------------------------------------------------------------------
// Unix
// ---------------------------------------------------------------------------

/// First line of every lock file, so a foreign file at the path is not taken
/// for a lock.
#[cfg(unix)]
const LOCK_MAGIC: &str = "bison-db lock v1";

/// A lock file that cannot be parsed is treated as being written by a live
/// opener until it is this old; after that it is a leftover of a crash between
/// creating and filling the file.
#[cfg(unix)]
const UNPARSEABLE_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// Who holds a lock, as recorded in the lock file.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct Holder {
    pid: u32,
    /// Process start time in clock ticks since boot (Linux), or 0 when unknown.
    start: u64,
    host: String,
    pidns: String,
}

#[cfg(unix)]
impl Holder {
    fn current() -> Holder {
        let pid = std::process::id();
        Holder {
            pid,
            start: process_start(pid).unwrap_or(0),
            host: read_trimmed("/proc/sys/kernel/hostname"),
            pidns: std::fs::read_link("/proc/self/ns/pid")
                .map(|l| l.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }

    fn encode(&self) -> String {
        format!(
            "{LOCK_MAGIC}\npid {}\nstart {}\nhost {}\npidns {}\n",
            self.pid, self.start, self.host, self.pidns
        )
    }

    fn parse(text: &str) -> Option<Holder> {
        let mut lines = text.lines();
        if lines.next()? != LOCK_MAGIC {
            return None;
        }
        let mut field = |name: &str| -> Option<String> {
            let line = lines.next()?;
            let rest = line.strip_prefix(name)?;
            Some(rest.trim_start_matches(' ').to_string())
        };
        let pid = field("pid")?.parse().ok()?;
        let start = field("start")?.parse().ok()?;
        let host = field("host")?;
        let pidns = field("pidns")?;
        Some(Holder {
            pid,
            start,
            host,
            pidns,
        })
    }

    /// Whether this recorded holder is known to be gone. Anything that cannot
    /// be checked counts as alive, so the lock errs on the side of refusing.
    fn is_known_dead(&self, me: &Holder) -> bool {
        if self.host != me.host || self.pidns != me.pidns {
            return false;
        }
        if !cfg!(any(target_os = "linux", target_os = "android")) {
            return false;
        }
        if self.pid == me.pid {
            // This process: another handle is open, unless the lock is from an
            // earlier process that had the same id.
            return self.start != 0 && me.start != 0 && self.start != me.start;
        }
        match std::fs::metadata(format!("/proc/{}", self.pid)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => true,
            Err(_) => false,
            Ok(_) => match process_start(self.pid) {
                Some(now) => self.start != 0 && now != self.start,
                None => false,
            },
        }
    }
}

#[cfg(unix)]
fn read_trimmed(path: &str) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// The start time of process `pid` in clock ticks since boot, from field 22 of
/// `/proc/<pid>/stat`. `None` where procfs is absent or the process is gone.
#[cfg(unix)]
fn process_start(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name (field 2) is parenthesised and may contain spaces, so
    // count fields from the last ')'. Field 3 is the first after it.
    let after = &stat[stat.rfind(')')? + 1..];
    after.split_whitespace().nth(22 - 3)?.parse().ok()
}

#[cfg(unix)]
fn acquire_impl(path: PathBuf) -> Result<StoreLock> {
    use std::os::unix::fs::OpenOptionsExt;
    let me = Holder::current();
    let token = me.encode();
    // A few rounds cover a stale lock being cleared or a lock released
    // between our create attempt and our read of it.
    for _ in 0..4 {
        let created = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&path);
        match created {
            Ok(file) => {
                if let Err(e) = fill_lock(file, &token) {
                    let _ = std::fs::remove_file(&path);
                    return Err(Error::Io(e));
                }
                return Ok(StoreLock { path, token });
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if !existing_lock_is_stale(&path, &me)? {
                    return Err(Error::Locked);
                }
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(Error::Io(e)),
                }
            }
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Err(Error::Locked)
}

#[cfg(unix)]
fn fill_lock(mut file: File, token: &str) -> io::Result<()> {
    file.write_all(token.as_bytes())?;
    file.sync_all()
}

/// Reads the lock file at `path` and decides whether its holder is gone.
#[cfg(unix)]
fn existing_lock_is_stale(path: &Path, me: &Holder) -> Result<bool> {
    let text = match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        // Released between our create attempt and this read. Reporting it as
        // stale makes the caller retry the create; removing a missing file is
        // a no-op.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(Error::Io(e)),
    };
    match Holder::parse(&text) {
        Some(holder) => Ok(holder.is_known_dead(me)),
        None => {
            if !text.is_empty() && !text.starts_with(LOCK_MAGIC) {
                // Not a bison-db lock file: leave it alone.
                return Ok(false);
            }
            // Empty or half-written: a live opener is mid-write, or one crashed
            // right after creating the file. Age tells them apart.
            let age = std::fs::metadata(path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok());
            Ok(age.is_some_and(|a| a > UNPARSEABLE_GRACE))
        }
    }
}

#[cfg(unix)]
impl Drop for StoreLock {
    fn drop(&mut self) {
        // Remove the file only while it still names this handle.
        if let Ok(text) = std::fs::read_to_string(&self.path) {
            if text == self.token {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_holder_roundtrips_through_text() {
        let me = Holder::current();
        assert_eq!(Holder::parse(&me.encode()), Some(me));
    }

    #[test]
    fn test_current_process_is_not_dead() {
        let me = Holder::current();
        assert!(!me.is_known_dead(&Holder::current()));
    }

    #[test]
    fn test_foreign_host_is_never_judged_dead() {
        let me = Holder::current();
        let other = Holder {
            pid: u32::MAX,
            start: 1,
            host: format!("{}-elsewhere", me.host),
            pidns: me.pidns.clone(),
        };
        assert!(!other.is_known_dead(&me));
    }
}
