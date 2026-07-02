use super::app::*;
use crate::config::*;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn sample_config() -> RawConfig {
    let mut servers = BTreeMap::new();
    servers.insert(
        "test-server".to_string(),
        RawServerConfig {
            url: Some("http://localhost:8096".to_string()),
            server_type: Some("emby".to_string()),
            libraries: None,
        },
    );
    RawConfig {
        servers: Some(servers),
        detection: Some(RawDetection {
            r: Some(RawWordList {
                stems: Some(vec!["test".to_string()]),
                exact: None,
            }),
            pg13: None,
            ignore: None,
            g_genres: Some(RawGenres {
                genres: Some(vec!["Rock".to_string(), "Jazz".to_string()]),
            }),
            deny_genres: None,
        }),
        general: Some(RawGeneral {
            overwrite: Some(true),
            clean_rating: None,
        }),
        report: None,
    }
}

fn sample_state() -> AppState {
    AppState::new(
        sample_config(),
        BTreeMap::from([("test-server".to_string(), "key123".to_string())]),
        PathBuf::from("/tmp/config.toml"),
        PathBuf::from("/tmp/.env"),
    )
}

#[test]
fn navigate_sections_down_three() {
    let mut state = sample_state();
    assert_eq!(state.section, Section::Servers);
    state.next_section();
    assert_eq!(state.section, Section::Genres);
    state.next_section();
    assert_eq!(state.section, Section::ForceRatings);
}

#[test]
fn toggle_pane_and_back() {
    let mut state = sample_state();
    assert_eq!(state.active_pane, Pane::Sidebar);
    state.toggle_pane();
    assert_eq!(state.active_pane, Pane::Content);
    state.toggle_pane();
    assert_eq!(state.active_pane, Pane::Sidebar);
}

#[test]
fn set_overwrite_marks_dirty() {
    let mut state = sample_state();
    assert!(!state.dirty);
    state.set_overwrite(false);
    assert!(state.dirty);
    assert!(!state.preferences_state.overwrite);
    assert_eq!(
        state.config.general.as_ref().unwrap().overwrite,
        Some(false)
    );
}

#[test]
fn quit_without_changes_sets_flag() {
    let mut state = sample_state();
    state.quit_requested = true;
    // Not dirty, so the main loop would break
    assert!(!state.dirty);
}

#[test]
fn initial_labels_tracks_startup_servers() {
    let state = sample_state();
    assert_eq!(state.initial_labels, vec!["test-server".to_string()]);
}

#[test]
fn section_count_genres_with_config() {
    let state = sample_state();
    assert_eq!(state.section_count(Section::Genres), Some(2));
}

#[test]
fn validate_label_valid() {
    assert!(validate_label("home-emby").is_ok());
    assert!(validate_label("server_1").is_ok());
    assert!(validate_label("test123").is_ok());
}

#[test]
fn validate_label_invalid() {
    assert!(validate_label("").is_err());
    assert!(validate_label("has space").is_err());
    assert!(validate_label("has.dot").is_err());
}

#[test]
fn validate_url_valid() {
    assert!(validate_url("http://localhost:8096").is_ok());
    assert!(validate_url("https://emby.example.com").is_ok());
}

#[test]
fn validate_url_invalid() {
    assert!(validate_url("localhost:8096").is_err());
    assert!(validate_url("ftp://server").is_err());
}

#[test]
fn is_duplicate_label_works() {
    let state = sample_state();
    assert!(is_duplicate_label(&state.config, "test-server"));
    assert!(!is_duplicate_label(&state.config, "other-server"));
}

#[test]
fn text_input_insert_and_delete() {
    let mut input = TextInputState::default();
    input.insert_char('h');
    input.insert_char('i');
    assert_eq!(input.text, "hi");
    assert_eq!(input.cursor, 2);
    input.delete_back();
    assert_eq!(input.text, "h");
    assert_eq!(input.cursor, 1);
}

#[test]
fn text_input_set_moves_cursor() {
    let mut input = TextInputState::default();
    input.set("hello");
    assert_eq!(input.text, "hello");
    assert_eq!(input.cursor, 5);
}
