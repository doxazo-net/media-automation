pub mod error;
pub mod types;

#[cfg(test)]
mod tests;

pub use error::MediaServerError;
pub use types::SystemInfoPublic;

use crate::config::ServerType;
use crate::util::strip_lrc_tags;
use serde::Deserialize;
use serde_json::Value;
use std::cell::OnceCell;
use std::time::Duration;

/// Resolve the server-side page `Limit` for a (possibly bounded) prefetch.
///
/// Unbounded (`None`) uses the full 500-item page. A bound shrinks the page to
/// the cap so a small `--limit` is a single tiny request. The result is always
/// in `[1, 500]`: the cap is clamped as a `usize` FIRST so a huge value (e.g.
/// `usize::MAX`) clamps down to 500 rather than wrapping negative on the `i64`
/// cast, and `Some(0)` clamps up to 1 so the server never receives a
/// zero/negative `Limit`. (`Some(0)` still returns an empty set overall - the
/// caller's `truncate(0)` discards the single fetched item.)
pub(crate) fn prefetch_page_size(max_items: Option<usize>) -> i64 {
    match max_items {
        // Clamp in usize (no wrap), then cast the guaranteed-small result.
        Some(m) => m.clamp(1, 500) as i64,
        None => 500,
    }
}

/// HTTP client for Emby/Jellyfin media server APIs.
pub struct MediaServerClient {
    base_url: String,
    api_key: String,
    server_type: ServerType,
    agent: ureq::Agent,
    user_id: OnceCell<String>,
}

