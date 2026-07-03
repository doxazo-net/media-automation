//! `smpr` library surface.
//!
//! The crate is primarily a binary (`src/main.rs`), but the modules are exposed
//! here as a library so out-of-tree consumers - notably the `cargo-fuzz` targets
//! under `fuzz/` - can link against the pure parsers (`strip_lrc_tags`,
//! `DetectionEngine::classify_lyrics`) without going through the CLI.
//!
//! All modules live here (not in `main.rs`) because they reference each other
//! via `crate::` - e.g. `rating` uses `crate::config`/`crate::server` - so they
//! must share one crate root. `main.rs` is a thin binary that imports from here.

pub mod config;
pub mod detection;
pub mod rating;
pub mod report;
pub mod server;
pub mod sources;
pub mod store;
pub mod tui;
pub mod util;
pub mod wizard;

// Convenience re-exports for fuzz targets and other external consumers.
pub use config::DetectionConfig;
pub use detection::DetectionEngine;
pub use util::strip_lrc_tags;
pub use util::{INSTRUMENTAL_MARKER, is_instrumental_marker};
