pub mod action;
pub mod scope;

#[cfg(test)]
mod tests;

use crate::config::{Config, ServerConfig, ServerType};
use crate::detection::DetectionEngine;
use crate::server::types::AudioItemView;
use crate::server::{MediaServerClient, MediaServerError};
use serde_json::Value;

/// Outcome of processing a single track.
#[derive(Debug, Clone, PartialEq)]
pub enum RatingAction {
    /// Rating was applied to the server.
    Set,
    /// Rating was removed (set to empty string).
    Cleared,
    /// Track was skipped (already rated + skip-existing, or no action needed).
    Skipped,
    /// Rating already matches the desired value.
    AlreadyCorrect,
    /// Dry-run: would have set a rating.
    DryRun,
    /// Dry-run: would have cleared a rating.
    DryRunClear,
    /// No rating applied because the genre-G fallback was vetoed by a
    /// `deny_genres` match (e.g. a film OST tagged `Soundtrack`). Left unrated
    /// and surfaced for manual review rather than blind-rated G.
    Review,
    /// Server update failed (non-auth error).
    Error(String),
}

impl RatingAction {
    /// CSV-friendly string representation.
    pub fn as_csv_str(&self) -> &str {
        match self {
            Self::Set => "set",
            Self::Cleared => "cleared",
            Self::Skipped => "skipped",
            Self::AlreadyCorrect => "already_correct",
            Self::DryRun => "dry_run",
            Self::DryRunClear => "dry_run_clear",
            Self::Review => "review",
            Self::Error(_) => "error",
        }
    }
}

/// Why a track was rated.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// Rating determined by lyrics classification.
    Lyrics,
    /// Rating determined by genre allow-list (G).
    Genre,
    /// Force subcommand or config force_rating.
    Force,
    /// Reset subcommand.
    Reset,
}

impl Source {
    /// CSV-friendly string representation.
    pub fn as_csv_str(&self) -> &str {
        match self {
            Self::Lyrics => "lyrics",
            Self::Genre => "genre",
            Self::Force => "force",
            Self::Reset => "reset",
        }
    }
}

/// Result of processing a single audio track.
#[derive(Debug)]
pub struct ItemResult {
    // Used in integration tests and future item-level operations.
    #[allow(dead_code)]
    pub item_id: String,
    pub path: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub tier: Option<String>,
    pub matched_words: Vec<String>,
    pub previous_rating: Option<String>,
    pub action: RatingAction,
    pub source: Source,
    pub server_name: String,
    /// True only when lyrics text was actually fetched and classified for this
    /// track (an explicit-tier hit or an evaluated-clean track). False for
    /// genuinely lyric-less tracks and for paths that never fetch lyrics
    /// (force/reset/genre-only). Distinguishes "no lyrics" from "clean lyrics",
    /// which otherwise collapse to the same tier/action/source and made lyrics
    /// coverage unauditable.
    pub has_lyrics: bool,
}

/// Resolved library/location scope for a workflow.
#[derive(Debug)]
pub struct LibraryScope {
    /// ParentId for prefetch query (None = all items).
    pub parent_id: Option<String>,
    /// Server-side path prefix for post-prefetch location filtering.
    pub location_path: Option<String>,
    /// Resolved library name (for force_rating lookup).
    pub library_name: Option<String>,
}

/// Errors that abort a workflow.
#[derive(Debug)]
pub enum RatingError {
    /// Server API error (non-auth).
    Server(MediaServerError),
    /// Auth error (401/403) — abort immediately.
    Auth(u16),
    /// Requested library not found.
    LibraryNotFound {
        name: String,
        available: Vec<String>,
    },
    /// Requested location not found.
    LocationNotFound {
        name: String,
        available: Vec<String>,
    },
    /// No music libraries found on server.
    NoMusicLibraries,
    /// Library matched but has no ItemId.
    MissingLibraryId(String),
}

