//! `rabsody items embed-metadata` / `batch-embed-metadata` - write the item's
//! ABS metadata into its audio file(s) via `POST /api/tools/.../embed-metadata`.
//!
//! Incident-driven (2026-06-21: an 869-item batch filled the Unraid VMs pool via
//! per-item backups). The safety model, in order of importance:
//!
//! 1. **Backup off by default.** ABS only backs up originals when `?backup=1`;
//!    RABSody defaults to no backup, so the disk-filling behavior never happens
//!    unless you opt in with `--backup`.
//! 2. **Serialized, not fired in bulk.** `batch-embed-metadata` embeds items one
//!    at a time, waiting for each server task to drain (via [`TaskPoller`]) before
//!    the next - it does NOT call ABS's `/tools/batch/embed-metadata`, which
//!    queues everything at once (the incident behavior).
//! 3. **Disk-headroom guard.** When `--backup` is set, RABSody requires a
//!    configured `[cache].dataPath`, checks free space before each item, and
//!    aborts below `--min-free`. It also purges the items cache every
//!    `--purge-every` items as defense-in-depth.
//!
//! All paths run through the write harness: dry-run unless `--apply`, ledgered.

use std::path::PathBuf;
use std::time::Duration;

use crate::api::{self, Client};
use crate::cache;
use crate::config::Credentials;
use crate::error::{Error, Result};
use crate::harness::{SelectionOpts, WriteContext, WriteOpts, WriteOutcome, WriteRequest, preview};
use crate::tasks::{TaskPoller, WaitResult};

/// Default minimum free space required under `--backup` (2 GiB).
const DEFAULT_MIN_FREE: u64 = 2 * 1024 * 1024 * 1024;
/// Default cadence for the defense-in-depth items-cache purge.
const DEFAULT_PURGE_EVERY: usize = 50;
/// Per-item embed task budget (large files can take a while to mux).
const TASK_TIMEOUT: Duration = Duration::from_secs(600);
const TASK_INTERVAL: Duration = Duration::from_secs(2);

/// `items embed-metadata <id>` - embed one item (dry-run unless `--apply`).
pub fn run_embed(id: String, backup: bool, force_chapters: bool, apply: bool) -> Result<()> {
    let client = api::client_only()?;
    let current = client.item_get_raw(&id)?;
    let req = embed_request(client.server(), &id, &current, backup);

    let ctx = WriteContext::new(apply)?;
    let outcome = ctx.execute(&req, || embed_one(&client, &id, backup, force_chapters))?;
    println!("{}", preview::format_line(&req, &outcome));
    if !ctx.should_apply() {
        println!("(dry-run; re-run with --apply to embed)");
    }
    Ok(())
}

/// `items batch-embed-metadata` - embed many items serially with the disk guard.
pub fn run_batch_embed(
    file: Option<String>,
    backup: bool,
    force_chapters: bool,
    min_free: Option<String>,
    purge_every: Option<usize>,
    write: WriteOpts,
) -> Result<()> {
    // Guard the divisor first: `(i + 1) % purge_every` below panics on 0.
    let purge_every = purge_every.unwrap_or(DEFAULT_PURGE_EVERY);
    if purge_every == 0 {
        return Err(Error::Config(
            "--purge-every must be greater than 0".to_string(),
        ));
    }
    let file_ids = match file {
        Some(path) => crate::items::parse_id_file(&crate::items::read_input(None, Some(path))?)?,
        None => Vec::new(),
    };
    let ids = collect_ids(file_ids, &write.selection);
    if ids.is_empty() {
        println!("no items selected (use --ids and/or --file)");
        return Ok(());
    }

    let creds = Credentials::load()?;
    let data_path = configured_data_path(&creds);
    let client = Client::new(&creds);

    // Backup is the only mode that accumulates disk; it requires a measurable
    // path so the headroom guard is real, not a no-op.
    let guard = if backup {
        let path = data_path.ok_or_else(|| {
            Error::Config(
                "--backup needs a disk-headroom check, but no `[cache].dataPath` is configured. \
                 Set it (when co-located with the ABS data dir) or run without --backup."
                    .to_string(),
            )
        })?;
        Some((
            path,
            parse_size(min_free.as_deref())?.unwrap_or(DEFAULT_MIN_FREE),
        ))
    } else {
        None
    };

    let ctx = WriteContext::new(write.apply)?;
    let mut outcomes = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        // Headroom check before each item (apply mode only - dry-run touches
        // nothing, so there is nothing to guard against).
        if write.apply
            && let Some((path, threshold)) = guard.as_ref()
        {
            let space = cache::free_space(path)?;
            if space.available < *threshold {
                // Abort as a failure (non-zero exit), not a silent partial run -
                // automation must see that items below the threshold were skipped.
                return Err(Error::Config(format!(
                    "aborted after {} item(s): {} free at {}, below --min-free {}",
                    i,
                    human(space.available),
                    path.display(),
                    human(*threshold),
                )));
            }
        }

        let current = client.item_get_raw(id)?;
        let req = embed_request(client.server(), id, &current, backup);
        let outcome = ctx.execute(&req, || embed_one(&client, id, backup, force_chapters))?;
        println!("{}", preview::format_line(&req, &outcome));
        // The harness collapses a non-auth embed failure (e.g. a task timeout)
        // into WriteOutcome::Error rather than Err. In apply mode that must stop
        // the batch: continuing would queue the next item while this one may
        // still be running on the server, breaking the one-at-a-time guarantee.
        if write.apply
            && let WriteOutcome::Error(msg) = &outcome
        {
            return Err(Error::Connection(format!(
                "embed failed for {id}; aborting batch before queueing more items: {msg}"
            )));
        }
        outcomes.push(outcome);

        // Defense-in-depth: purge the items cache every N items under --backup.
        if write.apply && backup && (i + 1) % purge_every == 0 {
            client.purge_items_cache()?;
            println!("(purged items cache after {} items)", i + 1);
        }
    }

    println!("{}", preview::format_summary(&outcomes));
    if !ctx.should_apply() {
        println!("(dry-run; re-run with --apply to embed)");
    }
    Ok(())
}

