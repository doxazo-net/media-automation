#![allow(dead_code)]

use crate::config::{RawConfig, RawGeneral};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Servers,
    Genres,
    ForceRatings,
    Detection,
    Preferences,
}

impl Section {
    pub const ALL: [Section; 5] = [
        Section::Servers,
        Section::Genres,
        Section::ForceRatings,
        Section::Detection,
        Section::Preferences,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Self::Servers => "Servers",
            Self::Genres => "G-Rated Genres",
            Self::ForceRatings => "Force Ratings",
            Self::Detection => "Detection Rules",
            Self::Preferences => "Preferences",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Self::Servers => "⊞",
            Self::Genres => "♫",
            Self::ForceRatings => "⚑",
            Self::Detection => "◈",
            Self::Preferences => "⚙",
        }
    }

    pub fn index(&self) -> usize {
        Self::ALL.iter().position(|s| s == self).unwrap()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Sidebar,
    Content,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Editing,
    FullScreen,
    /// Genre filter active — all chars route to filter text input.
    Filtering,
}

#[derive(Debug, Default, Clone)]
pub struct TextInputState {
    pub text: String,
    pub cursor: usize,
}

impl TextInputState {
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn delete_back(&mut self) {
        if self.cursor > 0 {
            let prev = self.text[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.text.drain(prev..self.cursor);
            self.cursor = prev;
        }
    }

    pub fn set(&mut self, s: &str) {
        self.text = s.to_string();
        self.cursor = self.text.len();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerField {
    Url,
    ApiKey,
    ServerType,
}

#[derive(Debug, Default)]
pub struct ServerListState {
    pub selected: usize,
    pub editing_field: Option<ServerField>,
    pub delete_requested: bool,
    pub text_input: TextInputState,
}

#[derive(Debug, Default)]
pub struct GenrePickerState {
    pub available: Vec<String>,
    pub selected: std::collections::HashSet<String>,
    pub cursor: usize,
    pub filter: String,
    pub filter_active: bool,
    pub snapshot: Option<std::collections::HashSet<String>>,
}

#[derive(Debug, Clone)]
pub struct TreeNode {
    pub label: String,
    pub depth: usize,
    pub is_library: bool,
    pub server_label: String,
    pub library_label: Option<String>,
    pub location_label: Option<String>,
    pub force_rating: Option<String>,
}

#[derive(Debug, Default)]
pub struct ForceTreeState {
    pub nodes: Vec<TreeNode>,
    pub cursor: usize,
    pub radio_cursor: usize,
    pub expanded: std::collections::HashSet<usize>,
    pub scroll_offset: usize,
    /// Last rendered pane height (set by render, used by scroll adjustment).
    pub view_height: usize,
}

pub const RATING_OPTIONS: [Option<&str>; 4] = [None, Some("G"), Some("PG-13"), Some("R")];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionCategory {
    RStems,
    RExact,
    Pg13Stems,
    Pg13Exact,
    FalsePositives,
}

impl DetectionCategory {
    pub const ALL: [DetectionCategory; 5] = [
        Self::RStems,
        Self::RExact,
        Self::Pg13Stems,
        Self::Pg13Exact,
        Self::FalsePositives,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Self::RStems => "R-Rated — Stems",
            Self::RExact => "R-Rated — Exact",
            Self::Pg13Stems => "PG-13 — Stems",
            Self::Pg13Exact => "PG-13 — Exact",
            Self::FalsePositives => "False Positives (Ignore)",
        }
    }
}

#[derive(Debug, Default)]
pub struct DetectionState {
    pub selected_category: usize,
    pub editing: bool,
    pub word_cursor: usize,
    pub adding: bool,
    pub text_input: TextInputState,
}

#[derive(Debug)]
pub struct PreferencesState {
    pub overwrite: bool,
}

impl Default for PreferencesState {
    fn default() -> Self {
        Self { overwrite: true }
    }
}

/// Validate a server label: alphanumeric, hyphens, underscores only.
pub fn validate_label(label: &str) -> Result<(), &'static str> {
    if label.is_empty() {
        return Err("Label cannot be empty");
    }
    if !label
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Label must be alphanumeric, hyphens, or underscores only");
    }
    Ok(())
}

