//! Single-instance advisory lock for `enrich` runs (issue #256).
//!
//! Scheduled/cron `enrich` invocations that overlap would hit the same store and
//! the same remote advisory APIs at once - duplicating API traffic (risking a
//! rate-limit / IP block) and contending on SQLite writes. This wraps a run in an
//! OS advisory file lock so a second instance detects the first and skips.
//!
//! The lock is an exclusive `flock`/`LockFileEx` on a lockfile derived from the
//! store path, via std-native `File::lock`/`try_lock` (stable since Rust 1.89).
//! The OS releases the lock when the file descriptor closes - on drop OR on
//! process exit (crash, kill, OOM) - so, unlike a PID file, a crash never leaves
//! a stale lock the next run has to reap.
//!
//! Scope and caveats:
//! - Only *store-writing* runs take the lock (the scheduled/cron path). The
//!   report-only calibration mode is manual and intentionally unlocked, so it
//!   can still issue remote API calls concurrently - the write path is what the
//!   lock serializes.
//! - The lock is keyed on the store *path string*: two configs naming the same
//!   physical file via different strings (relative vs absolute) get different
//!   lockfiles and would not mutually exclude. The store's SQLite `busy_timeout`
//!   is the write-serialization backstop for that case.
//! - `flock` on a network filesystem (NFS/SMB) may be a no-op; the intended
//!   deployment keeps the store on local disk next to the binary.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

/// Held for the duration of an enrich run; releases the OS lock on drop.
#[derive(Debug)]
pub struct EnrichLock {
    // The lock lives with the open file descriptor: dropping the file closes the
    // fd, which releases the advisory lock. Never read directly - held for RAII.
    _file: File,
    path: PathBuf,
}

impl EnrichLock {
    /// The lockfile path this guard holds (for logging).
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Derive the lockfile path from the store path: `<store>.lock`. Keyed on the
/// store so two runs against *different* stores never block each other.
pub fn lock_path_for_store(store_path: &Path) -> PathBuf {
    let mut s = store_path.as_os_str().to_os_string();
    s.push(".lock");
    PathBuf::from(s)
}

/// Try to acquire the enrich lock for `store_path`.
///
/// - `wait == false` (default): non-blocking. Returns `Ok(None)` when another
///   instance already holds the lock - the caller should skip this run and exit
///   successfully (a scheduled overlap is not an error).
/// - `wait == true`: block until the lock can be acquired.
///
/// The returned guard must be held for the whole run; dropping it releases the
/// lock.
pub fn acquire(store_path: &Path, wait: bool) -> io::Result<Option<EnrichLock>> {
    let path = lock_path_for_store(store_path);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)?;
    if wait {
        file.lock()?;
    } else {
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(e)) => return Err(e),
        }
    }
    Ok(Some(EnrichLock { _file: file, path }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_path_appends_lock_suffix() {
        let p = lock_path_for_store(Path::new("/data/smpr-sources.db"));
        assert_eq!(p, PathBuf::from("/data/smpr-sources.db.lock"));
    }

    #[test]
    fn second_nonblocking_acquire_is_skipped_then_freed_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("s.db");

        let first = acquire(&store, false).unwrap();
        assert!(first.is_some(), "first acquire should succeed");

        // A second non-blocking acquire while the first is held must report the
        // contention as Ok(None) (skip), not error and not falsely acquire.
        let second = acquire(&store, false).unwrap();
        assert!(
            second.is_none(),
            "second acquire must be skipped while held"
        );

        // Dropping the first releases the OS lock; a fresh acquire then succeeds.
        drop(first);
        let third = acquire(&store, false).unwrap();
        assert!(third.is_some(), "acquire should succeed after release");
    }

    #[test]
    fn distinct_stores_do_not_contend() {
        let dir = tempfile::tempdir().unwrap();
        let a = acquire(&dir.path().join("a.db"), false).unwrap();
        let b = acquire(&dir.path().join("b.db"), false).unwrap();
        assert!(
            a.is_some() && b.is_some(),
            "locks on different stores are independent"
        );
    }

    #[test]
    fn acquire_creates_missing_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("does/not/exist/s.db");
        let guard = acquire(&nested, false).unwrap();
        assert!(guard.is_some());
        assert!(lock_path_for_store(&nested).exists());
    }
}
