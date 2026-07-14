//! Integration tests — UAT servers only.
//! Gated behind SMPR_UAT_TEST=1 env var.
//! Servers: localhost:8096 (Emby), localhost:8097 (Jellyfin).

use super::super::detect_server_type;
use crate::config::ServerType;

fn uat_enabled() -> bool {
    std::env::var("SMPR_UAT_TEST").is_ok_and(|v| v == "1")
}

#[test]
fn detect_emby_uat() {
    if !uat_enabled() {
        eprintln!("skipping: SMPR_UAT_TEST not set");
        return;
    }
    let result = detect_server_type("http://localhost:8096");
    assert!(result.is_ok(), "detection failed: {:?}", result.err());
    assert_eq!(result.unwrap(), ServerType::Emby);
}

#[test]
fn detect_jellyfin_uat() {
    if !uat_enabled() {
        eprintln!("skipping: SMPR_UAT_TEST not set");
        return;
    }
    let result = detect_server_type("http://localhost:8097");
    assert!(result.is_ok(), "detection failed: {:?}", result.err());
    assert_eq!(result.unwrap(), ServerType::Jellyfin);
}

#[test]
fn detect_unreachable_returns_error() {
    if !uat_enabled() {
        eprintln!("skipping: SMPR_UAT_TEST not set");
        return;
    }
    let result = detect_server_type("http://localhost:19999");
    assert!(result.is_err());
}

use super::super::MediaServerClient;

fn emby_client() -> Option<MediaServerClient> {
    if !uat_enabled() {
        return None;
    }
    dotenvy::from_path(std::path::Path::new("../.env")).ok();
    let key = std::env::var("EMBY_API_KEY").ok()?;
    Some(MediaServerClient::new(
        "http://localhost:8096".to_string(),
        key,
        ServerType::Emby,
    ))
}

fn jellyfin_client() -> Option<MediaServerClient> {
    if !uat_enabled() {
        return None;
    }
    dotenvy::from_path(std::path::Path::new("../.env")).ok();
    let key = std::env::var("UAT_JELLYFIN_API_KEY").ok()?;
    Some(MediaServerClient::new(
        "http://localhost:8097".to_string(),
        key,
        ServerType::Jellyfin,
    ))
}

#[test]
fn emby_get_user_id() {
    let Some(client) = emby_client() else { return };
    let uid = client.get_user_id().unwrap();
    assert!(!uid.is_empty());
}

#[test]
fn jellyfin_get_user_id() {
    let Some(client) = jellyfin_client() else {
        return;
    };
    let uid = client.get_user_id().unwrap();
    assert!(!uid.is_empty());
}

#[test]
fn emby_prefetch_audio_items() {
    let Some(client) = emby_client() else { return };
    let items = client.prefetch_audio_items(false, None).unwrap();
    assert!(!items.is_empty(), "expected at least one audio item");
    let (view, raw) = &items[0];
    assert!(!view.id.is_empty());
    assert!(raw.get("Id").is_some());
}

#[test]
fn jellyfin_prefetch_audio_items() {
    let Some(client) = jellyfin_client() else {
        return;
    };
    let items = client.prefetch_audio_items(false, None).unwrap();
    assert!(!items.is_empty());
}

#[test]
fn emby_prefetch_audio_items_limited_respects_cap() {
    let Some(client) = emby_client() else { return };
    // Compare two small caps directly rather than doing a full unbounded crawl
    // (which is slow on a large library and just to learn its size). Each cap is
    // a single tiny request. A cap returns at most `cap` items, and exactly `cap`
    // when the library is larger; comparing 1 vs 2 proves the bound is applied.
    let one = client
        .prefetch_audio_items_limited(false, None, Some(1))
        .unwrap();
    let two = client
        .prefetch_audio_items_limited(false, None, Some(2))
        .unwrap();
    assert!(one.len() <= 1, "cap of 1 must return at most 1 item");
    assert!(two.len() <= 2, "cap of 2 must return at most 2 items");
    if two.len() < 2 {
        eprintln!("library too small (<2 items) to fully exercise the cap; skipping");
        return;
    }
    // Library has >=2 items: each cap returns exactly its bound.
    assert_eq!(one.len(), 1, "cap of 1 must return exactly 1 item");
    assert_eq!(two.len(), 2, "cap of 2 must return exactly 2 items");
}

#[test]
fn emby_discover_libraries() {
    let Some(client) = emby_client() else { return };
    let libs = client.discover_libraries().unwrap();
    assert!(!libs.is_empty(), "expected at least one music library");
    assert_eq!(libs[0].collection_type.as_deref(), Some("music"));
}

#[test]
fn jellyfin_discover_libraries() {
    let Some(client) = jellyfin_client() else {
        return;
    };
    let libs = client.discover_libraries().unwrap();
    assert!(!libs.is_empty());
}

#[test]
fn emby_list_genres() {
    let Some(client) = emby_client() else { return };
    let genres = client.list_genres().unwrap();
    assert!(!genres.is_empty(), "expected at least one genre");
    // Verify sorted (case-insensitive)
    let sorted: Vec<String> = {
        let mut g = genres.clone();
        g.sort_by_key(|a| a.to_lowercase());
        g
    };
    assert_eq!(genres, sorted);
}

#[test]
fn emby_get_item_read_only() {
    let Some(client) = emby_client() else { return };
    let items = client.prefetch_audio_items(false, None).unwrap();
    let (view, _) = &items[0];
    let full_item = client.get_item(&view.id).unwrap();
    assert!(full_item.get("Id").is_some());
    assert!(full_item.get("Name").is_some());
    // Verify round-trip body has more keys than the view
    let keys = full_item.as_object().unwrap().len();
    assert!(keys > 10, "expected full item body, got {keys} keys");
}

#[test]
fn emby_fetch_lyrics_known_lrc() {
    let Some(client) = emby_client() else { return };
    // Item 8177 has a known LRC sidecar
    let items = client.prefetch_audio_items(true, None).unwrap();
    let target = items.iter().find(|(v, _)| v.id == "8177");
    if let Some((view, raw)) = target {
        let lyrics = client.fetch_lyrics(view, raw).unwrap();
        assert!(lyrics.is_some(), "expected lyrics for item 8177");
        let text = lyrics.unwrap();
        assert!(!text.trim().is_empty(), "expected non-empty lyrics text");
    } else {
        eprintln!("item 8177 not found in prefetch; skipping lyrics test");
    }
}

#[test]
fn jellyfin_fetch_lyrics_graceful_none() {
    let Some(client) = jellyfin_client() else {
        return;
    };
    let items = client.prefetch_audio_items(false, None).unwrap();
    if let Some((view, raw)) = items.first() {
        // Most items won't have lyrics — verify graceful None
        let result = client.fetch_lyrics(view, raw);
        assert!(
            result.is_ok(),
            "lyrics fetch should not error: {:?}",
            result.err()
        );
    }
}
