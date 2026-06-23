//! Audiobookshelf HTTP API client (reads-first) + config + serde models.
//!
//! Credentials are reused from abs-cli's own config at `~/.abs-cli/config.json`
//! (server + accessToken), so RABSody needs no separate login flow for reads.
//!
//! This module is the API surface and is intentionally built ahead of its
//! consumers during the reads-first migration: models mirror the full ABS
//! response shapes and some client methods land before the commands that use
//! them. Dead-code is allowed here (real dead-code is still caught in `main`).
#![allow(dead_code)]

use std::time::Duration;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

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

/// abs-cli's on-disk config (`~/.abs-cli/config.json`).
#[derive(Debug, Deserialize)]
pub struct AbsConfig {
    pub server: String,
    #[serde(rename = "accessToken")]
    pub access_token: String,
    #[serde(rename = "defaultLibrary")]
    pub default_library: Option<String>,
}

impl AbsConfig {
    /// Load from `~/.abs-cli/config.json`.
    pub fn load() -> Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| Error::Config("could not resolve home directory".to_string()))?;
        let path = home.join(".abs-cli").join("config.json");
        let raw = std::fs::read_to_string(&path).map_err(|e| {
            Error::Config(format!("reading abs-cli config at {}: {e}", path.display()))
        })?;
        let cfg: AbsConfig = serde_json::from_str(&raw)
            .map_err(|e| Error::Config(format!("parsing abs-cli config.json: {e}")))?;
        Ok(cfg)
    }
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
    token: String,
}

impl Client {
    pub fn new(cfg: &AbsConfig) -> Self {
        // Explicit timeouts so a slow or half-open server can never hang the
        // CLI (or a CI job) indefinitely: bounded TCP/TLS connect plus an
        // end-to-end ceiling covering the whole call including body read.
        let agent: ureq::Agent = ureq::Agent::config_builder()
            // Surface HTTP status codes on the `Ok` path so `get_json` can map
            // 401/403 to a distinct auth error instead of a generic transport one.
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into();
        Self {
            agent,
            server: cfg.server.trim_end_matches('/').to_string(),
            token: cfg.access_token.clone(),
        }
    }

    fn get_json<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T> {
        let url = format!("{}{}", self.server, path);
        let mut req = self
            .agent
            .get(&url)
            .header("Authorization", &format!("Bearer {}", self.token));
        for (k, v) in query {
            req = req.query(*k, *v);
        }
        let resp = req.call()?;
        read_ok(resp, &url)
    }

    /// Authenticated POST with a JSON body, decoding a JSON response.
    fn post_json<B: Serialize, R: DeserializeOwned>(&self, path: &str, body: &B) -> Result<R> {
        let url = format!("{}{}", self.server, path);
        let resp = self
            .agent
            .post(&url)
            .header("Authorization", &format!("Bearer {}", self.token))
            .send_json(body)?;
        read_ok(resp, &url)
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
    /// NOTE: response shape (`Vec<Item>`) not yet verified against a live server.
    pub fn items_batch_get(&self, item_ids: &[&str]) -> Result<Vec<Item>> {
        let body = serde_json::json!({ "libraryItemIds": item_ids });
        self.post_json("/api/items/batch/get", &body)
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
}

/// Load the abs-cli config and return a [`Client`] (no library requirement).
pub fn client_only() -> Result<Client> {
    let cfg = AbsConfig::load()?;
    Ok(Client::new(&cfg))
}

/// Load the config and return a [`Client`] plus the resolved default library.
///
/// Treats an empty or whitespace-only `defaultLibrary` as missing config, so a
/// blank value can never produce a malformed `/api/libraries//items` path.
pub fn connect() -> Result<(Client, String)> {
    let cfg = AbsConfig::load()?;
    let library = cfg
        .default_library
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| Error::Config("no defaultLibrary set in abs-cli config".to_string()))?;
    Ok((Client::new(&cfg), library))
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
        Client::new(&AbsConfig {
            server: format!("http://127.0.0.1:{port}"),
            access_token: "test-token".to_string(),
            default_library: None,
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
