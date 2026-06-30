//! `rabsody items` - read and write library items.
//!
//! Reads (`list`/`get`/`batch-get`) print pretty JSON. Writes (`update`/
//! `batch-update`/`batch-update-progress`/`delete`/`batch-delete`) go through the
//! shared write harness: dry-run by default, `--apply` to mutate,
//! snapshot-before-write, and a ledger. Array fields (`tags`, `genres`) are
//! *unioned* with the item's current values by default - ABS replaces arrays
//! wholesale, so a naive write would clobber them - with
//! `--replace-tags`/`--replace-genres` to overwrite instead.
//!
//! `delete`/`batch-delete` are soft by default (database record only). `--hard`
//! also removes the item's files from disk via the server's `?hard=1`; because
//! that is irreversible, an apply-mode hard delete first prints every target
//! (id/title/path) and then requires the operator to type the literal word
//! `DELETE`. RABSody never touches media files itself - the server owns removal.

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
    /// Delete one item (soft = database record only; dry-run unless `--apply`).
    Delete {
        /// Library item ID.
        id: String,
        /// Also delete the item's files from disk (irreversible). In `--apply`
        /// mode this requires typing the literal word `DELETE` to confirm.
        #[arg(long)]
        hard: bool,
        /// Perform the delete (otherwise dry-run preview only).
        #[arg(long)]
        apply: bool,
    },
    /// Delete many items by ID in one atomic request (soft by default).
    BatchDelete {
        /// JSON file: array of item-ID strings (`-` for stdin). Combined with `--ids`.
        #[arg(long)]
        file: Option<String>,
        /// Also delete each item's files from disk (irreversible). In `--apply`
        /// mode this requires typing the literal word `DELETE` to confirm.
        #[arg(long)]
        hard: bool,
        #[command(flatten)]
        write: WriteOpts,
    },
    /// Embed the item's metadata into its audio file(s) (dry-run unless `--apply`).
    EmbedMetadata {
        /// Library item ID.
        id: String,
        /// Back up the original file(s) first (ABS `?backup=1`). Off by default -
        /// per-item backups are what filled the disk in the 2026-06-21 incident.
        #[arg(long)]
        backup: bool,
        /// Re-embed chapters (ABS `forceEmbedChapters=1`).
        #[arg(long)]
        force_chapters: bool,
        /// Perform the embed (otherwise dry-run preview only).
        #[arg(long)]
        apply: bool,
    },
    /// Embed metadata into many items, serialized with a disk-headroom guard.
    BatchEmbedMetadata {
        /// JSON file: array of item-ID strings (`-` for stdin). Combined with `--ids`.
        #[arg(long)]
        file: Option<String>,
        /// Back up originals first (requires `[cache].dataPath` for the disk guard).
        #[arg(long)]
        backup: bool,
        /// Re-embed chapters (ABS `forceEmbedChapters=1`).
        #[arg(long)]
        force_chapters: bool,
        /// Abort if free space falls below this (e.g. `2GiB`). Requires `--backup`
        /// (it guards the per-item backups; a no-op otherwise).
        #[arg(long, requires = "backup")]
        min_free: Option<String>,
        /// Purge the items cache every N items (default 50). Requires `--backup`
        /// (the purge only matters when backups accumulate).
        #[arg(long, requires = "backup")]
        purge_every: Option<usize>,
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
        ItemsCmd::Delete { id, hard, apply } => run_delete(id, hard, apply),
        ItemsCmd::BatchDelete { file, hard, write } => run_batch_delete(file, hard, write),
        ItemsCmd::EmbedMetadata {
            id,
            backup,
            force_chapters,
            apply,
        } => crate::embed::run_embed(id, backup, force_chapters, apply),
        ItemsCmd::BatchEmbedMetadata {
            file,
            backup,
            force_chapters,
            min_free,
            purge_every,
            write,
        } => crate::embed::run_batch_embed(
            file,
            backup,
            force_chapters,
            min_free,
            purge_every,
            write,
        ),
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

/// `rabsody items delete` - remove one item. Soft by default (database record
/// only); `--hard` also removes files from disk via the server. Dry-run unless
/// `--apply`. The full item is snapshotted before any apply-mode delete.
fn run_delete(id: String, hard: bool, apply: bool) -> Result<()> {
    let client = api::client_only()?;
    // Raw fetch: the pre-delete snapshot must preserve the whole item, and the
    // inspection display wants the on-disk path that the lean `Item` drops.
    let current = client.item_get_raw(&id)?;

    let req = WriteRequest {
        server: client.server().to_string(),
        item_id: id.clone(),
        label: raw_title(&current).unwrap_or(&id).to_string(),
        operation: delete_op(hard).to_string(),
        before: current.clone(),
        after: serde_json::Value::Null,
    };

    // Inspection: always show the target before deleting (AC: never hard-delete
    // without showing what will be removed).
    println!("targeting for {} delete:", delete_kind(hard));
    println!("{}", inspect_line(&id, &current));

    if hard && apply && !confirm_hard_delete(1)? {
        println!("aborted; no items deleted");
        return Ok(());
    }

    let ctx = WriteContext::new(apply)?;
    let outcome = ctx.execute(&req, || client.item_delete(&id, hard))?;
    println!("{}", preview::format_line(&req, &outcome));
    if !ctx.should_apply() {
        println!("(dry-run; re-run with --apply to delete)");
    }
    Ok(())
}

/// `rabsody items batch-delete` - atomically remove many items by ID. IDs come
/// from `--file` (JSON array) and/or `--ids`, unioned and deduped. Soft by
/// default; `--hard` also removes files. Dry-run unless `--apply`.
fn run_batch_delete(file: Option<String>, hard: bool, write: WriteOpts) -> Result<()> {
    let file_ids = match file {
        Some(path) => parse_id_file(&read_input(None, Some(path))?)?,
        None => Vec::new(),
    };
    let ids = collect_delete_ids(file_ids, &write.selection.ids, write.selection.limit);
    if ids.is_empty() {
        println!("no items selected (use --ids and/or --file)");
        return Ok(());
    }

    let client = api::client_only()?;
    let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let current = client.items_batch_get_raw(&id_refs)?;

    // Build a request per ID the server actually returned; deleting an absent
    // ID is meaningless and would pollute the ledger with a null pre-image.
    let mut reqs = Vec::new();
    let mut to_delete: Vec<String> = Vec::new();
    println!("targeting for {} delete:", delete_kind(hard));
    for id in &ids {
        let Some(item) = current.iter().find(|v| raw_id(v) == Some(id.as_str())) else {
            eprintln!("warning: item {id} not found; skipping");
            continue;
        };
        println!("{}", inspect_line(id, item));
        reqs.push(WriteRequest {
            server: client.server().to_string(),
            item_id: id.clone(),
            label: raw_title(item).unwrap_or(id).to_string(),
            operation: delete_op(hard).to_string(),
            before: item.clone(),
            after: serde_json::Value::Null,
        });
        to_delete.push(id.clone());
    }
    if reqs.is_empty() {
        println!("no matching items to delete");
        return Ok(());
    }

    if hard && write.apply && !confirm_hard_delete(reqs.len())? {
        println!("aborted; no items deleted");
        return Ok(());
    }

    let ctx = WriteContext::new(write.apply)?;
    let del_refs: Vec<&str> = to_delete.iter().map(String::as_str).collect();
    let outcomes = ctx.execute_batch(&reqs, || client.items_batch_delete(&del_refs, hard))?;
    for (req, outcome) in reqs.iter().zip(&outcomes) {
        println!("{}", preview::format_line(req, outcome));
    }
    println!("{}", preview::format_summary(&outcomes));
    if !ctx.should_apply() {
        println!("(dry-run; re-run with --apply to delete)");
    }
    Ok(())
}

/// Ledger/preview operation tag for a delete (`delete` vs `delete-hard`).
fn delete_op(hard: bool) -> &'static str {
    if hard { "delete-hard" } else { "delete" }
}

