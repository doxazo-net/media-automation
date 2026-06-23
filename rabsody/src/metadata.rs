//! `rabs metadata` - provider metadata: search / providers / covers.
//!
//! Output is pretty JSON to stdout (raw API data for piping/scripting).

use clap::Subcommand;

use crate::api;
use crate::error::Result;

#[derive(Subcommand)]
pub enum MetadataCmd {
    /// Search a provider for book metadata (`GET /api/search/books`).
    Search {
        /// Metadata provider, e.g. `audible`.
        #[arg(long)]
        provider: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        author: Option<String>,
        #[arg(long)]
        asin: Option<String>,
    },
    /// List available metadata providers (`GET /api/search/providers`).
    Providers,
    /// Search for cover images (`GET /api/search/covers`).
    ///
    /// ABS streams covers over Socket.IO, so results here may be partial/empty.
    Covers {
        #[arg(long)]
        provider: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        author: Option<String>,
    },
}

pub fn run(cmd: MetadataCmd) -> Result<()> {
    let client = api::client_only()?;
    match cmd {
        MetadataCmd::Search {
            provider,
            title,
            author,
            asin,
        } => {
            let results = client.search_books(
                title.as_deref().unwrap_or(""),
                author.as_deref().unwrap_or(""),
                asin.as_deref().unwrap_or(""),
                &provider,
            )?;
            crate::print_json(&results)
        }
        MetadataCmd::Providers => crate::print_json(&client.list_providers()?),
        MetadataCmd::Covers {
            provider,
            title,
            author,
        } => {
            let covers = client.search_covers(
                title.as_deref().unwrap_or(""),
                author.as_deref().unwrap_or(""),
                &provider,
            )?;
            crate::print_json(&covers)
        }
    }
}
