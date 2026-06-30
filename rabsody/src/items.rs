//! `rabsody items` - read and write library items.
//!
//! Reads (`list`/`get`/`batch-get`) print pretty JSON. Writes (`update`/
//! `batch-update`/`batch-update-progress`) go through the shared write harness:
//! dry-run by default, `--apply` to mutate, snapshot-before-write, and a ledger.
//! Array fields (`tags`, `genres`) are *unioned* with the item's current values
//! by default - ABS replaces arrays wholesale, so a naive write would clobber
//! them - with `--replace-tags`/`--replace-genres` to overwrite instead.

use std::io::Read;

use clap::Subcommand;

use crate::api::{self, BatchItemUpdate, Item, ItemsListParams, MediaPatch, ProgressUpdate};
use crate::error::{Error, Result};
use crate::harness::{WriteContext, WriteOpts, WriteOutcome, WriteRequest, preview};

#[derive(Subcommand)]
pub enum ItemsCmd {
    /// List items in a library (filter / sort / paginate).
    List {
        /// Library ID; defaults to the abs-cli `defaultLibrary` when omitted.
        library: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        page: Option<u32>,
        /// Sort path, e.g. `media.metadata.title`.
        #[arg(long)]
        sort: Option<String>,
        /// Sort in descending order.
        #[arg(long)]
        desc: bool,
        /// ABS filter expression.
        #[arg(long)]
        filter: Option<String>,
        /// Request the minified response shape.
        #[arg(long)]
        minified: bool,
        /// Extra data fields to include.
        #[arg(long)]
        include: Option<String>,
    },
    /// Get a single item by ID.
    Get {
        /// Library item ID.
        id: String,
        /// Include expanded media (audio files, chapters).
        #[arg(long)]
        expanded: bool,
        /// Extra data fields to include.
        #[arg(long)]
        include: Option<String>,
    },
    /// Get multiple items by ID in one request.
    BatchGet {
        /// Library item IDs (space-separated).
        #[arg(required = true, num_args = 1..)]
        ids: Vec<String>,
    },
    /// Update one item's metadata/tags (dry-run unless `--apply`).
    Update {
        /// Library item ID.
        id: String,
        /// Media-shaped JSON patch, e.g. `{"metadata":{"subtitle":"X"},"tags":["a"]}`.
        #[arg(long, conflicts_with = "file")]
        data: Option<String>,
        /// Read the JSON patch from a file (`-` for stdin).
        #[arg(long)]
        file: Option<String>,
        /// Replace tags wholesale instead of unioning with the item's current tags.
        #[arg(long)]
        replace_tags: bool,
        /// Replace genres wholesale instead of unioning with current genres.
        #[arg(long)]
        replace_genres: bool,
        /// Perform the write (otherwise dry-run preview only).
        #[arg(long)]
        apply: bool,
    },
    /// Update many items' metadata/tags from a JSON file (atomic batch).
    BatchUpdate {
        /// JSON file: array of `{"id":"..","metadata":{..},"tags":[..]}` (`-` for stdin).
        #[arg(long)]
        file: String,
        #[arg(long)]
        replace_tags: bool,
        #[arg(long)]
        replace_genres: bool,
        #[command(flatten)]
        write: WriteOpts,
    },
    /// Update listening progress for many items from a JSON file.
    BatchUpdateProgress {
        /// JSON file: array of `{"libraryItemId":"..", ..progress fields}` (`-` for stdin).
        #[arg(long)]
        file: String,
        #[command(flatten)]
        write: WriteOpts,
    },
}

pub fn run(cmd: ItemsCmd) -> Result<()> {
    match cmd {
        ItemsCmd::List {
            library,
            limit,
            page,
            sort,
            desc,
            filter,
            minified,
            include,
        } => {
            let (client, lib) = match library {
                Some(lib) => (api::client_only()?, lib),
                None => api::connect()?,
            };
            let params = ItemsListParams {
                limit,
                page,
                sort,
                desc,
                filter,
                minified,
                include,
            };
            crate::print_json(&client.items_list(&lib, &params)?)
        }
        ItemsCmd::Get {
            id,
            expanded,
            include,
        } => {
            let client = api::client_only()?;
            crate::print_json(&client.item_get(&id, expanded, include.as_deref())?)
        }
        ItemsCmd::BatchGet { ids } => {
            let client = api::client_only()?;
            let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            crate::print_json(&client.items_batch_get(&refs)?)
        }
        ItemsCmd::Update {
            id,
            data,
            file,
            replace_tags,
            replace_genres,
            apply,
        } => run_update(id, data, file, replace_tags, replace_genres, apply),
        ItemsCmd::BatchUpdate {
            file,
            replace_tags,
            replace_genres,
            write,
        } => run_batch_update(file, replace_tags, replace_genres, write),
        ItemsCmd::BatchUpdateProgress { file, write } => run_batch_update_progress(file, write),
    }
}

