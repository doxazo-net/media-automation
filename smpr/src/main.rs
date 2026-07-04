use smpr::{config, detection, enrich, rating, report, server, store, tui, wizard};

use clap::{Args, Parser, Subcommand};
use log::LevelFilter;
use std::path::PathBuf;
use std::process;

/// Options shared across rate, force, and reset subcommands.
#[derive(Args, Clone)]
struct CommonOpts {
    /// Scope to a specific music library
    #[arg(long)]
    library: Option<String>,

    /// Scope to a location within a library
    #[arg(long)]
    location: Option<String>,

    /// Target a named server (repeatable)
    #[arg(long)]
    server: Option<Vec<String>>,

    /// Analyze only -- no server updates
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// CSV report output path
    #[arg(long)]
    report: Option<String>,

    /// Path to TOML config file
    #[arg(long)]
    config: Option<String>,

    /// Path to .env file
    #[arg(long)]
    env_file: Option<String>,

    /// Server URL for one-off use (requires --api-key)
    #[arg(long, requires = "api_key")]
    server_url: Option<String>,

    /// API key for one-off use (requires --server-url)
    #[arg(long, requires = "server_url")]
    api_key: Option<String>,

    /// Debug logging
    #[arg(short, long)]
    verbose: bool,
}

/// Overwrite/skip behavior for rate and force subcommands.
#[derive(Args, Clone)]
struct OverwriteOpts {
    /// Re-evaluate tracks that already have a rating (default unless changed in config)
    #[arg(long, conflicts_with = "skip_existing")]
    overwrite: bool,

    /// Skip tracks that already have any rating (overrides config default)
    #[arg(long, conflicts_with = "overwrite")]
    skip_existing: bool,
}

impl OverwriteOpts {
    /// Resolve to Option<bool>: Some(true)=overwrite, Some(false)=skip, None=use config default.
    fn resolve(&self) -> Option<bool> {
        if self.overwrite {
            Some(true)
        } else if self.skip_existing {
            Some(false)
        } else {
            None
        }
    }
}

#[derive(Parser)]
#[command(
    name = "smpr",
    about = "Fetch lyrics from Emby/Jellyfin, detect explicit content, set parental ratings",
    version = env!("SMPR_VERSION")
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Fetch lyrics, detect explicit content, set ratings
    Rate {
        #[command(flatten)]
        common: CommonOpts,

        #[command(flatten)]
        overwrite: OverwriteOpts,

        /// Ignore per-library force_rating from config; evaluate lyrics normally
        #[arg(long)]
        ignore_forced: bool,
    },

    /// Set a fixed rating on all tracks in scope (no lyrics evaluation)
    Force {
        /// Rating to set (e.g. G, PG-13, R)
        rating: String,

        #[command(flatten)]
        common: CommonOpts,

        #[command(flatten)]
        overwrite: OverwriteOpts,
    },

    /// Remove OfficialRating from all tracks in scope
    Reset {
        #[command(flatten)]
        common: CommonOpts,
    },

    /// Query authoritative sources for in-scope tracks and cache verdicts.
    /// With `--report <path>` it writes a calibration CSV instead of the store.
    Enrich {
        #[command(flatten)]
        common: CommonOpts,

        /// Re-query tracks already cached in the store
        #[arg(long)]
        refresh: bool,
    },

    /// Interactive setup wizard for server connection and config
    Configure {
        /// Path to TOML config file
        #[arg(long)]
        config: Option<String>,

        /// Path to .env file
        #[arg(long)]
        env_file: Option<String>,

        /// Debug logging
        #[arg(short, long)]
        verbose: bool,
    },
}

/// Build a CliInput from CommonOpts + optional overwrite/ignore_forced flags.
fn build_cli_input(
    common: &CommonOpts,
    overwrite: Option<bool>,
    ignore_forced: bool,
) -> config::CliInput {
    config::CliInput {
        config_path: common.config.as_ref().map(PathBuf::from),
        env_file: common.env_file.as_ref().map(PathBuf::from),
        server_url: common.server_url.clone(),
        api_key: common.api_key.clone(),
        server_filter: common.server.clone(),
        overwrite,
        dry_run: common.dry_run,
        report: common.report.clone(),
        library: common.library.clone(),
        location: common.location.clone(),
        verbose: common.verbose,
        ignore_forced,
    }
}

fn load_config(
    common: &CommonOpts,
    overwrite: Option<bool>,
    ignore_forced: bool,
) -> config::Config {
    let cli_input = build_cli_input(common, overwrite, ignore_forced);
    config::Config::load_from_paths(&cli_input).unwrap_or_else(|e| {
        eprintln!("Error: {e}");
        process::exit(1);
    })
}

