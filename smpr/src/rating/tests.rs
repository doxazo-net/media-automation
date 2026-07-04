use super::*;
use crate::server::types::{AudioItemView, VirtualFolder};
use serde_json::json;

fn music_lib(name: &str, item_id: &str, locations: Vec<&str>) -> VirtualFolder {
    VirtualFolder {
        name: name.to_string(),
        item_id: item_id.to_string(),
        collection_type: Some("music".to_string()),
        locations: locations.into_iter().map(String::from).collect(),
    }
}

fn audio_item(id: &str, path: &str) -> (AudioItemView, serde_json::Value) {
    let val = json!({
        "Id": id,
        "Path": path,
        "Genres": []
    });
    let view = AudioItemView {
        id: id.to_string(),
        name: None,
        path: Some(path.to_string()),
        official_rating: None,
        album_artist: None,
        album: None,
        genres: vec![],
        run_time_ticks: None,
        provider_ids: None,
        date_created: None,
    };
    (view, val)
}

// ── resolve_from_libraries tests ────────────────────────────────────

#[test]
fn scope_no_flags_returns_none() {
    let libs = vec![music_lib("Music", "lib1", vec!["/music"])];
    let scope = scope::resolve_from_libraries(&libs, None, None).unwrap();
    assert!(scope.parent_id.is_none());
    assert!(scope.location_path.is_none());
}

#[test]
fn scope_library_by_name() {
    let libs = vec![
        music_lib("Music", "lib1", vec!["/music"]),
        music_lib("Audiobooks", "lib2", vec!["/audiobooks"]),
    ];
    let scope = scope::resolve_from_libraries(&libs, Some("Music"), None).unwrap();
    assert_eq!(scope.parent_id.as_deref(), Some("lib1"));
    assert!(scope.location_path.is_none());
    assert_eq!(scope.library_name.as_deref(), Some("Music"));
}

#[test]
fn scope_library_case_insensitive() {
    let libs = vec![music_lib("Music", "lib1", vec!["/music"])];
    let scope = scope::resolve_from_libraries(&libs, Some("music"), None).unwrap();
    assert_eq!(scope.parent_id.as_deref(), Some("lib1"));
}

#[test]
fn scope_library_not_found() {
    let libs = vec![music_lib("Music", "lib1", vec!["/music"])];
    let result = scope::resolve_from_libraries(&libs, Some("Nonexistent"), None);
    assert!(matches!(result, Err(RatingError::LibraryNotFound { .. })));
}

#[test]
fn scope_location_without_library() {
    let libs = vec![music_lib(
        "Music",
        "lib1",
        vec!["/music/rock", "/music/classical"],
    )];
    let scope = scope::resolve_from_libraries(&libs, None, Some("classical")).unwrap();
    assert_eq!(scope.parent_id.as_deref(), Some("lib1"));
    assert_eq!(scope.location_path.as_deref(), Some("/music/classical"));
}

#[test]
fn scope_location_with_library() {
    let libs = vec![music_lib(
        "Music",
        "lib1",
        vec!["/music/rock", "/music/classical"],
    )];
    let scope = scope::resolve_from_libraries(&libs, Some("Music"), Some("classical")).unwrap();
    assert_eq!(scope.parent_id.as_deref(), Some("lib1"));
    assert_eq!(scope.location_path.as_deref(), Some("/music/classical"));
}

#[test]
fn scope_location_not_found() {
    let libs = vec![music_lib("Music", "lib1", vec!["/music/rock"])];
    let result = scope::resolve_from_libraries(&libs, Some("Music"), Some("jazz"));
    assert!(matches!(result, Err(RatingError::LocationNotFound { .. })));
}

#[test]
fn scope_no_music_libraries() {
    let libs: Vec<VirtualFolder> = vec![];
    let result = scope::resolve_from_libraries(&libs, Some("Music"), None);
    assert!(matches!(result, Err(RatingError::NoMusicLibraries)));
}

#[test]
fn scope_location_windows_path() {
    let libs = vec![music_lib("Music", "lib1", vec!["D:\\Music\\classical"])];
    let scope = scope::resolve_from_libraries(&libs, None, Some("classical")).unwrap();
    assert_eq!(scope.location_path.as_deref(), Some("D:\\Music\\classical"));
}

// ── filter_by_location tests ────────────────────────────────────────

#[test]
fn filter_location_keeps_matching_paths() {
    let items = vec![
        audio_item("1", "/music/classical/bach.flac"),
        audio_item("2", "/music/rock/acdc.flac"),
        audio_item("3", "/music/classical/mozart.flac"),
    ];
    let filtered = scope::filter_by_location(items, "/music/classical");
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].0.id, "1");
    assert_eq!(filtered[1].0.id, "3");
}

#[test]
fn filter_location_empty_when_no_match() {
    let items = vec![audio_item("1", "/music/rock/acdc.flac")];
    let filtered = scope::filter_by_location(items, "/music/classical");
    assert!(filtered.is_empty());
}

#[test]
fn filter_location_empty_on_mount_view_mismatch() {
    // Posix location prefix vs UNC-style item paths share no prefix. Here the
    // items live under `Music`, not the requested `Classical`, so even the
    // leaf-segment fallback declines them and the empty-match WARN path stands.
    let items = vec![
        audio_item("1", r"\\outatime\Music\Bach\air.flac"),
        audio_item("2", r"\\outatime\Music\Mozart\k525.flac"),
    ];
    let filtered = scope::filter_by_location(items, "/share/Classical");
    assert!(filtered.is_empty());
}