/// `rabsody items update` - single-item metadata/tags write.
fn run_update(
    id: String,
    data: Option<String>,
    file: Option<String>,
    replace_tags: bool,
    replace_genres: bool,
    apply: bool,
) -> Result<()> {
    let raw = read_input(data, file)?;
    let mut patch: MediaPatch = serde_json::from_str(&raw)
        .map_err(|e| Error::Config(format!("parsing patch JSON: {e}")))?;

    let client = api::client_only()?;
    let current = client.item_get(&id, false, None)?;
    merge_arrays(&mut patch, &current, replace_tags, replace_genres);

    let ctx = WriteContext::new(apply)?;
    let req = WriteRequest {
        server: client.server().to_string(),
        item_id: id.clone(),
        label: title_of(&current),
        operation: "metadata".to_string(),
        before: serde_json::to_value(&current)
            .map_err(|e| Error::Config(format!("serializing current item: {e}")))?,
        after: serde_json::to_value(&patch)
            .map_err(|e| Error::Config(format!("serializing patch: {e}")))?,
    };

    if patch.is_empty() {
        let skipped = WriteOutcome::Skipped("no effective change".to_string());
        println!("{}", preview::format_line(&req, &skipped));
        return Ok(());
    }

    let outcome = ctx.execute(&req, || client.item_update_media(&id, &patch))?;
    println!("{}", preview::format_line(&req, &outcome));
    if !ctx.should_apply() {
        println!("(dry-run; re-run with --apply to write)");
    }
    Ok(())
}

/// `rabsody items batch-update` - atomic multi-item metadata/tags write.
fn run_batch_update(
    file: String,
    replace_tags: bool,
    replace_genres: bool,
    write: WriteOpts,
) -> Result<()> {
    let raw = read_input(None, Some(file))?;
    let entries: Vec<BatchEntry> = serde_json::from_str(&raw)
        .map_err(|e| Error::Config(format!("parsing batch JSON (expected an array): {e}")))?;
    let entries = write.selection.select(entries, |e| e.id.as_str());
    if entries.is_empty() {
        println!("no items selected");
        return Ok(());
    }

    let client = api::client_only()?;
    let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
    let current = client.items_batch_get(&ids)?;

    let mut updates = Vec::new();
    let mut reqs = Vec::new();
    for entry in &entries {
        // Skip IDs the server didn't return: snapshotting a null pre-image and
        // writing to a non-existent item is meaningless and pollutes the ledger.
        let Some(cur) = current.iter().find(|i| i.id == entry.id) else {
            eprintln!("warning: item {} not found; skipping", entry.id);
            continue;
        };
        let mut patch = entry.patch.clone();
        merge_arrays(&mut patch, cur, replace_tags, replace_genres);
        if patch.is_empty() {
            continue;
        }
        reqs.push(WriteRequest {
            server: client.server().to_string(),
            item_id: entry.id.clone(),
            label: title_of(cur),
            operation: "metadata".to_string(),
            before: serde_json::to_value(cur)
                .map_err(|e| Error::Config(format!("serializing item {}: {e}", entry.id)))?,
            after: serde_json::to_value(&patch)
                .map_err(|e| Error::Config(format!("serializing patch for {}: {e}", entry.id)))?,
        });
        updates.push(BatchItemUpdate {
            id: entry.id.clone(),
            media_payload: patch,
        });
    }

    if updates.is_empty() {
        println!("no effective changes");
        return Ok(());
    }

    let ctx = WriteContext::new(write.apply)?;
    let outcomes = ctx.execute_batch(&reqs, || client.items_batch_update(&updates).map(|_| ()))?;
    for (req, outcome) in reqs.iter().zip(&outcomes) {
        println!("{}", preview::format_line(req, outcome));
    }
    println!("{}", preview::format_summary(&outcomes));
    if !ctx.should_apply() {
        println!("(dry-run; re-run with --apply to write)");
    }
    Ok(())
}

/// `rabsody items batch-update-progress` - atomic multi-item progress write.
fn run_batch_update_progress(file: String, write: WriteOpts) -> Result<()> {
    let raw = read_input(None, Some(file))?;
    let entries: Vec<ProgressEntry> = serde_json::from_str(&raw)
        .map_err(|e| Error::Config(format!("parsing progress JSON (expected an array): {e}")))?;
    let entries = write
        .selection
        .select(entries, |e| e.library_item_id.as_str());
    if entries.is_empty() {
        println!("no items selected");
        return Ok(());
    }

    let client = api::client_only()?;
    let mut updates = Vec::new();
    let mut reqs = Vec::new();
    for entry in &entries {
        reqs.push(WriteRequest {
            server: client.server().to_string(),
            item_id: entry.library_item_id.clone(),
            label: entry.library_item_id.clone(),
            operation: "progress".to_string(),
            // Progress has no cheap pre-image here; record the requested change.
            before: serde_json::Value::Null,
            after: serde_json::to_value(&entry.fields)
                .map_err(|e| Error::Config(format!("serializing progress fields: {e}")))?,
        });
        updates.push(ProgressUpdate {
            library_item_id: entry.library_item_id.clone(),
            fields: entry.fields.clone(),
        });
    }

    let ctx = WriteContext::new(write.apply)?;
    let outcomes = ctx.execute_batch(&reqs, || client.batch_update_progress(&updates))?;
    for (req, outcome) in reqs.iter().zip(&outcomes) {
        println!("{}", preview::format_line(req, outcome));
    }
    println!("{}", preview::format_summary(&outcomes));
    if !ctx.should_apply() {
        println!("(dry-run; re-run with --apply to write)");
    }
    Ok(())
}

