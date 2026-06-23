//! `rabs items` - read library items (list / get / batch-get).
//!
//! Output is pretty JSON to stdout (raw API data for piping/scripting), unlike
//! the human-readable `report stats`.

use clap::Subcommand;

use crate::api::{self, ItemsListParams};
use crate::error::Result;

#[derive(Subcommand)]
pub enum ItemsCmd {
    /// List items in a library (filter / sort / paginate).
    List {
        /// Library ID; defaults to the abs-cli `defaultLibrary` when omitted.
        library: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        page: Option<u32>,
        /// Sort path, e.g. `media.metadata.title`.
        #[arg(long)]
        sort: Option<String>,
        /// Sort in descending order.
        #[arg(long)]
        desc: bool,
        /// ABS filter expression.
        #[arg(long)]
        filter: Option<String>,
        /// Request the minified response shape.
        #[arg(long)]
        minified: bool,
        /// Extra data fields to include.
        #[arg(long)]
        include: Option<String>,
    },
    /// Get a single item by ID.
    Get {
        /// Library item ID.
        id: String,
        /// Include expanded media (audio files, chapters).
        #[arg(long)]
        expanded: bool,
        /// Extra data fields to include.
        #[arg(long)]
        include: Option<String>,
    },
    /// Get multiple items by ID in one request.
    BatchGet {
        /// Library item IDs (space-separated).
        #[arg(required = true, num_args = 1..)]
        ids: Vec<String>,
    },
}

pub fn run(cmd: ItemsCmd) -> Result<()> {
    match cmd {
        ItemsCmd::List {
            library,
            limit,
            page,
            sort,
            desc,
            filter,
            minified,
            include,
        } => {
            // Only require defaultLibrary when no explicit library is given.
            let (client, lib) = match library {
                Some(lib) => (api::client_only()?, lib),
                None => api::connect()?,
            };
            let params = ItemsListParams {
                limit,
                page,
                sort,
                desc,
                filter,
                minified,
                include,
            };
            crate::print_json(&client.items_list(&lib, &params)?)
        }
        ItemsCmd::Get {
            id,
            expanded,
            include,
        } => {
            let client = api::client_only()?;
            crate::print_json(&client.item_get(&id, expanded, include.as_deref())?)
        }
        ItemsCmd::BatchGet { ids } => {
            let client = api::client_only()?;
            let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            crate::print_json(&client.items_batch_get(&refs)?)
        }
    }
}
