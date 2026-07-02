use super::io::*;
use crate::config::*;
use std::collections::BTreeMap;
use std::io::Write;
use tempfile::NamedTempFile;

#[test]
fn save_and_reload_config_roundtrip() {
    let mut servers = BTreeMap::new();
    servers.insert(
        "my-server".to_string(),
        RawServerConfig {
            url: Some("http://localhost:8096".to_string()),
            server_type: Some("emby".to_string()),
            libraries: None,
        },
    );
    let config = RawConfig {
        servers: Some(servers),
        detection: None,
        general: Some(RawGeneral {
            overwrite: Some(true),
            clean_rating: None,
        }),
        report: None,
    };

    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    save_config(&config, &path).unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    let reloaded: RawConfig = toml::from_str(&content).unwrap();
    assert_eq!(reloaded, config);
}

#[test]
fn load_env_keys_matches_labels() {
    let mut tmp = NamedTempFile::new().unwrap();
    writeln!(tmp, "HOME_EMBY_API_KEY=abc123").unwrap();
    writeln!(tmp, "OTHER_VAR=keep").unwrap();
    writeln!(tmp, "JELLYFIN_TEST_API_KEY=xyz789").unwrap();

    let labels = vec!["home-emby".to_string(), "jellyfin-test".to_string()];
    let keys = load_env_keys(tmp.path(), &labels).unwrap();

    assert_eq!(keys.get("home-emby"), Some(&"abc123".to_string()));
    assert_eq!(keys.get("jellyfin-test"), Some(&"xyz789".to_string()));
    assert_eq!(keys.len(), 2);
}

#[test]
fn save_env_preserves_other_lines() {
    let mut tmp = NamedTempFile::new().unwrap();
    writeln!(tmp, "OTHER_VAR=keep").unwrap();
    writeln!(tmp, "HOME_EMBY_API_KEY=old_key").unwrap();
    writeln!(tmp, "# a comment").unwrap();

    let path = tmp.path().to_path_buf();
    let mut env_keys = BTreeMap::new();
    env_keys.insert("home-emby".to_string(), "new_key".to_string());
    let known = vec!["home-emby".to_string()];

    save_env(&env_keys, &known, &path).unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("OTHER_VAR=keep"));
    assert!(content.contains("# a comment"));
    assert!(content.contains("HOME_EMBY_API_KEY=new_key"));
    assert!(!content.contains("old_key"));
}

#[test]
fn save_env_adds_new_key() {
    let mut tmp = NamedTempFile::new().unwrap();
    writeln!(tmp, "OTHER_VAR=keep").unwrap();

    let path = tmp.path().to_path_buf();
    let mut env_keys = BTreeMap::new();
    env_keys.insert("new-server".to_string(), "the_key".to_string());
    let known = vec!["new-server".to_string()];

    save_env(&env_keys, &known, &path).unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("OTHER_VAR=keep"));
    assert!(content.contains("NEW_SERVER_API_KEY=the_key"));
}

#[test]
fn save_env_removes_deleted_server_key() {
    let mut tmp = NamedTempFile::new().unwrap();
    writeln!(tmp, "HOME_EMBY_API_KEY=abc123").unwrap();
    writeln!(tmp, "OTHER_API_KEY=keep").unwrap();

    let path = tmp.path().to_path_buf();
    let env_keys = BTreeMap::new();
    let known = vec!["home-emby".to_string()];

    save_env(&env_keys, &known, &path).unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(!content.contains("HOME_EMBY_API_KEY"));
    assert!(content.contains("OTHER_API_KEY=keep"));
}

#[test]
fn label_to_env_key_conversion() {
    assert_eq!(label_to_env_var("home-emby"), "HOME_EMBY_API_KEY");
    assert_eq!(label_to_env_var("jellyfin_test"), "JELLYFIN_TEST_API_KEY");
}
