//! Shared write harness for RABSody's mutating commands.
//!
//! Every write command (items update, delete, chapters, covers, embed, ...) is
//! built on this module so the safety disciplines are *enforced defaults*, not
//! per-command afterthoughts:
//!
//! - **Dry-run by default.** [`WriteContext`] does nothing to the server unless
//!   `--apply` was passed; otherwise each write reports [`WriteOutcome::WouldApply`].
//! - **Snapshot before write (fail-safe).** In apply mode the item's current
//!   state is backed up *before* the write; if the backup fails the write is
//!   aborted (see [`backup`]).
//! - **Append-only ledger (fail-open).** Every applied write (success or error)
//!   is recorded to a JSON Lines ledger for audit/future revert; a ledger
//!   failure is logged but never aborts the workflow (see [`ledger`]).
//! - **Consistent selection.** [`SelectionOpts`] (`--ids`/`--limit`) composes
//!   into any command via clap `flatten`.
//! - **Consistent preview.** [`preview`] formats one line per change.
//!
//! Concrete server mutations are supplied by the caller as a closure to
//! [`WriteContext::execute`]; the harness owns *when* and *whether* it runs, not
//! *how* (the `Client` write methods live in the command modules).

pub mod backup;
pub mod ledger;
pub mod preview;

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};

pub use backup::BackupStore;
pub use ledger::Ledger;

/// Item selection flags shared by every write command. Flatten into a command's
/// clap args with `#[command(flatten)]`.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct SelectionOpts {
    /// Restrict to these item IDs (repeat the flag or comma-separate).
    #[arg(long, value_delimiter = ',')]
    pub ids: Vec<String>,

    /// Process at most this many items (applied after `--ids`).
    #[arg(long)]
    pub limit: Option<usize>,
}

impl SelectionOpts {
    /// Filter `items` by `--ids` (if any) then truncate to `--limit` (if set).
    /// `id_of` extracts the comparable ID from each item. Order is preserved.
    pub fn select<T>(&self, items: Vec<T>, id_of: impl Fn(&T) -> &str) -> Vec<T> {
        let mut out: Vec<T> = if self.ids.is_empty() {
            items
        } else {
            let want: std::collections::HashSet<&str> =
                self.ids.iter().map(String::as_str).collect();
            items
                .into_iter()
                .filter(|it| want.contains(id_of(it)))
                .collect()
        };
        if let Some(limit) = self.limit {
            out.truncate(limit);
        }
        out
    }
}

/// CLI flags every write command flattens: the `--apply` opt-in plus selection.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct WriteOpts {
    /// Actually perform writes. Without it, the command previews only (dry-run).
    #[arg(long)]
    pub apply: bool,

    #[command(flatten)]
    pub selection: SelectionOpts,
}

/// What happened (or would happen) to a single item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOutcome {
    /// Apply mode: the write succeeded.
    Applied,
    /// Dry-run: nothing was sent; this is what would have been written.
    WouldApply,
    /// Intentionally not written (e.g. no effective change).
    Skipped(String),
    /// Apply mode: the write (or its pre-write backup) failed.
    Error(String),
}

impl WriteOutcome {
    /// Short lowercase label for previews and the ledger.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::WouldApply => "would-apply",
            Self::Skipped(_) => "skipped",
            Self::Error(_) => "error",
        }
    }
}

/// One proposed mutation: the harness backs up `before`, runs the write, and
/// records the pair. `operation` is a free-form tag (e.g. `"metadata"`,
/// `"progress"`, `"embed"`) so the ledger spans every future write type.
#[derive(Debug, Clone)]
pub struct WriteRequest {
    pub server: String,
    pub item_id: String,
    /// Human-readable label for the preview (e.g. the item title).
    pub label: String,
    pub operation: String,
    pub before: Value,
    pub after: Value,
}

/// One append-only ledger entry. Serialized as a single JSON line.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteRecord {
    /// Unix epoch seconds when the write was attempted.
    pub ts: u64,
    pub server: String,
    pub item_id: String,
    pub operation: String,
    pub before: Value,
    pub after: Value,
    /// Outcome label (`applied` / `error`); dry-run writes are never recorded.
    pub outcome: String,
}

/// Resolve `<data-dir>/rabsody` (e.g. `~/.local/share/rabsody`), falling back to
/// `<config-dir>/rabsody/data` where no data dir exists. Backups and the ledger
/// live under here, consistent with `config.rs`'s use of the `dirs` crate.
pub(crate) fn data_root() -> Result<PathBuf> {
    if let Some(dir) = dirs::data_dir() {
        return Ok(dir.join("rabsody"));
    }
    dirs::config_dir()
        .map(|c| c.join("rabsody").join("data"))
        .ok_or_else(|| Error::Config("could not resolve a data directory".to_string()))
}

