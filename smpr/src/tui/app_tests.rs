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
        detection: Some(RawDetection::default()),
        general: Some(RawGeneral {
            overwrite: Some(true),
            clean_rating: None,
        }),
        report: None,
        overrides: None,
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
fn new_state_is_not_dirty() {
    let state = sample_state();
    assert!(!state.dirty);
}

#[test]
fn default_section_is_servers() {
    let state = sample_state();
    assert_eq!(state.section, Section::Servers);
}

#[test]
fn default_pane_is_sidebar() {
    let state = sample_state();
    assert_eq!(state.active_pane, Pane::Sidebar);
}

#[test]
fn next_section_wraps() {
    let mut state = sample_state();
    state.section = Section::Preferences;
    state.next_section();
    assert_eq!(state.section, Section::Servers);
}

#[test]
fn prev_section_wraps() {
    let mut state = sample_state();
    state.section = Section::Servers;
    state.prev_section();
    assert_eq!(state.section, Section::Preferences);
}

#[test]
fn toggle_pane() {
    let mut state = sample_state();
    assert_eq!(state.active_pane, Pane::Sidebar);
    state.toggle_pane();
    assert_eq!(state.active_pane, Pane::Content);
    state.toggle_pane();
    assert_eq!(state.active_pane, Pane::Sidebar);
}

#[test]
fn section_count_servers() {
    let state = sample_state();
    assert_eq!(state.section_count(Section::Servers), Some(1));
}

#[test]
fn section_count_no_genres() {
    let state = sample_state();
    assert_eq!(state.section_count(Section::Genres), Some(0));
}