impl std::fmt::Display for RatingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Server(e) => write!(f, "{e}"),
            Self::Auth(status) => write!(f, "auth error (HTTP {status})"),
            Self::LibraryNotFound { name, available } => {
                write!(
                    f,
                    "library '{}' not found. Available: {}",
                    name,
                    available.join(", ")
                )
            }
            Self::LocationNotFound { name, available } => {
                write!(
                    f,
                    "location '{}' not found. Available: {}",
                    name,
                    available.join(", ")
                )
            }
            Self::NoMusicLibraries => write!(f, "no music libraries found on server"),
            Self::MissingLibraryId(name) => {
                write!(f, "library '{}' has no ItemId", name)
            }
        }
    }
}

impl std::error::Error for RatingError {}

impl From<MediaServerError> for RatingError {
    fn from(e: MediaServerError) -> Self {
        match &e {
            MediaServerError::Http { status, .. } if *status == 401 || *status == 403 => {
                Self::Auth(*status)
            }
            _ => Self::Server(e),
        }
    }
}

/// Run the `rate` workflow for a single server.
///
/// Fetches lyrics, classifies content, and sets ratings.
/// Returns results for all items processed.
pub fn rate_workflow(
    client: &MediaServerClient,
    config: &Config,
    server_config: &ServerConfig,
    engine: &DetectionEngine,
) -> Result<Vec<ItemResult>, RatingError> {
    let lib_scope = resolve_library_scope(client, config)?;
    let include_media_sources = client.server_type() == &ServerType::Emby;
    let items =
        client.prefetch_audio_items(include_media_sources, lib_scope.parent_id.as_deref())?;
    let items = if let Some(ref loc_path) = lib_scope.location_path {
        scope::filter_by_location(items, loc_path)
    } else {
        items
    };

    log::info!("processing {} items for rate workflow", items.len());

    // Check for config-level force_rating (unless --ignore-forced)
    let force_rating = if config.ignore_forced {
        None
    } else {
        scope::lookup_force_rating(
            server_config,
            lib_scope.library_name.as_deref(),
            config.location_name.as_deref(),
        )
    };

    let mut results = Vec::new();
    for (view, raw) in &items {
        let result = rate_item(
            client,
            config,
            engine,
            view,
            raw,
            force_rating,
            &server_config.name,
        )?;
        results.push(result);
    }
    Ok(results)
}

