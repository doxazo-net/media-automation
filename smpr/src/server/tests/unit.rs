use super::super::error::MediaServerError;

#[test]
fn error_display_http() {
    let err = MediaServerError::Http {
        status: 404,
        body: "Not Found".to_string(),
    };
    assert_eq!(err.to_string(), "HTTP 404: Not Found");
}

#[test]
fn error_display_connection() {
    let err = MediaServerError::Connection("timeout".to_string());
    assert_eq!(err.to_string(), "connection error: timeout");
}

#[test]
fn error_display_parse() {
    let err = MediaServerError::Parse("invalid json".to_string());
    assert_eq!(err.to_string(), "parse error: invalid json");
}

#[test]
fn error_display_protocol() {
    let err = MediaServerError::Protocol("no users".to_string());
    assert_eq!(err.to_string(), "protocol error: no users");
}

use super::super::MediaServerClient;
use crate::config::ServerType;

#[test]
fn auth_header_emby() {
    let client = MediaServerClient::new(
        "http://localhost:8096".to_string(),
        "test-key".to_string(),
        ServerType::Emby,
    );
    let (name, value) = client.auth_header();
    assert_eq!(name, "X-Emby-Token");
    assert_eq!(value, "test-key");
}

#[test]
fn auth_header_jellyfin() {
    let client = MediaServerClient::new(
        "http://localhost:8097".to_string(),
        "test-key".to_string(),
        ServerType::Jellyfin,
    );
    let (name, value) = client.auth_header();
    assert_eq!(name, "X-MediaBrowser-Token");
    assert_eq!(value, "test-key");
}

#[test]
fn base_url_trailing_slash_stripped() {
    let client = MediaServerClient::new(
        "http://localhost:8096/".to_string(),
        "key".to_string(),
        ServerType::Emby,
    );
    assert_eq!(client.base_url(), "http://localhost:8096");
}

use super::super::types::SystemInfoPublic;

#[test]
fn parse_system_info_jellyfin() {
    let json = r#"{
        "LocalAddress": "http://172.22.0.2:8096",
        "ServerName": "jellyfin-test",
        "Version": "10.11.6",
        "ProductName": "Jellyfin Server",
        "OperatingSystem": "",
        "Id": "4b873737cd2b4629bca0db6243058554",
        "StartupWizardCompleted": true
    }"#;
    let info: SystemInfoPublic = serde_json::from_str(json).unwrap();
    assert_eq!(info.product_name.as_deref(), Some("Jellyfin Server"));
    assert_eq!(
        info.local_address.as_deref(),
        Some("http://172.22.0.2:8096")
    );
    assert!(info.local_addresses.is_none());
    assert_eq!(info.startup_wizard_completed, Some(true));
}

#[test]
fn parse_system_info_emby() {
    let json = r#"{
        "LocalAddresses": [],
        "RemoteAddresses": [],
        "ServerName": "OutaTime",
        "Version": "4.10.0.5",
        "Id": "909da4331156415eb771a38a9658aafc"
    }"#;
    let info: SystemInfoPublic = serde_json::from_str(json).unwrap();
    assert!(info.product_name.is_none());
    assert!(info.local_address.is_none());
    assert!(info.local_addresses.is_some());
    assert_eq!(info.server_name.as_deref(), Some("OutaTime"));
}

#[test]
fn parse_system_info_unknown_fields_ignored() {
    let json = r#"{"ServerName": "test", "UnknownField": 42, "AnotherOne": true}"#;
    let info: SystemInfoPublic = serde_json::from_str(json).unwrap();
    assert_eq!(info.server_name.as_deref(), Some("test"));
}

use super::super::detect_from_response;

// Tier 1: ProductName
#[test]
fn detect_jellyfin_by_product_name() {
    let info = SystemInfoPublic {
        product_name: Some("Jellyfin Server".to_string()),
        ..Default::default()
    };
    assert_eq!(detect_from_response(&info, ""), Some(ServerType::Jellyfin));
}

#[test]
fn detect_emby_by_product_name_other() {
    let info = SystemInfoPublic {
        product_name: Some("Emby Server".to_string()),
        ..Default::default()
    };
    assert_eq!(detect_from_response(&info, ""), Some(ServerType::Emby));
}

// Tier 2: Structural shape
#[test]
fn detect_jellyfin_by_local_address_singular() {
    let info = SystemInfoPublic {
        local_address: Some("http://172.22.0.2:8096".to_string()),
        ..Default::default()
    };
    assert_eq!(detect_from_response(&info, ""), Some(ServerType::Jellyfin));
}

