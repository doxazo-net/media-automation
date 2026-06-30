//! Append-only change ledger. Every applied write (success or error) is
//! recorded as one JSON line in `<data-dir>/rabsody/ledger.jsonl` for audit and
//! future revert. Ledger writes are fail-open: a failure here is logged by the
//! caller but never aborts a write that already happened.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use super::{WriteRecord, data_root};
use crate::error::{Error, Result};

/// Append-only JSON Lines ledger.
pub struct Ledger {
    path: PathBuf,
}

impl Ledger {
    /// Default location: `<data-dir>/rabsody/ledger.jsonl`.
    pub fn resolve() -> Result<Self> {
        Ok(Self {
            path: data_root()?.join("ledger.jsonl"),
        })
    }

    /// Construct against an explicit file path (used by tests).
    #[cfg(test)]
    pub fn with_path(path: PathBuf) -> Self {
        Self { path }
    }

    /// Append one record as a JSON line, creating the file/parent on first use.
    pub fn append(&self, record: &WriteRecord) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::Config(format!("creating ledger dir {}: {e}", parent.display()))
            })?;
        }
        let line = serde_json::to_string(record)
            .map_err(|e| Error::Config(format!("serializing ledger record: {e}")))?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| Error::Config(format!("opening ledger {}: {e}", self.path.display())))?;
        writeln!(f, "{line}")
            .map_err(|e| Error::Config(format!("appending to ledger {}: {e}", self.path.display())))
    }

    /// Read and parse every record (for a future audit/revert). Empty if the
    /// file does not exist; blank lines are skipped.
    /// Wired by the future `revert`/audit command; exercised by tests now.
    #[allow(dead_code)]
    pub fn read_all(&self) -> Result<Vec<WriteRecord>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = std::fs::File::open(&self.path)
            .map_err(|e| Error::Config(format!("opening ledger {}: {e}", self.path.display())))?;
        let mut out = Vec::new();
        for line in std::io::BufReader::new(file).lines() {
            let line = line.map_err(|e| Error::Config(format!("reading ledger: {e}")))?;
            if line.trim().is_empty() {
                continue;
            }
            let rec = serde_json::from_str(&line)
                .map_err(|e| Error::Config(format!("parsing ledger line: {e}")))?;
            out.push(rec);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str) -> WriteRecord {
        WriteRecord {
            ts: 1_700_000_000,
            server: "https://abs.example".to_string(),
            item_id: id.to_string(),
            operation: "metadata".to_string(),
            before: serde_json::json!({"title": "old"}),
            after: serde_json::json!({"title": "new"}),
            outcome: "applied".to_string(),
        }
    }

    #[test]
    fn append_then_read_all_preserves_order() {
        let path = std::env::temp_dir().join(format!("rabsody-ldg-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let ledger = Ledger::with_path(path.clone());

        ledger.append(&record("a")).unwrap();
        ledger.append(&record("b")).unwrap();

        let all = ledger.read_all().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].item_id, "a");
        assert_eq!(all[1].item_id, "b");
        assert_eq!(all[0].after, serde_json::json!({"title": "new"}));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_all_empty_when_absent() {
        let ledger = Ledger::with_path(
            std::env::temp_dir().join(format!("rabsody-ldg-none-{}.jsonl", std::process::id())),
        );
        assert!(ledger.read_all().unwrap().is_empty());
    }
}
