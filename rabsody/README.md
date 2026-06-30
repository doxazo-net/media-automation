# RABSody

A Rust CLI for curating an [Audiobookshelf](https://www.audiobookshelf.org/) (ABS)
library. The name is **R**ust + **ABS** + rhaps-**ody**; the binary is `rabsody`.

RABSody talks to the ABS HTTP API directly. It is the eventual native replacement
for shelling out to the third-party `abs-cli`, plus a home for higher-level
curation logic (ASIN identification, chapter repair, narrator/abridged audits,
field hygiene, age/AR enrichment).

## Status: reads-first

The tool is being built **reads-first** - no implemented command mutates the ABS
library, so there is currently zero risk to a live library. (`login` and
`config set` write only the local credential file.) Library-write commands
(metadata edits, chapter writes, embed/encode) land only after the shared write
harness (dry-run + backup + ledger) and per-command safety gates exist.

Implemented today:

- `rabsody login` - authenticate against the ABS server (`POST /login`) and write a
  native config; expired access tokens transparently refresh (`POST /auth/refresh`).
- `rabsody config get|set` - inspect/edit the native config (server, library, token).
- `rabsody doctor` - verify connectivity + credentials against the ABS server.
- `rabsody report stats` - library summary (item count, ASIN/ISBN coverage, abridged
  count, distinct genres/tags/narrators, top genres/tags).
- `rabsody items list|get|batch-get` - read library items (filter/sort/paginate,
  `--expanded` for audio files + chapters); JSON output.
- `rabsody metadata search|providers|covers` - provider metadata lookups (JSON).
- `rabsody search <query>` - search within the default library (JSON).
- `rabsody tasks list [--wait]` - list server tasks; `--wait` blocks until the queue
  drains (the reusable poller future bulk ops will serialize on).

Planned command families (stubbed): `asin`, `chapters`, `fields`.
See the **"RABSody: abs-cli parity"** milestone / the parity epic for the roadmap.

## Configuration

`rabsody login` writes a native TOML config at `<config-dir>/rabsody/config.toml`
(e.g. `~/.config/rabsody/config.toml`) holding `server`, `accessToken`,
`refreshToken`, and `defaultLibrary`. An expired access token is refreshed
transparently on the next request and the rotated tokens are persisted.

Until a native config exists, RABSody falls back to `abs-cli`'s
`~/.abs-cli/config.json` (same keys), so existing setups keep working; a refresh
persists back to whichever file supplied the credentials.

## Build & run

```sh
cargo run -- login --server https://abs.example.com --username alice
cargo run -- config get
cargo run -- doctor
cargo run -- report stats
cargo run -- items list --limit 20 --sort media.metadata.title
cargo run -- search "dune"
cargo run -- tasks list --wait --timeout 120
```

## Quality gates

Matches the repo's Rust conventions (Rust 1.94, edition 2024):

```sh
cargo fmt -- --check
cargo clippy -- -D warnings
cargo test -- --test-threads=1
```

## Safety notes

ABS bulk operations are disk-hungry: `embed-metadata` keeps a per-item backup and
`encode-m4b` writes a full new copy. A bulk run once filled the host's pool. Any
RABSody bulk command **must** guard free space and purge per-item cache backups
(`abs-cli cache purge-items` today; native equivalent on the roadmap).