#[test]
fn detect_emby_by_local_addresses_plural() {
    let info = SystemInfoPublic {
        local_addresses: Some(vec![]),
        ..Default::default()
    };
    assert_eq!(detect_from_response(&info, ""), Some(ServerType::Emby));
}

#[test]
fn detect_jellyfin_singular_takes_precedence_over_plural() {
    let info = SystemInfoPublic {
        local_address: Some("http://example.com".to_string()),
        local_addresses: Some(vec![]),
        ..Default::default()
    };
    assert_eq!(detect_from_response(&info, ""), Some(ServerType::Jellyfin));
}

// Tier 3: Server header
#[test]
fn detect_jellyfin_by_kestrel_header() {
    let info = SystemInfoPublic::default();
    assert_eq!(
        detect_from_response(&info, "Kestrel"),
        Some(ServerType::Jellyfin)
    );
}

#[test]
fn detect_emby_by_upnp_header() {
    let info = SystemInfoPublic::default();
    assert_eq!(
        detect_from_response(&info, "UPnP/1.0 DLNADOC/1.50"),
        Some(ServerType::Emby)
    );
}

// Tier 4: No signal
#[test]
fn detect_none_when_no_signals() {
    let info = SystemInfoPublic::default();
    assert_eq!(detect_from_response(&info, ""), None);
}

use super::super::types::{
    AudioItemView, GenreResponse, PrefetchResponse, UserInfo, VirtualFolder,
};

#[test]
fn parse_user_info() {
    let json = r#"{"Id": "af181ce70817479a88f588e7adc321c7", "Name": "Librarian"}"#;
    let user: UserInfo = serde_json::from_str(json).unwrap();
    assert_eq!(user.id, "af181ce70817479a88f588e7adc321c7");
    assert_eq!(user.name.as_deref(), Some("Librarian"));
}

#[test]
fn parse_user_info_extra_fields_ignored() {
    let json = r#"{"Id": "abc123", "Name": "Test", "HasPassword": true, "Policy": {}}"#;
    let user: UserInfo = serde_json::from_str(json).unwrap();
    assert_eq!(user.id, "abc123");
}

#[test]
fn parse_virtual_folder_music() {
    let json = r#"{
        "Name": "Music",
        "Locations": ["/classical/", "/music/"],
        "CollectionType": "music",
        "ItemId": "7990",
        "LibraryOptions": {}
    }"#;
    let lib: VirtualFolder = serde_json::from_str(json).unwrap();
    assert_eq!(lib.name, "Music");
    assert_eq!(lib.item_id, "7990");
    assert_eq!(lib.collection_type.as_deref(), Some("music"));
    assert_eq!(lib.locations.len(), 2);
}

#[test]
fn parse_genre_response() {
    let json = r#"{
        "Items": [
            {"Name": "Rock", "Id": "8088", "Type": "MusicGenre"},
            {"Name": "Hip-Hop", "Id": "8081", "Type": "MusicGenre"}
        ],
        "TotalRecordCount": 2
    }"#;
    let resp: GenreResponse = serde_json::from_str(json).unwrap();
    assert_eq!(resp.items.len(), 2);
    assert_eq!(resp.items[0].name, "Rock");
}

#[test]
fn parse_audio_item_view_emby() {
    let json = r#"{
        "Id": "9383",
        "Path": "/music/Watashi Wa/Eager Seas/01. Track.mp3",
        "Genres": ["Alternative Rock", "rock"],
        "Album": "Eager Seas",
        "AlbumArtist": "Watashi Wa",
        "Type": "Audio",
        "MediaType": "Audio"
    }"#;
    let item: AudioItemView = serde_json::from_str(json).unwrap();
    assert_eq!(item.id, "9383");
    assert_eq!(
        item.path.as_deref(),
        Some("/music/Watashi Wa/Eager Seas/01. Track.mp3")
    );
    assert!(item.official_rating.is_none());
    assert_eq!(item.genres.len(), 2);
}

#[test]
fn parse_audio_item_view_jellyfin() {
    let json = r#"{
        "Id": "7526015b24dce6f8fc732af486395856",
        "Path": "/music/Watashi Wa/Eager Seas/01. Track.mp3",
        "OfficialRating": "R",
        "Genres": ["Rock"],
        "Album": "Eager Seas",
        "AlbumArtist": "Watashi Wa",
        "HasLyrics": false,
        "ChannelId": null
    }"#;
    let item: AudioItemView = serde_json::from_str(json).unwrap();
    assert_eq!(item.id, "7526015b24dce6f8fc732af486395856");
    assert_eq!(item.official_rating.as_deref(), Some("R"));
}