impl MediaServerClient {
    /// Create a new client. `server_type` must be resolved before construction
    /// (via `detect_server_type` or TOML override).
    pub fn new(base_url: String, api_key: String, server_type: ServerType) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_per_call(Some(Duration::from_secs(15)))
            .http_status_as_error(false)
            .build()
            .new_agent();
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            server_type,
            agent,
            user_id: OnceCell::new(),
        }
    }

    /// Returns the base URL (trailing slash stripped).
    #[allow(dead_code)]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Returns the (header_name, header_value) pair for authentication.
    pub fn auth_header(&self) -> (&str, &str) {
        match self.server_type {
            ServerType::Emby => ("X-Emby-Token", &self.api_key),
            ServerType::Jellyfin => ("X-MediaBrowser-Token", &self.api_key),
        }
    }

    /// Returns the server type.
    pub fn server_type(&self) -> &ServerType {
        &self.server_type
    }

    /// Authenticated JSON request. Returns `Ok(None)` when response body is empty.
    pub fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Option<Value>, MediaServerError> {
        let url = format!("{}{}", self.base_url, path);
        let (auth_name, auth_value) = self.auth_header();

        let response = match method {
            "GET" => self
                .agent
                .get(&url)
                .header(auth_name, auth_value)
                .header("Accept", "application/json")
                .call()?,
            "POST" => {
                let req = self
                    .agent
                    .post(&url)
                    .header(auth_name, auth_value)
                    .header("Accept", "application/json");
                match body {
                    Some(b) => req.send_json(b)?,
                    None => req
                        .header("Content-Type", "application/json")
                        .send_empty()?,
                }
            }
            _ => {
                return Err(MediaServerError::Protocol(format!(
                    "unsupported method: {method}"
                )));
            }
        };

        let status = response.status().as_u16();
        if status >= 400 {
            let body_snippet = response.into_body().read_to_string().unwrap_or_default();
            let snippet = if body_snippet.len() > 1024 {
                format!(
                    "{}...",
                    &body_snippet[..body_snippet.floor_char_boundary(1024)]
                )
            } else {
                body_snippet
            };
            return Err(MediaServerError::Http {
                status,
                body: snippet,
            });
        }

        // Read body — empty body returns None
        let body_str = response.into_body().read_to_string().map_err(|e| {
            MediaServerError::Connection(format!("failed to read response body: {e}"))
        })?;
        if body_str.trim().is_empty() {
            return Ok(None);
        }
        let value: Value = serde_json::from_str(&body_str).map_err(|e| {
            MediaServerError::Parse(format!("non-JSON response on {method} {path}: {e}"))
        })?;
        Ok(Some(value))
    }

    /// Authenticated plain-text request. Returns raw response body.
    pub fn request_text(&self, method: &str, path: &str) -> Result<String, MediaServerError> {
        let url = format!("{}{}", self.base_url, path);
        let (auth_name, auth_value) = self.auth_header();

        let response = match method {
            "GET" => self.agent.get(&url).header(auth_name, auth_value).call()?,
            _ => {
                return Err(MediaServerError::Protocol(format!(
                    "unsupported method for request_text: {method}"
                )));
            }
        };

        let status = response.status().as_u16();
        if status >= 400 {
            let body_snippet = response.into_body().read_to_string().unwrap_or_default();
            let snippet = if body_snippet.len() > 1024 {
                format!(
                    "{}...",
                    &body_snippet[..body_snippet.floor_char_boundary(1024)]
                )
            } else {
                body_snippet
            };
            return Err(MediaServerError::Http {
                status,
                body: snippet,
            });
        }

        response
            .into_body()
            .read_to_string()
            .map_err(|e| MediaServerError::Connection(format!("read error: {e}")))
    }

    /// Fetch and cache the first user's ID (needed for user-scoped endpoints).
    pub fn get_user_id(&self) -> Result<&str, MediaServerError> {
        if let Some(id) = self.user_id.get() {
            return Ok(id);
        }
        let users_val = self
            .request("GET", "/Users", None)?
            .ok_or_else(|| MediaServerError::Protocol("no response from /Users".to_string()))?;
        let users: Vec<types::UserInfo> = serde_json::from_value(users_val)
            .map_err(|e| MediaServerError::Parse(format!("/Users response: {e}")))?;
        let first = users.first().ok_or_else(|| {
            MediaServerError::Protocol("no users returned from /Users".to_string())
        })?;
        if first.id.is_empty() {
            return Err(MediaServerError::Protocol(
                "first user has no Id field".to_string(),
            ));
        }
        let _ = self.user_id.set(first.id.clone());
        Ok(self.user_id.get().unwrap())
    }

    /// GET /Users/{userId}/Items/{id} — full item body for round-trip update.
    pub fn get_item(&self, item_id: &str) -> Result<Value, MediaServerError> {
        let uid = self.get_user_id()?;
        let path = format!("/Users/{uid}/Items/{item_id}");
        self.request("GET", &path, None)?
            .ok_or_else(|| MediaServerError::Protocol(format!("empty response for GET {path}")))
    }

    /// POST /Items/{id} — send full item body with modified fields.
    pub fn update_item(&self, item_id: &str, body: &Value) -> Result<(), MediaServerError> {
        let path = format!("/Items/{item_id}");
        self.request("POST", &path, Some(body))?;
        Ok(())
    }

    /// Paginated fetch of all audio items. Returns (AudioItemView, raw Value) pairs.
    pub fn prefetch_audio_items(
        &self,
        include_media_sources: bool,
        parent_id: Option<&str>,
    ) -> Result<Vec<(types::AudioItemView, Value)>, MediaServerError> {
        self.prefetch_audio_items_limited(include_media_sources, parent_id, None)
    }

    /// As `prefetch_audio_items`, but stop after collecting at most `max_items`
    /// items (used by bounded smoke-test runs, e.g. `enrich --limit`). The cap is
    /// pushed down into the server-side `Limit` so a small bound is a single tiny
    /// request rather than the full multi-page crawl. `None` fetches everything.
    ///
    /// Note the cap applies to the raw prefetch, BEFORE any client-side location
    /// filtering (that filter runs in the caller), so a bounded run is only useful
    /// unscoped or with a library-level `ParentId` — a `--location` sub-path filter
    /// may reduce the bounded page to fewer (or zero) items.
    pub fn prefetch_audio_items_limited(
        &self,
        include_media_sources: bool,
        parent_id: Option<&str>,
        max_items: Option<usize>,
    ) -> Result<Vec<(types::AudioItemView, Value)>, MediaServerError> {
        let mut fields =
            "Name,Path,OfficialRating,AlbumArtist,Album,Genres,RunTimeTicks,ProviderIds"
                .to_string();
        if include_media_sources && self.server_type == ServerType::Emby {
            fields.push_str(",MediaSources");
        }
        let uid = self.get_user_id()?;
        let parent_filter = parent_id
            .map(|id| format!("&ParentId={id}"))
            .unwrap_or_default();

        let mut all_items = Vec::new();
        let mut start_index: i64 = 0;
        // Shrink the page to the cap so a small bound is one tiny request.
        let page_size = prefetch_page_size(max_items);

        loop {
            let path = format!(
                "/Users/{uid}/Items?Recursive=true&IncludeItemTypes=Audio\
                 &Fields={fields}{parent_filter}\
                 &StartIndex={start_index}&Limit={page_size}"
            );
            let result = self.request("GET", &path, None)?;
            let Some(val) = result else {
                if !all_items.is_empty() {
                    log::warn!(
                        "server returned empty body mid-pagination after {} items; \
                         prefetch may be incomplete",
                        all_items.len()
                    );
                }
                break;
            };
            let page: types::PrefetchResponse = serde_json::from_value(val)
                .map_err(|e| MediaServerError::Parse(format!("prefetch response: {e}")))?;
            if page.items.is_empty() {
                break;
            }
            let batch_len = page.items.len() as i64;
            let pairs = extract_audio_items(page.items);
            all_items.extend(pairs);
            start_index += batch_len;
            log::debug!(
                "fetched {} / {} audio items",
                start_index,
                page.total_record_count
            );
            if let Some(max) = max_items
                && all_items.len() >= max
            {
                all_items.truncate(max);
                break;
            }
            if start_index >= page.total_record_count {
                break;
            }
        }

        log::info!("prefetched {} audio items from server", all_items.len());
        Ok(all_items)
    }

    /// GET /Library/VirtualFolders — return music libraries only.
    pub fn discover_libraries(&self) -> Result<Vec<types::VirtualFolder>, MediaServerError> {
        let result = self
            .request("GET", "/Library/VirtualFolders", None)?
            .ok_or_else(|| {
                MediaServerError::Protocol(
                    "empty response from /Library/VirtualFolders".to_string(),
                )
            })?;
        let folders: Vec<types::VirtualFolder> = serde_json::from_value(result)
            .map_err(|e| MediaServerError::Parse(format!("VirtualFolders: {e}")))?;
        let music: Vec<types::VirtualFolder> = folders
            .into_iter()
            .filter(|f| f.collection_type.as_deref() == Some("music"))
            .collect();
        log::info!(
            "discovered {} music library/libraries: {}",
            music.len(),
            music
                .iter()
                .map(|l| l.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        Ok(music)
    }

    /// GET /MusicGenres?Recursive=true — return sorted genre names.
    #[allow(dead_code)]
    pub fn list_genres(&self) -> Result<Vec<String>, MediaServerError> {
        let result = self
            .request("GET", "/MusicGenres?Recursive=true", None)?
            .ok_or_else(|| {
                MediaServerError::Protocol(
                    "empty response from /MusicGenres?Recursive=true".to_string(),
                )
            })?;
        let resp: types::GenreResponse = serde_json::from_value(result)
            .map_err(|e| MediaServerError::Parse(format!("MusicGenres: {e}")))?;
        let mut names: Vec<String> = resp
            .items
            .into_iter()
            .filter(|g| !g.name.is_empty())
            .map(|g| g.name)
            .collect();
        names.sort_by_cached_key(|name| name.to_lowercase());
        Ok(names)
    }

    /// Fetch lyrics for an audio item, abstracting server-specific logic.
    /// Returns Ok(Some(text)) with plain lyrics or Ok(None) if no lyrics found.
    pub fn fetch_lyrics(
        &self,
        item: &types::AudioItemView,
        raw: &Value,
    ) -> Result<Option<String>, MediaServerError> {
        match self.server_type {
            ServerType::Emby => self.fetch_lyrics_emby(item, raw),
            ServerType::Jellyfin => self.fetch_lyrics_jellyfin(&item.id),
        }
    }

    fn fetch_lyrics_emby(
        &self,
        item: &types::AudioItemView,
        raw: &Value,
    ) -> Result<Option<String>, MediaServerError> {
        // Try external subtitle stream first
        if let Some((media_source_id, stream_index)) = find_emby_lyrics_stream(raw) {
            let path = format!(
                "/Videos/{}/{}/Subtitles/{}/Stream.txt",
                item.id, media_source_id, stream_index
            );
            match self.request_text("GET", &path) {
                Ok(text) => {
                    let cleaned = strip_lrc_tags(&text);
                    // Marker-only streams (`♪ Instrumental ♪`) are instrumental
                    // sidecars, not lyrics: treat them as no-lyrics so the track
                    // falls through to the genre path instead of counting as
                    // clean, evaluated lyrics.
                    if !cleaned.trim().is_empty() && !crate::util::is_instrumental_marker(&cleaned)
                    {
                        return Ok(Some(cleaned));
                    }
                }
                Err(MediaServerError::Http { status, .. }) if status == 401 || status == 403 => {
                    return Err(MediaServerError::Http {
                        status,
                        body: format!("auth error fetching lyrics for {}", item.id),
                    });
                }
                Err(e) => {
                    log::warn!(
                        "Emby subtitle fetch failed for {} (stream {}): {e}",
                        item.path.as_deref().unwrap_or("<unknown>"),
                        stream_index
                    );
                }
            }
        }

        // Fallback: embedded lyrics from Extradata
        Ok(extract_embedded_lyrics(raw))
    }

    fn fetch_lyrics_jellyfin(&self, item_id: &str) -> Result<Option<String>, MediaServerError> {
        let path = format!("/Audio/{item_id}/Lyrics");
        match self.request("GET", &path, None) {
            Ok(Some(val)) => {
                let resp: types::LyricsResponse = serde_json::from_value(val)
                    .map_err(|e| MediaServerError::Parse(format!("lyrics response: {e}")))?;
                let lines: Vec<&str> = resp
                    .lyrics
                    .iter()
                    .filter_map(|l| l.text.as_deref())
                    .filter(|t| !t.is_empty())
                    .collect();
                if lines.is_empty() {
                    return Ok(None);
                }
                let text = strip_lrc_tags(&lines.join("\n"));
                if text.trim().is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(text))
                }
            }
            Ok(None) => Ok(None),
            Err(MediaServerError::Http { status, .. }) if status == 401 || status == 403 => {
                Err(MediaServerError::Http {
                    status,
                    body: format!("auth error fetching lyrics for {item_id}"),
                })
            }
            Err(MediaServerError::Http { status: 404, .. }) => Ok(None),
            Err(e) => {
                log::warn!("Jellyfin lyrics fetch failed for {item_id}: {e}");
                Ok(None)
            }
        }
    }
}

