//! `rabsody items encode-m4b start|cancel` - drive ABS's m4b encode.
//!
//! Each encode writes a FULL new copy of the audiobook, so the safety model is:
//!
//! 1. **Pre-flight free-space check sized to the source.** Before each encode
//!    (apply mode), require `cache::free_space().available >= source_bytes +
//!    headroom` against the configured `[cache].dataPath`; abort otherwise.
//! 2. **Strictly serialized.** Encodes run one item at a time, waiting for each
//!    server task to drain via [`TaskPoller`] (the server has no concurrency
//!    guard), the same loop shape as `items embed-metadata`.
//! 3. **Resume.** Already-encoded items (recorded `applied` in the harness
//!    ledger) are skipped on a re-run unless `--no-resume`.
//!
//! Source-matched defaults (m4a remux / mp3 transcode) are the server's own
//! source-aware behavior; the source codec/size are shown in the preview, and
//! `--bitrate`/`--codec` override the server defaults when given.

use std::path::PathBuf;
use std::time::Duration;

use clap::Subcommand;

use crate::api::{self, Client, Item};
use crate::cache;
use crate::config::Credentials;
use crate::error::{Error, Result};
use crate::harness::{Ledger, WriteContext, WriteOpts, WriteOutcome, WriteRequest, preview};
use crate::tasks::{TaskPoller, WaitResult};

/// Default headroom required beyond the source size (1 GiB).
const DEFAULT_HEADROOM: u64 = 1024 * 1024 * 1024;
const TASK_TIMEOUT: Duration = Duration::from_secs(1800);
const TASK_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Subcommand)]
pub enum EncodeM4bCmd {
    /// Start an m4b encode for one or more items (serialized, disk-checked).
    Start {
        /// JSON file: array of item-ID strings (`-` for stdin). Combined with `--ids`.
        #[arg(long)]
        file: Option<String>,
        /// Override the encode bitrate (e.g. `128k`); default is the server's.
        #[arg(long)]
        bitrate: Option<String>,
        /// Override the encode codec (e.g. `copy` to remux); default is the server's.
        #[arg(long)]
        codec: Option<String>,
        /// Extra free space required beyond the source size (e.g. `2GiB`, default 1GiB).
        #[arg(long)]
        min_free: Option<String>,
        /// Re-encode items already recorded as encoded in the ledger.
        #[arg(long)]
        no_resume: bool,
        #[command(flatten)]
        write: WriteOpts,
    },
    /// Cancel an in-progress m4b encode for one or more items.
    Cancel {
        /// JSON file: array of item-ID strings (`-` for stdin). Combined with `--ids`.
        #[arg(long)]
        file: Option<String>,
        #[command(flatten)]
        write: WriteOpts,
    },
}