#[test]
fn parse_prefetch_response() {
    let json = r#"{
        "Items": [
            {"Id": "1", "Name": "Track 1"},
            {"Id": "2", "Name": "Track 2"}
        ],
        "TotalRecordCount": 100
    }"#;
    let resp: PrefetchResponse = serde_json::from_str(json).unwrap();
    assert_eq!(resp.items.len(), 2);
    assert_eq!(resp.total_record_count, 100);
}

use super::super::extract_audio_items;

#[test]
fn extract_items_from_value_array() {
    let items: Vec<serde_json::Value> = serde_json::from_str(
        r#"[
        {"Id": "1", "Path": "/music/a.mp3", "Genres": []},
        {"Id": "2", "Path": "/music/b.mp3", "Genres": ["Rock"]}
    ]"#,
    )
    .unwrap();
    let pairs = extract_audio_items(items);
    assert_eq!(pairs.len(), 2);
    assert_eq!(pairs[0].0.id, "1");
    assert_eq!(pairs[1].0.genres, vec!["Rock"]);
}

#[test]
fn extract_items_skips_unparseable() {
    let items = vec![
        serde_json::json!({"Id": "1", "Path": "/a.mp3", "Genres": []}),
        serde_json::json!({"NotAnItem": true}),
        serde_json::json!({"Id": "3", "Path": "/c.mp3", "Genres": []}),
    ];
    let pairs = extract_audio_items(items);
    assert_eq!(pairs.len(), 2);
    assert_eq!(pairs[0].0.id, "1");
    assert_eq!(pairs[1].0.id, "3");
}

use super::super::types::LyricsResponse;

#[test]
fn parse_jellyfin_lyrics_response() {
    let json = r#"{
        "Lyrics": [
            {"Text": "Hello world", "Start": 0},
            {"Text": "Second line", "Start": 5000000}
        ]
    }"#;
    let resp: LyricsResponse = serde_json::from_str(json).unwrap();
    assert_eq!(resp.lyrics.len(), 2);
    assert_eq!(resp.lyrics[0].text.as_deref(), Some("Hello world"));
}

#[test]
fn parse_jellyfin_lyrics_empty() {
    let json = r#"{"Lyrics": []}"#;
    let resp: LyricsResponse = serde_json::from_str(json).unwrap();
    assert!(resp.lyrics.is_empty());
}

use super::super::find_emby_lyrics_stream;

#[test]
fn emby_find_external_lrc_stream() {
    let raw = serde_json::json!({
        "Id": "8177",
        "MediaSources": [{
            "Id": "mediasource_8177",
            "MediaStreams": [
                {"Codec": "mp3", "Type": "Audio", "Index": 0, "IsExternal": false},
                {"Codec": "lrc", "Type": "Subtitle", "Index": 1, "IsExternal": true,
                 "Path": "/music/test.lrc"}
            ]
        }]
    });
    let result = find_emby_lyrics_stream(&raw);
    assert!(result.is_some());
    let (media_source_id, stream_index) = result.unwrap();
    assert_eq!(media_source_id, "mediasource_8177");
    assert_eq!(stream_index, 1);
}

#[test]
fn emby_no_subtitle_streams() {
    let raw = serde_json::json!({
        "Id": "1234",
        "MediaSources": [{
            "Id": "ms_1234",
            "MediaStreams": [
                {"Codec": "mp3", "Type": "Audio", "Index": 0, "IsExternal": false}
            ]
        }]
    });
    assert!(find_emby_lyrics_stream(&raw).is_none());
}

#[test]
fn emby_internal_subtitle_not_matched() {
    let raw = serde_json::json!({
        "Id": "1234",
        "MediaSources": [{
            "Id": "ms_1234",
            "MediaStreams": [
                {"Codec": "lrc", "Type": "Subtitle", "Index": 1, "IsExternal": false,
                 "Extradata": "embedded lyrics text"}
            ]
        }]
    });
    assert!(find_emby_lyrics_stream(&raw).is_none());
}

#[test]
fn emby_non_lrc_external_subtitle_skipped() {
    let raw = serde_json::json!({
        "Id": "1234",
        "MediaSources": [{
            "Id": "ms_1234",
            "MediaStreams": [
                {"Codec": "srt", "Type": "Subtitle", "Index": 1, "IsExternal": true}
            ]
        }]
    });
    assert!(find_emby_lyrics_stream(&raw).is_none());
}

