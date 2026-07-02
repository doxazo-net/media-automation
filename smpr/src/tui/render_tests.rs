use super::app::*;
use super::render;
use crate::config::*;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
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

fn render_to_buffer(state: &AppState, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| render::render(frame, state)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let mut output = String::new();
    for y in 0..height {
        for x in 0..width {
            output.push_str(buf[(x, y)].symbol());
        }
        output.push('\n');
    }
    output
}

#[test]
fn render_80x24_shows_sidebar_and_content() {
    let state = sample_state();
    let output = render_to_buffer(&state, 80, 24);
    assert!(output.contains("Sections"));
    assert!(output.contains("Servers"));
    assert!(output.contains("smpr configure"));
}

#[test]
fn render_120x40_no_panic() {
    let state = sample_state();
    let _output = render_to_buffer(&state, 120, 40);
}

#[test]
fn render_60x20_no_panic() {
    let state = sample_state();
    let _output = render_to_buffer(&state, 60, 20);
}

#[test]
fn render_too_small_shows_message() {
    let state = sample_state();
    let output = render_to_buffer(&state, 30, 10);
    assert!(output.contains("too small"));
}

#[test]
fn render_narrow_hides_sidebar() {
    let state = sample_state();
    let output = render_to_buffer(&state, 50, 20);
    // At 50 cols (< 60), sidebar should be hidden — "Sections" header shouldn't appear
    assert!(!output.contains("Sections"));
}

#[test]
fn render_dirty_shows_modified() {
    let mut state = sample_state();
    state.dirty = true;
    let output = render_to_buffer(&state, 80, 24);
    assert!(output.contains("MODIFIED"));
}

#[test]
fn render_clean_shows_no_changes() {
    let state = sample_state();
    let output = render_to_buffer(&state, 80, 24);
    assert!(output.contains("no changes"));
}