/// A `batch-update` file entry: an item ID plus a media-shaped patch.
#[derive(serde::Deserialize)]
struct BatchEntry {
    id: String,
    #[serde(flatten)]
    patch: MediaPatch,
}

/// A `batch-update-progress` file entry: an item ID plus progress fields.
#[derive(serde::Deserialize)]
struct ProgressEntry {
    #[serde(rename = "libraryItemId")]
    library_item_id: String,
    #[serde(flatten)]
    fields: serde_json::Map<String, serde_json::Value>,
}

/// Read patch input from `--data`, or from a `--file` path (`-` = stdin).
fn read_input(data: Option<String>, file: Option<String>) -> Result<String> {
    if let Some(data) = data {
        return Ok(data);
    }
    match file.as_deref() {
        Some("-") => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| Error::Config(format!("reading stdin: {e}")))?;
            Ok(buf)
        }
        Some(path) => {
            std::fs::read_to_string(path).map_err(|e| Error::Config(format!("reading {path}: {e}")))
        }
        None => Err(Error::Config(
            "provide a patch via --data or --file".to_string(),
        )),
    }
}

/// Union `existing` with `incoming`, preserving `existing` order and appending
/// only values not already present (case-sensitive).
fn union(existing: &[String], incoming: &[String]) -> Vec<String> {
    let mut out = existing.to_vec();
    for v in incoming {
        if !out.iter().any(|e| e == v) {
            out.push(v.clone());
        }
    }
    out
}

/// Apply union-by-default semantics to the array fields (`tags`, `genres`) of a
/// patch against the item's current values, unless the caller asked to replace.
fn merge_arrays(patch: &mut MediaPatch, current: &Item, replace_tags: bool, replace_genres: bool) {
    if !replace_tags && let Some(tags) = patch.tags.as_mut() {
        *tags = union(&current.media.tags, tags);
    }
    if !replace_genres
        && let Some(meta) = patch.metadata.as_mut()
        && let Some(genres) = meta.genres.as_mut()
    {
        *genres = union(&current.media.metadata.genres, genres);
    }
}

/// The item's title for preview output, falling back to its ID.
fn title_of(item: &Item) -> String {
    item.media
        .metadata
        .title
        .clone()
        .unwrap_or_else(|| item.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn union_preserves_existing_order_and_dedups() {
        assert_eq!(union(&s(&["a", "b"]), &s(&["b", "c"])), s(&["a", "b", "c"]));
        assert_eq!(union(&s(&[]), &s(&["x"])), s(&["x"]));
        assert_eq!(union(&s(&["a"]), &s(&[])), s(&["a"]));
        // incoming dupes collapse
        assert_eq!(union(&s(&["a"]), &s(&["a", "a"])), s(&["a"]));
    }

    fn item_with(tags: &[&str], genres: &[&str]) -> Item {
        let json = serde_json::json!({
            "id": "li_1",
            "media": {
                "tags": tags,
                "metadata": { "title": "T", "genres": genres }
            }
        });
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn merge_arrays_unions_by_default() {
        let mut patch: MediaPatch =
            serde_json::from_str(r#"{"metadata":{"genres":["sci-fi"]},"tags":["new"]}"#).unwrap();
        let current = item_with(&["old"], &["fantasy"]);
        merge_arrays(&mut patch, &current, false, false);
        assert_eq!(patch.tags.unwrap(), s(&["old", "new"]));
        assert_eq!(
            patch.metadata.unwrap().genres.unwrap(),
            s(&["fantasy", "sci-fi"])
        );
    }

    #[test]
    fn merge_arrays_replace_overrides_union() {
        let mut patch: MediaPatch =
            serde_json::from_str(r#"{"metadata":{"genres":["sci-fi"]},"tags":["new"]}"#).unwrap();
        let current = item_with(&["old"], &["fantasy"]);
        merge_arrays(&mut patch, &current, true, true);
        assert_eq!(patch.tags.unwrap(), s(&["new"]));
        assert_eq!(patch.metadata.unwrap().genres.unwrap(), s(&["sci-fi"]));
    }

    #[test]
    fn scalar_only_patch_leaves_arrays_untouched() {
        let mut patch: MediaPatch =
            serde_json::from_str(r#"{"metadata":{"subtitle":"X"}}"#).unwrap();
        let current = item_with(&["old"], &["fantasy"]);
        merge_arrays(&mut patch, &current, false, false);
        assert!(patch.tags.is_none());
        assert!(patch.metadata.as_ref().unwrap().genres.is_none());
        assert!(!patch.is_empty());
    }

    #[test]
    fn empty_patch_is_detected() {
        let patch: MediaPatch = serde_json::from_str("{}").unwrap();
        assert!(patch.is_empty());
    }
}