/// Validate a server URL: must start with http:// or https://.
pub fn validate_url(url: &str) -> Result<(), &'static str> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("URL must start with http:// or https://");
    }
    Ok(())
}

/// Check for duplicate server labels.
pub fn is_duplicate_label(config: &RawConfig, label: &str) -> bool {
    config
        .servers
        .as_ref()
        .is_some_and(|s| s.contains_key(label))
}

pub struct AppState {
    // Config data (saved to disk)
    pub config: RawConfig,
    pub env_keys: BTreeMap<String, String>,
    pub config_path: PathBuf,
    pub env_path: PathBuf,
    /// Labels of servers that were loaded at startup (for .env deletion tracking).
    pub initial_labels: Vec<String>,

    // UI state
    pub active_pane: Pane,
    pub section: Section,
    pub dirty: bool,
    pub mode: Mode,
    pub read_only: bool,
    pub quit_requested: bool,
    pub error_message: Option<String>,
    pub info_message: Option<String>,

    // Per-section state
    pub server_state: ServerListState,
    pub genre_state: GenrePickerState,
    pub force_state: ForceTreeState,
    pub detection_state: DetectionState,
    pub preferences_state: PreferencesState,
}

impl AppState {
    pub fn new(
        config: RawConfig,
        env_keys: BTreeMap<String, String>,
        config_path: PathBuf,
        env_path: PathBuf,
    ) -> Self {
        let overwrite = config
            .general
            .as_ref()
            .and_then(|g| g.overwrite)
            .unwrap_or(true);

        let initial_labels = env_keys.keys().cloned().collect();

        Self {
            config,
            env_keys,
            config_path,
            env_path,
            initial_labels,
            active_pane: Pane::Sidebar,
            section: Section::Servers,
            dirty: false,
            mode: Mode::Normal,
            read_only: false,
            quit_requested: false,
            error_message: None,
            info_message: None,
            server_state: ServerListState::default(),
            genre_state: GenrePickerState::default(),
            force_state: ForceTreeState::default(),
            detection_state: DetectionState::default(),
            preferences_state: PreferencesState { overwrite },
        }
    }

    pub fn next_section(&mut self) {
        let idx = self.section.index();
        self.section = Section::ALL[(idx + 1) % Section::ALL.len()];
    }

    pub fn prev_section(&mut self) {
        let idx = self.section.index();
        self.section = if idx == 0 {
            Section::ALL[Section::ALL.len() - 1]
        } else {
            Section::ALL[idx - 1]
        };
    }

    pub fn toggle_pane(&mut self) {
        self.active_pane = match self.active_pane {
            Pane::Sidebar => Pane::Content,
            Pane::Content => Pane::Sidebar,
        };
    }

    pub fn section_count(&self, section: Section) -> Option<usize> {
        match section {
            Section::Servers => Some(self.config.servers.as_ref().map_or(0, |s| s.len())),
            Section::Genres => {
                let count = self
                    .config
                    .detection
                    .as_ref()
                    .and_then(|d| d.g_genres.as_ref())
                    .and_then(|g| g.genres.as_ref())
                    .map_or(0, |g| g.len());
                Some(count)
            }
            Section::ForceRatings => {
                let count = self
                    .config
                    .servers
                    .as_ref()
                    .map(|servers| {
                        servers
                            .values()
                            .flat_map(|s| {
                                s.libraries.as_ref().into_iter().flat_map(|libs| {
                                    libs.values().map(|lib| {
                                        let lib_count = usize::from(lib.force_rating.is_some());
                                        let loc_count = lib.locations.as_ref().map_or(0, |locs| {
                                            locs.values()
                                                .filter(|loc| loc.force_rating.is_some())
                                                .count()
                                        });
                                        lib_count + loc_count
                                    })
                                })
                            })
                            .sum()
                    })
                    .unwrap_or(0);
                Some(count)
            }
            Section::Detection | Section::Preferences => None,
        }
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn set_overwrite(&mut self, value: bool) {
        self.preferences_state.overwrite = value;
        let general = self.config.general.get_or_insert(RawGeneral {
            overwrite: None,
            clean_rating: None,
        });
        general.overwrite = Some(value);
        self.mark_dirty();
    }
}