/// Trigger one embed, then block until the server task queue drains so the next
/// item never starts while this one is still writing (the serialization the
/// incident needs).
fn embed_one(client: &Client, id: &str, backup: bool, force_chapters: bool) -> Result<()> {
    client.embed_metadata(id, backup, force_chapters)?;
    let poller = TaskPoller::new(client, TASK_TIMEOUT, TASK_INTERVAL);
    match poller.wait_until_drained()? {
        WaitResult::Drained => Ok(()),
        WaitResult::Timeout => Err(Error::Connection(format!(
            "embed task for {id} did not drain within {}s",
            TASK_TIMEOUT.as_secs()
        ))),
    }
}

/// Build the harness request for an embed. The pre-image is the item JSON (the
/// metadata being embedded) - an audit record, not a restorable snapshot, since
/// embed mutates the audio file's tags, not the DB item.
fn embed_request(server: &str, id: &str, item: &serde_json::Value, backup: bool) -> WriteRequest {
    WriteRequest {
        server: server.to_string(),
        item_id: id.to_string(),
        label: crate::items::raw_title(item).unwrap_or(id).to_string(),
        operation: if backup { "embed-backup" } else { "embed" }.to_string(),
        before: item.clone(),
        after: serde_json::Value::Null,
    }
}

/// The configured `[cache].dataPath`, if any.
fn configured_data_path(creds: &Credentials) -> Option<PathBuf> {
    creds
        .config
        .cache
        .as_ref()
        .and_then(|c| c.data_path.clone())
        .map(PathBuf::from)
}

/// Union `--file` IDs with `--ids`, deduped, in first-seen order, `--limit`-capped.
fn collect_ids(file_ids: Vec<String>, selection: &SelectionOpts) -> Vec<String> {
    crate::items::collect_delete_ids(file_ids, &selection.ids, selection.limit)
}

/// Parse a `--min-free` size: a bare byte count, or a number with a binary unit
/// suffix (`B`/`KiB`/`MiB`/`GiB`/`TiB`, case-insensitive). `None` input -> `None`.
fn parse_size(input: Option<&str>) -> Result<Option<u64>> {
    let Some(raw) = input else { return Ok(None) };
    let s = raw.trim();
    let bad = || {
        Error::Config(format!(
            "invalid size '{raw}' (try e.g. 2GiB, 500MiB, or bytes)"
        ))
    };
    // Split the trailing alphabetic unit from the leading number.
    let split = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let value: u64 = num.trim().parse().map_err(|_| bad())?;
    let mult = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "kib" | "k" => 1024,
        "mib" | "m" => 1024 * 1024,
        "gib" | "g" => 1024 * 1024 * 1024,
        "tib" | "t" => 1024u64 * 1024 * 1024 * 1024,
        _ => return Err(bad()),
    };
    value.checked_mul(mult).map(Some).ok_or_else(bad)
}

/// Render a byte count in binary units (shared shape with `cache`'s formatter).
fn human(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_size_handles_units_and_bare_bytes() {
        assert_eq!(parse_size(None).unwrap(), None);
        assert_eq!(parse_size(Some("1024")).unwrap(), Some(1024));
        assert_eq!(parse_size(Some("0")).unwrap(), Some(0));
        assert_eq!(
            parse_size(Some("2GiB")).unwrap(),
            Some(2 * 1024 * 1024 * 1024)
        );
        assert_eq!(
            parse_size(Some("500 MiB")).unwrap(),
            Some(500 * 1024 * 1024)
        );
        // Case-insensitive units and short forms.
        assert_eq!(parse_size(Some("1g")).unwrap(), Some(1024 * 1024 * 1024));
        assert_eq!(parse_size(Some("4KIB")).unwrap(), Some(4096));
    }

    #[test]
    fn parse_size_rejects_garbage() {
        assert!(parse_size(Some("")).is_err());
        assert!(parse_size(Some("abc")).is_err());
        assert!(parse_size(Some("12zb")).is_err());
        assert!(parse_size(Some("-5")).is_err());
    }

    #[test]
    fn batch_embed_rejects_zero_purge_every() {
        // The guard is the first statement, before any I/O, so this exercises
        // it without a server: --purge-every 0 must error, not panic later.
        let err = run_batch_embed(None, false, false, None, Some(0), WriteOpts::default());
        assert!(matches!(err, Err(Error::Config(_))));
    }

    #[test]
    fn embed_request_tracks_backup_in_operation() {
        let item = serde_json::json!({"id":"li_1","media":{"metadata":{"title":"T"}}});
        assert_eq!(embed_request("s", "li_1", &item, false).operation, "embed");
        assert_eq!(
            embed_request("s", "li_1", &item, true).operation,
            "embed-backup"
        );
        assert_eq!(embed_request("s", "li_1", &item, false).label, "T");
    }
}
