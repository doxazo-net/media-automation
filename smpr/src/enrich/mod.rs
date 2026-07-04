//! The enrich pass: query authoritative sources for in-scope tracks, gate the
//! matches (via `sources::matcher`), and cache the resulting verdicts in the
//! store - or, in report mode, emit a calibration CSV without writing.

use crate::config::{Config, ServerConfig, ServerType, SourcesConfig};
use crate::rating::LibraryScope;
use crate::rating::scope;
use crate::server::types::AudioItemView;
use crate::server::{MediaServerClient, MediaServerError};
use crate::sources::deezer::DeezerSource;
use crate::sources::itunes::ItunesSource;
use crate::sources::matcher::{self, MatchParams};
use crate::sources::{Source, SourceHit, SourceVerdict, TrackQuery};
use crate::store::{SourceStore, VerdictRecord};
use std::path::Path;

pub mod lock;

#[cfg(test)]
mod tests;

/// Write the `--report` calibration CSV. Creates parent directories as needed.
pub fn write_enrich_report(rows: &[EnrichRow], path: &Path) -> Result<(), csv::Error> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut wtr = csv::Writer::from_path(path)?;
    wtr.write_record([
        "track_key",
        "path",
        "matched_title",
        "matched_artist",
        "matched_id",
        "confidence",
        "duration_delta_s",
        "source",
        "source_verdict",
        "current_rating",
    ])?;
    for r in rows {
        wtr.write_record([
            r.track_key.clone(),
            r.path.clone().unwrap_or_default(),
            r.matched_title.clone().unwrap_or_default(),
            r.matched_artist.clone().unwrap_or_default(),
            r.matched_id.clone().unwrap_or_default(),
            r.confidence.map(|c| format!("{c:.3}")).unwrap_or_default(),
            r.duration_delta_s
                .map(|d| d.to_string())
                .unwrap_or_default(),
            r.source.clone().unwrap_or_default(),
            r.source_verdict.clone().unwrap_or_default(),
            r.current_rating.clone().unwrap_or_default(),
        ])?;
    }
    wtr.flush()?;
    Ok(())
}

/// A source's gated match for a track.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceMatch {
    pub source: String,
    pub hit: SourceHit,
    pub confidence: f64,
    pub duration_delta_s: Option<i64>,
}

/// Tallies from an enrich run.
#[derive(Debug, Default, PartialEq)]
pub struct EnrichSummary {
    pub matched: usize,
    pub no_match: usize,
    pub cached_skipped: usize,
    pub no_query_skipped: usize,
}

/// One row of the `--report` calibration CSV.
#[derive(Debug, Clone, PartialEq)]
pub struct EnrichRow {
    /// The normalized store key (copyable into a `[[overrides]]`/curation key).
    pub track_key: String,
    pub path: Option<String>,
    pub matched_title: Option<String>,
    pub matched_artist: Option<String>,
    pub matched_id: Option<String>,
    pub confidence: Option<f64>,
    pub duration_delta_s: Option<i64>,
    pub source: Option<String>,
    pub source_verdict: Option<String>,
    pub current_rating: Option<String>,
}

/// Build a query from an item; `None` when the item has no usable title (a title
/// is required to search a source).
pub fn track_query_from_item(item: &AudioItemView) -> Option<TrackQuery> {
    let title = item
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    Some(TrackQuery {
        artist: item.album_artist.clone(),
        album: item.album.clone(),
        title,
        duration_s: item.duration_s(),
    })
}

/// Stable per-track key: the MusicBrainz ID if tagged, else the normalized path,
/// else the opaque item id.
pub fn track_key_for_item(item: &AudioItemView) -> String {
    if let Some(mbid) = item.mbid() {
        return mbid.to_string();
    }
    match item.path.as_deref() {
        Some(p) if !p.is_empty() => normalize_path(p),
        _ => item.id.clone(),
    }
}

fn normalize_path(path: &str) -> String {
    path.to_lowercase().replace('\\', "/")
}

/// Whether an enrich run may advance the per-server incremental watermark
/// (issue #257). Only a write-mode, unscoped run in which every item persisted
/// is eligible:
/// - `report_only`: never writes the store, so has nothing durable to record.
/// - `scoped`: covers only part of the server; its partial max must not move the
///   server-wide watermark (a later incremental run would skip other scopes).
/// - `persist_failed`: an item was fetched but not stored; advancing past it
///   would leave it permanently un-re-fetched under incremental runs.
fn should_advance_watermark(report_only: bool, scoped: bool, persist_failed: bool) -> bool {
    !report_only && !scoped && !persist_failed
}