#[test]
fn sample_path_roots_dedups_and_caps() {
    let items = vec![
        audio_item("1", r"\\outatime\Music\Bach\air.flac"),
        audio_item("2", r"\\outatime\Music\Mozart\k525.flac"),
        audio_item("3", "/share/Classical/x.flac"),
    ];
    let roots = scope::sample_path_roots(&items);
    // UNC paths collapse to one root; posix path is a distinct root. Leading
    // markers are preserved so UNC (`//`) and POSIX (`/`) stay distinguishable.
    assert_eq!(
        roots,
        vec![
            "//outatime/music".to_string(),
            "/share/classical".to_string()
        ]
    );
    assert!(roots.len() <= 5);
}

#[test]
fn filter_location_trailing_slash() {
    let items = vec![audio_item("1", "/music/classical/bach.flac")];
    let filtered = scope::filter_by_location(items, "/music/classical/");
    assert_eq!(filtered.len(), 1);
}

#[test]
fn filter_location_case_insensitive() {
    let items = vec![audio_item("1", "/Music/Classical/bach.flac")];
    let filtered = scope::filter_by_location(items, "/music/classical");
    assert_eq!(filtered.len(), 1);
}

#[test]
fn filter_location_windows_backslash() {
    let items = vec![audio_item("1", "D:\\Music\\classical\\bach.flac")];
    let filtered = scope::filter_by_location(items, "D:\\Music\\classical");
    assert_eq!(filtered.len(), 1);
}

#[test]
fn filter_location_leaf_fallback_recovers_unc_paths() {
    // Issue #216: Emby reports the location as posix `/share/Classical` but the
    // indexed item paths are UNC `\\host\Classical\...`. The full-prefix match
    // shares nothing, so without a fallback the run rates zero items. The
    // leaf-segment fallback must recover the Classical tracks.
    let items = vec![
        audio_item("1", r"\\outatime\Classical\Bach\air.flac"),
        audio_item("2", r"\\outatime\Classical\Mozart\k525.flac"),
    ];
    let filtered = scope::filter_by_location(items, "/share/Classical");
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].0.id, "1");
    assert_eq!(filtered[1].0.id, "2");
}

#[test]
fn filter_location_leaf_fallback_is_segment_bounded() {
    // The fallback matches the leaf as a whole `/classical/` path segment, so a
    // sibling folder `Classical_Remix` (same library, UNC view) must NOT be
    // swept in just because its name starts with the leaf.
    let items = vec![
        audio_item("1", r"\\outatime\Classical\Bach\air.flac"),
        audio_item("2", r"\\outatime\Classical_Remix\bootleg.flac"),
    ];
    let filtered = scope::filter_by_location(items, "/share/Classical");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].0.id, "1");
}

#[test]
fn filter_location_primary_match_skips_fallback() {
    // When the full prefix matches (aligned mount views), behavior is unchanged
    // and the leaf fallback never engages -- a same-leaf item under a different
    // root is excluded exactly as today.
    let items = vec![
        audio_item("1", "/music/classical/bach.flac"),
        audio_item("2", "/other/classical/decoy.flac"),
    ];
    let filtered = scope::filter_by_location(items, "/music/classical");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].0.id, "1");
}

#[test]
fn filter_location_fallback_still_empty_when_no_leaf_match() {
    // Neither the prefix nor the leaf segment matches: the loud empty-result
    // path is preserved.
    let items = vec![
        audio_item("1", r"\\outatime\Classical\Bach\air.flac"),
        audio_item("2", r"\\outatime\Kids_Music\raffi\baby.flac"),
    ];
    let filtered = scope::filter_by_location(items, "/share/Jazz");
    assert!(filtered.is_empty());
}

// ── force-rating rule tests (issue #235) ────────────────────────────

use crate::config::{LibraryConfig, LocationConfig, OverrideRule, ServerConfig};
use std::collections::BTreeMap;

/// "Music" library force PG-13, with its "classical" location force G.
fn server_with_force_ratings() -> ServerConfig {
    let mut locations = BTreeMap::new();
    locations.insert(
        "classical".to_string(),
        LocationConfig {
            force_rating: Some("G".to_string()),
        },
    );
    let mut libraries = BTreeMap::new();
    libraries.insert(
        "Music".to_string(),
        LibraryConfig {
            force_rating: Some("PG-13".to_string()),
            locations,
        },
    );
    ServerConfig {
        name: "test".to_string(),
        url: "http://localhost".to_string(),
        api_key: "key".to_string(),
        server_type: None,
        libraries,
    }
}

/// VirtualFolders whose "Music" library spans Classical, Rock, and Music folders.
fn force_libs() -> Vec<VirtualFolder> {
    vec![music_lib(
        "Music",
        "lib1",
        vec!["/share/Classical", "/share/Rock", "/share/Music"],
    )]
}

#[test]
fn force_rule_location_overrides_library() {
    // Classical folder: library (PG-13) and location (G) both match; location wins.
    let rules = scope::build_force_rules(&server_with_force_ratings(), &force_libs());
    assert_eq!(
        scope::resolve_force_rating(&rules, Some("/share/Classical/bach.flac")),
        Some("G")
    );
}

#[test]
fn force_rule_library_fallback() {
    // Rock folder: only the library-level force applies.
    let rules = scope::build_force_rules(&server_with_force_ratings(), &force_libs());
    assert_eq!(
        scope::resolve_force_rating(&rules, Some("/share/Rock/acdc.flac")),
        Some("PG-13")
    );
}