/// Human word for the delete kind, used in inspection headers and the prompt.
fn delete_kind(hard: bool) -> &'static str {
    if hard { "hard" } else { "soft" }
}

/// `id` of a raw item JSON value, if present.
fn raw_id(item: &serde_json::Value) -> Option<&str> {
    item.get("id").and_then(serde_json::Value::as_str)
}

/// `media.metadata.title` of a raw item JSON value, if present.
pub(crate) fn raw_title(item: &serde_json::Value) -> Option<&str> {
    item.get("media")?.get("metadata")?.get("title")?.as_str()
}

/// On-disk `path` of a raw item JSON value, if present.
fn raw_path(item: &serde_json::Value) -> Option<&str> {
    item.get("path").and_then(serde_json::Value::as_str)
}

/// One inspection line for a delete target: `  <id>  <title>  [<path>]`.
fn inspect_line(id: &str, item: &serde_json::Value) -> String {
    let title = raw_title(item).unwrap_or(id);
    match raw_path(item) {
        Some(path) => format!("  {id}  {title}  [{path}]"),
        None => format!("  {id}  {title}"),
    }
}

/// True only for the exact literal confirmation word, after trimming whitespace.
fn is_delete_confirmation(input: &str) -> bool {
    input.trim() == "DELETE"
}