/// Current Unix time in whole seconds (0 if the clock predates the epoch).
pub(crate) fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Current Unix time in milliseconds (0 if the clock predates the epoch). Used
/// for backup filenames, where second precision can collide within one second.
pub(crate) fn epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Owns write policy: apply-vs-dry-run, the backup store, and the ledger.
pub struct WriteContext {
    apply: bool,
    backup: BackupStore,
    ledger: Ledger,
}

impl WriteContext {
    /// Build from CLI flags, resolving the default data-dir backup/ledger paths.
    pub fn new(apply: bool) -> Result<Self> {
        Ok(Self {
            apply,
            backup: BackupStore::resolve()?,
            ledger: Ledger::resolve()?,
        })
    }

    /// Build with explicit stores (used by tests to point at a temp dir).
    #[cfg(test)]
    pub fn with_stores(apply: bool, backup: BackupStore, ledger: Ledger) -> Self {
        Self {
            apply,
            backup,
            ledger,
        }
    }

    /// True when `--apply` was given. Commands can branch on this for messaging.
    pub fn should_apply(&self) -> bool {
        self.apply
    }

    /// Run one write under harness policy.
    ///
    /// Dry-run: `Ok(WriteOutcome::WouldApply)` without touching the server, the
    /// backup store, or the ledger. Apply: snapshots `req.before` first and
    /// reports `Ok(WriteOutcome::Error)` if that backup fails (write aborted),
    /// then runs `write` and records the attempt to the ledger (a ledger failure
    /// is logged, not propagated).
    ///
    /// An **auth failure** (`Error::Auth`) from the write is returned as `Err`,
    /// not collapsed into a per-item `WriteOutcome::Error` string, so callers
    /// fail fast (non-zero exit) instead of masking a 401/403 as a transient,
    /// per-item error while the CLI still exits 0. Other write errors stay
    /// `Ok(WriteOutcome::Error)`.
    pub fn execute(
        &self,
        req: &WriteRequest,
        write: impl FnOnce() -> Result<()>,
    ) -> Result<WriteOutcome> {
        if !self.apply {
            return Ok(WriteOutcome::WouldApply);
        }

        // Fail-safe: never mutate the server if we couldn't capture a revertable
        // snapshot first.
        if let Err(e) = self
            .backup
            .save_snapshot(&req.server, &req.item_id, &req.before)
        {
            return Ok(WriteOutcome::Error(format!(
                "backup failed, write aborted: {e}"
            )));
        }

        let result = write();
        let outcome = match &result {
            Ok(()) => WriteOutcome::Applied,
            Err(e) => WriteOutcome::Error(e.to_string()),
        };

        // Fail-open: an unwritable ledger must not lose a write that already
        // happened, so record-failures are logged, not propagated.
        self.record(req, &outcome);

        // Fail-fast on auth so a 401/403 is never reported as a per-item error
        // with a successful process exit.
        match result {
            Err(e @ Error::Auth { .. }) => Err(e),
            _ => Ok(outcome),
        }
    }

    /// Append one ledger entry for `req`/`outcome` (fail-open).
    fn record(&self, req: &WriteRequest, outcome: &WriteOutcome) {
        let record = WriteRecord {
            ts: epoch_secs(),
            server: req.server.clone(),
            item_id: req.item_id.clone(),
            operation: req.operation.clone(),
            before: req.before.clone(),
            after: req.after.clone(),
            outcome: match outcome {
                WriteOutcome::Error(e) => format!("error: {e}"),
                other => other.label().to_string(),
            },
        };
        if let Err(e) = self.ledger.append(&record) {
            log::error!("ledger append failed (non-fatal): {e}");
        }
    }

