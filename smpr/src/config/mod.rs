// Resolved config types are foundational — fields will be consumed by
// server, detection, rating, and report modules as they're implemented.
#![allow(dead_code)]

pub mod defaults;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
pub struct RawConfig {
    pub servers: Option<BTreeMap<String, RawServerConfig>>,
    pub detection: Option<RawDetection>,
    pub general: Option<RawGeneral>,
    pub report: Option<RawReport>,
    /// Per-song rating overrides (`[[overrides]]` array-of-tables).
    pub overrides: Option<Vec<RawOverride>>,
}

/// One `[[overrides]]` entry: force or exempt a track whose path matches `match`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RawOverride {
    /// Case-insensitive, separator-normalized substring matched against the
    /// item's reported path.
    #[serde(rename = "match")]
    pub match_key: Option<String>,
    /// Force this rating (e.g. "G", "PG-13", "R"). Ignored when `skip` is true.
    pub rating: Option<String>,
    /// Leave the track's rating untouched (never rate it).
    pub skip: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RawServerConfig {
    pub url: Option<String>,
    #[serde(rename = "type")]
    pub server_type: Option<String>,
    pub libraries: Option<BTreeMap<String, RawLibraryConfig>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RawLibraryConfig {
    pub force_rating: Option<String>,
    pub locations: Option<BTreeMap<String, RawLocationConfig>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RawLocationConfig {
    pub force_rating: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
pub struct RawDetection {
    pub r: Option<RawWordList>,
    pub pg13: Option<RawWordList>,
    pub ignore: Option<RawIgnore>,
    pub g_genres: Option<RawGenres>,
    pub deny_genres: Option<RawGenres>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RawWordList {
    pub stems: Option<Vec<String>>,
    pub exact: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RawIgnore {
    pub false_positives: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RawGenres {
    pub genres: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RawGeneral {
    pub overwrite: Option<bool>,
    /// Rating applied to tracks whose lyrics are clean (no explicit content).
    /// Defaults to "G" so clean tracks stay playable under a parental gate that
    /// blocks unrated items. Set to "" to instead clear the rating (the legacy
    /// behavior) and leave clean tracks unrated.
    pub clean_rating: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RawReport {
    pub output_path: Option<String>,
}

pub fn parse_toml(content: &str) -> Result<RawConfig, toml::de::Error> {
    toml::from_str(content)
}

// ── Resolved config types ──────────────────────────────────────────

#[derive(Debug)]
pub struct Config {
    pub servers: Vec<ServerConfig>,
    pub detection: DetectionConfig,
    pub overwrite: bool,
    /// Rating to apply to clean-lyric tracks. `Some("G")` by default; `None`
    /// (from an empty `clean_rating`) restores the legacy clear-on-clean behavior.
    pub clean_rating: Option<String>,
    pub dry_run: bool,
    pub report_path: Option<PathBuf>,
    pub library_name: Option<String>,
    pub location_name: Option<String>,
    pub verbose: bool,
    pub ignore_forced: bool,
    /// Per-song rating overrides, highest precedence (override > force > lyrics/genre).
    pub overrides: Vec<OverrideRule>,
}

/// A resolved per-song override. `match_key` is pre-normalized (lowercased,
/// forward-slash) so it compares directly against a normalized item path.
#[derive(Debug, Clone, PartialEq)]
pub struct OverrideRule {
    pub match_key: String,
    /// The rating to force. `None` together with `skip == true` means "leave
    /// this track's rating untouched".
    pub rating: Option<String>,
    pub skip: bool,
}

#[derive(Debug)]
pub struct ServerConfig {
    pub name: String,
    pub url: String,
    pub api_key: String,
    pub server_type: Option<ServerType>,
    pub libraries: BTreeMap<String, LibraryConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerType {
    Emby,
    Jellyfin,
}

#[derive(Debug)]
pub struct LibraryConfig {
    pub force_rating: Option<String>,
    pub locations: BTreeMap<String, LocationConfig>,
}

#[derive(Debug)]
pub struct LocationConfig {
    pub force_rating: Option<String>,
}

#[derive(Debug)]
pub struct DetectionConfig {
    pub r_stems: Vec<String>,
    pub r_exact: Vec<String>,
    pub pg13_stems: Vec<String>,
    pub pg13_exact: Vec<String>,
    pub false_positives: Vec<String>,
    pub g_genres: Vec<String>,
    /// Genres that veto the genre-G fallback even when a `g_genres` entry also
    /// matches (e.g. film OSTs tagged both `Soundtrack` and `Classical`).
    pub deny_genres: Vec<String>,
}

// ── Errors ─────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ConfigError {
    /// TOML file exists but cannot be parsed.
    TomlParse(toml::de::Error),
    /// IO error reading the TOML file.
    Io(std::io::Error),
    /// A server declared in TOML has no `url` field.
    ServerMissingUrl(String),
    /// API key env var not found for a named server.
    MissingApiKey(String),
    /// Invalid `type` value (must be "emby" or "jellyfin").
    InvalidServerType { server: String, value: String },
    /// .env file explicitly specified but could not be loaded.
    EnvFile(String),
    /// `--server` filter names a server not present in config.
    UnknownServerFilter {
        requested: String,
        available: Vec<String>,
    },
    /// Only one of --server-url / --api-key provided.
    IncompleteOneOff,
    /// No servers configured (neither TOML nor one-off CLI).
    NoServers,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TomlParse(e) => write!(f, "TOML parse error: {e}"),
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::ServerMissingUrl(name) => {
                write!(f, "server '{name}' has no url")
            }
            Self::MissingApiKey(name) => {
                write!(f, "missing API key env var for server '{name}'")
            }
            Self::InvalidServerType { server, value } => {
                write!(
                    f,
                    "invalid server type '{value}' for server '{server}' (expected 'emby' or 'jellyfin')"
                )
            }
            Self::EnvFile(msg) => write!(f, "env file error: {msg}"),
            Self::UnknownServerFilter {
                requested,
                available,
            } => {
                write!(
                    f,
                    "unknown server '{requested}' in --server filter. Available: {}",
                    available.join(", ")
                )
            }
            Self::IncompleteOneOff => {
                write!(f, "--server-url and --api-key must be provided together")
            }
            Self::NoServers => write!(f, "no servers configured"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TomlParse(e) => Some(e),
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

// ── CLI input ──────────────────────────────────────────────────────

/// Subset of CLI options needed for config loading.
/// Kept separate from clap structs so config module doesn't depend on clap.
#[derive(Debug, Default)]
pub struct CliInput {
    pub config_path: Option<PathBuf>,
    pub env_file: Option<PathBuf>,
    pub server_url: Option<String>,
    pub api_key: Option<String>,
    pub server_filter: Option<Vec<String>>,
    pub overwrite: Option<bool>,
    pub dry_run: bool,
    pub report: Option<String>,
    pub library: Option<String>,
    pub location: Option<String>,
    pub verbose: bool,
    pub ignore_forced: bool,
}

// ── Config path auto-discovery ─────────────────────────────────────

/// Resolve the default config file path when --config is not specified.
///
/// Precedence (highest first; `--config` itself is handled upstream in
/// `load_from_paths`): CWD `explicit_config.toml` > platform
/// `~/.config/smpr/config.toml` > binary-adjacent `config.toml`.
///
/// The binary-adjacent step is a fallback for portable single-folder installs
/// where the platform config dir is ephemeral or absent — e.g. Unraid's
/// RAM-backed `/root` is wiped on every reboot, so a config dropped next to the
/// binary on persistent storage keeps working without an explicit `--config`.
pub fn resolve_default_config_path() -> Option<PathBuf> {
    // 1. Check CWD for explicit_config.toml
    if let Some(path) = std::env::current_dir()
        .ok()
        .and_then(|cwd| resolve_default_config_path_from(&cwd))
    {
        return Some(path);
    }

    // 2. Check platform config dir
    if let Some(dir) = dirs::config_dir() {
        let platform_config = dir.join("smpr").join("config.toml");
        if platform_config.is_file() {
            return Some(platform_config);
        }
    }

    // 3. Check next to the executable (portable-install fallback)
    resolve_binary_adjacent_config_path()
}

/// Locate `config.toml` next to the running executable.
///
/// Impure: queries the process for its own path. Any failure (no exe path,
/// canonicalize error, no parent dir) is treated as "not found" (`None`), never
/// an error. Delegates the file check to the pure [`config_file_in_dir`].
fn resolve_binary_adjacent_config_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let dir = exe.parent()?;
    config_file_in_dir(dir)
}

/// Pure helper: `Some(dir/config.toml)` iff that path is an existing file.
fn config_file_in_dir(dir: &std::path::Path) -> Option<PathBuf> {
    let candidate = dir.join("config.toml");
    candidate.is_file().then_some(candidate)
}

/// Testable version that takes CWD as a parameter.
/// Only checks CWD — does not check the platform config dir.
/// Use `resolve_default_config_path()` for the full resolution chain.
pub fn resolve_default_config_path_from(cwd: &std::path::Path) -> Option<PathBuf> {
    let cwd_config = cwd.join("explicit_config.toml");
    if cwd_config.is_file() {
        return Some(cwd_config);
    }
    None
}

/// Resolve the default .env file path when --env-file is not specified.
/// Checks same directory as resolved config file first, then CWD.
pub fn resolve_default_env_path(config_path: Option<&std::path::Path>) -> Option<PathBuf> {
    // 1. Check same directory as resolved config file
    if let Some(config) = config_path
        && let Some(parent) = config.parent()
    {
        let env_path = parent.join(".env");
        if env_path.is_file() {
            return Some(env_path);
        }
    }

    // 2. Fallback to CWD
    if let Ok(cwd) = std::env::current_dir() {
        let cwd_env = cwd.join(".env");
        if cwd_env.is_file() {
            return Some(cwd_env);
        }
    }

    None
}

// ── Config loading ─────────────────────────────────────────────────

impl Config {
    /// Build a fully resolved `Config` from TOML file, .env file, and CLI flags.
    pub fn load_from_paths(cli: &CliInput) -> Result<Config, ConfigError> {
        // 1. Resolve config path
        let resolved_config_path = cli.config_path.clone().or_else(resolve_default_config_path);

        // 2. Parse TOML
        let raw = match &resolved_config_path {
            Some(path) if cli.config_path.is_some() => {
                // User explicitly specified --config; missing file is an error
                let content = std::fs::read_to_string(path).map_err(ConfigError::Io)?;
                parse_toml(&content).map_err(ConfigError::TomlParse)?
            }
            Some(path) => {
                // Auto-discovered config — best-effort in one-off mode
                log::debug!("Auto-discovered config at {}", path.display());
                let oneoff = cli.server_url.is_some() && cli.api_key.is_some();
                match std::fs::read_to_string(path) {
                    Ok(content) => match parse_toml(&content) {
                        Ok(raw) => raw,
                        Err(e) if oneoff => {
                            log::warn!(
                                "Ignoring malformed auto-discovered config in one-off mode ({}): {e}",
                                path.display()
                            );
                            RawConfig::default()
                        }
                        Err(e) => return Err(ConfigError::TomlParse(e)),
                    },
                    Err(e) if oneoff => {
                        log::warn!(
                            "Ignoring unreadable auto-discovered config in one-off mode ({}): {e}",
                            path.display()
                        );
                        RawConfig::default()
                    }
                    Err(e) => return Err(ConfigError::Io(e)),
                }
            }
            None => RawConfig::default(),
        };

        // 3. Load .env file (explicit path must succeed)
        if let Some(env_path) = &cli.env_file {
            dotenvy::from_path(env_path)
                .map_err(|e| ConfigError::EnvFile(format!("{}: {e}", env_path.display())))?;
        } else if let Some(env_path) = resolve_default_env_path(resolved_config_path.as_deref()) {
            log::debug!("Auto-discovered .env at {}", env_path.display());
            if let Err(e) = dotenvy::from_path(&env_path) {
                log::warn!(
                    "Failed to load auto-discovered .env at {}: {e}",
                    env_path.display()
                );
            }
        }

        // 4. Resolve servers
        let servers = resolve_servers(&raw, cli)?;

        // 5. Resolve detection
        let detection = resolve_detection(&raw);

        // 6. Resolve overwrite: CLI > TOML > default (true)
        let overwrite = cli.overwrite.unwrap_or_else(|| {
            raw.general
                .as_ref()
                .and_then(|g| g.overwrite)
                .unwrap_or(true)
        });

        // 6b. Resolve clean_rating: TOML > default ("G"). An explicitly empty
        // string opts out (None = clear clean tracks, the legacy behavior).
        let clean_rating = match raw.general.as_ref().and_then(|g| g.clean_rating.clone()) {
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(s),
            None => Some("G".to_string()),
        };

        // 7. Resolve report path: CLI > TOML > None
        let report_path = cli.report.as_ref().map(PathBuf::from).or_else(|| {
            raw.report
                .as_ref()
                .and_then(|r| r.output_path.as_ref())
                .map(PathBuf::from)
        });

        // 8. Resolve per-song overrides (TOML only).
        let overrides = resolve_overrides(&raw);

        Ok(Config {
            servers,
            detection,
            overwrite,
            clean_rating,
            dry_run: cli.dry_run,
            report_path,
            library_name: cli.library.clone(),
            location_name: cli.location.clone(),
            verbose: cli.verbose,
            ignore_forced: cli.ignore_forced,
            overrides,
        })
    }
}

fn resolve_servers(raw: &RawConfig, cli: &CliInput) -> Result<Vec<ServerConfig>, ConfigError> {
    // One-off mode: --server-url + --api-key
    match (&cli.server_url, &cli.api_key) {
        (Some(url), Some(key)) => {
            return Ok(vec![ServerConfig {
                name: "cli".to_string(),
                url: url.clone(),
                api_key: key.clone(),
                server_type: None,
                libraries: BTreeMap::new(),
            }]);
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err(ConfigError::IncompleteOneOff);
        }
        (None, None) => {}
    }

    // TOML servers
    let raw_servers = match &raw.servers {
        Some(s) if !s.is_empty() => s,
        _ => return Err(ConfigError::NoServers),
    };

    // Validate --server filter names before resolving secrets
    if let Some(filter) = &cli.server_filter {
        let available: Vec<String> = raw_servers.keys().cloned().collect();
        for name in filter {
            if !raw_servers.contains_key(name) {
                return Err(ConfigError::UnknownServerFilter {
                    requested: name.clone(),
                    available,
                });
            }
        }
    }

    let mut servers = Vec::new();
    for (label, raw_srv) in raw_servers {
        // Skip servers not in the filter
        if let Some(filter) = &cli.server_filter
            && !filter.contains(label)
        {
            continue;
        }

        let url = raw_srv
            .url
            .as_ref()
            .ok_or_else(|| ConfigError::ServerMissingUrl(label.clone()))?;

        // API key: {LABEL_UPPER}_API_KEY (hyphens → underscores)
        let env_key = format!("{}_API_KEY", label.to_uppercase().replace('-', "_"));
        let api_key =
            std::env::var(&env_key).map_err(|_| ConfigError::MissingApiKey(label.clone()))?;

        let server_type = match &raw_srv.server_type {
            Some(t) => Some(parse_server_type(label, t)?),
            None => None,
        };

        let libraries = resolve_libraries(raw_srv);

        servers.push(ServerConfig {
            name: label.clone(),
            url: url.clone(),
            api_key,
            server_type,
            libraries,
        });
    }

    if servers.is_empty() {
        return Err(ConfigError::NoServers);
    }

    Ok(servers)
}

fn parse_server_type(label: &str, value: &str) -> Result<ServerType, ConfigError> {
    match value.to_lowercase().as_str() {
        "emby" => Ok(ServerType::Emby),
        "jellyfin" => Ok(ServerType::Jellyfin),
        _ => Err(ConfigError::InvalidServerType {
            server: label.to_string(),
            value: value.to_string(),
        }),
    }
}

fn resolve_libraries(raw_srv: &RawServerConfig) -> BTreeMap<String, LibraryConfig> {
    let Some(raw_libs) = &raw_srv.libraries else {
        return BTreeMap::new();
    };

    raw_libs
        .iter()
        .map(|(name, raw_lib)| {
            let locations = raw_lib
                .locations
                .as_ref()
                .map(|locs| {
                    locs.iter()
                        .map(|(loc_name, raw_loc)| {
                            (
                                loc_name.clone(),
                                LocationConfig {
                                    force_rating: raw_loc.force_rating.clone(),
                                },
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();

            (
                name.clone(),
                LibraryConfig {
                    force_rating: raw_lib.force_rating.clone(),
                    locations,
                },
            )
        })
        .collect()
}

fn to_owned_vec(defaults: &[&str]) -> Vec<String> {
    defaults.iter().map(|s| s.to_string()).collect()
}

/// Resolve `[[overrides]]` into normalized [`OverrideRule`]s. Match-keys are
/// normalized (lowercased, forward-slash) so they compare directly against a
/// normalized item path. Entries with no `match`, or with neither a `rating`
/// nor `skip = true`, are dropped with a warning (a no-op override is almost
/// always a mistake).
fn resolve_overrides(raw: &RawConfig) -> Vec<OverrideRule> {
    let Some(raw_overrides) = &raw.overrides else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ov in raw_overrides {
        let match_key = ov
            .match_key
            .as_deref()
            .map(|m| crate::util::normalize_path(m.trim()))
            .unwrap_or_default();
        if match_key.is_empty() {
            log::warn!("ignoring [[overrides]] entry with empty/missing `match`");
            continue;
        }
        let skip = ov.skip.unwrap_or(false);
        let rating = ov
            .rating
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(String::from);
        if !skip && rating.is_none() {
            log::warn!(
                "ignoring [[overrides]] entry match='{match_key}': needs a `rating` or `skip = true`"
            );
            continue;
        }
        out.push(OverrideRule {
            match_key,
            rating,
            skip,
        });
    }
    out
}

fn resolve_detection(raw: &RawConfig) -> DetectionConfig {
    let det = raw.detection.as_ref();
    let r = det.and_then(|d| d.r.as_ref());
    let pg13 = det.and_then(|d| d.pg13.as_ref());

    DetectionConfig {
        r_stems: r
            .and_then(|w| w.stems.clone())
            .unwrap_or_else(|| to_owned_vec(defaults::R_STEMS)),
        r_exact: r
            .and_then(|w| w.exact.clone())
            .unwrap_or_else(|| to_owned_vec(defaults::R_EXACT)),
        pg13_stems: pg13
            .and_then(|w| w.stems.clone())
            .unwrap_or_else(|| to_owned_vec(defaults::PG13_STEMS)),
        pg13_exact: pg13
            .and_then(|w| w.exact.clone())
            .unwrap_or_else(|| to_owned_vec(defaults::PG13_EXACT)),
        false_positives: det
            .and_then(|d| d.ignore.as_ref())
            .and_then(|i| i.false_positives.clone())
            .unwrap_or_else(|| to_owned_vec(defaults::FALSE_POSITIVES)),
        g_genres: det
            .and_then(|d| d.g_genres.as_ref())
            .and_then(|g| g.genres.clone())
            .unwrap_or_default(),
        deny_genres: det
            .and_then(|d| d.deny_genres.as_ref())
            .and_then(|g| g.genres.clone())
            .unwrap_or_default(),
    }
}