fn rate_item(
    client: &MediaServerClient,
    config: &Config,
    engine: &DetectionEngine,
    view: &AudioItemView,
    raw: &Value,
    force_rating: Option<&str>,
    server_name: &str,
) -> Result<ItemResult, RatingError> {
    let label = view.path.as_deref().unwrap_or(&view.id);
    let prev = view.official_rating.as_deref();

    // Config force_rating takes priority (unless --ignore-forced)
    if let Some(forced) = force_rating {
        let act = action::decide_rating_action(forced, prev, config.overwrite, config.dry_run);
        let act = if matches!(act, RatingAction::Set) {
            action::apply_rating(client, &view.id, forced, label)?
        } else {
            act
        };
        return Ok(ItemResult {
            item_id: view.id.clone(),
            path: view.path.clone(),
            artist: view.album_artist.clone(),
            album: view.album.clone(),
            tier: Some(forced.to_string()),
            matched_words: vec![],
            previous_rating: prev.map(String::from),
            action: act,
            source: Source::Force,
            has_lyrics: false,
            server_name: server_name.to_string(),
        });
    }

    // Fetch lyrics
    let lyrics_text = match client.fetch_lyrics(view, raw) {
        Ok(text) => text,
        Err(MediaServerError::Http { status, .. }) if status == 401 || status == 403 => {
            return Err(RatingError::Auth(status));
        }
        Err(e) => {
            log::warn!("failed to fetch lyrics for {}: {}", label, e);
            None
        }
    };

    if let Some(text) = lyrics_text {
        let (tier, matched) = engine.classify_lyrics(&text);

        if let Some(tier) = tier {
            // Explicit content found
            let act = action::decide_rating_action(tier, prev, config.overwrite, config.dry_run);
            let act = if matches!(act, RatingAction::Set) {
                action::apply_rating(client, &view.id, tier, label)?
            } else {
                act
            };
            return Ok(ItemResult {
                item_id: view.id.clone(),
                path: view.path.clone(),
                artist: view.album_artist.clone(),
                album: view.album.clone(),
                tier: Some(tier.to_string()),
                matched_words: matched,
                previous_rating: prev.map(String::from),
                action: act,
                source: Source::Lyrics,
                has_lyrics: true,
                server_name: server_name.to_string(),
            });
        }

        // Clean lyrics. By default set the configured clean rating (e.g. "G")
        // so the track stays playable under a parental gate that blocks unrated
        // items; if clean_rating is None (opt-out) fall back to clearing.
        if let Some(clean) = config.clean_rating.as_deref() {
            let act = action::decide_rating_action(clean, prev, config.overwrite, config.dry_run);
            let act = if matches!(act, RatingAction::Set) {
                action::apply_rating(client, &view.id, clean, label)?
            } else {
                act
            };
            return Ok(ItemResult {
                item_id: view.id.clone(),
                path: view.path.clone(),
                artist: view.album_artist.clone(),
                album: view.album.clone(),
                tier: Some(clean.to_string()),
                matched_words: vec![],
                previous_rating: prev.map(String::from),
                action: act,
                source: Source::Lyrics,
                has_lyrics: true,
                server_name: server_name.to_string(),
            });
        }
        // Opt-out: clear existing rating if overwrite enabled.
        let act = action::decide_clear_action(prev, config.overwrite, config.dry_run);
        let act = if matches!(act, RatingAction::Cleared) {
            action::apply_rating(client, &view.id, "", label)?
        } else {
            act
        };
        return Ok(ItemResult {
            item_id: view.id.clone(),
            path: view.path.clone(),
            artist: view.album_artist.clone(),
            album: view.album.clone(),
            tier: None,
            matched_words: vec![],
            previous_rating: prev.map(String::from),
            action: act,
            source: Source::Lyrics,
            has_lyrics: true,
            server_name: server_name.to_string(),
        });
    }

    // No lyrics — try genre fallback
    if let Some(matched_genre) = engine.match_g_genre(&view.genres) {
        // A denied genre vetoes the blind genre-G: leave the track unrated and
        // flag it for review rather than applying G we can't verify from lyrics.
        if let Some(denied) = engine.denied_genre(&view.genres) {
            log::debug!("{label}: genre-G vetoed by deny_genres ('{denied}'); left for review");
            return Ok(ItemResult {
                item_id: view.id.clone(),
                path: view.path.clone(),
                artist: view.album_artist.clone(),
                album: view.album.clone(),
                tier: None,
                matched_words: vec![denied.to_string()],
                previous_rating: prev.map(String::from),
                action: RatingAction::Review,
                source: Source::Genre,
                has_lyrics: false,
                server_name: server_name.to_string(),
            });
        }
        let act = action::decide_rating_action("G", prev, config.overwrite, config.dry_run);
        let act = if matches!(act, RatingAction::Set) {
            action::apply_rating(client, &view.id, "G", label)?
        } else {
            act
        };
        return Ok(ItemResult {
            item_id: view.id.clone(),
            path: view.path.clone(),
            artist: view.album_artist.clone(),
            album: view.album.clone(),
            tier: Some("G".to_string()),
            matched_words: vec![matched_genre.to_string()],
            previous_rating: prev.map(String::from),
            action: act,
            source: Source::Genre,
            has_lyrics: false,
            server_name: server_name.to_string(),
        });
    }

    // No lyrics, no genre match — skip
    Ok(ItemResult {
        item_id: view.id.clone(),
        path: view.path.clone(),
        artist: view.album_artist.clone(),
        album: view.album.clone(),
        tier: None,
        matched_words: vec![],
        previous_rating: prev.map(String::from),
        action: RatingAction::Skipped,
        source: Source::Lyrics,
        has_lyrics: false,
        server_name: server_name.to_string(),
    })
}

