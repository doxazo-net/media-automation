use super::WizardError;
use super::detection::DetectionAdditions;
use super::library::GenreConfig;
use super::preferences::Preferences;
use super::server::ServerInfo;
use crate::config::{
    RawConfig, RawDetection, RawGeneral, RawGenres, RawIgnore, RawLibraryConfig, RawLocationConfig,
    RawServerConfig, RawWordList, defaults,
};
use std::path::Path;

#[allow(clippy::too_many_arguments)]
pub fn write_config(
    config_path: &Path,
    env_path: &Path,
    existing: Option<&RawConfig>,
    server: &ServerInfo,
    api_key: &str,
    genres: &GenreConfig,
    detection: &DetectionAdditions,
    prefs: &Preferences,
    adding_server: bool,
) -> Result<(), WizardError> {
    // Ensure parent directory exists
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Check for duplicate server label
    if let Some(existing_config) = existing
        && let Some(servers) = &existing_config.servers
        && servers.contains_key(&server.label)
    {
        return Err(WizardError::Prompt(format!(
            "server '{}' already exists in config. Choose a different label.",
            server.label
        )));
    }

    // Build the TOML config
    let config = build_raw_config(existing, server, genres, detection, prefs, adding_server);
    let toml_str = toml::to_string_pretty(&config)?;
    std::fs::write(config_path, toml_str)?;

    // Write/update .env
    write_env(env_path, server, api_key)?;

    Ok(())
}

fn build_raw_config(
    existing: Option<&RawConfig>,
    server: &ServerInfo,
    genres: &GenreConfig,
    detection: &DetectionAdditions,
    prefs: &Preferences,
    adding_server: bool,
) -> RawConfig {
    // Start with existing or empty
    let mut servers = existing.and_then(|e| e.servers.clone()).unwrap_or_default();

    // Add the new server
    let server_type_str = match server.server_type {
        crate::config::ServerType::Emby => "emby",
        crate::config::ServerType::Jellyfin => "jellyfin",
    };
    // Build library entries from discovered libraries
    let libraries = if genres.libraries.is_empty() {
        None
    } else {
        let mut libs = std::collections::BTreeMap::new();
        for lib in &genres.libraries {
            let locations = if lib.locations.is_empty() {
                None
            } else {
                let mut locs = std::collections::BTreeMap::new();
                for loc_path in &lib.locations {
                    let base_name = crate::util::location_leaf(loc_path).to_string();
                    let loc_name = if locs.contains_key(&base_name) {
                        loc_path.clone()
                    } else {
                        base_name
                    };
                    locs.insert(loc_name, RawLocationConfig { force_rating: None });
                }
                Some(locs)
            };
            libs.insert(
                lib.name.clone(),
                RawLibraryConfig {
                    force_rating: None,
                    locations,
                },
            );
        }
        Some(libs)
    };

    servers.insert(
        server.label.clone(),
        RawServerConfig {
            url: Some(server.url.clone()),
            server_type: Some(server_type_str.to_string()),
            libraries,
        },
    );

    // Build detection with defaults + additions
    let r_stems = merge_defaults_and_extras(defaults::R_STEMS, &detection.extra_r_stems);
    let r_exact = merge_defaults_and_extras(defaults::R_EXACT, &detection.extra_r_exact);
    let pg13_stems = merge_defaults_and_extras(defaults::PG13_STEMS, &detection.extra_pg13_stems);
    let pg13_exact = merge_defaults_and_extras(defaults::PG13_EXACT, &detection.extra_pg13_exact);
    let false_positives =
        merge_defaults_and_extras(defaults::FALSE_POSITIVES, &detection.extra_false_positives);

    let detection_section = if adding_server {
        // When adding a server, preserve existing detection config
        existing.and_then(|e| e.detection.clone())
    } else {
        Some(RawDetection {
            r: Some(RawWordList {
                stems: Some(r_stems),
                exact: Some(r_exact),
            }),
            pg13: Some(RawWordList {
                stems: Some(pg13_stems),
                exact: Some(pg13_exact),
            }),
            ignore: Some(RawIgnore {
                false_positives: Some(false_positives),
            }),
            g_genres: if genres.genres.is_empty() {
                None
            } else {
                Some(RawGenres {
                    genres: Some(genres.genres.clone()),
                })
            },
            // The wizard does not prompt for deny_genres yet; omit from output.
            deny_genres: None,
        })
    };

    let general = if adding_server {
        existing.and_then(|e| e.general.clone())
    } else {
        Some(RawGeneral {
            overwrite: Some(prefs.overwrite),
        })
    };

    RawConfig {
        servers: Some(servers),
        detection: detection_section,
        general,
        report: existing.and_then(|e| e.report.clone()),
    }
}

fn merge_defaults_and_extras(defaults: &[&str], extras: &[String]) -> Vec<String> {
    let mut result: Vec<String> = defaults.iter().map(|s| s.to_string()).collect();
    for extra in extras {
        if !result.contains(extra) {
            result.push(extra.clone());
        }
    }
    result
}