    /// Run an *atomic* batch write under harness policy: one server call covers
    /// every item, so the per-item [`Self::execute`] path (one call each) would
    /// break atomicity. Dry-run returns all [`WriteOutcome::WouldApply`]. Apply
    /// snapshots every item first (aborting all if any backup fails), runs the
    /// single `write`, then ledgers every item with the shared outcome.
    pub fn execute_batch(
        &self,
        reqs: &[WriteRequest],
        write: impl FnOnce() -> Result<()>,
    ) -> Result<Vec<WriteOutcome>> {
        // Nothing to do: never invoke the write closure for an empty batch (it
        // would fire an empty server call and have nowhere to surface an error).
        if reqs.is_empty() {
            return Ok(Vec::new());
        }
        if !self.apply {
            return Ok(reqs.iter().map(|_| WriteOutcome::WouldApply).collect());
        }

        // Fail-safe: back up every item before the atomic write; if any snapshot
        // fails, abort the whole batch without calling the server.
        for req in reqs {
            if let Err(e) = self
                .backup
                .save_snapshot(&req.server, &req.item_id, &req.before)
            {
                let msg = format!("backup failed for {}, batch aborted: {e}", req.item_id);
                return Ok(reqs
                    .iter()
                    .map(|_| WriteOutcome::Error(msg.clone()))
                    .collect());
            }
        }

        let result = write();
        let outcome = match &result {
            Ok(()) => WriteOutcome::Applied,
            Err(e) => WriteOutcome::Error(e.to_string()),
        };

        for req in reqs {
            self.record(req, &outcome);
        }

        // Fail-fast on auth (see `execute`); otherwise report the shared outcome
        // for every item in the batch.
        match result {
            Err(e @ Error::Auth { .. }) => Err(e),
            _ => Ok(reqs.iter().map(|_| outcome.clone()).collect()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn req(id: &str) -> WriteRequest {
        WriteRequest {
            server: "https://abs.example".to_string(),
            item_id: id.to_string(),
            label: "Some Book".to_string(),
            operation: "metadata".to_string(),
            before: serde_json::json!({"title": "old"}),
            after: serde_json::json!({"title": "new"}),
        }
    }

    fn temp_ctx(apply: bool, dir: &std::path::Path) -> WriteContext {
        WriteContext::with_stores(
            apply,
            BackupStore::with_dir(dir.join("backups")),
            Ledger::with_path(dir.join("ledger.jsonl")),
        )
    }

    #[test]
    fn select_filters_by_ids_then_limit() {
        let sel = SelectionOpts {
            ids: vec!["b".into(), "d".into()],
            limit: None,
        };
        let got = sel.select(vec!["a", "b", "c", "d"], |s| s);
        assert_eq!(got, vec!["b", "d"]);

        let sel = SelectionOpts {
            ids: vec![],
            limit: Some(2),
        };
        assert_eq!(sel.select(vec!["a", "b", "c"], |s| s), vec!["a", "b"]);

        // ids first, then limit
        let sel = SelectionOpts {
            ids: vec!["a".into(), "b".into(), "c".into()],
            limit: Some(2),
        };
        assert_eq!(sel.select(vec!["a", "b", "c"], |s| s), vec!["a", "b"]);

        // empty ids = no id filter; missing limit = all
        let sel = SelectionOpts::default();
        assert_eq!(sel.select(vec!["a", "b"], |s| s), vec!["a", "b"]);
    }

    #[test]
    fn dry_run_does_not_invoke_write_or_persist() {
        let dir = std::env::temp_dir().join(format!("rabsody-h-dry-{}", std::process::id()));
        let ctx = temp_ctx(false, &dir);
        let called = Cell::new(false);
        let outcome = ctx
            .execute(&req("1"), || {
                called.set(true);
                Ok(())
            })
            .unwrap();
        assert_eq!(outcome, WriteOutcome::WouldApply);
        assert!(!called.get(), "write callback must not run in dry-run");
        // Nothing persisted.
        assert!(ctx.ledger.read_all().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn apply_runs_write_backs_up_and_ledgers() {
        let dir = std::env::temp_dir().join(format!("rabsody-h-apply-{}", std::process::id()));
        let ctx = temp_ctx(true, &dir);
        let called = Cell::new(false);
        let outcome = ctx
            .execute(&req("42"), || {
                called.set(true);
                Ok(())
            })
            .unwrap();
        assert_eq!(outcome, WriteOutcome::Applied);
        assert!(called.get());

        // Backup snapshot written for the item.
        let backups = ctx.backup.list_backups().unwrap();
        assert_eq!(backups.len(), 1);

        // Ledger recorded the applied write with before/after.
        let records = ctx.ledger.read_all().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].item_id, "42");
        assert_eq!(records[0].outcome, "applied");
        assert_eq!(records[0].before, serde_json::json!({"title": "old"}));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn apply_records_write_error_in_ledger() {
        let dir = std::env::temp_dir().join(format!("rabsody-h-err-{}", std::process::id()));
        let ctx = temp_ctx(true, &dir);
        let outcome = ctx
            .execute(&req("7"), || {
                Err(Error::Http {
                    status: 500,
                    body: "boom".to_string(),
                })
            })
            .unwrap();
        assert!(matches!(outcome, WriteOutcome::Error(_)));
        let records = ctx.ledger.read_all().unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].outcome.starts_with("error:"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn apply_propagates_auth_error_for_fail_fast() {
        let dir = std::env::temp_dir().join(format!("rabsody-h-auth-{}", std::process::id()));
        let ctx = temp_ctx(true, &dir);
        // A 401/403 must surface as Err (so the CLI exits non-zero), not as an
        // Ok(WriteOutcome::Error) that the caller could print and still exit 0.
        let result = ctx.execute(&req("9"), || Err(Error::Auth { status: 401 }));
        assert!(matches!(result, Err(Error::Auth { status: 401 })));
        // The attempt is still recorded in the ledger before failing fast.
        assert_eq!(ctx.ledger.read_all().unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }
}
