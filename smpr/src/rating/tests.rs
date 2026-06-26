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
        path: Some(path.to_string()),
        official_rating: None,
        album_artist: None,
        album: None,
        genres: vec![],
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

// ── lookup_force_rating tests ───────────────────────────────────────

use crate::config::{LibraryConfig, LocationConfig, ServerConfig};
use std::collections::BTreeMap;

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

#[test]
fn force_rating_location_overrides_library() {
    let cfg = server_with_force_ratings();
    let rating = scope::lookup_force_rating(&cfg, Some("Music"), Some("classical"));
    assert_eq!(rating, Some("G"));
}

#[test]
fn force_rating_library_fallback() {
    let cfg = server_with_force_ratings();
    let rating = scope::lookup_force_rating(&cfg, Some("Music"), Some("rock"));
    assert_eq!(rating, Some("PG-13"));
}

#[test]
fn force_rating_library_only() {
    let cfg = server_with_force_ratings();
    let rating = scope::lookup_force_rating(&cfg, Some("Music"), None);
    assert_eq!(rating, Some("PG-13"));
}

#[test]
fn force_rating_no_library_match() {
    let cfg = server_with_force_ratings();
    let rating = scope::lookup_force_rating(&cfg, Some("Audiobooks"), None);
    assert_eq!(rating, None);
}

#[test]
fn force_rating_no_library_name() {
    let cfg = server_with_force_ratings();
    let rating = scope::lookup_force_rating(&cfg, None, None);
    assert_eq!(rating, None);
}

#[test]
fn force_rating_case_insensitive() {
    let cfg = server_with_force_ratings();
    let rating = scope::lookup_force_rating(&cfg, Some("music"), Some("Classical"));
    assert_eq!(rating, Some("G"));
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
            path: Some("/music/artist/album/track.flac".into()),
            artist: Some("Artist".into()),
            album: Some("Album".into()),
            tier: Some("R".into()),
            matched_words: vec!["word1".into(), "word2".into()],
            previous_rating: Some("G".into()),
            action: RatingAction::Set,
            source: Source::Lyrics,
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
        "artist,album,track,tier,matched_words,previous_rating,action,source,server"
    );
    assert!(lines[1].contains("Artist"));
    assert!(lines[1].contains("Album"));
    assert!(lines[1].contains("track.flac"));
    assert!(lines[1].contains("R"));
    assert!(lines[1].contains("word1; word2"));
    assert!(lines[1].contains("set"));
    assert!(lines[1].contains("lyrics"));
    assert!(lines[1].contains("home-emby"));
    assert!(lines[2].contains("clean.flac"));
    assert!(lines[2].contains("skipped"));
}

#[test]
fn summary_counts_actions() {
    let results = vec![
        ItemResult {
            item_id: "1".into(),
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
    ];
    let counts = SummaryCounts::from_results(&results);
    assert_eq!(counts.lyrics_evaluated, 4); // source=Lyrics, excluding no-lyrics skip (#6)
    assert_eq!(counts.r_rated, 2); // tier=R
    assert_eq!(counts.pg13, 1); // tier=PG-13
    assert_eq!(counts.clean, 1); // source=Lyrics, tier=None, not a no-lyrics skip (#4)
    assert_eq!(counts.ratings_set, 1); // action=Set, source=Lyrics
    assert_eq!(counts.already_correct, 1); // action=AlreadyCorrect, source=Lyrics
    assert_eq!(counts.cleared, 1);
    assert_eq!(counts.g_genre_set, 1); // action=Set, source=Genre
    assert_eq!(counts.g_genre_already, 1); // action=AlreadyCorrect, source=Genre
    assert_eq!(counts.dry_run, 1);
    assert_eq!(counts.skipped, 1);
    assert_eq!(counts.needs_review, 1); // action=Review (deny_genres veto), not counted as skipped
    assert_eq!(counts.errors, 0);
}

/// Integration tests — UAT servers only. Gated behind SMPR_UAT_TEST=1.
/// All tests are READ-ONLY (dry-run). No mutations to UAT data.
#[cfg(test)]
mod integration {
    use crate::config::{Config, DetectionConfig, ServerConfig, ServerType};
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
            dry_run: true,
            report_path: None,
            library_name: Some("Music".into()),
            location_name: None,
            verbose: false,
            ignore_forced: false,
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
        let results = rate_workflow(&client, &cfg, &srv, &engine).unwrap();
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