/// Reconcile matches across sources: positive-wins - an `Explicit` verdict from
/// any source wins (highest confidence among those), else the highest-confidence
/// match overall.
pub fn reconcile(matches: Vec<SourceMatch>) -> Option<SourceMatch> {
    // A NaN confidence sorts as Less so it can never win max_by (a match with an
    // invalid score must not be selected over a valid one).
    let by_conf = |a: &&SourceMatch, b: &&SourceMatch| {
        a.confidence
            .partial_cmp(&b.confidence)
            .unwrap_or(std::cmp::Ordering::Less)
    };
    matches
        .iter()
        .filter(|m| m.hit.verdict == SourceVerdict::Explicit)
        .max_by(by_conf)
        .or_else(|| matches.iter().max_by(by_conf))
        .cloned()
}

/// Query every source for a track and gate each source's hits, returning the
/// per-source matches (a source that errors is logged and skipped).
pub fn match_track(
    query: &TrackQuery,
    sources: &[Box<dyn Source>],
    params: &MatchParams,
) -> Vec<SourceMatch> {
    let mut matches = Vec::new();
    for source in sources {
        match source.lookup(query) {
            Ok(hits) => {
                if let Some(m) = matcher::best_match(query, &hits, params) {
                    matches.push(SourceMatch {
                        source: source.name().to_string(),
                        hit: m.hit,
                        confidence: m.confidence,
                        duration_delta_s: m.duration_delta_s,
                    });
                }
            }
            Err(e) => log::warn!("source '{}' lookup failed: {e}", source.name()),
        }
    }
    matches
}

/// Build the active source adapters from config, in the configured sequence
/// order (only real source adapters; `lyrics`/`genre` are rate-time tiers).
fn build_sources(cfg: &SourcesConfig) -> Vec<Box<dyn Source>> {
    let mut sources: Vec<Box<dyn Source>> = Vec::new();
    for name in &cfg.sequence {
        match name.as_str() {
            "deezer" if cfg.deezer_enabled => sources.push(Box::new(DeezerSource::new())),
            "itunes" if cfg.itunes_enabled => sources.push(Box::new(ItunesSource::new())),
            // "spotify" lands in a later milestone (dormant adapter).
            _ => {}
        }
    }
    sources
}

fn verdict_record(
    key: &str,
    item: &AudioItemView,
    query: &TrackQuery,
    chosen: &SourceMatch,
    server_name: &str,
) -> VerdictRecord {
    VerdictRecord {
        track_key: key.to_string(),
        mbid: item.mbid().map(str::to_string),
        server_name: Some(server_name.to_string()),
        artist: query.artist.clone(),
        album: query.album.clone(),
        title: Some(query.title.clone()),
        duration_s: query.duration_s,
        source: chosen.source.clone(),
        source_track_id: chosen.hit.source_track_id.clone(),
        source_verdict: chosen.hit.verdict,
        match_confidence: chosen.confidence,
        duration_delta_s: chosen.duration_delta_s,
        curated_override: None,
        notes: None,
    }
}

/// Resolve library scope and prefetch the in-scope items (mirrors the rate
/// workflow's scope handling).
fn scoped_items(
    client: &MediaServerClient,
    config: &Config,
    since: Option<&str>,
    limit: Option<usize>,
) -> Result<Vec<(AudioItemView, serde_json::Value)>, MediaServerError> {
    let need_scope = config.library_name.is_some() || config.location_name.is_some();
    let libraries = if need_scope {
        Some(client.discover_libraries()?)
    } else {
        None
    };
    let lib_scope = match libraries.as_deref() {
        Some(libs) => scope::resolve_from_libraries(
            libs,
            config.library_name.as_deref(),
            config.location_name.as_deref(),
        )
        .map_err(|e| MediaServerError::Protocol(format!("scope resolution: {e:?}")))?,
        None => LibraryScope {
            parent_id: None,
            location_path: None,
            library_name: None,
        },
    };
    let include_media_sources = client.server_type() == &ServerType::Emby;
    // One prefetch that honors both the incremental watermark (`since`, #257) and
    // the bounded-smoke-test cap (`limit`, #254).
    let items = client.prefetch_impl(
        include_media_sources,
        lib_scope.parent_id.as_deref(),
        since,
        limit,
    )?;
    if limit.is_some() && lib_scope.location_path.is_some() {
        log::warn!(
            "enrich --limit bounds the prefetch BEFORE the --location filter; \
             the bounded page may contain few or no items under that location. \
             For a quick smoke test, run --limit without --location."
        );
    }
    Ok(match lib_scope.location_path {
        Some(loc) => scope::filter_by_location(items, &loc),
        None => items,
    })
}

