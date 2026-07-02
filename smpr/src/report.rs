use crate::rating::ItemResult;
use std::path::Path;

/// Write detection results to a CSV file.
///
/// Creates parent directories if needed. Errors are logged, not fatal.
pub fn write_report(results: &[ItemResult], path: &Path) {
    if let Err(e) = write_report_inner(results, path) {
        log::error!("cannot write report to {}: {}", path.display(), e);
    } else {
        log::info!("report written to {}", path.display());
    }
}

fn write_report_inner(
    results: &[ItemResult],
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut wtr = csv::Writer::from_path(path)?;
    wtr.write_record([
        "artist",
        "album",
        "track",
        "path",
        "tier",
        "matched_words",
        "previous_rating",
        "action",
        "source",
        "server",
        "has_lyrics",
    ])?;
    for r in results {
        let track = r
            .path
            .as_deref()
            .and_then(|p| p.rsplit(['/', '\\']).next())
            .unwrap_or("");
        // Normalized match-key: the exact string a `[[overrides]]` `match`
        // substring is compared against. Copy any portion into an override entry.
        let path_key = r
            .path
            .as_deref()
            .map(crate::util::normalize_path)
            .unwrap_or_default();
        wtr.write_record([
            r.artist.as_deref().unwrap_or(""),
            r.album.as_deref().unwrap_or(""),
            track,
            &path_key,
            r.tier.as_deref().unwrap_or(""),
            &r.matched_words.join("; "),
            r.previous_rating.as_deref().unwrap_or(""),
            r.action.as_csv_str(),
            r.source.as_csv_str(),
            &r.server_name,
            if r.has_lyrics { "true" } else { "false" },
        ])?;
    }
    wtr.flush()?;
    Ok(())
}