/// Extract (AudioItemView, Value) pairs from raw JSON item values.
/// Consumes the input Vec to avoid cloning each Value.
/// Logs a warning for items that fail to deserialize.
pub fn extract_audio_items(items: Vec<Value>) -> Vec<(types::AudioItemView, Value)> {
    items
        .into_iter()
        .filter_map(|v| match types::AudioItemView::deserialize(&v) {
            Ok(view) => Some((view, v)),
            Err(e) => {
                log::warn!("skipping unparseable audio item: {e}");
                None
            }
        })
        .collect()
}

/// Determine server type from a parsed SystemInfoPublic and the Server response header.
/// Returns `None` if no signal is conclusive (caller should error with manual-override guidance).
pub fn detect_from_response(info: &SystemInfoPublic, server_header: &str) -> Option<ServerType> {
    // Tier 1: ProductName (official Jellyfin identification mechanism)
    if let Some(product) = &info.product_name {
        if product == "Jellyfin Server" {
            return Some(ServerType::Jellyfin);
        }
        // ProductName present but not Jellyfin → Emby
        return Some(ServerType::Emby);
    }

    // Tier 2: Structural shape (LocalAddress singular vs LocalAddresses plural)
    // Singular takes precedence if both are present
    if info.local_address.is_some() {
        return Some(ServerType::Jellyfin);
    }
    if info.local_addresses.is_some() {
        return Some(ServerType::Emby);
    }

    // Tier 3: Server response header
    if server_header.contains("Kestrel") {
        return Some(ServerType::Jellyfin);
    }
    if !server_header.is_empty() {
        // Any non-empty, non-Kestrel server header → assume Emby
        return Some(ServerType::Emby);
    }

    None
}

