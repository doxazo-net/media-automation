//! Hand-rolled error type for RABSody.
//!
//! Per repo conventions (`.github/copilot-instructions.md`): error enums use
//! hand-rolled `Display`/`Error` impls - no `thiserror`, no `anyhow`. Callers
//! get `?` ergonomics via [`Result`] plus `.map_err()` into a specific variant,
//! and auth failures are a distinct variant so they are never masked as
//! transient connection errors.

use std::fmt;

/// Result alias for all RABSody operations.
pub type Result<T> = std::result::Result<T, Error>;

/// The single error type returned across the RABSody API client and CLI.
#[derive(Debug)]
pub enum Error {
    /// Local configuration problem: missing file, bad JSON, or a missing/empty
    /// required field.
    Config(String),
    /// Authentication failed (HTTP 401/403): token missing, expired, or
    /// rejected. Kept distinct so callers never confuse it with a transient
    /// network failure.
    Auth { status: u16 },
    /// A non-success HTTP response other than auth, with a truncated body.
    Http { status: u16, body: String },
    /// Server unreachable, TLS failure, or a request timed out.
    Connection(String),
    /// Response body was not the expected JSON shape.
    Parse(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(msg) => write!(f, "config error: {msg}"),
            Self::Auth { status } => write!(
                f,
                "authentication failed (HTTP {status}): check the abs-cli accessToken"
            ),
            Self::Http { status, body } => write!(f, "HTTP {status}: {body}"),
            Self::Connection(msg) => write!(f, "connection error: {msg}"),
            Self::Parse(msg) => write!(f, "parse error: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<ureq::Error> for Error {
    fn from(err: ureq::Error) -> Self {
        // The agent is built with `http_status_as_error(false)`, so HTTP status
        // codes are handled on the `Ok` path in `get_json`; only transport,
        // connect, and timeout errors reach this conversion.
        Self::Connection(err.to_string())
    }
}
