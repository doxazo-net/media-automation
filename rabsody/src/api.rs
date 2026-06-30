//! Audiobookshelf HTTP API client (reads-first) + serde models.
//!
//! Credentials resolve via [`crate::config`] (native config preferred, abs-cli
//! fallback). The client transparently refreshes an expired access token on a
//! 401/403 using the stored refresh token (`POST /auth/refresh`), persisting the
//! rotated tokens.
//!
//! This module is the API surface and is intentionally built ahead of its
//! consumers during the reads-first migration: models mirror the full ABS
//! response shapes and some client methods land before the commands that use
//! them. Dead-code is allowed here (real dead-code is still caught in `main`).
#![allow(dead_code)]

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Duration;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::config::{self, Credentials, StoredConfig};
use crate::error::{Error, Result};

/// Percent-encode set for a single URL path segment: encode everything that is
/// not an RFC 3986 "unreserved" character (ALPHA / DIGIT / `-` `.` `_` `~`), so
/// a CLI-provided ID containing `/`, `?`, `#`, `%`, etc. can never alter the
/// request path.
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Percent-encode a value for safe interpolation into a URL path segment.
fn encode_segment(value: &str) -> String {
    utf8_percent_encode(value, PATH_SEGMENT).to_string()
}

/// ABS auth response (`POST /login` and `POST /auth/refresh` share this shape).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthResponse {
    user: AuthUser,
    #[serde(default)]
    user_default_library_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthUser {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

/// A page of library items.
#[derive(Debug, Deserialize)]
pub struct ItemsPage {
    pub results: Vec<Item>,
    pub total: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub media: Media,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Media {
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(rename = "numChapters", default)]
    pub num_chapters: u32,
    pub metadata: Metadata,
    /// Expanded-only: per-file audio details. Absent in minified responses.
    #[serde(
        rename = "audioFiles",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub audio_files: Option<Vec<AudioFile>>,
    /// Expanded-only: chapter list. Absent in minified responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chapters: Option<Vec<Chapter>>,
    /// Expanded-only: processed track array on Book responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracks: Option<Vec<AudioFile>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Metadata {
    pub title: Option<String>,
    #[serde(rename = "authorName")]
    pub author_name: Option<String>,
    #[serde(rename = "narratorName")]
    pub narrator_name: Option<String>,
    pub asin: Option<String>,
    pub isbn: Option<String>,
    pub language: Option<String>,
    #[serde(default)]
    pub abridged: bool,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(rename = "seriesName")]
    pub series_name: Option<String>,
    pub subtitle: Option<String>,
}

/// Partial `media.metadata` for a write. Every field is `Option`; only `Some`
/// fields serialize (`skip_serializing_if`), so ABS receives - and merges - just
/// the fields the caller is changing. `genres` is an array ABS *replaces*
/// wholesale, so callers that want union semantics must pass the merged vec.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub narrator_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_year: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isbn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abridged: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explicit: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genres: Option<Vec<String>>,
}

/// Body for `PATCH /api/items/{id}/media`: the metadata patch plus `tags` (also
/// an array ABS replaces wholesale - pass the merged vec for union semantics).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MediaPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<MetadataPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

impl MetadataPatch {
    /// True when no field is set (would serialize to `{}`). Direct field checks
    /// avoid a serde round-trip allocation on every item.
    fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.subtitle.is_none()
            && self.author_name.is_none()
            && self.narrator_name.is_none()
            && self.series_name.is_none()
            && self.publisher.is_none()
            && self.published_year.is_none()
            && self.description.is_none()
            && self.isbn.is_none()
            && self.asin.is_none()
            && self.language.is_none()
            && self.abridged.is_none()
            && self.explicit.is_none()
            && self.genres.is_none()
    }
}

impl MediaPatch {
    /// True when nothing would be sent (no metadata fields and no tags).
    pub fn is_empty(&self) -> bool {
        self.tags.is_none() && self.metadata.as_ref().map(|m| m.is_empty()).unwrap_or(true)
    }
}