#[test]
fn force_rule_full_run_applies_by_path() {
    // Acceptance (#235): with NO scope flags, an item is forced purely by its path.
    let rules = scope::build_force_rules(&server_with_force_ratings(), &force_libs());
    assert_eq!(
        scope::resolve_force_rating(&rules, Some("/share/Music/pop/song.flac")),
        Some("PG-13")
    );
}

#[test]
fn force_rule_no_match_outside_library() {
    let rules = scope::build_force_rules(&server_with_force_ratings(), &force_libs());
    assert_eq!(
        scope::resolve_force_rating(&rules, Some("/other/place/x.flac")),
        None
    );
}

#[test]
fn force_rule_no_path_is_none() {
    let rules = scope::build_force_rules(&server_with_force_ratings(), &force_libs());
    assert_eq!(scope::resolve_force_rating(&rules, None), None);
}

#[test]
fn force_rule_case_insensitive() {
    let rules = scope::build_force_rules(&server_with_force_ratings(), &force_libs());
    assert_eq!(
        scope::resolve_force_rating(&rules, Some("/SHARE/Classical/BACH.flac")),
        Some("G")
    );
}

#[test]
fn force_rule_mount_view_mismatch_leaf_fallback() {
    // A UNC-indexed item shares no prefix with the posix location path; the
    // bounded leaf-segment fallback still forces it (location beats library).
    let rules = scope::build_force_rules(&server_with_force_ratings(), &force_libs());
    assert_eq!(
        scope::resolve_force_rating(&rules, Some(r"\\outatime\Classical\Bach\air.flac")),
        Some("G")
    );
}

#[test]
fn force_rule_leaf_does_not_beat_a_real_prefix_match() {
    // CR #237: in an aligned-mount run, a folder literally named "Classical"
    // inside the Music tree must NOT be forced by the classical *location* rule
    // via its leaf just because location rules outrank library rules. Prefix
    // matches are resolved first; the leaf phase only runs when none exist. So
    // the decoy resolves to the library force (PG-13), not G.
    let rules = scope::build_force_rules(&server_with_force_ratings(), &force_libs());
    assert_eq!(
        scope::resolve_force_rating(&rules, Some("/share/Music/VA/Classical/decoy.flac")),
        Some("PG-13")
    );
    // The genuine classical track (real prefix match) is still forced G.
    assert_eq!(
        scope::resolve_force_rating(&rules, Some("/share/Classical/real.flac")),
        Some("G")
    );
}

#[test]
fn force_rule_unconfigured_library_yields_nothing() {
    // A configured library with no matching VirtualFolder produces no rules.
    let rules = scope::build_force_rules(&server_with_force_ratings(), &[]);
    assert!(rules.is_empty());
    assert_eq!(
        scope::resolve_force_rating(&rules, Some("/share/Classical/x.flac")),
        None
    );
}

// ── per-song override tests (issue #236) ────────────────────────────

fn sample_overrides() -> Vec<OverrideRule> {
    vec![
        OverrideRule {
            match_key: "artist/album".into(),
            rating: Some("G".into()),
            skip: false,
        },
        OverrideRule {
            match_key: "artist/album/07. track".into(),
            rating: None,
            skip: true,
        },
    ]
}

#[test]
fn override_album_wide_match() {
    let ovs = sample_overrides();
    let hit = scope::resolve_override(&ovs, Some("/music/Artist/Album/05. Song.flac")).unwrap();
    assert_eq!(hit.rating.as_deref(), Some("G"));
    assert!(!hit.skip);
}

#[test]
fn override_longest_key_wins() {
    // The single-track key is more specific than the album key, so it wins.
    let ovs = sample_overrides();
    let hit = scope::resolve_override(&ovs, Some("/music/Artist/Album/07. Track.flac")).unwrap();
    assert!(hit.skip);
}

#[test]
fn override_no_match() {
    let ovs = sample_overrides();
    assert!(scope::resolve_override(&ovs, Some("/music/Other/Thing/01. x.flac")).is_none());
    assert!(scope::resolve_override(&ovs, None).is_none());
}

#[test]
fn override_match_count_helper() {
    assert!(scope::path_contains_key(
        Some("/music/Artist/Album/05. Song.flac"),
        "artist/album"
    ));
    assert!(!scope::path_contains_key(
        Some("/music/Zzz/01. y.flac"),
        "artist/album"
    ));
    assert!(!scope::path_contains_key(None, "artist/album"));
}

// ── override precedence through rate_item (issue #236) ───────────────

use crate::config::{Config, DetectionConfig, ServerType, SourcesConfig};
use crate::detection::DetectionEngine;
use crate::server::MediaServerClient;

fn override_test_config(overrides: Vec<OverrideRule>, dry_run: bool) -> Config {
    Config {
        servers: vec![],
        detection: DetectionConfig {
            r_stems: vec!["fuck".into()],
            r_exact: vec![],
            pg13_stems: vec![],
            pg13_exact: vec![],
            false_positives: vec![],
            g_genres: vec![],
            deny_genres: vec![],
        },
        overwrite: true,
        clean_rating: Some("G".into()),
        dry_run,
        report_path: None,
        library_name: None,
        location_name: None,
        verbose: false,
        ignore_forced: false,
        no_sources: false,
        overrides,
        sources: SourcesConfig::default(),
    }
}