#[test]
fn emby_find_external_txt_stream() {
    // Unsynced lyrics are served as a `txt`-codec external subtitle stream.
    let raw = serde_json::json!({
        "Id": "9001",
        "MediaSources": [{
            "Id": "ms_9001",
            "MediaStreams": [
                {"Codec": "mp3", "Type": "Audio", "Index": 0, "IsExternal": false},
                {"Codec": "txt", "Type": "Subtitle", "Index": 2, "IsExternal": true,
                 "Path": "/music/test.txt"}
            ]
        }]
    });
    let (media_source_id, stream_index) = find_emby_lyrics_stream(&raw).unwrap();
    assert_eq!(media_source_id, "ms_9001");
    assert_eq!(stream_index, 2);
}

#[test]
fn emby_find_external_text_codec_stream() {
    let raw = serde_json::json!({
        "Id": "9002",
        "MediaSources": [{
            "Id": "ms_9002",
            "MediaStreams": [
                {"Codec": "text", "Type": "Subtitle", "Index": 1, "IsExternal": true}
            ]
        }]
    });
    let (media_source_id, stream_index) = find_emby_lyrics_stream(&raw).unwrap();
    assert_eq!(media_source_id, "ms_9002");
    assert_eq!(stream_index, 1);
}

#[test]
fn emby_prefers_lrc_over_txt() {
    // Both sidecars present (txt listed first): the synced lrc must win.
    let raw = serde_json::json!({
        "Id": "9003",
        "MediaSources": [{
            "Id": "ms_9003",
            "MediaStreams": [
                {"Codec": "txt", "Type": "Subtitle", "Index": 1, "IsExternal": true},
                {"Codec": "lrc", "Type": "Subtitle", "Index": 2, "IsExternal": true}
            ]
        }]
    });
    let (_, stream_index) = find_emby_lyrics_stream(&raw).unwrap();
    assert_eq!(
        stream_index, 2,
        "expected the lrc stream (index 2) to be preferred"
    );
}

#[test]
fn emby_candidate_missing_index_does_not_abort_search() {
    // A malformed lrc candidate lacking Index must not prevent finding a valid
    // later txt candidate.
    let raw = serde_json::json!({
        "Id": "9004",
        "MediaSources": [{
            "Id": "ms_9004",
            "MediaStreams": [
                {"Codec": "lrc", "Type": "Subtitle", "IsExternal": true},
                {"Codec": "txt", "Type": "Subtitle", "Index": 3, "IsExternal": true}
            ]
        }]
    });
    let (media_source_id, stream_index) = find_emby_lyrics_stream(&raw).unwrap();
    assert_eq!(media_source_id, "ms_9004");
    assert_eq!(stream_index, 3);
}

#[test]
fn audio_item_duration_and_mbid_derivation() {
    use super::super::types::AudioItemView;
    let view: AudioItemView = serde_json::from_value(serde_json::json!({
        "Id": "1",
        "Name": "Some Title",
        "RunTimeTicks": 2_150_000_000i64, // 215s
        "ProviderIds": { "MusicBrainzTrack": "mb-abc", "MusicBrainzAlbum": "mb-xyz" }
    }))
    .unwrap();
    assert_eq!(view.name.as_deref(), Some("Some Title"));
    assert_eq!(view.duration_s(), Some(215));
    assert_eq!(view.mbid(), Some("mb-abc"));
}

#[test]
fn audio_item_missing_ticks_and_ids_are_none() {
    use super::super::types::AudioItemView;
    let view: AudioItemView = serde_json::from_value(serde_json::json!({ "Id": "1" })).unwrap();
    assert_eq!(view.duration_s(), None);
    assert_eq!(view.mbid(), None);
}

#[test]
fn prefetch_page_size_unbounded_is_full_page() {
    use super::super::prefetch_page_size;
    assert_eq!(prefetch_page_size(None), 500);
}

#[test]
fn prefetch_page_size_small_cap_shrinks_page() {
    use super::super::prefetch_page_size;
    // A small bound must shrink the page so it is a single tiny request.
    assert_eq!(prefetch_page_size(Some(5)), 5);
    assert_eq!(prefetch_page_size(Some(1)), 1);
}

#[test]
fn prefetch_page_size_clamps_into_1_500() {
    use super::super::prefetch_page_size;
    // Zero clamps up to 1 (never a zero/negative server Limit)...
    assert_eq!(prefetch_page_size(Some(0)), 1);
    // ...and any cap above the page size clamps down to 500.
    assert_eq!(prefetch_page_size(Some(501)), 500);
    assert_eq!(prefetch_page_size(Some(10_000)), 500);
    // A huge cap clamps DOWN to the max page size (500) - clamping in usize
    // first avoids the i64-cast wrap that would otherwise force 1-item pages.
    assert_eq!(prefetch_page_size(Some(usize::MAX)), 500);
}