/// One entry in a `POST /api/items/batch/update` array.
#[derive(Debug, Clone, Serialize)]
pub struct BatchItemUpdate {
    pub id: String,
    #[serde(rename = "mediaPayload")]
    pub media_payload: MediaPatch,
}

/// Wrapper for `POST /api/items/batch/get` (`{ "libraryItems": [...] }`).
#[derive(Debug, Default, Deserialize)]
struct BatchGetResponse {
    #[serde(rename = "libraryItems", default)]
    library_items: Vec<Item>,
}

/// `{"success":bool,"updates":N}` from the batch-update endpoint.
#[derive(Debug, Default, Deserialize)]
pub struct BatchUpdateResult {
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub updates: u32,
}

/// One entry in a `PATCH /api/me/progress/batch/update` array. Extra progress
/// fields (`currentTime`, `progress`, ...) ride along via `flatten`.
#[derive(Debug, Clone, Serialize)]
pub struct ProgressUpdate {
    #[serde(rename = "libraryItemId")]
    pub library_item_id: String,
    #[serde(flatten)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

/// One audio file (or processed track) in an expanded item's media.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioFile {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub ino: String,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub metadata: AudioFileMetadata,
}

/// File-level metadata for an [`AudioFile`].
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioFileMetadata {
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub ext: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub size: u64,
}

/// One chapter in an expanded item's media.
#[derive(Debug, Serialize, Deserialize)]
pub struct Chapter {
    #[serde(default)]
    pub id: u32,
    #[serde(default)]
    pub start: f64,
    #[serde(default)]
    pub end: f64,
    #[serde(default)]
    pub title: String,
}

/// Paginated list response from `GET /api/libraries/{id}/items`, including the
/// pagination metadata (unlike the internal [`ItemsPage`]).
#[derive(Debug, Serialize, Deserialize)]
pub struct ItemsListResponse {
    #[serde(default)]
    pub results: Vec<Item>,
    #[serde(default)]
    pub total: u32,
    #[serde(default)]
    pub limit: u32,
    #[serde(default)]
    pub page: u32,
}

/// Query parameters for [`Client::items_list`]. Only set fields are sent.
#[derive(Debug, Default)]
pub struct ItemsListParams {
    pub limit: Option<u32>,
    pub page: Option<u32>,
    pub sort: Option<String>,
    pub desc: bool,
    pub filter: Option<String>,
    pub minified: bool,
    pub include: Option<String>,
}

impl ItemsListParams {
    /// Build the `(key, value)` query pairs, omitting unset/false fields. Pure
    /// (no I/O) so it is unit-testable without a server.
    fn to_query_pairs(&self) -> Vec<(&'static str, String)> {
        let mut q: Vec<(&'static str, String)> = Vec::new();
        if let Some(limit) = self.limit {
            q.push(("limit", limit.to_string()));
        }
        if let Some(page) = self.page {
            q.push(("page", page.to_string()));
        }
        if let Some(sort) = &self.sort {
            q.push(("sort", sort.clone()));
        }
        if self.desc {
            q.push(("desc", "1".to_string()));
        }
        if let Some(filter) = &self.filter {
            q.push(("filter", filter.clone()));
        }
        if self.minified {
            q.push(("minified", "1".to_string()));
        }
        if let Some(include) = &self.include {
            q.push(("include", include.clone()));
        }
        q
    }
}

/// A metadata provider entry from `GET /api/search/providers`.
///
/// NOTE: shape not yet verified against a live ABS server (token was invalid at
/// build time); see the PR notes. Fields are lenient so a shape surprise
/// surfaces as missing data rather than a hard parse failure.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProviderInfo {
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
}

/// A cover-search result from `GET /api/search/covers`.
///
/// NOTE: ABS streams cover results over its Socket.IO connection, so this HTTP
/// response may be partial or empty. Shape unverified against a live server.
#[derive(Debug, Serialize, Deserialize)]
pub struct CoverResult {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
}