#[test]
fn override_forces_rating_over_lyrics() {
    // An override resolves to its rating WITHOUT fetching lyrics (override >
    // lyrics precedence). Dry-run + a bogus client URL guarantee no network call
    // is reached on this path; if lyrics were fetched the client would be hit.
    let client =
        MediaServerClient::new("http://127.0.0.1:9".into(), "key".into(), ServerType::Emby);
    let cfg = override_test_config(
        vec![OverrideRule {
            match_key: "artist/album".into(),
            rating: Some("R".into()),
            skip: false,
        }],
        true,
    );
    let engine = DetectionEngine::new(&cfg.detection);
    let (view, raw) = audio_item("x", "/music/Artist/Album/01. song.flac");
    let res = rate_item(&client, &cfg, &engine, &view, &raw, None, None, "srv").unwrap();
    assert_eq!(res.source, Source::Override);
    assert_eq!(res.action, RatingAction::DryRun);
    assert_eq!(res.tier.as_deref(), Some("R"));
    assert!(!res.has_lyrics);
}

fn store_with_verdict(
    key: &str,
    verdict: crate::sources::SourceVerdict,
) -> crate::store::SourceStore {
    let store = crate::store::SourceStore::open_in_memory().unwrap();
    store
        .upsert(&crate::store::VerdictRecord {
            track_key: key.to_string(),
            mbid: None,
            server_name: None,
            artist: None,
            album: None,
            title: None,
            duration_s: None,
            source: "itunes".to_string(),
            source_track_id: None,
            source_verdict: verdict,
            match_confidence: 1.0,
            duration_delta_s: None,
            curated_override: None,
            notes: None,
        })
        .unwrap();
    store
}

#[test]
fn authoritative_explicit_sets_r() {
    // An Explicit store verdict sets R via the authoritative tier, short-circuiting
    // before lyrics (dry-run + bogus client guarantee no network call).
    let client =
        MediaServerClient::new("http://127.0.0.1:9".into(), "key".into(), ServerType::Emby);
    let cfg = override_test_config(vec![], true);
    let engine = DetectionEngine::new(&cfg.detection);
    let (view, raw) = audio_item("x", "/music/Artist/Album/01. song.flac");
    let key = crate::enrich::track_key_for_item(&view);
    let store = store_with_verdict(&key, crate::sources::SourceVerdict::Explicit);
    let res = rate_item(
        &client,
        &cfg,
        &engine,
        &view,
        &raw,
        None,
        Some(&store),
        "srv",
    )
    .unwrap();
    assert_eq!(res.source, Source::Authoritative);
    assert_eq!(res.tier.as_deref(), Some("R"));
    assert_eq!(res.action, RatingAction::DryRun);
}

#[test]
fn authoritative_not_explicit_falls_through() {
    // A NotExplicit verdict does not short-circuit; the item falls through to the
    // (stream-less, network-free) no-lyrics path - not the authoritative tier.
    let client =
        MediaServerClient::new("http://127.0.0.1:9".into(), "key".into(), ServerType::Emby);
    let cfg = override_test_config(vec![], true);
    let engine = DetectionEngine::new(&cfg.detection);
    let (view, raw) = audio_item("x", "/music/Artist/Album/01. song.flac");
    let key = crate::enrich::track_key_for_item(&view);
    let store = store_with_verdict(&key, crate::sources::SourceVerdict::NotExplicit);
    let res = rate_item(
        &client,
        &cfg,
        &engine,
        &view,
        &raw,
        None,
        Some(&store),
        "srv",
    )
    .unwrap();
    assert_ne!(res.source, Source::Authoritative);
}

#[test]
fn force_rating_outranks_authoritative() {
    // force_rating is resolved before the authoritative tier, so it wins even when
    // an Explicit verdict is present.
    let client =
        MediaServerClient::new("http://127.0.0.1:9".into(), "key".into(), ServerType::Emby);
    let cfg = override_test_config(vec![], true);
    let engine = DetectionEngine::new(&cfg.detection);
    let (view, raw) = audio_item("x", "/music/Artist/Album/01. song.flac");
    let key = crate::enrich::track_key_for_item(&view);
    let store = store_with_verdict(&key, crate::sources::SourceVerdict::Explicit);
    let res = rate_item(
        &client,
        &cfg,
        &engine,
        &view,
        &raw,
        Some("G"),
        Some(&store),
        "srv",
    )
    .unwrap();
    assert_eq!(res.source, Source::Force);
    assert_eq!(res.tier.as_deref(), Some("G"));
}

#[test]
fn ignore_forced_bypasses_authoritative() {
    let client =
        MediaServerClient::new("http://127.0.0.1:9".into(), "key".into(), ServerType::Emby);
    let mut cfg = override_test_config(vec![], true);
    cfg.ignore_forced = true;
    let engine = DetectionEngine::new(&cfg.detection);
    let (view, raw) = audio_item("x", "/music/Artist/Album/01. song.flac");
    let key = crate::enrich::track_key_for_item(&view);
    let store = store_with_verdict(&key, crate::sources::SourceVerdict::Explicit);
    let res = rate_item(
        &client,
        &cfg,
        &engine,
        &view,
        &raw,
        None,
        Some(&store),
        "srv",
    )
    .unwrap();
    assert_ne!(res.source, Source::Authoritative);
}