/// Resolve library/location scope via the server API.
fn resolve_library_scope(
    client: &MediaServerClient,
    config: &Config,
) -> Result<LibraryScope, RatingError> {
    if config.library_name.is_none() && config.location_name.is_none() {
        return Ok(LibraryScope {
            parent_id: None,
            location_path: None,
            library_name: None,
        });
    }
    let libraries = client.discover_libraries()?;
    scope::resolve_from_libraries(
        &libraries,
        config.library_name.as_deref(),
        config.location_name.as_deref(),
    )
}

/// Run the `force` workflow for a single server.
///
/// Sets a fixed rating on all tracks in scope. No lyrics evaluation.
pub fn force_workflow(
    client: &MediaServerClient,
    config: &Config,
    server_config: &ServerConfig,
    target_rating: &str,
) -> Result<Vec<ItemResult>, RatingError> {
    let lib_scope = resolve_library_scope(client, config)?;
    let items = client.prefetch_audio_items(false, lib_scope.parent_id.as_deref())?;
    let items = if let Some(ref loc_path) = lib_scope.location_path {
        scope::filter_by_location(items, loc_path)
    } else {
        items
    };

    log::info!("force-rating {} items as '{}'", items.len(), target_rating);

    let mut results = Vec::new();
    for (view, _) in &items {
        let label = view.path.as_deref().unwrap_or(&view.id);
        let prev = view.official_rating.as_deref();
        let act =
            action::decide_rating_action(target_rating, prev, config.overwrite, config.dry_run);
        let act = if matches!(act, RatingAction::Set) {
            action::apply_rating(client, &view.id, target_rating, label)?
        } else {
            act
        };
        results.push(ItemResult {
            item_id: view.id.clone(),
            path: view.path.clone(),
            artist: view.album_artist.clone(),
            album: view.album.clone(),
            tier: Some(target_rating.to_string()),
            matched_words: vec![],
            previous_rating: prev.map(String::from),
            action: act,
            source: Source::Force,
            has_lyrics: false,
            server_name: server_config.name.clone(),
        });
    }
    Ok(results)
}

/// Run the `reset` workflow for a single server.
///
/// Removes OfficialRating from all tracks in scope.
pub fn reset_workflow(
    client: &MediaServerClient,
    config: &Config,
    server_config: &ServerConfig,
) -> Result<Vec<ItemResult>, RatingError> {
    let lib_scope = resolve_library_scope(client, config)?;
    let items = client.prefetch_audio_items(false, lib_scope.parent_id.as_deref())?;
    let items = if let Some(ref loc_path) = lib_scope.location_path {
        scope::filter_by_location(items, loc_path)
    } else {
        items
    };

    log::info!("resetting ratings on {} items", items.len());

    let mut results = Vec::new();
    for (view, _) in &items {
        let label = view.path.as_deref().unwrap_or(&view.id);
        let prev = view.official_rating.as_deref();
        let act = action::decide_clear_action(prev, true, config.dry_run);
        let act = if matches!(act, RatingAction::Cleared) {
            // apply_rating("") returns Cleared on success, Error on failure
            action::apply_rating(client, &view.id, "", label)?
        } else {
            act
        };
        results.push(ItemResult {
            item_id: view.id.clone(),
            path: view.path.clone(),
            artist: view.album_artist.clone(),
            album: view.album.clone(),
            tier: None,
            matched_words: vec![],
            previous_rating: prev.map(String::from),
            action: act,
            source: Source::Reset,
            has_lyrics: false,
            server_name: server_config.name.clone(),
        });
    }
    Ok(results)
}

/// Counts for summary output.
#[derive(Debug, Default)]
pub struct SummaryCounts {
    pub lyrics_evaluated: usize,
    pub r_rated: usize,
    pub pg13: usize,
    pub clean: usize,
    pub ratings_set: usize,
    pub already_correct: usize,
    pub cleared: usize,
    pub g_genre_set: usize,
    pub g_genre_already: usize,
    pub g_genre_dry: usize,
    pub dry_run: usize,
    pub skipped: usize,
    pub needs_review: usize,
    /// Tracks in the lyrics path for which no lyrics were found (genuinely
    /// lyric-less). Distinct from `clean` (lyrics found, no explicit content).
    pub no_lyrics: usize,
    pub errors: usize,
}

