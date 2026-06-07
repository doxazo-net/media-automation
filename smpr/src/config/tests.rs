use super::*;
use std::io::Write as IoWrite;
use tempfile::NamedTempFile;

#[test]
fn parse_full_toml() {
    let toml = r#"
[servers.home-emby]
url = "http://192.168.1.126:8096"
type = "emby"

[servers.home-emby.libraries.Music]
force_rating = "PG-13"

[servers.home-emby.libraries.Music.locations.classical]
force_rating = "G"

[servers.prod-jellyfin]
url = "http://prod.example.com:8096"
type = "jellyfin"

[servers.prod-jellyfin.libraries."Classical Music"]
force_rating = "G"

[detection.r]
stems = ["fuck", "shit"]
exact = ["blowjob"]

[detection.pg13]
stems = ["bitch"]
exact = ["hoe"]

[detection.ignore]
false_positives = ["cocktail", "hancock"]

[detection.g_genres]
genres = ["Classical", "Soundtrack"]

[general]
overwrite = false

[report]
output_path = "/tmp/report.csv"
"#;

    let raw = parse_toml(toml).expect("should parse full TOML");

    // Servers
    let servers = raw.servers.as_ref().unwrap();
    assert_eq!(servers.len(), 2);

    let emby = &servers["home-emby"];
    assert_eq!(emby.url.as_deref(), Some("http://192.168.1.126:8096"));
    assert_eq!(emby.server_type.as_deref(), Some("emby"));
    let libs = emby.libraries.as_ref().unwrap();
    assert_eq!(libs["Music"].force_rating.as_deref(), Some("PG-13"));
    let locs = libs["Music"].locations.as_ref().unwrap();
    assert_eq!(locs["classical"].force_rating.as_deref(), Some("G"));

    let jf = &servers["prod-jellyfin"];
    assert_eq!(jf.server_type.as_deref(), Some("jellyfin"));
    let jf_libs = jf.libraries.as_ref().unwrap();
    assert_eq!(
        jf_libs["Classical Music"].force_rating.as_deref(),
        Some("G")
    );

    // Detection
    let det = raw.detection.as_ref().unwrap();
    let r = det.r.as_ref().unwrap();
    assert_eq!(r.stems.as_deref().unwrap(), &["fuck", "shit"]);
    assert_eq!(r.exact.as_deref().unwrap(), &["blowjob"]);
    let pg13 = det.pg13.as_ref().unwrap();
    assert_eq!(pg13.stems.as_deref().unwrap(), &["bitch"]);
    assert_eq!(pg13.exact.as_deref().unwrap(), &["hoe"]);
    let ignore = det.ignore.as_ref().unwrap();
    assert_eq!(
        ignore.false_positives.as_deref().unwrap(),
        &["cocktail", "hancock"]
    );
    let genres = det.g_genres.as_ref().unwrap();
    assert_eq!(
        genres.genres.as_deref().unwrap(),
        &["Classical", "Soundtrack"]
    );

    // General
    assert_eq!(raw.general.as_ref().unwrap().overwrite, Some(false));

    // Report
    assert_eq!(
        raw.report.as_ref().unwrap().output_path.as_deref(),
        Some("/tmp/report.csv")
    );
}

#[test]
fn parse_minimal_toml() {
    let toml = r#"
[servers.myserver]
url = "http://localhost:8096"
"#;

    let raw = parse_toml(toml).expect("should parse minimal TOML");
    let servers = raw.servers.as_ref().unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(
        servers["myserver"].url.as_deref(),
        Some("http://localhost:8096")
    );
    assert!(servers["myserver"].server_type.is_none());
    assert!(servers["myserver"].libraries.is_none());
    assert!(raw.detection.is_none());
    assert!(raw.general.is_none());
    assert!(raw.report.is_none());
}

#[test]
fn parse_empty_toml() {
    let raw = parse_toml("").expect("should parse empty TOML");
    assert!(raw.servers.is_none());
    assert!(raw.detection.is_none());
    assert!(raw.general.is_none());
    assert!(raw.report.is_none());
}

#[test]
fn parse_server_with_type_override() {
    let toml = r#"
[servers.jf]
url = "http://jf.local:8096"
type = "jellyfin"
"#;

    let raw = parse_toml(toml).expect("should parse");
    let servers = raw.servers.unwrap();
    assert_eq!(servers["jf"].server_type.as_deref(), Some("jellyfin"));
}