#[test]
fn none_store_skips_authoritative() {
    // No store (--no-sources / no enrich run) -> the tier is skipped entirely.
    let client =
        MediaServerClient::new("http://127.0.0.1:9".into(), "key".into(), ServerType::Emby);
    let cfg = override_test_config(vec![], true);
    let engine = DetectionEngine::new(&cfg.detection);
    let (view, raw) = audio_item("x", "/music/Artist/Album/01. song.flac");
    let res = rate_item(&client, &cfg, &engine, &view, &raw, None, None, "srv").unwrap();
    assert_ne!(res.source, Source::Authoritative);
}

#[test]
fn no_sources_bypasses_tier_even_with_store() {
    // --no-sources must disable the tier even when a store with an Explicit
    // verdict is passed directly to rate_item.
    let client =
        MediaServerClient::new("http://127.0.0.1:9".into(), "key".into(), ServerType::Emby);
    let mut cfg = override_test_config(vec![], true);
    cfg.no_sources = true;
    let engine = DetectionEngine::new(&cfg.detection);
    let (view, raw) = audio_item("x", "/music/Artist/Album/01. song.flac");
    let key = crate::enrich::track_key_for_item(&view);
    let store = store_with_verdict(&key, crate::sources::SourceVerdict::Explicit);
    let res = rate_item(
        &client,
        &cfg,
        &engine,
        &view,
        &raw,
        None,
        Some(&store),
        "srv",
    )
    .unwrap();
    assert_ne!(res.source, Source::Authoritative);
}

#[test]
fn authoritative_cleaned_falls_through() {
    // A Cleaned verdict (radio edit) is not R; it falls through to lyrics.
    let client =
        MediaServerClient::new("http://127.0.0.1:9".into(), "key".into(), ServerType::Emby);
    let cfg = override_test_config(vec![], true);
    let engine = DetectionEngine::new(&cfg.detection);
    let (view, raw) = audio_item("x", "/music/Artist/Album/01. song.flac");
    let key = crate::enrich::track_key_for_item(&view);
    let store = store_with_verdict(&key, crate::sources::SourceVerdict::Cleaned);
    let res = rate_item(
        &client,
        &cfg,
        &engine,
        &view,
        &raw,
        None,
        Some(&store),
        "srv",
    )
    .unwrap();
    assert_ne!(res.source, Source::Authoritative);
}

#[test]
fn override_outranks_authoritative() {
    // A per-song override wins over an Explicit store verdict (override > authoritative).
    let client =
        MediaServerClient::new("http://127.0.0.1:9".into(), "key".into(), ServerType::Emby);
    let cfg = override_test_config(
        vec![OverrideRule {
            match_key: "artist/album".into(),
            rating: Some("G".into()),
            skip: false,
        }],
        true,
    );
    let engine = DetectionEngine::new(&cfg.detection);
    let (view, raw) = audio_item("x", "/music/Artist/Album/01. song.flac");
    let key = crate::enrich::track_key_for_item(&view);
    let store = store_with_verdict(&key, crate::sources::SourceVerdict::Explicit);
    let res = rate_item(
        &client,
        &cfg,
        &engine,
        &view,
        &raw,
        None,
        Some(&store),
        "srv",
    )
    .unwrap();
    assert_eq!(res.source, Source::Override);
    assert_eq!(res.tier.as_deref(), Some("G"));
}

#[test]
fn curated_override_explicit_fires_at_rate() {
    // A user curation of Explicit (over a source NotExplicit) drives R at rate
    // time - effective_verdict honors the curated override.
    let client =
        MediaServerClient::new("http://127.0.0.1:9".into(), "key".into(), ServerType::Emby);
    let cfg = override_test_config(vec![], true);
    let engine = DetectionEngine::new(&cfg.detection);
    let (view, raw) = audio_item("x", "/music/Artist/Album/01. song.flac");
    let key = crate::enrich::track_key_for_item(&view);
    let store = store_with_verdict(&key, crate::sources::SourceVerdict::NotExplicit);
    store
        .set_curated(&key, Some(crate::sources::SourceVerdict::Explicit))
        .unwrap();
    let res = rate_item(
        &client,
        &cfg,
        &engine,
        &view,
        &raw,
        None,
        Some(&store),
        "srv",
    )
    .unwrap();
    assert_eq!(res.source, Source::Authoritative);
    assert_eq!(res.tier.as_deref(), Some("R"));
}

#[test]
fn override_skip_leaves_rating_untouched() {
    // skip = true short-circuits with no server call even when NOT a dry run, and
    // preserves the existing rating.
    let client =
        MediaServerClient::new("http://127.0.0.1:9".into(), "key".into(), ServerType::Emby);
    let cfg = override_test_config(
        vec![OverrideRule {
            match_key: "artist/album".into(),
            rating: None,
            skip: true,
        }],
        false,
    );
    let engine = DetectionEngine::new(&cfg.detection);
    let (mut view, raw) = audio_item("x", "/music/Artist/Album/01. song.flac");
    view.official_rating = Some("R".into());
    let res = rate_item(&client, &cfg, &engine, &view, &raw, None, None, "srv").unwrap();
    assert_eq!(res.source, Source::Override);
    assert_eq!(res.action, RatingAction::Skipped);
    assert_eq!(res.previous_rating.as_deref(), Some("R"));
}