/// Which workflow to run (avoids passing &Commands through the borrow checker).
enum Workflow {
    Rate,
    Force(String), // target_rating
    Reset,
}

/// Resolve a server's type from config or by unauthenticated detection.
fn resolve_server_type(sc: &config::ServerConfig) -> Result<config::ServerType, String> {
    match sc.server_type.clone() {
        Some(t) => Ok(t),
        None => server::detect_server_type(&sc.url)
            .map_err(|e| format!("failed to detect server type for '{}': {e}", sc.name)),
    }
}

/// Enrich all servers' in-scope tracks. With an explicit `--report` path it runs
/// in report-only mode (calibration CSV, no store writes); otherwise it populates
/// the source store. The report path is the enrich CLI flag only - deliberately
/// NOT the config-resolved `report_path`, so an unrelated `[report]` TOML section
/// (used by rate/force/reset) can't silently flip enrich into report-only.
fn run_enrich(cfg: &config::Config, report_path: Option<PathBuf>, refresh: bool) {
    let report_only = report_path.is_some();
    // Report-only runs never touch the store, so don't open (or create) it -
    // a read-only / unwritable store path must not fail a calibration pass.
    let store = if report_only {
        None
    } else {
        match store::SourceStore::open(&cfg.sources.store_path) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!(
                    "Error: failed to open source store at {}: {e}",
                    cfg.sources.store_path.display()
                );
                process::exit(1);
            }
        }
    };

    let mut summary = enrich::EnrichSummary::default();
    let mut rows: Vec<enrich::EnrichRow> = Vec::new();
    let mut had_failure = false;

    for server_config in &cfg.servers {
        let server_type = match resolve_server_type(server_config) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("Error: {e}");
                had_failure = true;
                continue;
            }
        };
        let client = server::MediaServerClient::new(
            server_config.url.clone(),
            server_config.api_key.clone(),
            server_type,
        );
        match enrich::enrich_workflow(
            &client,
            cfg,
            server_config,
            store.as_ref(),
            report_only,
            refresh,
        ) {
            Ok((s, r)) => {
                summary.matched += s.matched;
                summary.no_match += s.no_match;
                summary.cached_skipped += s.cached_skipped;
                summary.no_query_skipped += s.no_query_skipped;
                rows.extend(r);
            }
            Err(e) => {
                eprintln!("Error: enrich for '{}' failed: {e}", server_config.name);
                had_failure = true;
            }
        }
    }

    if let Some(path) = &report_path {
        match enrich::write_enrich_report(&rows, path) {
            Ok(()) => eprintln!(
                "Wrote enrich report ({} rows) to {}",
                rows.len(),
                path.display()
            ),
            Err(e) => {
                eprintln!("Error: writing enrich report failed: {e}");
                had_failure = true;
            }
        }
    }

    eprintln!(
        "Enrich: {} matched, {} no-match, {} cached-skipped, {} no-title-skipped{}",
        summary.matched,
        summary.no_match,
        summary.cached_skipped,
        summary.no_query_skipped,
        if report_only {
            " (report-only; store not written)"
        } else {
            ""
        }
    );

    if had_failure {
        process::exit(1);
    }
}

fn run_workflows(cfg: &config::Config, workflow: &Workflow) {
    let multi = cfg.servers.len() > 1;
    let mut all_results: Vec<rating::ItemResult> = Vec::new();
    let mut had_failure = false;

    for server_config in &cfg.servers {
        let server_type = match resolve_server_type(server_config) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("Error: {e}");
                had_failure = true;
                continue;
            }
        };

        let label = if multi {
            format!("{} ({:?})", server_config.name, server_type)
        } else {
            String::new()
        };
        if multi {
            eprintln!("--- Processing {} ---", label);
        }

        let client = server::MediaServerClient::new(
            server_config.url.clone(),
            server_config.api_key.clone(),
            server_type,
        );

        let results = match workflow {
            Workflow::Rate => {
                let engine = detection::DetectionEngine::new(&cfg.detection);
                rating::rate_workflow(&client, cfg, server_config, &engine)
            }
            Workflow::Force(target_rating) => {
                rating::force_workflow(&client, cfg, server_config, target_rating)
            }
            Workflow::Reset => rating::reset_workflow(&client, cfg, server_config),
        };

        match results {
            Ok(results) => {
                if multi {
                    rating::print_summary(&results, &label);
                }
                all_results.extend(results);
            }
            Err(e) => {
                eprintln!(
                    "Error: {} failed: {e}",
                    if label.is_empty() { "Server" } else { &label }
                );
                had_failure = true;
            }
        }
    }

    // Write report
    if let Some(ref report_path) = cfg.report_path {
        report::write_report(&all_results, report_path);
    }

    // Print summary (single server, or overall for multi)
    if !multi {
        rating::print_summary(&all_results, "");
    }

    if had_failure {
        process::exit(1);
    }
}

