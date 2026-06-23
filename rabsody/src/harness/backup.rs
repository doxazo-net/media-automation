//! Pre-write snapshots. Before any applied write, the item's current JSON is
//! saved here so a future `revert` can restore it. Snapshots are plain JSON
//! files named `{epoch}_{server}_{itemId}.json` under `<data-dir>/rabsody/backups/`.

use std::path::PathBuf;

use serde_json::Value;

use super::{data_root, epoch_secs};
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
    /// backup directory on first use. Returns the file written.
    pub fn save_snapshot(&self, server: &str, item_id: &str, item: &Value) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.dir).map_err(|e| {
            Error::Config(format!("creating backup dir {}: {e}", self.dir.display()))
        })?;
        let name = format!(
            "{}_{}_{}.json",
            epoch_secs(),
            sanitize(server),
            sanitize(item_id)
        );
        let path = self.dir.join(name);
        let body = serde_json::to_string_pretty(item)
            .map_err(|e| Error::Config(format!("serializing backup: {e}")))?;
        std::fs::write(&path, body)
            .map_err(|e| Error::Config(format!("writing backup {}: {e}", path.display())))?;
        Ok(path)
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
        let dir = std::env::temp_dir().join(format!("rabs-bk-{}", std::process::id()));
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
    fn list_backups_empty_when_dir_absent() {
        let store = BackupStore::with_dir(
            std::env::temp_dir().join(format!("rabs-bk-none-{}", std::process::id())),
        );
        assert!(store.list_backups().unwrap().is_empty());
    }
}