#[test]
fn ignore_forced_bypasses_override() {
    // --ignore-forced suppresses per-song overrides too. `audio_item` sets no
    // media streams and empty genres, so with the override skipped the item
    // falls through the lyrics path to the no-lyrics/no-genre skip: source
    // Lyrics, has_lyrics false. (No network: a stream-less Emby raw resolves
    // lyrics purely from the JSON, never reaching the bogus client.)
    let client =
        MediaServerClient::new("http://127.0.0.1:9".into(), "key".into(), ServerType::Emby);
    let mut cfg = override_test_config(
        vec![OverrideRule {
            match_key: "artist/album".into(),
            rating: Some("R".into()),
            skip: false,
        }],
        true,
    );
    cfg.ignore_forced = true;
    let engine = DetectionEngine::new(&cfg.detection);
    let (view, raw) = audio_item("x", "/music/Artist/Album/01. song.flac");
    let res = rate_item(&client, &cfg, &engine, &view, &raw, None, None, "srv").unwrap();
    assert_eq!(res.source, Source::Lyrics);
    assert!(!res.has_lyrics);
}

// ── decide_rating_action tests ──────────────────────────────────────

use super::action;

#[test]
fn decide_already_correct() {
    let result = action::decide_rating_action("R", Some("R"), true, false);
    assert_eq!(result, RatingAction::AlreadyCorrect);
}

#[test]
fn decide_dry_run() {
    let result = action::decide_rating_action("R", Some("G"), true, true);
    assert_eq!(result, RatingAction::DryRun);
}

#[test]
fn decide_skip_existing() {
    let result = action::decide_rating_action("R", Some("G"), false, false);
    assert_eq!(result, RatingAction::Skipped);
}

#[test]
fn decide_set_no_previous() {
    let result = action::decide_rating_action("R", None, true, false);
    assert_eq!(result, RatingAction::Set);
}

#[test]
fn decide_set_different_rating() {
    let result = action::decide_rating_action("R", Some("G"), true, false);
    assert_eq!(result, RatingAction::Set);
}

#[test]
fn decide_set_overwrite_true_no_skip() {
    let result = action::decide_rating_action("PG-13", Some("R"), true, false);
    assert_eq!(result, RatingAction::Set);
}

// ── decide_clear_action tests ───────────────────────────────────────

#[test]
fn decide_clear_overwrite_has_rating() {
    let result = action::decide_clear_action(Some("R"), true, false);
    assert_eq!(result, RatingAction::Cleared);
}

#[test]
fn decide_clear_no_rating() {
    let result = action::decide_clear_action(None, true, false);
    assert_eq!(result, RatingAction::Skipped);
}

#[test]
fn decide_clear_skip_existing() {
    let result = action::decide_clear_action(Some("R"), false, false);
    assert_eq!(result, RatingAction::Skipped);
}

#[test]
fn decide_clear_dry_run() {
    let result = action::decide_clear_action(Some("R"), true, true);
    assert_eq!(result, RatingAction::DryRunClear);
}

#[test]
fn decide_clear_empty_string_rating() {
    let result = action::decide_clear_action(Some(""), true, false);
    assert_eq!(result, RatingAction::Skipped);
}

// Additional force/reset decision tests

#[test]
fn force_skip_existing_with_rating() {
    let result = action::decide_rating_action("G", Some("R"), false, false);
    assert_eq!(result, RatingAction::Skipped);
}

#[test]
fn force_set_no_existing() {
    let result = action::decide_rating_action("G", None, true, false);
    assert_eq!(result, RatingAction::Set);
}

#[test]
fn reset_no_rating_to_clear() {
    let result = action::decide_clear_action(None, true, false);
    assert_eq!(result, RatingAction::Skipped);
}

#[test]
fn reset_clears_existing() {
    let result = action::decide_clear_action(Some("R"), true, false);
    assert_eq!(result, RatingAction::Cleared);
}

#[test]
fn reset_dry_run() {
    let result = action::decide_clear_action(Some("PG-13"), true, true);
    assert_eq!(result, RatingAction::DryRunClear);
}

#[test]
fn report_csv_output() {
    let results = vec![
        ItemResult {
            item_id: "id1".into(),
            // Mixed case + backslashes so the assertions below actually exercise
            // the path column's normalization contract (CR #237).
            path: Some(r"C:\Music\Artist\Album\Track.FLAC".into()),
            artist: Some("Artist".into()),
            album: Some("Album".into()),
            tier: Some("R".into()),
            matched_words: vec!["word1".into(), "word2".into()],
            previous_rating: Some("G".into()),
            action: RatingAction::Set,
            source: Source::Lyrics,
            has_lyrics: true,
            server_name: "home-emby".into(),
        },
        ItemResult {
            item_id: "id2".into(),
            path: Some("/music/artist2/album2/clean.flac".into()),
            artist: None,
            album: None,
            tier: None,
            matched_words: vec![],
            previous_rating: None,
            action: RatingAction::Skipped,
            source: Source::Lyrics,
            has_lyrics: false,
            server_name: "home-emby".into(),
        },
    ];
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("report.csv");
    crate::report::write_report(&results, &path);
    let content = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(
        lines[0],
        "artist,album,track,path,tier,matched_words,previous_rating,action,source,server,has_lyrics"
    );
    assert!(lines[1].contains("Artist"));
    assert!(lines[1].contains("Album"));
    // `track` column keeps the raw filename (case preserved).
    assert!(lines[1].contains("Track.FLAC"));
    // `path` column is the normalized match-key: lowercased AND separator-
    // normalized (backslashes -> forward slashes), never the raw input form.
    assert!(lines[1].contains("c:/music/artist/album/track.flac"));
    assert!(!lines[1].contains(r"C:\Music"));
    assert!(lines[1].contains("R"));
    assert!(lines[1].contains("word1; word2"));
    assert!(lines[1].contains("set"));
    assert!(lines[1].contains("lyrics"));
    assert!(lines[1].contains("home-emby"));
    assert!(lines[1].ends_with("true")); // has_lyrics column
    assert!(lines[2].contains("clean.flac"));
    assert!(lines[2].contains("skipped"));
    assert!(lines[2].ends_with("false")); // has_lyrics column
}

