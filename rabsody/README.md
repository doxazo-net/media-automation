# RABSody

A Rust CLI for curating an [Audiobookshelf](https://www.audiobookshelf.org/) (ABS)
library. The name is **R**ust + **ABS** + rhaps-**ody**; the binary is `rabsody`.

RABSody talks to the ABS HTTP API directly. It is the eventual native replacement
for shelling out to the third-party `abs-cli`, plus a home for higher-level
curation logic (ASIN identification, chapter repair, narrator/abridged audits,
field hygiene, age/AR enrichment).

## Status

The tool was built **reads-first**; library-mutating commands now land behind the
shared write harness (dry-run by default, snapshot-before-write, append-only
ledger) plus per-command safety gates. Reads (`list`/`get`/`report`/`search`)
never touch the library. Writes (`items update`/`batch-update`/
`batch-update-progress`) and deletes (`items delete`/`batch-delete`) require an
explicit `--apply` to leave dry-run; an irreversible `--hard` delete additionally
requires a typed `DELETE` confirmation. (`login` and `config set` write only the
local credential file.)

Implemented today:

- `rabsody login` - authenticate against the ABS server (`POST /login`) and write a
  native config; expired access tokens transparently refresh (`POST /auth/refresh`).
- `rabsody config get|set` - inspect/edit the native config (server, library, token).
- `rabsody doctor` - verify connectivity + credentials against the ABS server.
- `rabsody report stats` - library summary (item count, ASIN/ISBN coverage, abridged
  count, distinct genres/tags/narrators, top genres/tags).
- `rabsody items list|get|batch-get` - read library items (filter/sort/paginate,
  `--expanded` for audio files + chapters); JSON output.
- `rabsody items update|batch-update|batch-update-progress` - write item
  metadata/tags/progress through the shared write harness (dry-run by default;
  `--apply` to commit; arrays unioned unless `--replace-tags`/`--replace-genres`).
- `rabsody items delete|batch-delete` - remove items: soft (database record only)
  by default, `--hard` also deletes files from disk. Dry-run unless `--apply`; an
  apply-mode `--hard` delete prints every target then requires typing `DELETE`.
- `rabsody items embed-metadata|batch-embed-metadata` - embed the item's metadata
  into its audio file(s). Backups are **off by default** (the per-item backups
  are what filled the disk in the 2026-06-21 incident); opt in with `--backup`.
  `batch-embed-metadata` serializes one item at a time (waiting for each server
  task to drain), and under `--backup` enforces a disk-headroom guard against
  `[cache].dataPath` (`--min-free`, default 2GiB) plus a periodic items-cache
  purge (`--purge-every`, default 50). Dry-run unless `--apply`.
- `rabsody metadata search|providers|covers` - provider metadata lookups (JSON).
- `rabsody search <query>` - search within the default library (JSON).
- `rabsody tasks list [--wait]` - list server tasks; `--wait` blocks until the queue
  drains (the reusable poller future bulk ops will serialize on).
- `rabsody cache purge|purge-items` - clear the server cache (dry-run unless
  `--apply`; the cache regenerates on demand).
- `rabsody cache free-space [--path <dir>] [--json]` - report free/total disk
  space for a local path. ABS has no disk free-space API, so this measures a
  *local* path (the ABS data/cache dir when co-located): `--path` or the
  `[cache].dataPath` config. It is the primitive the bulk embed/encode disk
  guards build on.

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

An optional `[cache]` table holds native-only settings:

```toml
[cache]
dataPath = "/mnt/user/appdata/audiobookshelf"  # local ABS data/cache dir
```

`dataPath` is the local filesystem path `cache free-space` queries (and the
embed/encode disk guards will use). It is absent from the abs-cli JSON fallback.

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