/// A single result from the provider metadata search.
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub asin: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub narrator: Option<String>,
    /// Provider duration is in MINUTES (the items endpoint reports seconds).
    pub duration: Option<f64>,
    #[serde(default)]
    pub abridged: bool,
    pub isbn: Option<String>,
    pub language: Option<String>,
}

/// Library search response (`GET /api/libraries/{id}/search`).
///
/// Modeled on the ABS shape (a `book` array of matches plus other categories);
/// not yet verified against a live server. Lenient so surprises don't hard-fail.
#[derive(Debug, Serialize, Deserialize)]
pub struct LibrarySearchResponse {
    #[serde(default)]
    pub book: Vec<LibrarySearchBook>,
    #[serde(default)]
    pub series: Vec<serde_json::Value>,
    #[serde(default)]
    pub authors: Vec<serde_json::Value>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// One book match in a [`LibrarySearchResponse`]; the hit is the nested item.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibrarySearchBook {
    pub library_item: Option<Item>,
    pub match_key: Option<String>,
    pub match_text: Option<String>,
}

/// A server task from `GET /api/tasks`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub is_finished: bool,
}

/// Wrapper for `GET /api/tasks` - the endpoint returns an OBJECT with a `tasks`
/// array, not a bare array.
#[derive(Debug, Deserialize)]
pub struct TasksResponse {
    #[serde(default)]
    pub tasks: Vec<Task>,
}

pub struct Client {
    agent: ureq::Agent,
    server: String,
    // Interior mutability so a transparent refresh can rotate the tokens behind
    // `&self`. Single-threaded CLI, so `RefCell` is sufficient.
    access_token: RefCell<String>,
    refresh_token: RefCell<Option<String>>,
    // File the credentials came from; rotated tokens persist back here.
    source_path: PathBuf,
}

/// The shared agent config: explicit timeouts (never hang the CLI/CI) and HTTP
/// status surfaced on the `Ok` path so callers can map 401/403 to a distinct
/// auth error.
fn build_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into()
}

impl Client {
    pub fn new(creds: &Credentials) -> Self {
        Self {
            agent: build_agent(),
            server: creds.config.server.trim_end_matches('/').to_string(),
            access_token: RefCell::new(creds.config.access_token.clone()),
            refresh_token: RefCell::new(creds.config.refresh_token.clone()),
            source_path: creds.source_path.clone(),
        }
    }

    fn get_json<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T> {
        let url = format!("{}{}", self.server, path);
        let resp = self.send_get(&url, query)?;
        if status_is_auth(&resp) && self.try_refresh()? {
            return read_ok(self.send_get(&url, query)?, &url);
        }
        read_ok(resp, &url)
    }

    /// Authenticated POST with a JSON body, decoding a JSON response.
    fn post_json<B: Serialize, R: DeserializeOwned>(&self, path: &str, body: &B) -> Result<R> {
        let url = format!("{}{}", self.server, path);
        let resp = self.send_post(&url, body)?;
        if status_is_auth(&resp) && self.try_refresh()? {
            return read_ok(self.send_post(&url, body)?, &url);
        }
        read_ok(resp, &url)
    }

    fn send_get(
        &self,
        url: &str,
        query: &[(&str, &str)],
    ) -> Result<ureq::http::Response<ureq::Body>> {
        let token = self.access_token.borrow().clone();
        let mut req = self
            .agent
            .get(url)
            .header("Authorization", &format!("Bearer {token}"));
        for (k, v) in query {
            req = req.query(*k, *v);
        }
        Ok(req.call()?)
    }

    fn send_post<B: Serialize>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<ureq::http::Response<ureq::Body>> {
        let token = self.access_token.borrow().clone();
        Ok(self
            .agent
            .post(url)
            .header("Authorization", &format!("Bearer {token}"))
            .send_json(body)?)
    }