#[test]
fn summary_counts_actions() {
    let results = vec![
        ItemResult {
            item_id: "1".into(),
            has_lyrics: true,
            path: None,
            artist: None,
            album: None,
            tier: Some("R".into()),
            matched_words: vec!["fuck".into()],
            previous_rating: None,
            action: RatingAction::Set,
            source: Source::Lyrics,
            server_name: "s".into(),
        },
        ItemResult {
            item_id: "2".into(),
            has_lyrics: true,
            path: None,
            artist: None,
            album: None,
            tier: Some("PG-13".into()),
            matched_words: vec!["bitch".into()],
            previous_rating: None,
            action: RatingAction::DryRun,
            source: Source::Lyrics,
            server_name: "s".into(),
        },
        ItemResult {
            item_id: "3".into(),
            has_lyrics: false,
            path: None,
            artist: None,
            album: None,
            tier: Some("G".into()),
            matched_words: vec!["Classical".into()],
            previous_rating: None,
            action: RatingAction::Set,
            source: Source::Genre,
            server_name: "s".into(),
        },
        ItemResult {
            item_id: "4".into(),
            has_lyrics: true,
            path: None,
            artist: None,
            album: None,
            tier: None,
            matched_words: vec![],
            previous_rating: Some("R".into()),
            action: RatingAction::Cleared,
            source: Source::Lyrics,
            server_name: "s".into(),
        },
        ItemResult {
            item_id: "5".into(),
            has_lyrics: true,
            path: None,
            artist: None,
            album: None,
            tier: Some("R".into()),
            matched_words: vec![],
            previous_rating: Some("R".into()),
            action: RatingAction::AlreadyCorrect,
            source: Source::Lyrics,
            server_name: "s".into(),
        },
        ItemResult {
            item_id: "6".into(),
            has_lyrics: false,
            path: None,
            artist: None,
            album: None,
            tier: None,
            matched_words: vec![],
            previous_rating: None,
            action: RatingAction::Skipped,
            source: Source::Lyrics,
            server_name: "s".into(),
        },
        ItemResult {
            item_id: "7".into(),
            has_lyrics: false,
            path: None,
            artist: None,
            album: None,
            tier: Some("G".into()),
            matched_words: vec!["Ambient".into()],
            previous_rating: Some("G".into()),
            action: RatingAction::AlreadyCorrect,
            source: Source::Genre,
            server_name: "s".into(),
        },
        ItemResult {
            item_id: "8".into(),
            has_lyrics: false,
            path: None,
            artist: None,
            album: None,
            tier: None,
            matched_words: vec!["Soundtrack".into()],
            previous_rating: None,
            action: RatingAction::Review,
            source: Source::Genre,
            server_name: "s".into(),
        },
        // #9: clean lyrics, no prior rating -> Skipped. Previously miscounted as
        // a no-lyrics skip; has_lyrics=true is the signal that fixes it.
        ItemResult {
            item_id: "9".into(),
            has_lyrics: true,
            path: None,
            artist: None,
            album: None,
            tier: None,
            matched_words: vec![],
            previous_rating: None,
            action: RatingAction::Skipped,
            source: Source::Lyrics,
            server_name: "s".into(),
        },
    ];
    let counts = SummaryCounts::from_results(&results);
    assert_eq!(counts.lyrics_evaluated, 5); // has_lyrics=true: #1,#2,#4,#5,#9
    assert_eq!(counts.r_rated, 2); // tier=R
    assert_eq!(counts.pg13, 1); // tier=PG-13
    assert_eq!(counts.clean, 2); // has_lyrics=true, tier=None: #4 (cleared), #9 (unrated)
    assert_eq!(counts.no_lyrics, 1); // source=Lyrics, has_lyrics=false: #6
    assert_eq!(counts.ratings_set, 1); // action=Set, source=Lyrics
    assert_eq!(counts.already_correct, 1); // action=AlreadyCorrect, source=Lyrics
    assert_eq!(counts.cleared, 1);
    assert_eq!(counts.g_genre_set, 1); // action=Set, source=Genre
    assert_eq!(counts.g_genre_already, 1); // action=AlreadyCorrect, source=Genre
    assert_eq!(counts.dry_run, 1);
    assert_eq!(counts.skipped, 2); // action=Skipped: #6 (no-lyrics), #9 (clean unrated)
    assert_eq!(counts.needs_review, 1); // action=Review (deny_genres veto), not counted as skipped
    assert_eq!(counts.errors, 0);
}

#[test]
fn force_rated_items_excluded_from_lyrics_tier_counts() {
    // A `force R` item carries tier=Some("R") but has_lyrics=false. The tier
    // sub-counts print under "Lyrics evaluated", so they must NOT count it (CR #230).
    let results = vec![ItemResult {
        item_id: "f1".into(),
        has_lyrics: false,
        path: None,
        artist: None,
        album: None,
        tier: Some("R".into()),
        matched_words: vec![],
        previous_rating: None,
        action: RatingAction::Set,
        source: Source::Force,
        server_name: "s".into(),
    }];
    let counts = SummaryCounts::from_results(&results);
    assert_eq!(counts.r_rated, 0); // force-rated, not lyrics-evaluated
    assert_eq!(counts.lyrics_evaluated, 0);
    assert_eq!(counts.ratings_set, 1); // still counted as a rating set
}