#[test]
fn parse_partial_detection_override() {
    let toml = r#"
[detection.r]
stems = ["custom"]
exact = ["custom_exact"]
"#;

    let raw = parse_toml(toml).expect("should parse partial detection");
    let det = raw.detection.as_ref().unwrap();
    assert!(det.r.is_some());
    assert_eq!(
        det.r.as_ref().unwrap().stems.as_deref().unwrap(),
        &["custom"]
    );
    assert_eq!(
        det.r.as_ref().unwrap().exact.as_deref().unwrap(),
        &["custom_exact"]
    );
    assert!(det.pg13.is_none());
    assert!(det.ignore.is_none());
    assert!(det.g_genres.is_none());
}

#[test]
fn parse_unknown_fields_ignored() {
    let toml = r#"
unknown_top_level = "should be ignored"
another_unknown = 42

[servers.test]
url = "http://localhost:8096"
extra_field = "ignored"

[detection]
unknown_nested = true

[some_unknown_section]
key = "value"
"#;

    let raw = parse_toml(toml).expect("unknown fields should be silently ignored");
    let servers = raw.servers.unwrap();
    assert_eq!(
        servers["test"].url.as_deref(),
        Some("http://localhost:8096")
    );
}

// ── Config::load_from_paths tests ──────────────────────────────────

#[test]
fn load_config_from_toml_and_env() {
    let toml_content = r#"
[servers.home-emby]
url = "http://192.168.1.126:8096"
type = "emby"

[servers.home-emby.libraries.Music]
force_rating = "PG-13"

[detection.r]
stems = ["custom_r"]

[general]
overwrite = false

[report]
output_path = "/tmp/default_report.csv"
"#;

    let mut toml_file = NamedTempFile::new().unwrap();
    toml_file.write_all(toml_content.as_bytes()).unwrap();

    let mut env_file = NamedTempFile::new().unwrap();
    writeln!(env_file, "HOME_EMBY_API_KEY=test-key-123").unwrap();

    let cli = CliInput {
        config_path: Some(toml_file.path().to_path_buf()),
        env_file: Some(env_file.path().to_path_buf()),
        dry_run: true,
        verbose: true,
        library: Some("Music".to_string()),
        ..Default::default()
    };

    let cfg = Config::load_from_paths(&cli).expect("should load config");

    // Server resolved
    assert_eq!(cfg.servers.len(), 1);
    assert_eq!(cfg.servers[0].name, "home-emby");
    assert_eq!(cfg.servers[0].url, "http://192.168.1.126:8096");
    assert_eq!(cfg.servers[0].api_key, "test-key-123");
    assert_eq!(cfg.servers[0].server_type, Some(ServerType::Emby));
    assert_eq!(
        cfg.servers[0].libraries["Music"].force_rating.as_deref(),
        Some("PG-13")
    );

    // Detection: r.stems overridden, rest defaults
    assert_eq!(cfg.detection.r_stems, vec!["custom_r"]);
    assert_eq!(
        cfg.detection.r_exact,
        defaults::R_EXACT
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        cfg.detection.pg13_stems,
        defaults::PG13_STEMS
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        cfg.detection.false_positives,
        defaults::FALSE_POSITIVES
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    );

    // Overwrite from TOML (false)
    assert!(!cfg.overwrite);

    // CLI pass-through
    assert!(cfg.dry_run);
    assert!(cfg.verbose);
    assert_eq!(cfg.library_name.as_deref(), Some("Music"));

    // Report from TOML
    assert_eq!(
        cfg.report_path,
        Some(PathBuf::from("/tmp/default_report.csv"))
    );
}

// ── Error and edge case tests ──────────────────────────────────────

#[test]
fn error_missing_api_key() {
    let toml_content = r#"
[servers.nokey-server]
url = "http://localhost:8096"
"#;
    let mut toml_file = NamedTempFile::new().unwrap();
    toml_file.write_all(toml_content.as_bytes()).unwrap();

    // Ensure the env var does NOT exist
    unsafe {
        std::env::remove_var("NOKEY_SERVER_API_KEY");
    }

    let cli = CliInput {
        config_path: Some(toml_file.path().to_path_buf()),
        ..Default::default()
    };

    let err = Config::load_from_paths(&cli).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("missing API key"),
        "expected missing API key error, got: {msg}"
    );
}