fn main() {
    let cli = Cli::parse();

    // Determine verbose from any subcommand before initializing logger
    let verbose = match &cli.command {
        Commands::Rate { common, .. }
        | Commands::Force { common, .. }
        | Commands::Reset { common }
        | Commands::Enrich { common, .. } => common.verbose,
        Commands::Configure { verbose, .. } => *verbose,
    };

    env_logger::Builder::new()
        .filter_level(if verbose {
            LevelFilter::Debug
        } else {
            LevelFilter::Warn
        })
        .format_target(false)
        .format_timestamp(None)
        .init();

    match cli.command {
        Commands::Rate {
            common,
            overwrite,
            ignore_forced,
        } => {
            let cfg = load_config(&common, overwrite.resolve(), ignore_forced);
            run_workflows(&cfg, &Workflow::Rate);
        }
        Commands::Force {
            rating: target_rating,
            common,
            overwrite,
        } => {
            let cfg = load_config(&common, overwrite.resolve(), false);
            run_workflows(&cfg, &Workflow::Force(target_rating));
        }
        Commands::Enrich { common, refresh } => {
            // Enrich's report mode is driven by its own --report flag only, not
            // the config-resolved report_path (which TOML [report] can set).
            let report_path = common.report.clone().map(PathBuf::from);
            let cfg = load_config(&common, None, false);
            run_enrich(&cfg, report_path, refresh);
        }
        Commands::Reset { common } => {
            let cfg = load_config(&common, None, false);
            run_workflows(&cfg, &Workflow::Reset);
        }
        Commands::Configure {
            config,
            env_file,
            verbose: v,
        } => {
            let (config_path, env_path) =
                wizard::resolve_config_paths(config.as_deref(), env_file.as_deref())
                    .unwrap_or_else(|e| {
                        eprintln!("Error: {e}");
                        process::exit(1);
                    });

            // Try to load existing config
            let existing = if config_path.is_file() {
                match std::fs::read_to_string(&config_path) {
                    Ok(content) => match config::parse_toml(&content) {
                        Ok(raw) => Some(raw),
                        Err(e) => {
                            eprintln!(
                                "Error: config at {} could not be parsed: {e}",
                                config_path.display()
                            );
                            process::exit(1);
                        }
                    },
                    Err(e) => {
                        eprintln!("Error: could not read {}: {e}", config_path.display());
                        process::exit(1);
                    }
                }
            } else {
                None
            };

            let has_servers = existing
                .as_ref()
                .is_some_and(|e| e.servers.as_ref().is_some_and(|s| !s.is_empty()));

            if !has_servers {
                // No existing config or empty — run wizard for onboarding
                if let Err(e) = wizard::run_wizard(config.as_deref(), env_file.as_deref(), v, false)
                {
                    eprintln!("Error: {e}");
                    process::exit(1);
                }
            } else {
                // Config exists — ask what to do
                let choice = inquire::Select::new(
                    "Configuration found. What would you like to do?",
                    vec!["Edit existing config", "Set up a new server"],
                )
                .prompt();

                match choice {
                    Ok("Edit existing config") => {
                        let existing = existing.unwrap();
                        let server_labels: Vec<String> = existing
                            .servers
                            .as_ref()
                            .map(|s| s.keys().cloned().collect())
                            .unwrap_or_default();

                        let env_keys = match tui::io::load_env_keys(&env_path, &server_labels) {
                            Ok(keys) => keys,
                            Err(e) => {
                                eprintln!("Error: could not read {}: {e}", env_path.display());
                                process::exit(1);
                            }
                        };

                        if let Err(e) = tui::run_editor(existing, env_keys, config_path, env_path) {
                            eprintln!("Error: {e}");
                            process::exit(1);
                        }
                    }
                    Ok("Set up a new server") => {
                        if let Err(e) =
                            wizard::run_wizard(config.as_deref(), env_file.as_deref(), v, true)
                        {
                            eprintln!("Error: {e}");
                            process::exit(1);
                        }
                    }
                    Ok(_) => unreachable!(),
                    Err(
                        inquire::InquireError::OperationCanceled
                        | inquire::InquireError::OperationInterrupted,
                    ) => {
                        // User cancelled — silent exit
                    }
                    Err(e) => {
                        eprintln!("Error: {e}");
                        process::exit(1);
                    }
                }
            }
        }
    }
}