#[test]
fn clean_lyrics_rated_g_counts_as_clean_not_explicit() {
    // With clean_rating="G", a clean-lyric track carries tier=Some("G"),
    // source=Lyrics, has_lyrics=true. It must count as Clean + lyrics-evaluated
    // (not R/PG-13), and as a rating set.
    let results = vec![ItemResult {
        item_id: "c1".into(),
        has_lyrics: true,
        path: None,
        artist: None,
        album: None,
        tier: Some("G".into()),
        matched_words: vec![],
        previous_rating: None,
        action: RatingAction::Set,
        source: Source::Lyrics,
        server_name: "s".into(),
    }];
    let c = SummaryCounts::from_results(&results);
    assert_eq!(c.lyrics_evaluated, 1);
    assert_eq!(c.clean, 1); // clean despite tier=G
    assert_eq!(c.r_rated, 0);
    assert_eq!(c.pg13, 0);
    assert_eq!(c.ratings_set, 1); // source=Lyrics Set -> ratings_set (not g_genre_set)
    assert_eq!(c.g_genre_set, 0);
}

/// Integration tests — UAT servers only. Gated behind SMPR_UAT_TEST=1.
/// All tests are READ-ONLY (dry-run). No mutations to UAT data.
#[cfg(test)]
mod integration {
    use crate::config::{Config, DetectionConfig, ServerConfig, ServerType, SourcesConfig};
    use crate::detection::DetectionEngine;
    use crate::rating::*;
    use crate::server::MediaServerClient;
    use std::collections::BTreeMap;

    fn uat_enabled() -> bool {
        std::env::var("SMPR_UAT_TEST").map_or(false, |v| v == "1")
    }

    fn uat_jellyfin_client() -> MediaServerClient {
        dotenvy::from_filename("../.env").ok();
        let api_key = std::env::var("UAT_JELLYFIN_API_KEY")
            .expect("UAT_JELLYFIN_API_KEY must be set for integration tests");
        MediaServerClient::new(
            "http://localhost:8097".into(),
            api_key,
            ServerType::Jellyfin,
        )
    }

    fn dry_run_config() -> Config {
        Config {
            servers: vec![],
            detection: DetectionConfig {
                r_stems: vec!["fuck".into(), "shit".into()],
                r_exact: vec!["blowjob".into()],
                pg13_stems: vec!["bitch".into()],
                pg13_exact: vec!["hoe".into()],
                false_positives: vec!["cocktail".into()],
                g_genres: vec!["Classical".into()],
                deny_genres: vec![],
            },
            overwrite: true,
            clean_rating: Some("G".into()),
            dry_run: true,
            report_path: None,
            library_name: Some("Music".into()),
            location_name: None,
            verbose: false,
            ignore_forced: false,
            no_sources: false,
            overrides: vec![],
            sources: SourcesConfig::default(),
        }
    }

    fn test_server_config() -> ServerConfig {
        ServerConfig {
            name: "uat-jellyfin".into(),
            url: "http://localhost:8097".into(),
            api_key: String::new(),
            server_type: Some(ServerType::Jellyfin),
            libraries: BTreeMap::new(),
        }
    }

    #[test]
    fn uat_rate_dry_run() {
        if !uat_enabled() {
            eprintln!("skipping: SMPR_UAT_TEST not set");
            return;
        }
        let client = uat_jellyfin_client();
        let cfg = dry_run_config();
        let srv = test_server_config();
        let engine = DetectionEngine::new(&cfg.detection);
        let results = rate_workflow(&client, &cfg, &srv, &engine, None).unwrap();
        assert!(!results.is_empty(), "expected at least one item");
        // All should be dry-run (no Set or Cleared)
        for r in &results {
            assert!(
                !matches!(r.action, RatingAction::Set | RatingAction::Cleared),
                "dry-run should not mutate: {:?} on {}",
                r.action,
                r.item_id
            );
        }
    }

    #[test]
    fn uat_force_dry_run() {
        if !uat_enabled() {
            eprintln!("skipping: SMPR_UAT_TEST not set");
            return;
        }
        let client = uat_jellyfin_client();
        let cfg = dry_run_config();
        let srv = test_server_config();
        let results = force_workflow(&client, &cfg, &srv, "G").unwrap();
        assert!(!results.is_empty());
        for r in &results {
            assert!(matches!(
                r.action,
                RatingAction::DryRun | RatingAction::AlreadyCorrect
            ));
            assert_eq!(r.source, Source::Force);
        }
    }

    #[test]
    fn uat_reset_dry_run() {
        if !uat_enabled() {
            eprintln!("skipping: SMPR_UAT_TEST not set");
            return;
        }
        let client = uat_jellyfin_client();
        let cfg = dry_run_config();
        let srv = test_server_config();
        let results = reset_workflow(&client, &cfg, &srv).unwrap();
        assert!(!results.is_empty());
        for r in &results {
            assert!(matches!(
                r.action,
                RatingAction::DryRunClear | RatingAction::Skipped
            ));
            assert_eq!(r.source, Source::Reset);
        }
    }
}
