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

use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::error::{Error, Result};

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

#[derive(Debug, Deserialize)]
pub struct Item {
    pub id: String,
    pub media: Media,
}

#[derive(Debug, Deserialize)]
pub struct Media {
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(rename = "numChapters", default)]
    pub num_chapters: u32,
    pub metadata: Metadata,
}

#[derive(Debug, Deserialize)]
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

/// A single result from the provider metadata search.
#[derive(Debug, Deserialize)]
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
        let mut resp = req.call()?;
        let status = resp.status().as_u16();
        if status == 401 || status == 403 {
            return Err(Error::Auth { status });
        }
        if !resp.status().is_success() {
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(Error::Http {
                status,
                body: truncate(&body),
            });
        }
        resp.body_mut()
            .read_json::<T>()
            .map_err(|e| Error::Parse(format!("decoding response from {url}: {e}")))
    }

    /// `GET /api/me` - identity / auth check.
    pub fn me(&self) -> Result<serde_json::Value> {
        self.get_json("/api/me", &[])
    }

    /// One page of items for a library.
    pub fn items_page(&self, library: &str, page: u32, limit: u32) -> Result<ItemsPage> {
        // ABS library IDs are server-generated UUIDs read from trusted local
        // config (abs-cli's `defaultLibrary`), never CLI/user input, so they
        // contain no URL-reserved characters and need no percent-encoding here.
        let path = format!("/api/libraries/{library}/items");
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

    /// `GET /api/search/books` - provider metadata search.
    pub fn search_books(
        &self,
        title: &str,
        author: &str,
        provider: &str,
    ) -> Result<Vec<SearchResult>> {
        self.get_json(
            "/api/search/books",
            &[("title", title), ("author", author), ("provider", provider)],
        )
    }
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
