//! RABSody - a Rust CLI for curating an Audiobookshelf (ABS) library.
//!
//! Reads-first: every command implemented so far is read-only. Write commands
//! (asin/chapters/metadata/fields/age edits) are stubbed until the client's
//! write path is built and hardened, so there is currently zero risk to the
//! live library.

mod api;
mod error;

use api::{AbsConfig, Client};
use clap::{Parser, Subcommand};
use error::{Error, Result};
use std::collections::BTreeMap;

#[derive(Parser)]
#[command(
    name = "rabs",
    version,
    about = "RABSody - Audiobookshelf curation in Rust"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Verify connectivity and credentials against the ABS server.
    Doctor,
    /// Read-only reporting over the library.
    #[command(subcommand)]
    Report(ReportCmd),
    /// ASIN identification and correction (planned).
    #[command(subcommand)]
    Asin(Planned),
    /// Chapter assessment, repair, and title reformatting (planned).
    #[command(subcommand)]
    Chapters(Planned),
    /// Narrator / abridged / edition metadata audit (planned).
    #[command(subcommand)]
    Metadata(Planned),
    /// Title / subtitle / author / spelling field hygiene (planned).
    #[command(subcommand)]
    Fields(Planned),
}

#[derive(Subcommand)]
enum ReportCmd {
    /// Summarize the library (counts, identifier coverage, top genres/tags).
    Stats,
}

/// Placeholder for command families whose actions are not yet implemented.
#[derive(Subcommand)]
enum Planned {
    /// Not yet implemented (reads-first migration in progress).
    #[command(external_subcommand)]
    Any(#[allow(dead_code)] Vec<String>),
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    if let Err(e) = run() {
        eprintln!("rabs: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Doctor => doctor(),
        Command::Report(ReportCmd::Stats) => report_stats(),
        Command::Asin(_) | Command::Chapters(_) | Command::Metadata(_) | Command::Fields(_) => {
            println!("This command family is planned but not yet implemented.");
            println!("RABSody is being migrated reads-first; see README for the roadmap.");
            Ok(())
        }
    }
}

fn connect() -> Result<(Client, String)> {
    let cfg = AbsConfig::load()?;
    // Treat an empty or whitespace-only defaultLibrary as missing config, so a
    // blank value can never produce a malformed `/api/libraries//items` path.
    let library = cfg
        .default_library
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| Error::Config("no defaultLibrary set in abs-cli config".to_string()))?;
    Ok((Client::new(&cfg), library))
}

fn doctor() -> Result<()> {
    let cfg = AbsConfig::load()?;
    println!("server:        {}", cfg.server);
    println!(
        "default lib:   {}",
        cfg.default_library.as_deref().unwrap_or("(none)")
    );
    let client = Client::new(&cfg);
    let me = client.me()?;
    let user = me.get("username").and_then(|v| v.as_str()).unwrap_or("?");
    let kind = me.get("type").and_then(|v| v.as_str()).unwrap_or("?");
    println!("authenticated: {user} ({kind})");
    if let Some(lib) = cfg.default_library.as_deref() {
        let page = client.items_page(lib, 0, 1)?;
        println!("library items: {}", page.total);
    }
    println!("OK");
    Ok(())
}

fn report_stats() -> Result<()> {
    let (client, library) = connect()?;
    let items = client.all_items(&library)?;
    let total = items.len();

    let mut with_asin = 0usize;
    let mut with_isbn = 0usize;
    let mut abridged = 0usize;
    let mut missing_narrator = 0usize;
    let mut genres: BTreeMap<String, usize> = BTreeMap::new();
    let mut tags: BTreeMap<String, usize> = BTreeMap::new();
    let mut narrators: BTreeMap<String, usize> = BTreeMap::new();

    for it in &items {
        let m = &it.media.metadata;
        if m.asin.as_deref().is_some_and(|s| !s.is_empty()) {
            with_asin += 1;
        }
        if m.isbn.as_deref().is_some_and(|s| !s.is_empty()) {
            with_isbn += 1;
        }
        if m.abridged {
            abridged += 1;
        }
        match m.narrator_name.as_deref() {
            Some(n) if !n.is_empty() => {
                *narrators.entry(n.to_string()).or_default() += 1;
            }
            _ => missing_narrator += 1,
        }
        for g in &m.genres {
            *genres.entry(g.clone()).or_default() += 1;
        }
        for t in &it.media.tags {
            *tags.entry(t.clone()).or_default() += 1;
        }
    }

    let pct = |n: usize| {
        if total == 0 {
            0.0
        } else {
            100.0 * n as f64 / total as f64
        }
    };
    println!("Library items:        {total}");
    println!("  with ASIN:          {with_asin} ({:.0}%)", pct(with_asin));
    println!("  with ISBN:          {with_isbn} ({:.0}%)", pct(with_isbn));
    println!("  abridged flagged:   {abridged}");
    println!("  missing narrator:   {missing_narrator}");
    println!("  distinct genres:    {}", genres.len());
    println!("  distinct tags:      {}", tags.len());
    println!("  distinct narrators: {}", narrators.len());
    println!("\nTop 10 genres:");
    for (name, n) in top_n(&genres, 10) {
        println!("  {n:5}  {name}");
    }
    println!("\nTop 10 tags:");
    for (name, n) in top_n(&tags, 10) {
        println!("  {n:5}  {name}");
    }
    Ok(())
}

fn top_n(map: &BTreeMap<String, usize>, n: usize) -> Vec<(String, usize)> {
    let mut v: Vec<(String, usize)> = map.iter().map(|(k, c)| (k.clone(), *c)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.truncate(n);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_n_orders_by_count_desc_then_name() {
        let mut m = BTreeMap::new();
        m.insert("b".to_string(), 2);
        m.insert("a".to_string(), 2);
        m.insert("c".to_string(), 5);
        // count desc, then name asc on ties; truncated to n
        assert_eq!(
            top_n(&m, 2),
            vec![("c".to_string(), 5), ("a".to_string(), 2)]
        );
    }
}