/// Auto-detect server type via GET /System/Info/Public (unauthenticated).
/// Returns `ServerType::Emby` or `ServerType::Jellyfin`.
/// Errors if the endpoint is unreachable or no signal can determine the type.
pub fn detect_server_type(url: &str) -> Result<ServerType, MediaServerError> {
    let clean_url = url.trim_end_matches('/');
    let endpoint = format!("{clean_url}/System/Info/Public");

    let agent = ureq::Agent::config_builder()
        .timeout_per_call(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .build()
        .new_agent();

    let response = agent
        .get(&endpoint)
        .header("Accept", "application/json")
        .call()
        .map_err(|e| {
            MediaServerError::Connection(format!(
                "cannot reach {endpoint} to auto-detect server type: {e}"
            ))
        })?;

    let status = response.status().as_u16();
    if status >= 400 {
        return Err(MediaServerError::Http {
            status,
            body: format!("auto-detection failed at {endpoint}"),
        });
    }

    // Read Server header before consuming body
    let server_header = response
        .headers()
        .get("Server")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let body_str = response.into_body().read_to_string().unwrap_or_default();

    let info: SystemInfoPublic = serde_json::from_str(&body_str).unwrap_or_default();

    detect_from_response(&info, &server_header).ok_or_else(|| {
        MediaServerError::Protocol(format!(
            "cannot determine server type at {clean_url}. \
             Set type = \"emby\" or type = \"jellyfin\" in your TOML config."
        ))
    })
}

/// Find the best external lyric subtitle stream in an Emby item's MediaSources.
/// Returns (media_source_id, stream_index) if found.
///
/// The upstream lyrics tool writes both synced `.lrc` (Emby codec `lrc`) and
/// unsynced `.txt` (codec `txt`/`text`) sidecars; Emby exposes both as external
/// subtitle streams. We accept all three codecs but PREFER `lrc` (synced,
/// richer) when an item has both. Unrelated external subtitle formats (e.g.
/// `srt`) are ignored. A candidate missing a numeric `Index` is skipped rather
/// than aborting the search, so a later valid candidate can still be found.
pub fn find_emby_lyrics_stream(raw: &Value) -> Option<(String, i64)> {
    let sources = raw.get("MediaSources")?.as_array()?;
    let mut txt_fallback: Option<(String, i64)> = None;
    for source in sources {
        let Some(media_source_id) = source.get("Id").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(streams) = source.get("MediaStreams").and_then(|v| v.as_array()) else {
            continue;
        };
        for stream in streams {
            if stream.get("Type").and_then(|v| v.as_str()) != Some("Subtitle") {
                continue;
            }
            if !stream
                .get("IsExternal")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                continue;
            }
            let codec = stream.get("Codec").and_then(|v| v.as_str()).unwrap_or("");
            let is_lrc = codec.eq_ignore_ascii_case("lrc");
            let is_txt = codec.eq_ignore_ascii_case("txt") || codec.eq_ignore_ascii_case("text");
            if !is_lrc && !is_txt {
                continue;
            }
            let Some(index) = stream.get("Index").and_then(|v| v.as_i64()) else {
                continue;
            };
            if is_lrc {
                // Synced lyrics are preferred; return as soon as one is found.
                return Some((media_source_id.to_string(), index));
            }
            // Remember the first txt/text candidate but keep scanning for an lrc.
            txt_fallback.get_or_insert((media_source_id.to_string(), index));
        }
    }
    txt_fallback
}