/// Union `--file` IDs with `--ids`, preserving first-seen order, dropping blanks
/// and duplicates, then truncating to `limit` (the shared `--limit` flag).
pub(crate) fn collect_delete_ids(
    file_ids: Vec<String>,
    flag_ids: &[String],
    limit: Option<usize>,
) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for id in file_ids.into_iter().chain(flag_ids.iter().cloned()) {
        let id = id.trim().to_string();
        if !id.is_empty() && seen.insert(id.clone()) {
            out.push(id);
        }
    }
    if let Some(limit) = limit {
        out.truncate(limit);
    }
    out
}

/// Parse the `--file` input for batch-delete: a JSON array of ID strings.
pub(crate) fn parse_id_file(raw: &str) -> Result<Vec<String>> {
    serde_json::from_str(raw).map_err(|e| {
        Error::Config(format!(
            "parsing ID file (expected a JSON array of strings): {e}"
        ))
    })
}

/// Interactive guard for `--hard --apply`: prompt and require the literal word
/// `DELETE` on stdin. Returns whether the user confirmed. EOF (e.g. empty piped
/// stdin) counts as "not confirmed"; Ctrl+C aborts the process outright, so
/// there is no path that silently proceeds with a hard delete.
fn confirm_hard_delete(count: usize) -> Result<bool> {
    use std::io::Write;
    eprint!(
        "About to PERMANENTLY delete {count} item(s) and their files from disk. \
         This cannot be undone.\nType DELETE to proceed: "
    );
    std::io::stderr()
        .flush()
        .map_err(|e| Error::Config(format!("writing confirmation prompt: {e}")))?;
    let mut line = String::new();
    let n = std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| Error::Config(format!("reading confirmation: {e}")))?;
    Ok(n > 0 && is_delete_confirmation(&line))
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
pub(crate) fn read_input(data: Option<String>, file: Option<String>) -> Result<String> {
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

    #[test]
    fn delete_op_and_kind_track_hard_flag() {
        assert_eq!(delete_op(false), "delete");
        assert_eq!(delete_op(true), "delete-hard");
        assert_eq!(delete_kind(false), "soft");
        assert_eq!(delete_kind(true), "hard");
    }

    #[test]
    fn confirmation_requires_exact_literal_word() {
        assert!(is_delete_confirmation("DELETE"));
        // Surrounding whitespace / trailing newline is trimmed.
        assert!(is_delete_confirmation("  DELETE\n"));
        // Anything else is rejected: wrong case, extra words, partial, empty.
        assert!(!is_delete_confirmation("delete"));
        assert!(!is_delete_confirmation("DELETE now"));
        assert!(!is_delete_confirmation("DELET"));
        assert!(!is_delete_confirmation("yes"));
        assert!(!is_delete_confirmation(""));
    }

    #[test]
    fn collect_delete_ids_unions_dedups_trims_and_limits() {
        // --file ids first, then --ids; first-seen order preserved, dupes dropped.
        let got = collect_delete_ids(s(&["a", "b"]), &s(&["b", "c"]), None);
        assert_eq!(got, s(&["a", "b", "c"]));
        // Blank / whitespace-only entries are dropped; surviving ids are trimmed.
        let got = collect_delete_ids(s(&["  x  ", "", "  "]), &s(&["x", "y"]), None);
        assert_eq!(got, s(&["x", "y"]));
        // --limit truncates after the union.
        let got = collect_delete_ids(s(&["a", "b", "c"]), &s(&["d"]), Some(2));
        assert_eq!(got, s(&["a", "b"]));
        // Nothing selected.
        assert!(collect_delete_ids(vec![], &[], None).is_empty());
    }

    #[test]
    fn parse_id_file_reads_json_string_array() {
        assert_eq!(
            parse_id_file(r#"["li_1","li_2"]"#).unwrap(),
            s(&["li_1", "li_2"])
        );
        // A non-array / wrong-shape file is a clear config error, not a panic.
        assert!(parse_id_file(r#"{"id":"li_1"}"#).is_err());
        assert!(parse_id_file("not json").is_err());
    }

    #[test]
    fn inspect_line_shows_id_title_and_path() {
        let item = serde_json::json!({
            "id": "li_1",
            "path": "/audiobooks/Author/Book",
            "media": { "metadata": { "title": "A Book" } }
        });
        assert_eq!(
            inspect_line("li_1", &item),
            "  li_1  A Book  [/audiobooks/Author/Book]"
        );
        assert_eq!(raw_id(&item), Some("li_1"));
        assert_eq!(raw_title(&item), Some("A Book"));
        assert_eq!(raw_path(&item), Some("/audiobooks/Author/Book"));
    }

    #[test]
    fn inspect_line_falls_back_to_id_when_title_and_path_missing() {
        let item = serde_json::json!({ "id": "li_9" });
        // No title -> id; no path -> path segment omitted entirely.
        assert_eq!(inspect_line("li_9", &item), "  li_9  li_9");
        assert_eq!(raw_title(&item), None);
        assert_eq!(raw_path(&item), None);
    }
}
