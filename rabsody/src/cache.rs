//! `rabsody cache` - server cache management plus a local free-space query.
//!
//! `purge`/`purge-items` clear the ABS server cache (which regenerates on
//! demand). They run through the shared write harness: dry-run by default,
//! `--apply` to act, ledgered for audit. The cache pre-image is intentionally
//! empty - a purged cache has nothing to restore.
//!
//! `free-space` reports the free/total bytes of a *local* filesystem path (the
//! ABS data/cache dir, when RABSody is co-located with the server). ABS exposes
//! no disk free-space API, so this is the only way to get a real `df` reading;
//! it is the primitive the bulk embed/encode disk guards (#190/#191) build on.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use serde::Serialize;
use serde_json::Value;

use crate::api;
use crate::config::Credentials;
use crate::error::{Error, Result};
use crate::harness::{WriteContext, WriteRequest, preview};

#[derive(Subcommand)]
pub enum CacheCmd {
    /// Purge the entire server cache (dry-run unless `--apply`).
    Purge {
        /// Perform the purge (otherwise dry-run preview only).
        #[arg(long)]
        apply: bool,
    },
    /// Purge the items cache only (dry-run unless `--apply`).
    PurgeItems {
        /// Perform the purge (otherwise dry-run preview only).
        #[arg(long)]
        apply: bool,
    },
    /// Report free/total disk space for a local path (the ABS data/cache dir).
    FreeSpace {
        /// Filesystem path to query. Falls back to `[cache].dataPath` in config.
        #[arg(long)]
        path: Option<String>,
        /// Emit structured JSON instead of a human-readable line.
        #[arg(long)]
        json: bool,
    },
}

pub fn run(cmd: CacheCmd) -> Result<()> {
    match cmd {
        CacheCmd::Purge { apply } => run_purge(apply, false),
        CacheCmd::PurgeItems { apply } => run_purge(apply, true),
        CacheCmd::FreeSpace { path, json } => run_free_space(path, json),
    }
}

/// Purge the server cache (whole cache, or items-only when `items`). Routed
/// through the write harness so `--apply` gates the call and the action is
/// ledgered; the cache regenerates, so the snapshot pre-image is empty.
fn run_purge(apply: bool, items: bool) -> Result<()> {
    let client = api::client_only()?;
    let (operation, item_id, label) = if items {
        ("cache-items-purge", "cache:items", "items cache")
    } else {
        ("cache-purge", "cache", "server cache")
    };
    let req = WriteRequest {
        server: client.server().to_string(),
        item_id: item_id.to_string(),
        label: label.to_string(),
        operation: operation.to_string(),
        before: Value::Null,
        after: Value::Null,
    };

    let ctx = WriteContext::new(apply)?;
    let outcome = ctx.execute(&req, || {
        if items {
            client.purge_items_cache()
        } else {
            client.purge_cache()
        }
    })?;
    println!("{}", preview::format_line(&req, &outcome));
    if !ctx.should_apply() {
        println!("(dry-run; re-run with --apply to purge)");
    }
    Ok(())
}

/// Query and report free disk space for a resolved local path.
fn run_free_space(path_flag: Option<String>, json: bool) -> Result<()> {
    let path = resolve_free_space_path(path_flag)?;
    let space = free_space(&path)?;
    if json {
        crate::print_json(&space)
    } else {
        println!("{}", space.human());
        Ok(())
    }
}

/// Resolve the path to query, single source of truth for precedence: the
/// `--path` flag wins; otherwise `[cache].dataPath` from the native config; with
/// neither, error (ABS has no server free-space API, so there is nothing to
/// measure). `--path` short-circuits before the config/credential load, so a
/// one-off `free-space --path` needs no configured credentials.
fn resolve_free_space_path(path_flag: Option<String>) -> Result<PathBuf> {
    if let Some(path) = path_flag {
        return Ok(PathBuf::from(path));
    }
    Credentials::load()?
        .config
        .cache
        .and_then(|c| c.data_path)
        .map(PathBuf::from)
        .ok_or_else(|| {
            Error::Config(
                "no path to query: ABS exposes no server free-space API. Pass --path, \
                 or set `[cache].dataPath` in the config when RABSody is co-located \
                 with the ABS data directory."
                    .to_string(),
            )
        })
}

/// Free/total disk space for a local filesystem path. `available` is the space
/// usable by the current user (the actionable figure for a write headroom
/// check); `used` counts filesystem-reserved blocks as used (conservative).
#[derive(Debug, Serialize)]
pub struct DiskSpace {
    pub path: String,
    pub total: u64,
    pub available: u64,
    pub used: u64,
    pub pct_used: f64,
}

impl DiskSpace {
    fn new(path: &Path, total: u64, available: u64) -> Self {
        let used = total.saturating_sub(available);
        let pct_used = if total > 0 {
            used as f64 * 100.0 / total as f64
        } else {
            0.0
        };
        Self {
            path: path.display().to_string(),
            total,
            available,
            used,
            pct_used,
        }
    }

    /// One-line human-readable summary.
    fn human(&self) -> String {
        format!(
            "{}: {} available of {} ({:.1}% used)",
            self.path,
            human_bytes(self.available),
            human_bytes(self.total),
            self.pct_used,
        )
    }
}

/// Query free disk space for `path` via `statvfs` (the `fs4` crate).
pub fn free_space(path: &Path) -> Result<DiskSpace> {
    let total = fs4::total_space(path)
        .map_err(|e| Error::Config(format!("querying total space at {}: {e}", path.display())))?;
    let available = fs4::available_space(path).map_err(|e| {
        Error::Config(format!(
            "querying available space at {}: {e}",
            path.display()
        ))
    })?;
    Ok(DiskSpace::new(path, total, available))
}

/// Render a byte count in binary units (KiB/MiB/GiB/...), one decimal place.
fn human_bytes(n: u64) -> String {
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
    fn disk_space_computes_used_and_pct() {
        let ds = DiskSpace::new(Path::new("/x"), 100, 25);
        assert_eq!(ds.used, 75);
        assert!((ds.pct_used - 75.0).abs() < f64::EPSILON);
        // total == 0 must not divide by zero.
        let z = DiskSpace::new(Path::new("/x"), 0, 0);
        assert_eq!(z.used, 0);
        assert_eq!(z.pct_used, 0.0);
        // available > total (shouldn't happen, but must not underflow).
        let o = DiskSpace::new(Path::new("/x"), 10, 20);
        assert_eq!(o.used, 0);
    }

    #[test]
    fn human_bytes_scales_binary_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(1 << 30), "1.0 GiB");
        assert_eq!(human_bytes(1 << 40), "1.0 TiB");
    }

    #[test]
    fn free_space_reads_a_real_path() {
        // The temp dir always exists; total must be positive and available
        // cannot exceed total.
        let dir = std::env::temp_dir();
        let space = free_space(&dir).unwrap();
        assert!(space.total > 0);
        assert!(space.available <= space.total);
    }
}