    /// On a 401/403, exchange the stored refresh token for a fresh access token
    /// (`POST /auth/refresh` with the `x-refresh-token` header), rotate + persist
    /// the tokens, and report whether a retry is worthwhile.
    ///
    /// A missing refresh token or a failed/empty refresh exchange returns
    /// `Ok(false)` so the original auth error surfaces. But a *persistence*
    /// failure returns `Err`: ABS rotates the refresh token, so a refresh that
    /// succeeds in memory yet never reaches disk would leave a stale (now
    /// invalid) refresh token for the next run. Persist before updating the
    /// in-memory tokens so we never report success on tokens we couldn't store.
    fn try_refresh(&self) -> Result<bool> {
        let refresh = match self.refresh_token.borrow().clone() {
            Some(token) => token,
            None => return Ok(false),
        };
        let url = format!("{}/auth/refresh", self.server);
        let resp = match self
            .agent
            .post(&url)
            .header("x-refresh-token", &refresh)
            .send_empty()
        {
            Ok(resp) => resp,
            Err(_) => return Ok(false),
        };
        let auth: AuthResponse = match read_ok(resp, &url) {
            Ok(auth) => auth,
            Err(_) => return Ok(false),
        };
        if auth.user.access_token.is_empty() {
            return Ok(false);
        }
        // Persist the rotated tokens so the next run (and abs-cli) stay valid;
        // fail loudly if that write fails rather than continuing on tokens that
        // exist only in memory.
        config::persist_tokens(
            &self.source_path,
            &auth.user.access_token,
            auth.user.refresh_token.as_deref(),
        )?;
        *self.access_token.borrow_mut() = auth.user.access_token.clone();
        if auth.user.refresh_token.is_some() {
            *self.refresh_token.borrow_mut() = auth.user.refresh_token.clone();
        }
        Ok(true)
    }

    /// `GET /api/me` - identity / auth check.
    pub fn me(&self) -> Result<serde_json::Value> {
        self.get_json("/api/me", &[])
    }

    /// One page of items for a library.
    pub fn items_page(&self, library: &str, page: u32, limit: u32) -> Result<ItemsPage> {
        let path = format!("/api/libraries/{}/items", encode_segment(library));
        self.get_json(
            &path,
            &[("limit", &limit.to_string()), ("page", &page.to_string())],
        )
    }