/// Enrich all in-scope tracks. When `report_only`, no store writes happen and a
/// row is emitted for every queryable item (title-less items are skipped);
/// otherwise matches are upserted to the store and cached items are skipped
/// unless `refresh`.
#[allow(clippy::too_many_arguments)]
pub fn enrich_workflow(
    client: &MediaServerClient,
    config: &Config,
    server_config: &ServerConfig,
    store: Option<&SourceStore>,
    report_only: bool,
    refresh: bool,
    incremental: bool,
    limit: Option<usize>,
) -> Result<(EnrichSummary, Vec<EnrichRow>), MediaServerError> {
    let sources = build_sources(&config.sources);
    let params = MatchParams {
        min_confidence: config.sources.match_min_confidence,
        duration_tolerance_s: config.sources.duration_tolerance_s,
    };

    // Incremental prefetch (issue #257): fetch only items newer than the stored
    // watermark. Only meaningful for a write-mode run with a store; report-only
    // calibration always fetches the full scope. No watermark yet => full crawl
    // (the initial backfill, which then records the watermark below). A read
    // failure is distinguished from a genuine first run so the logs aren't
    // misleading during ops (Copilot review, PR #259).
    let scoped = config.library_name.is_some() || config.location_name.is_some();
    let since: Option<String> = if incremental && !report_only {
        match store.map(|s| s.get_watermark(&server_config.name)) {
            Some(Ok(Some(w))) => Some(w),
            Some(Ok(None)) => {
                log::info!(
                    "enrich: no watermark for '{}'; full crawl (initial backfill)",
                    server_config.name
                );
                None
            }
            Some(Err(e)) => {
                log::warn!(
                    "enrich: watermark read failed for '{}': {e}; doing a full crawl",
                    server_config.name
                );
                None
            }
            None => None,
        }
    } else {
        None
    };

    let items = scoped_items(client, config, since.as_deref(), limit)?;
    log::info!("enrich: processing {} items", items.len());

    let mut summary = EnrichSummary::default();
    let mut rows = Vec::new();
    // If any upsert fails, the watermark must NOT advance past this run's items:
    // an item we fetched but failed to persist would otherwise never be
    // re-fetched incrementally (it sinks below the watermark), leaving a silent
    // hole recoverable only by a full non-incremental crawl.
    let mut persist_failed = false;

    for (item, _raw) in &items {
        let key = track_key_for_item(item);

        // In write mode, skip already-cached tracks unless refreshing. Report
        // mode always processes so the calibration view is complete. A store
        // read error is logged (not silently treated as a miss) and falls
        // through to re-querying.
        if !report_only
            && !refresh
            && let Some(store) = store
        {
            match store.get(&key) {
                Ok(Some(_)) => {
                    summary.cached_skipped += 1;
                    continue;
                }
                Ok(None) => {}
                Err(e) => log::warn!("enrich: store read failed for '{key}': {e}"),
            }
        }

        let Some(query) = track_query_from_item(item) else {
            summary.no_query_skipped += 1;
            continue;
        };

        let chosen = reconcile(match_track(&query, &sources, &params));
        match &chosen {
            Some(m) => {
                summary.matched += 1;
                if !report_only && let Some(store) = store {
                    let record = verdict_record(&key, item, &query, m, &server_config.name);
                    if let Err(e) = store.upsert(&record) {
                        log::warn!("enrich: store upsert failed for '{key}': {e}");
                        persist_failed = true;
                    }
                }
            }
            None => summary.no_match += 1,
        }

        // Report mode emits one row per queryable item (matched or not).
        if report_only {
            let matched = chosen.as_ref();
            rows.push(EnrichRow {
                track_key: key,
                path: item.path.clone(),
                matched_title: matched.map(|m| m.hit.title.clone()),
                matched_artist: matched.and_then(|m| m.hit.artist.clone()),
                matched_id: matched.and_then(|m| m.hit.source_track_id.clone()),
                confidence: matched.map(|m| m.confidence),
                duration_delta_s: matched.and_then(|m| m.duration_delta_s),
                source: matched.map(|m| m.source.clone()),
                source_verdict: matched.map(|m| m.hit.verdict.as_str().to_string()),
                current_rating: item.official_rating.clone(),
            });
        }
    }

    // Advance the watermark to the newest DateCreated seen when this run is
    // eligible (see `should_advance_watermark`). A persist failure deliberately
    // holds the watermark back so the next incremental run re-fetches and
    // retries; surface that so it is not a silent stall.
    if let Some(store) = store {
        if should_advance_watermark(report_only, scoped, persist_failed) {
            // Pick the chronologically-latest DateCreated via ts_key (not a raw
            // string max), so mixed fractional-second precision can't record a
            // watermark that isn't actually the newest (Codoki review, PR #259).
            if let Some(max) = items
                .iter()
                .filter_map(|(v, _)| v.date_created.as_deref())
                .max_by(|a, b| crate::server::ts_key(a).cmp(&crate::server::ts_key(b)))
                && let Err(e) = store.set_watermark(&server_config.name, max)
            {
                log::warn!(
                    "enrich: failed to persist watermark for '{}': {e}",
                    server_config.name
                );
            }
        } else if persist_failed && !report_only && !scoped {
            log::warn!(
                "enrich: an upsert failed this run; NOT advancing the '{}' watermark, \
                 so the next incremental run re-fetches and retries the affected items",
                server_config.name
            );
        }
    }

    Ok((summary, rows))
}