#[test]
fn error_server_no_url() {
    let toml_content = r#"
[servers.bad-server]
type = "emby"
"#;
    let mut toml_file = NamedTempFile::new().unwrap();
    toml_file.write_all(toml_content.as_bytes()).unwrap();

    unsafe {
        std::env::set_var("BAD_SERVER_API_KEY", "dummy");
    }

    let cli = CliInput {
        config_path: Some(toml_file.path().to_path_buf()),
        ..Default::default()
    };

    let err = Config::load_from_paths(&cli).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("no url"),
        "expected 'no url' error, got: {msg}"
    );

    unsafe {
        std::env::remove_var("BAD_SERVER_API_KEY");
    }
}

#[test]
fn error_invalid_server_type() {
    let toml_content = r#"
[servers.plex-server]
url = "http://localhost:32400"
type = "plex"
"#;
    let mut toml_file = NamedTempFile::new().unwrap();
    toml_file.write_all(toml_content.as_bytes()).unwrap();

    unsafe {
        std::env::set_var("PLEX_SERVER_API_KEY", "dummy");
    }

    let cli = CliInput {
        config_path: Some(toml_file.path().to_path_buf()),
        ..Default::default()
    };

    let err = Config::load_from_paths(&cli).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("invalid server type") && msg.contains("plex"),
        "expected invalid server type error, got: {msg}"
    );

    unsafe {
        std::env::remove_var("PLEX_SERVER_API_KEY");
    }
}

#[test]
fn error_unknown_server_filter() {
    let toml_content = r#"
[servers.real-server]
url = "http://localhost:8096"
"#;
    let mut toml_file = NamedTempFile::new().unwrap();
    toml_file.write_all(toml_content.as_bytes()).unwrap();

    unsafe {
        std::env::set_var("REAL_SERVER_API_KEY", "dummy");
    }

    let cli = CliInput {
        config_path: Some(toml_file.path().to_path_buf()),
        server_filter: Some(vec!["nonexistent".to_string()]),
        ..Default::default()
    };

    let err = Config::load_from_paths(&cli).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("unknown server") && msg.contains("nonexistent"),
        "expected unknown server filter error, got: {msg}"
    );
    assert!(
        msg.contains("real-server"),
        "expected available servers to be listed, got: {msg}"
    );

    unsafe {
        std::env::remove_var("REAL_SERVER_API_KEY");
    }
}

#[test]
fn error_no_servers_configured() {
    let toml_content = "";
    let mut toml_file = NamedTempFile::new().unwrap();
    toml_file.write_all(toml_content.as_bytes()).unwrap();

    let cli = CliInput {
        config_path: Some(toml_file.path().to_path_buf()),
        ..Default::default()
    };

    let err = Config::load_from_paths(&cli).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("no servers"),
        "expected 'no servers' error, got: {msg}"
    );
}

#[test]
fn oneoff_server_ignores_toml_servers() {
    let toml_content = r#"
[servers.toml-server]
url = "http://should-be-ignored:8096"

[detection.r]
stems = ["custom_stem"]

[general]
overwrite = false
"#;
    let mut toml_file = NamedTempFile::new().unwrap();
    toml_file.write_all(toml_content.as_bytes()).unwrap();

    let cli = CliInput {
        config_path: Some(toml_file.path().to_path_buf()),
        server_url: Some("http://oneoff:8096".to_string()),
        api_key: Some("oneoff-key".to_string()),
        ..Default::default()
    };

    let cfg = Config::load_from_paths(&cli).expect("should load with one-off server");

    // Only the one-off server, TOML server ignored
    assert_eq!(cfg.servers.len(), 1);
    assert_eq!(cfg.servers[0].name, "cli");
    assert_eq!(cfg.servers[0].url, "http://oneoff:8096");
    assert_eq!(cfg.servers[0].api_key, "oneoff-key");

    // Detection and general still loaded from TOML
    assert_eq!(cfg.detection.r_stems, vec!["custom_stem"]);
    assert!(!cfg.overwrite);
}