pub fn run(cmd: EncodeM4bCmd) -> Result<()> {
    match cmd {
        EncodeM4bCmd::Start {
            file,
            bitrate,
            codec,
            min_free,
            no_resume,
            write,
        } => run_start(file, bitrate, codec, min_free, no_resume, write),
        EncodeM4bCmd::Cancel { file, write } => run_cancel(file, write),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_start(
    file: Option<String>,
    bitrate: Option<String>,
    codec: Option<String>,
    min_free: Option<String>,
    no_resume: bool,
    write: WriteOpts,
) -> Result<()> {
    let ids = collect_ids(file, &write)?;
    if ids.is_empty() {
        println!("no items selected (use --ids and/or --file)");
        return Ok(());
    }

    let creds = Credentials::load()?;
    let data_path = configured_data_path(&creds);
    let client = Client::new(&creds);
    let headroom = crate::embed::parse_size(min_free.as_deref())?.unwrap_or(DEFAULT_HEADROOM);

    // Resume: skip items already encoded (recorded `applied` in the ledger).
    let done = if no_resume {
        std::collections::HashSet::new()
    } else {
        completed_encodes(&Ledger::resolve()?.read_all()?, client.server())
    };

    let ctx = WriteContext::new(write.apply)?;
    let mut outcomes = Vec::new();
    for id in &ids {
        if done.contains(id) {
            println!("[skipped] {id} (already encoded; --no-resume to force)");
            continue;
        }
        let item = client.item_get(id, true, None)?;
        let source = source_size(&item);

        // Pre-flight: each encode writes a full copy, so require source + headroom.
        // In apply mode the check is mandatory and fails closed: without a
        // configured data path or a known source size we cannot guarantee the
        // headroom, so refuse rather than silently skip the guard.
        if write.apply {
            if source == 0 {
                return Err(Error::Config(format!(
                    "cannot determine source size for {id}; refusing to encode without a source-sized disk check"
                )));
            }
            let path = data_path.as_ref().ok_or_else(|| {
                Error::Config(
                    "items encode-m4b --apply requires [cache].dataPath for the disk preflight"
                        .to_string(),
                )
            })?;
            let space = cache::free_space(path)?;
            let required = source.saturating_add(headroom);
            if space.available < required {
                return Err(Error::Config(format!(
                    "insufficient space for {id}: need {} (source {} + headroom {}), {} free at {}",
                    human(required),
                    human(source),
                    human(headroom),
                    human(space.available),
                    path.display(),
                )));
            }
        }

        let req = WriteRequest {
            server: client.server().to_string(),
            item_id: id.clone(),
            label: encode_label(&item, source),
            operation: "encode-m4b".to_string(),
            before: serde_json::to_value(&item).map_err(|e| {
                Error::Parse(format!(
                    "encode-m4b snapshot serialization failed for {id}: {e}"
                ))
            })?,
            after: serde_json::Value::Null,
        };
        let outcome = ctx.execute(&req, || {
            client.encode_m4b_start(id, bitrate.as_deref(), codec.as_deref())?;
            drain_task(&client, id)
        })?;
        println!("{}", preview::format_line(&req, &outcome));
        // Abort on a real failure so we never queue the next encode while this
        // one may still be running (no server-side concurrency guard).
        if write.apply
            && let WriteOutcome::Error(msg) = &outcome
        {
            return Err(Error::Connection(format!(
                "encode failed for {id}; aborting before starting more: {msg}"
            )));
        }
        outcomes.push(outcome);
    }

    println!("{}", preview::format_summary(&outcomes));
    if !ctx.should_apply() {
        println!("(dry-run; re-run with --apply to encode)");
    }
    Ok(())
}

fn run_cancel(file: Option<String>, write: WriteOpts) -> Result<()> {
    let ids = collect_ids(file, &write)?;
    if ids.is_empty() {
        println!("no items selected (use --ids and/or --file)");
        return Ok(());
    }
    let client = api::client_only()?;
    let ctx = WriteContext::new(write.apply)?;
    let mut outcomes = Vec::new();
    for id in &ids {
        let req = WriteRequest {
            server: client.server().to_string(),
            item_id: id.clone(),
            label: id.clone(),
            operation: "encode-m4b-cancel".to_string(),
            before: serde_json::Value::Null,
            after: serde_json::Value::Null,
        };
        let outcome = ctx.execute(&req, || client.encode_m4b_cancel(id))?;
        println!("{}", preview::format_line(&req, &outcome));
        outcomes.push(outcome);
    }
    println!("{}", preview::format_summary(&outcomes));
    if !ctx.should_apply() {
        println!("(dry-run; re-run with --apply to cancel)");
    }
    Ok(())
}

/// Block until the server task queue drains so the next encode never overlaps.
fn drain_task(client: &Client, id: &str) -> Result<()> {
    let poller = TaskPoller::new(client, TASK_TIMEOUT, TASK_INTERVAL);
    match poller.wait_until_drained()? {
        WaitResult::Drained => Ok(()),
        WaitResult::Timeout => Err(Error::Connection(format!(
            "encode task for {id} did not drain within {}s",
            TASK_TIMEOUT.as_secs()
        ))),
    }
}

/// Selected IDs from `--file` (JSON array) unioned with `--ids`, deduped.
fn collect_ids(file: Option<String>, write: &WriteOpts) -> Result<Vec<String>> {
    let file_ids = match file {
        Some(path) => crate::items::parse_id_file(&crate::items::read_input(None, Some(path))?)?,
        None => Vec::new(),
    };
    Ok(crate::items::collect_delete_ids(
        file_ids,
        &write.selection.ids,
        write.selection.limit,
    ))
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

/// Sum of the item's source audio-file sizes in bytes.
fn source_size(item: &Item) -> u64 {
    item.media
        .audio_files
        .as_ref()
        .map(|files| files.iter().map(|f| f.metadata.size).sum())
        .unwrap_or(0)
}

/// The source container extension (lowercased) of the first audio file, used to
/// surface the remux-vs-transcode expectation in the preview.
fn source_ext(item: &Item) -> Option<String> {
    item.media
        .audio_files
        .as_ref()
        .and_then(|files| files.first())
        .map(|f| f.metadata.ext.trim_start_matches('.').to_ascii_lowercase())
}

/// `true` when the source is already in an AAC-family container (m4a/m4b/aac/
/// mp4), so an m4b encode can remux (copy) rather than transcode.
fn is_remuxable(ext: &str) -> bool {
    matches!(ext, "m4a" | "m4b" | "aac" | "mp4")
}

/// Preview label: title plus the source size and remux/transcode expectation.
fn encode_label(item: &Item, source: u64) -> String {
    let title = item
        .media
        .metadata
        .title
        .clone()
        .unwrap_or_else(|| item.id.clone());
    match source_ext(item) {
        Some(ext) => {
            let kind = if is_remuxable(&ext) {
                "remux"
            } else {
                "transcode"
            };
            format!("{title} [{}, {ext} -> {kind}]", human(source))
        }
        None => format!("{title} [{}]", human(source)),
    }
}

/// Item IDs recorded as a successfully `applied` `encode-m4b` write **on the
/// given server** in the ledger. Scoping by server prevents a record from one
/// server from suppressing an unencoded item when the same ledger is reused
/// against a different server.
fn completed_encodes(
    records: &[crate::harness::WriteRecord],
    server: &str,
) -> std::collections::HashSet<String> {
    records
        .iter()
        .filter(|r| r.server == server && r.operation == "encode-m4b" && r.outcome == "applied")
        .map(|r| r.item_id.clone())
        .collect()
}

/// Render a byte count in binary units.
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
    use crate::harness::WriteRecord;

    fn item_with(ext: &str, sizes: &[u64]) -> Item {
        let files: Vec<_> = sizes
            .iter()
            .map(|s| serde_json::json!({"metadata": {"ext": ext, "size": s}}))
            .collect();
        serde_json::from_value(serde_json::json!({
            "id": "li_1",
            "media": { "metadata": { "title": "T" }, "audioFiles": files }
        }))
        .unwrap()
    }

    #[test]
    fn source_size_sums_audio_files() {
        assert_eq!(source_size(&item_with("mp3", &[100, 250, 7])), 357);
        // No audio files -> 0, never panics.
        let bare: Item =
            serde_json::from_value(serde_json::json!({"id":"x","media":{"metadata":{}}})).unwrap();
        assert_eq!(source_size(&bare), 0);
    }

    #[test]
    fn remuxable_matches_aac_family_only() {
        for ext in ["m4a", "m4b", "aac", "mp4"] {
            assert!(is_remuxable(ext), "{ext} should remux");
        }
        for ext in ["mp3", "flac", "ogg", "wav", ""] {
            assert!(!is_remuxable(ext), "{ext} should transcode");
        }
    }

    #[test]
    fn label_shows_size_and_remux_decision() {
        let m4a = item_with("m4a", &[1024 * 1024]);
        assert_eq!(
            encode_label(&m4a, source_size(&m4a)),
            "T [1.0 MiB, m4a -> remux]"
        );
        let mp3 = item_with("mp3", &[2 * 1024 * 1024]);
        assert_eq!(
            encode_label(&mp3, source_size(&mp3)),
            "T [2.0 MiB, mp3 -> transcode]"
        );
    }

    #[test]
    fn completed_encodes_filters_applied_encode_records() {
        let rec = |id: &str, op: &str, outcome: &str| WriteRecord {
            ts: 0,
            server: "s".into(),
            item_id: id.into(),
            operation: op.into(),
            before: serde_json::Value::Null,
            after: serde_json::Value::Null,
            outcome: outcome.into(),
        };
        let records = vec![
            rec("a", "encode-m4b", "applied"),
            rec("b", "encode-m4b", "error: boom"), // failed -> not done
            rec("c", "embed", "applied"),          // wrong op -> not done
            rec("d", "encode-m4b", "applied"),
        ];
        let done = completed_encodes(&records, "s");
        assert!(done.contains("a") && done.contains("d"));
        assert!(!done.contains("b") && !done.contains("c"));
        assert_eq!(done.len(), 2);
    }

    #[test]
    fn completed_encodes_scopes_to_the_active_server() {
        let rec = |server: &str, id: &str| WriteRecord {
            ts: 0,
            server: server.into(),
            item_id: id.into(),
            operation: "encode-m4b".into(),
            before: serde_json::Value::Null,
            after: serde_json::Value::Null,
            outcome: "applied".into(),
        };
        let records = vec![rec("home", "a"), rec("other", "b")];
        // Resuming against "home" must not treat "b" (encoded on "other") as done.
        let done = completed_encodes(&records, "home");
        assert!(done.contains("a"));
        assert!(!done.contains("b"));
        assert_eq!(done.len(), 1);
    }
}