/// Extract embedded lyrics from Extradata on internal subtitle streams.
fn extract_embedded_lyrics(raw: &Value) -> Option<String> {
    let sources = raw.get("MediaSources")?.as_array()?;
    let mut fragments = Vec::new();
    for source in sources {
        let Some(streams) = source.get("MediaStreams").and_then(|v| v.as_array()) else {
            continue;
        };
        for stream in streams {
            if stream
                .get("IsExternal")
                .and_then(|v| v.as_bool())
                .unwrap_or(true)
            {
                continue;
            }
            if stream.get("Type").and_then(|v| v.as_str()) != Some("Subtitle") {
                continue;
            }
            if let Some(extradata) = stream.get("Extradata").and_then(|v| v.as_str()) {
                let trimmed = extradata.trim();
                if !trimmed.is_empty() {
                    fragments.push(trimmed.to_string());
                }
            }
        }
    }
    if fragments.is_empty() {
        return None;
    }
    Some(strip_lrc_tags(&fragments.join("\n")))
}

/// Authenticate via POST /Users/AuthenticateByName.
/// Standalone function — no client instance needed (called before API key exists).
/// Returns the AccessToken from the response.
#[allow(dead_code)]
pub fn authenticate_by_name(
    url: &str,
    username: &str,
    password: &str,
) -> Result<String, MediaServerError> {
    let clean_url = url.trim_end_matches('/');
    let endpoint = format!("{clean_url}/Users/AuthenticateByName");

    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let device_id = uuid::Uuid::new_v4().to_string();
    let version = env!("SMPR_VERSION");

    let auth_header = format!(
        "MediaBrowser Client=\"smpr\", Device=\"{hostname}\", \
         DeviceId=\"{device_id}\", Version=\"{version}\""
    );

    let body = serde_json::json!({
        "Username": username,
        "Pw": password,
    });

    let agent = ureq::Agent::config_builder()
        .timeout_per_call(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .build()
        .new_agent();

    let response = agent
        .post(&endpoint)
        .header("X-Emby-Authorization", &auth_header)
        .header("Content-Type", "application/json")
        .send_json(&body)
        .map_err(|e| {
            MediaServerError::Connection(format!("cannot reach {endpoint} for authentication: {e}"))
        })?;

    let status = response.status().as_u16();
    if status >= 400 {
        let body_snippet = response.into_body().read_to_string().unwrap_or_default();
        return Err(MediaServerError::Http {
            status,
            body: format!("authentication failed: {body_snippet}"),
        });
    }

    let body_str = response
        .into_body()
        .read_to_string()
        .map_err(|e| MediaServerError::Connection(format!("read error: {e}")))?;
    let val: Value = serde_json::from_str(&body_str)
        .map_err(|e| MediaServerError::Parse(format!("auth response: {e}")))?;

    val.get("AccessToken")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            MediaServerError::Protocol("authentication response missing AccessToken".to_string())
        })
}