#[test]
fn overwrite_precedence_cli_over_toml() {
    let toml_content = r#"
[general]
overwrite = false
"#;
    let mut toml_file = NamedTempFile::new().unwrap();
    toml_file.write_all(toml_content.as_bytes()).unwrap();

    let cli = CliInput {
        config_path: Some(toml_file.path().to_path_buf()),
        server_url: Some("http://localhost:8096".to_string()),
        api_key: Some("key".to_string()),
        overwrite: Some(true), // CLI says true
        ..Default::default()
    };

    let cfg = Config::load_from_paths(&cli).expect("should load");
    assert!(cfg.overwrite, "CLI true should override TOML false");
}

#[test]
fn overwrite_default_when_toml_omits() {
    let toml_content = "";
    let mut toml_file = NamedTempFile::new().unwrap();
    toml_file.write_all(toml_content.as_bytes()).unwrap();

    let cli = CliInput {
        config_path: Some(toml_file.path().to_path_buf()),
        server_url: Some("http://localhost:8096".to_string()),
        api_key: Some("key".to_string()),
        // overwrite: None — no CLI flag
        ..Default::default()
    };

    let cfg = Config::load_from_paths(&cli).expect("should load");
    assert!(
        cfg.overwrite,
        "default should be true when neither CLI nor TOML set"
    );
}

#[test]
fn empty_toml_uses_defaults() {
    // Use an empty TOML to ensure defaults, not platform auto-discovery
    let dir = tempfile::tempdir().unwrap();
    let empty_config = dir.path().join("empty.toml");
    std::fs::write(&empty_config, "").unwrap();
    let cli = CliInput {
        config_path: Some(empty_config),
        server_url: Some("http://localhost:8096".to_string()),
        api_key: Some("key".to_string()),
        ..Default::default()
    };

    let cfg = Config::load_from_paths(&cli).expect("no config path should use defaults");

    // Detection defaults
    assert_eq!(
        cfg.detection.r_stems,
        defaults::R_STEMS
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        cfg.detection.pg13_stems,
        defaults::PG13_STEMS
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    );
    assert!(cfg.overwrite); // default
}

#[test]
fn error_explicit_config_not_found() {
    let cli = CliInput {
        config_path: Some(PathBuf::from("/tmp/nonexistent_smpr_config_12345.toml")),
        server_url: Some("http://localhost:8096".to_string()),
        api_key: Some("key".to_string()),
        ..Default::default()
    };

    let err = Config::load_from_paths(&cli).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("IO error"),
        "expected IO error for missing explicit config, got: {msg}"
    );
}

#[test]
fn error_env_file_not_found() {
    let cli = CliInput {
        config_path: None,
        env_file: Some(PathBuf::from("/tmp/nonexistent_smpr_env_12345.env")),
        server_url: Some("http://localhost:8096".to_string()),
        api_key: Some("key".to_string()),
        ..Default::default()
    };

    let err = Config::load_from_paths(&cli).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("env file error"),
        "expected env file error for missing explicit env file, got: {msg}"
    );
}

#[test]
fn server_filter_keeps_only_matching() {
    let toml_content = r#"
[servers.alpha]
url = "http://alpha:8096"

[servers.beta]
url = "http://beta:8096"

[servers.gamma]
url = "http://gamma:8096"
"#;
    let mut toml_file = NamedTempFile::new().unwrap();
    toml_file.write_all(toml_content.as_bytes()).unwrap();

    unsafe {
        std::env::set_var("ALPHA_API_KEY", "key-a");
        std::env::set_var("BETA_API_KEY", "key-b");
        std::env::set_var("GAMMA_API_KEY", "key-g");
    }

    let cli = CliInput {
        config_path: Some(toml_file.path().to_path_buf()),
        server_filter: Some(vec!["alpha".to_string(), "gamma".to_string()]),
        ..Default::default()
    };

    let cfg = Config::load_from_paths(&cli).expect("should load with filter");
    assert_eq!(cfg.servers.len(), 2);

    let names: Vec<&str> = cfg.servers.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"alpha"), "should contain alpha");
    assert!(names.contains(&"gamma"), "should contain gamma");
    assert!(!names.contains(&"beta"), "should NOT contain beta");

    unsafe {
        std::env::remove_var("ALPHA_API_KEY");
        std::env::remove_var("BETA_API_KEY");
        std::env::remove_var("GAMMA_API_KEY");
    }
}

