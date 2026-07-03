// Typed API response structs. All use #[serde(rename_all = "PascalCase")].

use serde::Deserialize;
use serde_json::Value;

/// Response from GET /System/Info/Public (unauthenticated).
/// Both Emby and Jellyfin serve this endpoint but with different fields.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct SystemInfoPublic {
    pub product_name: Option<String>,
    pub server_name: Option<String>,
    pub version: Option<String>,
    pub id: Option<String>,
    pub local_address: Option<String>,
    pub local_addresses: Option<Vec<String>>,
    pub startup_wizard_completed: Option<bool>,
}

/// User info from GET /Users.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserInfo {
    pub id: String,
    // Present in API response; not consumed by current workflows.
    #[allow(dead_code)]
    pub name: Option<String>,
}

/// Music library from GET /Library/VirtualFolders.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct VirtualFolder {
    pub name: String,
    pub item_id: String,
    pub collection_type: Option<String>,
    #[serde(default)]
    pub locations: Vec<String>,
}

/// Single genre from GET /MusicGenres.
// Used by list_genres(), which is defined for future genre-inspection workflows.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct GenreItem {
    pub name: String,
}

/// Response from GET /MusicGenres.
// Used by list_genres(), which is defined for future genre-inspection workflows.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct GenreResponse {
    #[serde(default)]
    pub items: Vec<GenreItem>,
}

/// Read-only view of an audio item — deserialized alongside raw Value.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct AudioItemView {
    pub id: String,
    /// Track title (Emby/Jellyfin `Name`). Needed to query external sources.
    pub name: Option<String>,
    pub path: Option<String>,
    pub official_rating: Option<String>,
    pub album_artist: Option<String>,
    pub album: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    /// Playback length in 100-nanosecond ticks (Emby/Jellyfin `RunTimeTicks`).
    pub run_time_ticks: Option<i64>,
    /// External provider IDs (e.g. `MusicBrainzTrack`), when tagged.
    pub provider_ids: Option<std::collections::HashMap<String, String>>,
}

impl AudioItemView {
    /// Track duration in whole seconds (`RunTimeTicks` are 100-ns units, so
    /// 10,000,000 ticks per second). `None` when the server reported no length.
    pub fn duration_s(&self) -> Option<i64> {
        self.run_time_ticks.map(|ticks| ticks / 10_000_000)
    }

    /// The MusicBrainz recording ID Picard bakes into the file, if present.
    /// This is the stable local match key preferred over the reported path.
    pub fn mbid(&self) -> Option<&str> {
        self.provider_ids
            .as_ref()?
            .get("MusicBrainzTrack")
            .map(String::as_str)
    }
}

/// Paginated response from GET /Users/{uid}/Items.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PrefetchResponse {
    #[serde(default)]
    pub items: Vec<Value>,
    #[serde(default)]
    pub total_record_count: i64,
}

/// Response from GET /Audio/{id}/Lyrics (Jellyfin only).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LyricsResponse {
    #[serde(default)]
    pub lyrics: Vec<LyricLine>,
}

/// Single lyric line in a Jellyfin lyrics response.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LyricLine {
    pub text: Option<String>,
    // Present in API response; not consumed by current workflows.
    #[allow(dead_code)]
    pub start: Option<i64>,
}