    /// All items for a library, paginated.
    pub fn all_items(&self, library: &str) -> Result<Vec<Item>> {
        const PAGE: u32 = 500;
        let mut out = Vec::new();
        let mut page = 0;
        loop {
            let p = self.items_page(library, page, PAGE)?;
            let got = p.results.len();
            out.extend(p.results);
            if out.len() as u32 >= p.total || got == 0 {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// `GET /api/search/books` - provider metadata search by title/author/asin.
    ///
    /// `provider` is always sent; the others only when non-empty. NOTE: ABS
    /// support for the `asin` query param is unverified against a live server.
    pub fn search_books(
        &self,
        title: &str,
        author: &str,
        asin: &str,
        provider: &str,
    ) -> Result<Vec<SearchResult>> {
        let mut query: Vec<(&str, &str)> = vec![("provider", provider)];
        for (k, v) in [("title", title), ("author", author), ("asin", asin)] {
            if !v.is_empty() {
                query.push((k, v));
            }
        }
        self.get_json("/api/search/books", &query)
    }

    /// `GET /api/libraries/{library}/items` with filter/sort/pagination, keeping
    /// the pagination metadata.
    pub fn items_list(&self, library: &str, params: &ItemsListParams) -> Result<ItemsListResponse> {
        let owned = params.to_query_pairs();
        let query: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let path = format!("/api/libraries/{}/items", encode_segment(library));
        self.get_json(&path, &query)
    }

    /// `GET /api/items/{item_id}`, optionally expanded with audio files/chapters.
    pub fn item_get(&self, item_id: &str, expanded: bool, include: Option<&str>) -> Result<Item> {
        let mut owned: Vec<(&str, String)> = Vec::new();
        if expanded {
            owned.push(("expanded", "1".to_string()));
        }
        if let Some(include) = include {
            owned.push(("include", include.to_string()));
        }
        let query: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let path = format!("/api/items/{}", encode_segment(item_id));
        self.get_json(&path, &query)
    }

    /// `POST /api/items/batch/get` - fetch multiple items by ID in one request.
    ///
    /// `POST /api/items/batch/get` - fetch many items by ID. ABS wraps the
    /// result in `{ "libraryItems": [...] }` (verified against ABS 2.35.1).
    pub fn items_batch_get(&self, item_ids: &[&str]) -> Result<Vec<Item>> {
        let body = serde_json::json!({ "libraryItemIds": item_ids });
        let resp: BatchGetResponse = self.post_json("/api/items/batch/get", &body)?;
        Ok(resp.library_items)
    }

    /// `GET /api/libraries/{library}/search?q=` - in-library search.
    pub fn search_library(&self, library: &str, query: &str) -> Result<LibrarySearchResponse> {
        let path = format!("/api/libraries/{}/search", encode_segment(library));
        self.get_json(&path, &[("q", query)])
    }

    /// `GET /api/tasks` - current server tasks (unwraps the `{tasks:[...]}` object).
    pub fn list_tasks(&self) -> Result<Vec<Task>> {
        let resp: TasksResponse = self.get_json("/api/tasks", &[])?;
        Ok(resp.tasks)
    }

    /// `GET /api/search/providers` - available metadata providers.
    pub fn list_providers(&self) -> Result<Vec<ProviderInfo>> {
        self.get_json("/api/search/providers", &[])
    }

    /// `GET /api/search/covers` - cover search for a title/author via a provider.
    ///
    /// NOTE: ABS streams cover results over Socket.IO; this HTTP call may return
    /// partial/empty results. Endpoint + params unverified against a live server.
    pub fn search_covers(
        &self,
        title: &str,
        author: &str,
        provider: &str,
    ) -> Result<Vec<CoverResult>> {
        self.get_json(
            "/api/search/covers",
            &[("title", title), ("author", author), ("provider", provider)],
        )
    }

    fn send_patch<B: Serialize>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<ureq::http::Response<ureq::Body>> {
        let token = self.access_token.borrow().clone();
        Ok(self
            .agent
            .patch(url)
            .header("Authorization", &format!("Bearer {token}"))
            .send_json(body)?)
    }

    /// Authenticated PATCH with a JSON body, decoding a JSON response. Mirrors
    /// [`Self::post_json`], including the transparent token-refresh retry.
    fn patch_json<B: Serialize, R: DeserializeOwned>(&self, path: &str, body: &B) -> Result<R> {
        let url = format!("{}{}", self.server, path);
        let resp = self.send_patch(&url, body)?;
        if status_is_auth(&resp) && self.try_refresh()? {
            return read_ok(self.send_patch(&url, body)?, &url);
        }
        read_ok(resp, &url)
    }

    /// The server base URL this client targets (used to label backups/ledger).
    pub fn server(&self) -> &str {
        &self.server
    }

    /// `PATCH /api/items/{id}/media` - partial update of an item's media
    /// metadata + tags. ABS merges scalar `metadata` fields server-side but
    /// *replaces* array fields (`tags`, `genres`), so callers must pre-merge
    /// arrays (see `items::merge_arrays`).
    pub fn item_update_media(&self, item_id: &str, patch: &MediaPatch) -> Result<()> {
        let path = format!("/api/items/{}/media", encode_segment(item_id));
        let _: serde_json::Value = self.patch_json(&path, patch)?;
        Ok(())
    }

    /// `POST /api/items/batch/update` - atomic batch metadata update.
    pub fn items_batch_update(&self, updates: &[BatchItemUpdate]) -> Result<BatchUpdateResult> {
        self.post_json("/api/items/batch/update", &updates)
    }

    /// PATCH that only checks status (no JSON decode) - for endpoints that reply
    /// with a non-JSON body like the plain `OK` ABS returns for some writes.
    fn patch_ok<B: Serialize>(&self, path: &str, body: &B) -> Result<()> {
        let url = format!("{}{}", self.server, path);
        let resp = self.send_patch(&url, body)?;
        if status_is_auth(&resp) && self.try_refresh()? {
            return check_ok(self.send_patch(&url, body)?, &url);
        }
        check_ok(resp, &url)
    }

    /// `PATCH /api/me/progress/batch/update` - batch listening-progress update.
    /// ABS replies with a plain `OK` body, so this checks status only.
    pub fn batch_update_progress(&self, updates: &[ProgressUpdate]) -> Result<()> {
        self.patch_ok("/api/me/progress/batch/update", &updates)
    }
}

/// `POST /login` with username/password; returns the resulting [`Credentials`]
/// (targeting the native config path, so `rabsody login` writes a native TOML).
pub fn login(server: &str, username: &str, password: &str) -> Result<Credentials> {
    let server = server.trim_end_matches('/').to_string();
    let url = format!("{server}/login");
    let resp = build_agent()
        .post(&url)
        .send_json(serde_json::json!({ "username": username, "password": password }))?;
    let auth: AuthResponse = read_ok(resp, &url)?;
    if auth.user.access_token.is_empty() {
        return Err(Error::Parse(
            "login response contained no access token".to_string(),
        ));
    }
    Ok(Credentials {
        config: StoredConfig {
            server,
            access_token: auth.user.access_token,
            refresh_token: auth.user.refresh_token,
            default_library: auth.user_default_library_id,
        },
        source_path: StoredConfig::native_path()?,
    })
}

/// Resolve credentials (native config preferred, abs-cli fallback) into a
/// [`Client`] with no library requirement.
pub fn client_only() -> Result<Client> {
    Ok(Client::new(&Credentials::load()?))
}

/// Resolve credentials and return a [`Client`] plus the default library.
///
/// Treats an empty or whitespace-only `defaultLibrary` as missing, so a blank
/// value can never produce a malformed `/api/libraries//items` path.
pub fn connect() -> Result<(Client, String)> {
    let creds = Credentials::load()?;
    let library = creds
        .config
        .default_library
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            Error::Config(
                "no default library set; run `rabsody config set library <id>`".to_string(),
            )
        })?;
    Ok((Client::new(&creds), library))
}

/// True when a response carries a 401/403 status.
fn status_is_auth(resp: &ureq::http::Response<ureq::Body>) -> bool {
    matches!(resp.status().as_u16(), 401 | 403)
}

/// Map a ureq response to a decoded value or the appropriate [`Error`]: 401/403
/// -> [`Error::Auth`], other non-2xx -> [`Error::Http`] (truncated body), decode
/// failure -> [`Error::Parse`].
fn read_ok<R: DeserializeOwned>(
    mut resp: ureq::http::Response<ureq::Body>,
    url: &str,
) -> Result<R> {
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Err(Error::Auth { status });
    }
    if !resp.status().is_success() {
        // Bound the read: only a 500-char snippet is kept, so cap well below
        // ureq's 10 MB default to avoid buffering a huge error payload.
        let body = resp
            .body_mut()
            .with_config()
            .limit(64 * 1024)
            .read_to_string()
            .map_err(|e| {
                Error::Connection(format!(
                    "reading HTTP {status} response body from {url}: {e}"
                ))
            })?;
        return Err(Error::Http {
            status,
            body: truncate(&body),
        });
    }
    resp.body_mut()
        .read_json::<R>()
        .map_err(|e| Error::Parse(format!("decoding response from {url}: {e}")))
}