#[test]
fn error_partial_oneoff_server_url_only() {
    let cli = CliInput {
        server_url: Some("http://localhost:8096".to_string()),
        // api_key intentionally omitted
        ..Default::default()
    };

    let err = Config::load_from_paths(&cli).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("--server-url") && msg.contains("--api-key"),
        "expected incomplete one-off error, got: {msg}"
    );
}

#[test]
fn error_partial_oneoff_api_key_only() {
    let cli = CliInput {
        api_key: Some("key".to_string()),
        // server_url intentionally omitted
        ..Default::default()
    };

    let err = Config::load_from_paths(&cli).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("--server-url") && msg.contains("--api-key"),
        "expected incomplete one-off error, got: {msg}"
    );
}

#[test]
fn server_filter_skips_unneeded_resolution() {
    // alpha has an API key, beta does NOT — but we only request alpha
    let toml_content = r#"
[servers.alpha]
url = "http://alpha:8096"

[servers.beta]
url = "http://beta:8096"
"#;
    let mut toml_file = NamedTempFile::new().unwrap();
    toml_file.write_all(toml_content.as_bytes()).unwrap();

    unsafe {
        std::env::set_var("ALPHA_API_KEY", "key-a");
        std::env::remove_var("BETA_API_KEY");
    }

    let cli = CliInput {
        config_path: Some(toml_file.path().to_path_buf()),
        server_filter: Some(vec!["alpha".to_string()]),
        ..Default::default()
    };

    // Should succeed even though beta has no API key
    let cfg = Config::load_from_paths(&cli).expect("filter should skip beta");
    assert_eq!(cfg.servers.len(), 1);
    assert_eq!(cfg.servers[0].name, "alpha");

    unsafe {
        std::env::remove_var("ALPHA_API_KEY");
    }
}

#[test]
fn config_error_source_toml() {
    let toml_err = toml::from_str::<toml::Value>("{{invalid").unwrap_err();
    let err = super::ConfigError::TomlParse(toml_err);
    assert!(
        std::error::Error::source(&err).is_some(),
        "TomlParse should return source"
    );
}

#[test]
fn config_error_source_io() {
    let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "test");
    let err = super::ConfigError::Io(io_err);
    assert!(
        std::error::Error::source(&err).is_some(),
        "Io should return source"
    );
}

#[test]
fn config_error_source_none_for_others() {
    let err = super::ConfigError::NoServers;
    assert!(
        std::error::Error::source(&err).is_none(),
        "NoServers should return None"
    );
}

#[test]
fn raw_config_serialize_roundtrip() {
    let toml_str = r#"
[servers.test-server]
url = "http://localhost:8096"
type = "emby"

[detection.r]
stems = ["fuck"]
exact = ["blowjob"]

[detection.g_genres]
genres = ["Classical", "Ambient"]

[general]
overwrite = false
"#;
    let parsed: super::RawConfig = toml::from_str(toml_str).unwrap();
    let serialized = toml::to_string_pretty(&parsed).unwrap();
    let reparsed: super::RawConfig = toml::from_str(&serialized).unwrap();

    // Verify key fields survived the round-trip
    let servers = reparsed.servers.unwrap();
    assert!(servers.contains_key("test-server"));
    let srv = &servers["test-server"];
    assert_eq!(srv.url.as_deref(), Some("http://localhost:8096"));
    assert_eq!(srv.server_type.as_deref(), Some("emby"));

    let det = reparsed.detection.unwrap();
    let r = det.r.unwrap();
    assert_eq!(r.stems.unwrap(), vec!["fuck"]);

    let genres = det.g_genres.unwrap();
    assert_eq!(genres.genres.unwrap(), vec!["Classical", "Ambient"]);

    assert_eq!(reparsed.general.unwrap().overwrite, Some(false));
}

#[test]
fn resolve_discovers_cwd_config() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("explicit_config.toml");
    std::fs::write(
        &config_path,
        r#"
[servers.test]
url = "http://localhost:8096"
"#,
    )
    .unwrap();

    let resolved = super::resolve_default_config_path_from(dir.path());
    assert_eq!(resolved, Some(config_path));
}

#[test]
fn resolve_returns_none_when_no_config_found() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = super::resolve_default_config_path_from(dir.path());
    assert!(resolved.is_none());
}

proptest::proptest! {
    // parse_toml must return Ok or Err on any input - never panic.
    #[test]
    fn parse_toml_never_panics(s in ".*") {
        let _ = super::parse_toml(&s);
    }
}