impl SummaryCounts {
    pub fn from_results(results: &[ItemResult]) -> Self {
        let mut c = Self::default();
        for r in results {
            // Lyrics evaluated = lyrics were actually fetched and classified,
            // driven by the explicit has_lyrics flag rather than inferred from
            // tier/action (which cannot tell "no lyrics" from "clean lyrics").
            if r.has_lyrics {
                c.lyrics_evaluated += 1;
            }
            // No lyrics = a track in the lyrics path that had none. This is the
            // true no-lyrics fallthrough, previously conflated with clean tracks.
            if r.source == Source::Lyrics && !r.has_lyrics {
                c.no_lyrics += 1;
            }
            // Tier counts. Scoped to lyrics-evaluated items so the R/PG-13
            // sub-lines (printed under "Lyrics evaluated") stay consistent with
            // `clean`/`lyrics_evaluated` and force-rated items don't inflate them.
            if r.has_lyrics {
                match r.tier.as_deref() {
                    Some("R") => c.r_rated += 1,
                    Some("PG-13") => c.pg13 += 1,
                    _ => {}
                }
            }
            // Clean = lyrics were evaluated but carried no explicit content
            // (tier not R/PG-13). Covers both representations: clean->cleared
            // (tier None) and clean->G (tier Some("G"), the default policy).
            if r.has_lyrics && !matches!(r.tier.as_deref(), Some("R") | Some("PG-13")) {
                c.clean += 1;
            }
            // Action counts by source
            match (&r.action, &r.source) {
                (RatingAction::Set, Source::Genre) => c.g_genre_set += 1,
                (RatingAction::Set, _) => c.ratings_set += 1,
                (RatingAction::AlreadyCorrect, Source::Genre) => c.g_genre_already += 1,
                (RatingAction::AlreadyCorrect, _) => c.already_correct += 1,
                (RatingAction::Cleared, _) => c.cleared += 1,
                (RatingAction::DryRun, Source::Genre) => c.g_genre_dry += 1,
                (RatingAction::DryRun, _) => c.dry_run += 1,
                (RatingAction::DryRunClear, _) => c.dry_run += 1,
                (RatingAction::Skipped, _) => c.skipped += 1,
                (RatingAction::Review, _) => c.needs_review += 1,
                (RatingAction::Error(_), _) => c.errors += 1,
            }
        }
        c
    }
}

/// Print summary counts to stdout.
pub fn print_summary(results: &[ItemResult], label: &str) {
    let c = SummaryCounts::from_results(results);
    if !label.is_empty() {
        println!("\n=== {} ===", label);
    }
    println!();
    println!("=== Rating Summary ===");
    if c.lyrics_evaluated > 0 {
        println!("  Lyrics evaluated:    {}", c.lyrics_evaluated);
        println!("    R-rated:           {}", c.r_rated);
        println!("    PG-13:             {}", c.pg13);
        println!("    Clean:             {}", c.clean);
    }
    if c.no_lyrics > 0 {
        println!("  No lyrics:           {}", c.no_lyrics);
    }
    println!("  Ratings set:         {}", c.ratings_set);
    println!("  Already correct:     {}", c.already_correct);
    println!("  Ratings cleared:     {}", c.cleared);
    if c.g_genre_set > 0 || c.g_genre_already > 0 || c.g_genre_dry > 0 {
        println!("  G (genre-matched):   {}", c.g_genre_set);
        println!("  Already G (genre):   {}", c.g_genre_already);
        if c.g_genre_dry > 0 {
            println!("  Dry-run G (genre):   {}", c.g_genre_dry);
        }
    }
    if c.skipped > 0 {
        println!("  Skipped:             {}", c.skipped);
    }
    if c.needs_review > 0 {
        println!("  Needs review (deny): {}", c.needs_review);
    }
    if c.dry_run > 0 {
        println!("  Dry-run would act:   {}", c.dry_run);
    }
    println!("  Errors:              {}", c.errors);
}