/// Like [`read_ok`] but only validates the HTTP status (no body decode), for
/// write endpoints whose success body is not JSON (ABS returns a plain `OK`).
fn check_ok(mut resp: ureq::http::Response<ureq::Body>, url: &str) -> Result<()> {
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Err(Error::Auth { status });
    }
    if !resp.status().is_success() {
        let body = resp
            .body_mut()
            .with_config()
            .limit(64 * 1024)
            .read_to_string()
            .map_err(|e| {
                Error::Connection(format!(
                    "reading HTTP {status} response body from {url}: {e}"
                ))
            })?;
        return Err(Error::Http {
            status,
            body: truncate(&body),
        });
    }
    Ok(())
}

/// Truncate an HTTP error body to a bounded snippet for error messages, on a
/// char boundary so multi-byte UTF-8 is never split.
fn truncate(body: &str) -> String {
    const MAX: usize = 500;
    if body.len() <= MAX {
        return body.to_string();
    }
    let end = (0..=MAX)
        .rev()
        .find(|&i| body.is_char_boundary(i))
        .unwrap_or(0);
    format!("{}...", &body[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serve one canned raw HTTP response on a loopback port, then close.
    /// The OS backlog queues the client's connect, so there is no startup race.
    fn serve_once(raw: &'static str) -> u16 {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf); // drain request line/headers
                let _ = stream.write_all(raw.as_bytes());
            }
        });
        port
    }

    fn client_for(port: u16) -> Client {
        Client::new(&Credentials {
            config: StoredConfig {
                server: format!("http://127.0.0.1:{port}"),
                access_token: "test-token".to_string(),
                refresh_token: None,
                default_library: None,
            },
            source_path: std::path::PathBuf::from("/dev/null"),
        })
    }

    #[test]
    fn read_ok_maps_401_403_to_auth() {
        for status in [401u16, 403] {
            let raw: &'static str = if status == 401 {
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            } else {
                "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            };
            let client = client_for(serve_once(raw));
            match client.me() {
                Err(Error::Auth { status: s }) => assert_eq!(s, status),
                other => panic!("expected Auth {status}, got {other:?}"),
            }
        }
    }

    #[test]
    fn read_ok_maps_non_2xx_to_http_with_body() {
        let port = serve_once(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 4\r\nConnection: close\r\n\r\nboom",
        );
        match client_for(port).me() {
            Err(Error::Http { status, body }) => {
                assert_eq!(status, 500);
                assert_eq!(body, "boom");
            }
            other => panic!("expected Http 500, got {other:?}"),
        }
    }

    #[test]
    fn read_ok_maps_bad_json_to_parse() {
        let port = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 7\r\nConnection: close\r\n\r\nnotjson",
        );
        match client_for(port).me() {
            Err(Error::Parse(_)) => {}
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn list_query_omits_unset_and_false_fields() {
        // Default params -> no query at all.
        assert!(ItemsListParams::default().to_query_pairs().is_empty());

        let params = ItemsListParams {
            limit: Some(50),
            page: Some(2),
            sort: Some("media.metadata.title".to_string()),
            desc: true,
            filter: None,
            minified: false,
            include: Some("rssfeed".to_string()),
        };
        let q = params.to_query_pairs();
        // Set fields present in declaration order; unset (filter) and false
        // (minified) fields omitted; bools serialize as "1".
        assert_eq!(
            q,
            vec![
                ("limit", "50".to_string()),
                ("page", "2".to_string()),
                ("sort", "media.metadata.title".to_string()),
                ("desc", "1".to_string()),
                ("include", "rssfeed".to_string()),
            ]
        );
    }

    #[test]
    fn encode_segment_escapes_reserved_path_chars() {
        // UUID-like values (unreserved chars only) pass through untouched.
        assert_eq!(encode_segment("li_abc-123.def_ghi"), "li_abc-123.def_ghi");
        // Path-reserved / dangerous characters are percent-encoded.
        assert_eq!(encode_segment("../x"), "..%2Fx");
        assert_eq!(encode_segment("a?b#c%d"), "a%3Fb%23c%25d");
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let short = "héllo";
        assert_eq!(truncate(short), short);

        // A long multi-byte string must truncate on a char boundary (no panic).
        let long = "é".repeat(400); // 800 bytes > MAX(500)
        let out = truncate(&long);
        assert!(out.ends_with("..."));
        assert!(out.len() <= 500 + 3);
    }
}
