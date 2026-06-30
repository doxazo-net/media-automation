//! Pre-write snapshots. Before any applied write, the item's current JSON is
//! saved here so a future `revert` can restore it. Snapshots are plain JSON
//! files named `{epochMillis}_{server}_{itemId}.json` under
//! `<data-dir>/rabsody/backups/` (with a `-N` suffix on the rare same-millis
//! collision), so repeated snapshots of one item never overwrite each other.

use std::path::PathBuf;

use serde_json::Value;

use super::{data_root, epoch_millis};
use crate::error::{Error, Result};

/// Manages the backup directory and snapshot files.
pub struct BackupStore {
    dir: PathBuf,
}

impl BackupStore {
    /// Default location: `<data-dir>/rabsody/backups/`.
    pub fn resolve() -> Result<Self> {
        Ok(Self {
            dir: data_root()?.join("backups"),
        })
    }

    /// Construct against an explicit directory (used by tests).
    #[cfg(test)]
    pub fn with_dir(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Snapshot `item` (its full current JSON) before a write. Creates the
    /// backup directory on first use. Returns the file written. The filename is
    /// millisecond-stamped with a `-N` suffix on collision, and each candidate
    /// is opened with `create_new` (atomic create-or-fail), so concurrent
    /// writers can never clobber each other's snapshot - no TOCTOU window.
    pub fn save_snapshot(&self, server: &str, item_id: &str, item: &Value) -> Result<PathBuf> {
        use std::io::Write;

        std::fs::create_dir_all(&self.dir).map_err(|e| {
            Error::Config(format!("creating backup dir {}: {e}", self.dir.display()))
        })?;
        let body = serde_json::to_string_pretty(item)
            .map_err(|e| Error::Config(format!("serializing backup: {e}")))?;
        let base = format!(
            "{}_{}_{}",
            epoch_millis(),
            sanitize(server),
            sanitize(item_id)
        );

        let mut n = 0u32;
        loop {
            let path = if n == 0 {
                self.dir.join(format!("{base}.json"))
            } else {
                self.dir.join(format!("{base}-{n}.json"))
            };
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    f.write_all(body.as_bytes()).map_err(|e| {
                        Error::Config(format!("writing backup {}: {e}", path.display()))
                    })?;
                    return Ok(path);
                }
                // Name taken (possibly by a concurrent writer): try the next suffix.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => {
                    return Err(Error::Config(format!(
                        "creating backup {}: {e}",
                        path.display()
                    )));
                }
            }
        }
    }

    /// All snapshot files (`*.json`), sorted (the `{epoch}` prefix makes this
    /// chronological). Returns empty if the directory does not exist yet.
    /// Wired by the future `revert` command; exercised by tests now.
    #[allow(dead_code)]
    pub fn list_backups(&self) -> Result<Vec<PathBuf>> {
        if !self.dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let entries = std::fs::read_dir(&self.dir).map_err(|e| {
            Error::Config(format!("reading backup dir {}: {e}", self.dir.display()))
        })?;
        for entry in entries {
            let path = entry
                .map_err(|e| Error::Config(format!("reading backup entry: {e}")))?
                .path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                out.push(path);
            }
        }
        out.sort();
        Ok(out)
    }

    /// Read back a snapshot by file name (for a future `revert`).
    #[allow(dead_code)]
    pub fn get_backup(&self, name: &str) -> Result<Value> {
        let path = self.dir.join(name);
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| Error::Config(format!("reading backup {}: {e}", path.display())))?;
        serde_json::from_str(&raw)
            .map_err(|e| Error::Config(format!("parsing backup {}: {e}", path.display())))
    }
}

/// Make a string safe for a filename: keep `[A-Za-z0-9._-]`, replace anything
/// else (slashes, colons in a server URL, etc.) with `_`.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_path_and_url_chars() {
        assert_eq!(
            sanitize("https://abs.example:443"),
            "https___abs.example_443"
        );
        assert_eq!(sanitize("li_abc-123"), "li_abc-123");
    }

    #[test]
    fn save_list_get_round_trip() {
        let dir = std::env::temp_dir().join(format!("rabsody-bk-{}", std::process::id()));
        let store = BackupStore::with_dir(dir.clone());
        let item = serde_json::json!({"id": "li_1", "media": {"metadata": {"title": "T"}}});
        let path = store
            .save_snapshot("https://abs.example", "li_1", &item)
            .unwrap();
        assert!(path.exists());

        let backups = store.list_backups().unwrap();
        assert_eq!(backups.len(), 1);

        let name = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(store.get_backup(name).unwrap(), item);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn repeated_snapshots_do_not_overwrite() {
        let dir = std::env::temp_dir().join(format!("rabsody-bk-dup-{}", std::process::id()));
        let store = BackupStore::with_dir(dir.clone());
        let a = serde_json::json!({"v": 1});
        let b = serde_json::json!({"v": 2});
        // Same server/item, back-to-back (same second, likely same millisecond).
        let p1 = store.save_snapshot("s", "li_1", &a).unwrap();
        let p2 = store.save_snapshot("s", "li_1", &b).unwrap();
        assert_ne!(p1, p2, "two snapshots must be distinct files");
        assert_eq!(store.list_backups().unwrap().len(), 2);
        // Both snapshots preserved (no clobber).
        assert_eq!(
            std::fs::read_to_string(&p1).unwrap().contains("\"v\": 1"),
            true
        );
        assert_eq!(
            std::fs::read_to_string(&p2).unwrap().contains("\"v\": 2"),
            true
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn concurrent_snapshots_never_clobber() {
        use std::sync::Arc;
        let dir = std::env::temp_dir().join(format!("rabsody-bk-conc-{}", std::process::id()));
        let store = Arc::new(BackupStore::with_dir(dir.clone()));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let s = Arc::clone(&store);
                std::thread::spawn(move || {
                    s.save_snapshot("s", "li_1", &serde_json::json!({ "i": i }))
                        .unwrap()
                })
            })
            .collect();
        let paths: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let distinct: std::collections::HashSet<_> = paths.iter().collect();
        assert_eq!(
            distinct.len(),
            8,
            "every concurrent snapshot is a distinct file"
        );
        assert_eq!(store.list_backups().unwrap().len(), 8);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_backups_empty_when_dir_absent() {
        let store = BackupStore::with_dir(
            std::env::temp_dir().join(format!("rabsody-bk-none-{}", std::process::id())),
        );
        assert!(store.list_backups().unwrap().is_empty());
    }
}
