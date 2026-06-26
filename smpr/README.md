# smpr — Set Music Parental Rating

Automatically rate music tracks on your Emby or Jellyfin server. Fetches lyrics,
detects explicit content, and sets parental ratings (R / PG-13 / G) so you can
filter what plays on shared devices.

## Installation

### Download

Pre-built binaries are available on
[GitHub Releases](https://github.com/sydlexius/media-automation/releases):

| Platform | Binary |
| -------- | ------ |
| Linux (x86_64, static) | `smpr-linux-x86_64` |
| macOS (Intel) | `smpr-macos-intel` |
| macOS (Apple Silicon) | `smpr-macos-apple-silicon` |
| Windows (x86_64) | `smpr-windows-x86_64.exe` |

### Build from source

```bash
cd smpr
cargo build --release
# Binary at: target/release/smpr
```

## Getting Started

### First-time setup

Run the interactive wizard to connect to your server and create a config file:

```bash
smpr configure
```

The wizard walks you through:

1. Server URL and authentication
2. Music library discovery
3. Genre allow-list selection (genres auto-rated G)
4. Detection word list review
5. Update behavior preferences

It creates a TOML config file and a `.env` file with your API key.

### Rating your library

```bash
# Preview what would change (no modifications)
smpr rate --library Music --dry-run

# Apply ratings based on lyrics analysis
smpr rate --library Music

# Generate a CSV report of all decisions
smpr rate --library Music --report report.csv
```

### Other commands

```bash
# Force-rate an entire library (no lyrics check)
smpr force G --library "Kids Music"

# Remove all ratings from a library
smpr reset --library Music
```

For full options: `smpr --help` or `smpr rate --help`.

## Editing Your Config

If you already have a config file, `smpr configure` opens a TUI editor where
you can modify all settings:

- **Servers** — edit URL, API key, server type; scan for libraries (`r` key)
- **G-Rated Genres** — pick genres that get auto-rated G (fetches from server)
- **Force Ratings** — set per-library or per-location rating overrides
  (`n`/`g`/`p`/`r` keys)
- **Detection Rules** — view and edit the word lists used for content detection
- **Preferences** — toggle whether already-rated tracks get re-evaluated

Press `s` to save, `q` to quit.

## Configuration

### Config file locations

smpr looks for config in this order:

1. `--config <path>` flag (explicit path)
2. `explicit_config.toml` in the current directory
3. Platform config directory:
   - Linux: `~/.config/smpr/config.toml`
   - macOS: `~/Library/Application Support/smpr/config.toml`
   - Windows: `%APPDATA%\smpr\config.toml`
4. `config.toml` next to the `smpr` binary

Step 4 is a fallback for portable, single-folder installs where the platform
config directory is ephemeral or absent (for example Unraid's RAM-backed
`/root`, which is wiped on every reboot). Keep `config.toml` and its `.env`
beside the binary on persistent storage and they are picked up automatically.

### API keys

API keys are stored in a `.env` file alongside your config (never in the TOML):

```bash
HOME_EMBY_API_KEY=your-api-key-here
```

The variable name is derived from the server label: `home-emby` becomes
`HOME_EMBY_API_KEY` (uppercase, hyphens to underscores).

### Example config

```toml
[servers.home-emby]
url = "http://192.168.1.126:8096"
# type = "emby"  # optional; auto-detected

[servers.home-emby.libraries.Music]
# force_rating = "G"  # force all tracks in this library to G

[servers.home-emby.libraries.Music.locations.Classical]
force_rating = "G"  # force tracks in this location to G

[detection.g_genres]
genres = ["Ambient", "Classical", "Instrumental", "Piano"]

# Genres that VETO the genre-G fallback even if a g_genres entry also matches.
# Use for film OSTs tagged "Classical": instead of a blind G, such tracks are
# left unrated and reported with action "review".
[detection.deny_genres]
genres = ["Soundtrack", "Original Score"]

[general]
overwrite = false  # skip tracks that already have a rating
```

### Location scoping (`--location`)

`--location <name>` keeps only items whose reported `Path` starts with that
library location's path. Matching is case-insensitive and separator-normalized
(`\` becomes `/`). If your server reports library locations in one path scheme
but items report another (for example posix locations like `/share/Classical`
while items carry UNC paths like `\\host\Music\...`), the prefix never matches
and the scope is empty; smpr emits a `WARN` naming the prefix it used and sample
item path roots so the mismatch is obvious rather than a silent empty run.

### Multi-server

You can configure multiple servers. Use `--server <name>` to target specific
ones, or omit it to process all:

```bash
smpr rate --server home-emby --library Music
```

## How It Works

1. **Fetch** — retrieves lyrics for each audio track from your server
2. **Detect** — scans lyrics for explicit words using two tiers:
   - **R**: strong profanity (stem + exact matching)
   - **PG-13**: moderate profanity (only checked if no R-tier match)
3. **Genre fallback** — tracks with no lyrics get rated G if their genre is in
   the allow-list (e.g., Classical, Instrumental)
4. **Apply** — sets `OfficialRating` on the track via the server API
5. **Report** — optionally writes a CSV with every decision made

False positives are handled by a configurable ignore list (e.g., "cocktail",
"hancock", "documentary" won't trigger on "cock" or "cum" stems).

## Development

### Tests

```bash
cd smpr
cargo test -- --test-threads=1   # config tests mutate process-global env vars
```

### Fuzzing

The pure parsers are exercised by [`cargo-fuzz`](https://rust-fuzz.github.io/book/)
(libFuzzer) targets under [`fuzz/`](fuzz/). Coverage-guided fuzzing surfaces
panics, regex blow-ups, and pathological inputs that example tests miss.

Requires the nightly toolchain and `cargo-fuzz`:

```bash
rustup toolchain install nightly
cargo +nightly install cargo-fuzz
```

Run a target (Ctrl-C to stop, or time-box with `-max_total_time`):

```bash
cd smpr
cargo +nightly fuzz run strip_lrc_tags                       # run until stopped
cargo +nightly fuzz run classify_lyrics -- -max_total_time=60
```

Targets:

- `strip_lrc_tags` — the LRC timestamp/metadata stripper ([`util.rs`](src/util.rs))
- `classify_lyrics` — the tiered explicit-content classifier ([`detection.rs`](src/detection.rs))

Seed corpora live in `fuzz/corpus/<target>/` (committed); libFuzzer grows them
locally as it explores (the generated additions are git-ignored). A crash writes
a reproducer to `fuzz/artifacts/<target>/`; replay it with
`cargo +nightly fuzz run <target> <artifact-path>`.

Fuzzing is not wired into CI (it needs nightly and runs unbounded); run it
locally when touching the parsers.

> The crate exposes a `lib` target ([`src/lib.rs`](src/lib.rs)) so the fuzz
> crate can link against these functions; `main.rs` is a thin binary over it.