fn write_env(env_path: &Path, server: &ServerInfo, api_key: &str) -> Result<(), WizardError> {
    let env_key = format!("{}_API_KEY", server.label.to_uppercase().replace('-', "_"));
    let clean_key = api_key.trim().replace(['\n', '\r'], "");
    let new_line = format!("{env_key}={clean_key}");

    // Read existing .env if it exists
    let existing_content = if env_path.exists() {
        std::fs::read_to_string(env_path)?
    } else {
        String::new()
    };

    // Replace existing key or append
    let mut lines: Vec<String> = existing_content.lines().map(|l| l.to_string()).collect();
    let mut found = false;
    for line in &mut lines {
        if line.starts_with(&format!("{env_key}=")) {
            *line = new_line.clone();
            found = true;
            break;
        }
    }
    if !found {
        lines.push(new_line);
    }

    // Ensure trailing newline
    let mut output = lines.join("\n");
    if !output.ends_with('\n') {
        output.push('\n');
    }

    // Ensure parent directory exists
    if let Some(parent) = env_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(env_path, output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerType;

    #[test]
    fn write_new_config_creates_toml_and_env() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let env_path = dir.path().join(".env");

        let server = ServerInfo {
            url: "http://localhost:8096".to_string(),
            label: "home-emby".to_string(),
            server_type: ServerType::Emby,
        };
        let genres = GenreConfig {
            genres: vec!["Classical".to_string(), "Ambient".to_string()],
            libraries: vec![],
        };
        let detection = DetectionAdditions {
            extra_r_stems: vec!["newword".to_string()],
            extra_r_exact: vec![],
            extra_pg13_stems: vec![],
            extra_pg13_exact: vec![],
            extra_false_positives: vec![],
        };
        let prefs = Preferences { overwrite: true };

        write_config(
            &config_path,
            &env_path,
            None,
            &server,
            "test-api-key",
            &genres,
            &detection,
            &prefs,
            false,
        )
        .unwrap();

        let toml_content = std::fs::read_to_string(&config_path).unwrap();
        assert!(toml_content.contains("[servers.home-emby]"));
        assert!(toml_content.contains("http://localhost:8096"));
        assert!(toml_content.contains("emby"));
        assert!(toml_content.contains("Classical"));
        assert!(toml_content.contains("newword"));

        let env_content = std::fs::read_to_string(&env_path).unwrap();
        assert!(env_content.contains("HOME_EMBY_API_KEY=test-api-key"));
    }

    #[test]
    fn write_config_appends_server_to_existing() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let env_path = dir.path().join(".env");

        // Write initial config
        std::fs::write(
            &config_path,
            r#"
[servers.existing-server]
url = "http://localhost:8096"
type = "emby"

[general]
overwrite = true
"#,
        )
        .unwrap();
        std::fs::write(&env_path, "EXISTING_SERVER_API_KEY=old-key\n").unwrap();

        let existing =
            crate::config::parse_toml(&std::fs::read_to_string(&config_path).unwrap()).unwrap();

        let server = ServerInfo {
            url: "http://localhost:8097".to_string(),
            label: "home-jellyfin".to_string(),
            server_type: ServerType::Jellyfin,
        };
        let genres = GenreConfig {
            genres: vec![],
            libraries: vec![],
        };
        let detection = DetectionAdditions {
            extra_r_stems: vec![],
            extra_r_exact: vec![],
            extra_pg13_stems: vec![],
            extra_pg13_exact: vec![],
            extra_false_positives: vec![],
        };
        let prefs = Preferences { overwrite: true };

        write_config(
            &config_path,
            &env_path,
            Some(&existing),
            &server,
            "new-api-key",
            &genres,
            &detection,
            &prefs,
            true,
        )
        .unwrap();

        let toml_content = std::fs::read_to_string(&config_path).unwrap();
        assert!(toml_content.contains("[servers.existing-server]"));
        assert!(toml_content.contains("[servers.home-jellyfin]"));

        let env_content = std::fs::read_to_string(&env_path).unwrap();
        assert!(env_content.contains("EXISTING_SERVER_API_KEY=old-key"));
        assert!(env_content.contains("HOME_JELLYFIN_API_KEY=new-api-key"));
    }

    #[test]
    fn env_replaces_existing_key() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let env_path = dir.path().join(".env");

        std::fs::write(&env_path, "HOME_EMBY_API_KEY=old-key\nOTHER_KEY=keep\n").unwrap();

        let server = ServerInfo {
            url: "http://localhost:8096".to_string(),
            label: "home-emby".to_string(),
            server_type: ServerType::Emby,
        };
        let genres = GenreConfig {
            genres: vec![],
            libraries: vec![],
        };
        let detection = DetectionAdditions {
            extra_r_stems: vec![],
            extra_r_exact: vec![],
            extra_pg13_stems: vec![],
            extra_pg13_exact: vec![],
            extra_false_positives: vec![],
        };
        let prefs = Preferences { overwrite: true };

        write_config(
            &config_path,
            &env_path,
            None,
            &server,
            "new-key",
            &genres,
            &detection,
            &prefs,
            false,
        )
        .unwrap();

        let env_content = std::fs::read_to_string(&env_path).unwrap();
        assert!(env_content.contains("HOME_EMBY_API_KEY=new-key"));
        assert!(env_content.contains("OTHER_KEY=keep"));
        assert!(!env_content.contains("old-key"));
    }
}
