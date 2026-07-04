# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## >> ON SESSION START / RESUME: read SESSION-STATE.md FIRST <<

`SESSION-STATE.md` (gitignored, repo root) is the transient resume checkpoint:
open work items, next actions, and durable key facts (prod paths, Emby IDs/endpoints).

## Project Overview

Rust CLI (`smpr`) for Emby/Jellyfin parental rating management. Fetches lyrics from the Emby or Jellyfin API, detects explicit content using tiered word detection (R / PG-13), and sets `OfficialRating` on matching audio tracks.

## Keep GitHub posts G-rated

This project's job is flagging explicit content, so its detection **data** necessarily contains profanity — the word-list source files (`smpr/src/config/defaults.rs`, user `config.toml`) are exempt; they must contain the terms to function. But every human-facing GitHub surface — issue and PR **titles and bodies**, PR/issue **comments**, **commit messages** (a squash-merge uses the PR title, so keep that clean), and **release notes** — must stay **G-rated**: do not spell out profanity in prose. Describe it generically ("a profanity stem", "a masked substring", "an innocent token containing a profanity substring"). Release notes additionally must not be hard-wrapped (GitHub renders newlines literally). If something un-G-rated slips onto a shared surface, redact it.

## Repository Layout

```text
/
├── .env / .env.prod          # shared credentials (gitignored)
├── .env.example              # credentials template (committed)
├── README.md
├── SECURITY.md
└── smpr/                     # Rust project
    ├── Cargo.toml
    └── src/
        ├── main.rs           # CLI (clap): rate, force, reset, configure
        ├── config/           # Config struct, TOML/.env loading, defaults
        │   ├── mod.rs        # RawConfig/Config types, CliInput, load_from_paths, server/detection resolution
        │   ├── defaults.rs   # Built-in R/PG-13 word lists and false positives
        │   └── tests.rs      # Config loading tests
        ├── detection.rs      # DetectionEngine: classify_lyrics, detect_stems, detect_exact, match_g_genre
        ├── rating/           # Workflow orchestration
        │   ├── mod.rs        # rate_workflow, force_workflow, reset_workflow, print_summary
        │   ├── action.rs     # decide_rating_action, decide_clear_action, apply_rating (GET+POST round-trip)
        │   ├── scope.rs      # resolve_from_libraries, filter_by_location, build_force_rules/resolve_force_rating, resolve_override
        │   └── tests.rs      # Scope and action unit tests
        ├── report.rs         # CSV report writer (csv crate)
        ├── server/           # HTTP client and API abstractions
        │   ├── mod.rs        # MediaServerClient, detect_server_type, fetch_lyrics, prefetch_audio_items
        │   ├── types.rs      # Typed API response structs (SystemInfoPublic, VirtualFolder, AudioItemView, etc.)
        │   ├── error.rs      # MediaServerError enum (Http, Connection, Parse, Protocol)
        │   └── tests.rs      # Server detection and lyrics extraction tests
        ├── tui/              # ratatui-based TUI (reserved for future use; mod.rs only)
        ├── util.rs           # strip_lrc_tags (LRC timestamp/metadata removal)
        └── wizard/           # inquire-based interactive configure wizard
            ├── mod.rs        # run_wizard entry point, config dir resolution
            ├── server.rs     # Server URL + type detection prompts
            ├── auth.rs       # API key or username/password authentication
            ├── library.rs    # Library discovery + genre allow-list selection
            ├── detection.rs  # Custom word-list additions prompts
            ├── preferences.rs # Overwrite behavior preference
            └── output.rs     # TOML config + .env file writer
```

## Commands

```bash
# Lint & format
cd smpr
cargo fmt -- --check
cargo clippy -- -D warnings

# Tests (sequential: config tests mutate process-global env vars)
cargo test --verbose -- --test-threads=1

# Build
cargo build --release
```

Always run `cargo fmt` and `cargo clippy -- -D warnings` before committing - pre-commit hooks do not run in subagent contexts, so run them manually.

## Architecture

### CLI (main.rs)

Clap-derived parser with four subcommands:

- **`rate`** — fetch lyrics, classify content, set ratings. Flags: `--library`, `--location`, `--server`, `--dry-run`, `--report`, `--overwrite`/`--skip-existing`, `--ignore-forced`, `--limit N` (bounded smoke test — caps the prefetch BEFORE the `--location` filter, same as `enrich --limit`; use it unscoped or with `--library` only), plus config/env flags.
- **`force <RATING>`** — set a fixed rating on all tracks in scope without lyrics evaluation. Same scoping and overwrite flags as `rate`.
- **`reset`** — remove `OfficialRating` from all tracks in scope.
- **`configure`** — launch the interactive setup wizard.

`CommonOpts` holds shared flags for rate/force/reset. `OverwriteOpts` resolves `--overwrite`/`--skip-existing` to `Option<bool>`. `build_cli_input()` converts clap structs to `config::CliInput` (decouples config module from clap).

`run_workflows()` iterates over resolved servers, auto-detects server type if needed, creates a `MediaServerClient`, dispatches to the appropriate workflow, collects results, writes the CSV report, and prints the summary.

### Config merge (config/mod.rs)

`Config::load_from_paths(&CliInput)` is the single entry point. Resolution order:

1. **Config path**: `--config` flag > CWD `explicit_config.toml` > `~/.config/smpr/config.toml` (platform config dir) > `config.toml` next to the binary (portable-install fallback for environments where the platform dir is ephemeral/absent, e.g. Unraid's RAM-backed `/root`)
2. **TOML parse**: explicit `--config` path must exist; auto-discovered paths are best-effort (ignored in one-off mode if unreadable)
3. **.env file**: `--env-file` flag > same dir as resolved config > CWD `.env`. Uses `dotenvy::from_path`.
4. **Server resolution** (`resolve_servers`): `--server-url`+`--api-key` (one-off) > TOML `[servers.*]` sections. API keys from env vars as `{LABEL_UPPER}_API_KEY` (hyphens replaced with underscores). `--server` flag filters to named servers.
5. **Detection**: TOML `[detection.r]`, `[detection.pg13]`, `[detection.ignore]`, `[detection.g_genres]`, `[detection.deny_genres]` > hardcoded defaults in `config/defaults.rs`. `[detection.deny_genres]` (default empty) vetoes the genre-G fallback when a track carries one of its genres even if a `g_genres` entry also matches (e.g. a film OST tagged both `Soundtrack` and `Classical`); such tracks are left unrated and reported with action `review`.
6. **Overwrite**: CLI flag > TOML `[general].overwrite` > default `true`.
6b. **Clean rating**: TOML `[general].clean_rating` > default `"G"`. The rating applied to clean-lyric tracks (no explicit content) so they stay playable under a parental gate that blocks unrated items. An explicit empty string (`clean_rating = ""`) opts out and restores the legacy behavior (clear the rating, leaving clean tracks unrated).
7. **Report path**: CLI `--report` > TOML `[report].output_path` > None.
8. **Per-song overrides**: TOML `[[overrides]]` array-of-tables (see Per-Song Overrides below). Resolved into normalized `OverrideRule`s; no CLI equivalent.

Key types: `RawConfig` (serde TOML shape) vs `Config` (resolved, validated). `CliInput` is a plain struct that decouples config loading from clap.

### Server interaction (server/mod.rs)

`MediaServerClient` wraps a `ureq::Agent` with base URL, API key, and server type. Key methods:

- `request(method, path, body)` — authenticated JSON request; returns `Ok(None)` for empty bodies
- `request_text(method, path)` — authenticated plain-text request (for Emby subtitle streams)
- `get_user_id()` — cached `GET /Users` to resolve first user ID (needed for user-scoped endpoints)
- `get_item(id)` / `update_item(id, body)` — full-body round-trip for rating updates (`GET /Users/{uid}/Items/{id}` then `POST /Items/{id}`)
- `prefetch_audio_items(include_media_sources, parent_id)` — paginated `GET /Users/{uid}/Items` with page size 500
- `discover_libraries()` — `GET /Library/VirtualFolders`, filtered to `CollectionType == "music"`
- `fetch_lyrics(item, raw)` — dispatches to Emby or Jellyfin path

Auth headers: `X-Emby-Token` (Emby) or `X-MediaBrowser-Token` (Jellyfin).

Server type detection (`detect_server_type`): `GET /System/Info/Public` (unauthenticated). Three-tier detection:

1. `ProductName == "Jellyfin Server"` → Jellyfin; any other ProductName → Emby
2. Structural: `LocalAddress` (singular) → Jellyfin; `LocalAddresses` (plural) → Emby
3. `Server` header: "Kestrel" → Jellyfin; other → Emby

Lyrics fetch (Emby): `find_emby_lyrics_stream` selects an external subtitle stream with codec `lrc`, `txt`, or `text` (preferring synced `lrc` when both exist; unrelated formats like `srt` are ignored), fetched via `GET /Videos/{id}/{msid}/Subtitles/{idx}/Stream.txt`, fallback to embedded `Extradata`. A marker-only stream (`♪ Instrumental ♪`, detected by `util::is_instrumental_marker`) is treated as no-lyrics, not clean lyrics. Lyrics fetch (Jellyfin): `GET /Audio/{id}/Lyrics`, parse structured `LyricsResponse`.

### Detection engine (detection.rs)

`DetectionEngine` is constructed from `DetectionConfig`, pre-lowercasing stems and compiling exact-match regexes.

`classify_lyrics(text)` returns `(Option<tier>, Vec<matched_words>)`:

1. Tokenize lowercased text with `[a-z']+` regex
2. **R tier first**: `detect_stems` (substring match with false-positive filter) + `detect_exact` (word-boundary regex `\b{word}\b`)
3. **PG-13 tier**: same approach, only checked if R tier found nothing
4. R takes priority — if any R match found, PG-13 is not checked
5. Results are deduped across stem + exact hits

`match_g_genre(genres)` — returns the first genre that matches the allow-list (case-insensitive). Used as a fallback when no lyrics are found.

### Rating workflows (rating/mod.rs)

All three workflows follow the same pattern: resolve library scope → prefetch items → filter by location → process each item → return `Vec<ItemResult>`.

- **`rate_workflow`**: for each item, precedence is **per-song override > config `force_rating` > lyrics/genre** (all forced tiers bypassed by `--ignore-forced`): resolve any `[[overrides]]` match (force its rating, or `skip`) → resolve config `force_rating` for the item's path (see below) → fetch lyrics → `classify_lyrics` → set R/PG-13 on explicit content, else set `[general].clean_rating` (default `"G"`) on clean tracks (or clear them when `clean_rating` is empty). No-lyrics items fall through to genre allow-list check (`match_g_genre → "G"`).
- **`force_workflow`**: set a fixed rating on all items in scope. No lyrics evaluation.
- **`reset_workflow`**: clear `OfficialRating` on all items in scope.

**Action decisions** (`rating/action.rs`): `decide_rating_action` and `decide_clear_action` are pure functions — no server calls. `apply_rating` performs the GET+POST round-trip. Auth errors (401/403) abort the entire workflow via `RatingError::Auth`.

**Library scoping** (`rating/scope.rs`): `resolve_from_libraries` resolves `--library` and `--location` flags against `VirtualFolder` data. `filter_by_location` is a post-prefetch, case-insensitive, separator-normalized path-prefix filter against each item's reported `Path`. When a resolved `--location` filters a non-empty set down to zero, it emits a `log::warn!` (visible at the default log level) naming the prefix used and sample real item path roots, because that almost always means the item paths use a different mount view than the library location (e.g. UNC `\\host\share` vs posix `/share`) rather than a genuinely empty folder. Config-level `force_rating` is applied **per item by path**, so a full-library run honors the same forces a scoped run would (issue #235; a scope-name lookup silently ignored them in full runs). `build_force_rules` resolves each configured library/location name to its real `VirtualFolder` folder path(s) and `resolve_force_rating` matches an item's path against them (normalized prefix, with the same bounded leaf-segment fallback as `filter_by_location` for UNC-vs-posix mount-view mismatch); precedence is location > library, ties broken by longest prefix. `resolve_override` matches a normalized-path substring (longest key wins) for the per-song `[[overrides]]` feature (issue #236).

**Result types**: `ItemResult` captures item metadata, tier, matched words, previous rating, action taken, source (Lyrics/Genre/Force/Reset/Override), server name, and `has_lyrics` (true only when lyrics were fetched and classified — distinguishes a lyric-less track from one with clean lyrics, which drives the `Clean` vs `No lyrics` summary counts). `RatingAction` enum: Set, Cleared, Skipped, AlreadyCorrect, DryRun, DryRunClear, Review (genre-G vetoed by `deny_genres`; left unrated for manual review), Error.

### Configure wizard (wizard/)

`run_wizard` drives a multi-step interactive flow using `inquire` prompts:

1. Detect existing config; offer to add a server if one exists
2. `server.rs` — prompt for server URL, validate connectivity, auto-detect type
3. `auth.rs` — prompt for API key directly or authenticate via username/password
4. `library.rs` — discover music libraries, prompt for genre allow-list selection
5. `detection.rs` — prompt for additional custom word stems/exact words
6. `preferences.rs` — overwrite behavior preference
7. `output.rs` — write TOML config + `.env` file

Steps 3-5 are skipped when adding a server to an existing config.

### Report (report.rs)

`write_report` writes a CSV with columns: artist, album, track, path, tier, matched_words, previous_rating, action, source, server, has_lyrics. The `path` column is the normalized match-key (lowercased, forward-slash full path) — copy any substring of it into an `[[overrides]]` `match`. Creates parent directories if needed. Errors are logged, not fatal. `has_lyrics` is true only when lyrics were fetched and classified (distinguishes a genuinely lyric-less track from one with clean lyrics).

### Utilities (util.rs)

`strip_lrc_tags` — removes LRC timestamp tags (`[00:15.30]`) and metadata lines (`[ar:Artist Name]`) from lyrics text. Used by both Emby and Jellyfin lyrics paths.

## API Pattern (Emby and Jellyfin)

- **Auth**: `X-Emby-Token` header (Emby) or `X-MediaBrowser-Token` header (Jellyfin)
- **Server type detection**: `GET /System/Info/Public` (unauthenticated) — `ProductName == "Jellyfin Server"` → Jellyfin; fallback to structural shape and `Server` header
- **Library discovery**: `GET /Library/VirtualFolders` — returns `Name`, `ItemId`, `CollectionType`, `Locations[]`
- **Bulk audio listing**: `GET /Users/{userId}/Items?Recursive=true&IncludeItemTypes=Audio&Fields=Path,OfficialRating,AlbumArtist,Album,Genres` (paginated at 500; `MediaSources` appended for Emby lyrics fetch; `&ParentId={id}` for library scoping)
- **Item reads are user-scoped**: `GET /Users/{userId}/Items/{id}` (not `GET /Items/{id}` which returns 404)
- **Item updates require the full item body**: `POST /Items/{id}` with the complete JSON from the GET (mutate `OfficialRating` then POST back)
- **Genre listing**: `GET /MusicGenres?Recursive=true`
- **Lyrics (Emby)**: `GET /Videos/{id}/{mediaSourceId}/Subtitles/{streamIndex}/Stream.txt` for external streams; embedded lyrics from `Extradata` on internal subtitle streams
- **Lyrics (Jellyfin)**: `GET /Audio/{id}/Lyrics` — structured JSON response with `Lyrics[].Text`
- **Authentication** (wizard only): `POST /Users/AuthenticateByName` with `X-Emby-Authorization` header → returns `AccessToken`

## Named Server Model

Servers are defined in TOML `[servers.*]` sections with API keys in `.env` as `{LABEL}_API_KEY` (hyphens → underscores):

```toml
[servers.home-emby]
url = "http://192.168.1.126:8096"
# type = "emby"  # optional; auto-detected if omitted

[servers.home-emby.libraries.Music]
# force_rating = "G"  # optional; force all tracks in this library to G

[servers.home-emby.libraries.Music.locations.Classical]
# force_rating = "G"  # optional; force tracks in this location to G
```

```bash
HOME_EMBY_API_KEY=your-key
```

Server type is auto-detected; override with `type = "emby"` or `type = "jellyfin"` in the TOML section. `--server NAME` selects a specific server (repeatable). `--server-url` + `--api-key` provides one-off credentials without any TOML config.

## Per-Song Overrides

`[[overrides]]` (top-level array-of-tables in `config.toml`) force or exempt individual tracks, independent of lyrics/genre. This is the tool for the residual false-positive class that has **no** content-level fix (a real word that is non-profane in context, e.g. a garden "hoe" or a rooster "cock crow") and cannot be fixed by `false_positives` (the offending token *is* the bare word). It is a **user-local** config feature, not a shipped default.

```toml
[[overrides]]
match  = "artist/album"          # case-insensitive, separator-normalized path substring
rating = "G"                      # force this rating (album-wide)

[[overrides]]
match = "artist/album/07. track"  # a longer substring pins a single track
skip  = true                      # never touch this track's rating
```

- **Keying**: a normalized-path substring matched against each item's reported path (not full paths — Emby reports UNC for audio vs posix for sidecars — and not opaque item IDs). Precision is the user's choice: short = album/artist, longer (incl. filename) = one track. The report's `path` column is the exact string to copy from.
- **Precedence**: override > location/library `force_rating` > lyrics/genre. Among overrides, the longest (most specific) matching key wins. `rating` forces that rating; `skip = true` leaves the existing rating untouched.
- **Match-count logging**: each run logs how many items every override reached (`override match='…' matched N item(s)`), warning on 0 (a typo / wrong separators) so a misfire is visible.
- Entries with no `match`, or with neither `rating` nor `skip = true`, are dropped with a warning. `--ignore-forced` bypasses overrides along with `force_rating`.

## Configuration

- API keys go in `.env` at the repo root (or alongside the TOML config) as `{LABEL}_API_KEY` — never in TOML or committed files
- Use `--env-file .env.prod` to target production credentials
- Word lists, genre allow-list, and library force_rating rules go in `explicit_config.toml` (or platform config dir, e.g., `~/.config/smpr/config.toml` on Linux)
- Only `.env.example` is committed; `.env` variants are gitignored. TOML configs are user-managed (wizard writes to the platform config dir, not the repo)
- `overwrite` (default `true`): when true, `rate` re-evaluates all tracks including re-rating clean tracks to `clean_rating`; when false, skips tracks with existing ratings (`--skip-existing`)
- `clean_rating` (default `"G"`): rating applied to clean-lyric tracks so they remain playable under a parental gate that blocks unrated Music; set `clean_rating = ""` to clear clean tracks instead (legacy behavior)
- `force_rating` (library/location): applied **per item by path** on every run, including full-library runs (not only when scoped with `--location`/`--library`)
- `[[overrides]]`: per-song force/skip by normalized-path substring (see Per-Song Overrides); highest precedence
- Precedence: CLI flags > env vars > `.env` file > TOML config > hardcoded defaults

## CI

GitHub Actions workflows in `.github/workflows/`:

- **ci.yml**: runs on push/PR to `main`
  - **Lint**: `cargo fmt -- --check` + `cargo clippy -- -D warnings` (Rust 1.94)
  - **Test**: `cargo test --verbose -- --test-threads=1` (sequential — config tests mutate process-global env vars)
  - **Build**: `cargo build --release`

- **release.yml**: runs on **binary-namespaced tag push** — `smpr-v*` or `rabsody-v*` (e.g. `smpr-v0.4.1`, `rabsody-v0.2.0`). One parameterized workflow serves both binaries.
  - **Setup**: parses the tag → `bin` (crate dir) + `version` (prefix stripped)
  - **Check**: lint + test gate for that crate (ubuntu-only)
  - **Build**: cross-compile matrix — Linux (musl static), macOS Intel, macOS Apple Silicon, Windows; passes `{BIN}_VERSION_OVERRIDE=<version>` so the binary embeds the tag version
  - **Release**: title `<bin> <version>`, notes from the **annotated tag message** (`--notes-from-tag`)

### Releasing (tag-only, no PR, no version bump)

The git tag is the single source of truth for the version. Each crate's
`Cargo.toml` `version` is a frozen `0.0.0` placeholder; `build.rs` derives the
real version from the tag via `git describe --match '<bin>-v*'` (or a
`{BIN}_VERSION_OVERRIDE` env var in CI), surfaced to clap `--version` and the
smpr auth header as `env!("{BIN}_VERSION")`. **Do not hand-edit the `version`
field.** To release, push an annotated tag (the message becomes the release
notes):

```bash
git tag -a smpr-v0.4.1 -m "release notes here"
git push origin smpr-v0.4.1
```

Legacy bare `v0.x` tags predate this scheme and are smpr's history; new releases
use the namespaced form. Per-binary config lives in `.claude/release.toml`.

Pre-commit hooks: `cargo fmt -- --check` and `cargo clippy -- -D warnings` (the subagent-bypass caveat is noted under Commands - run them manually).
