# RABSody

A Rust CLI for curating an [Audiobookshelf](https://www.audiobookshelf.org/) (ABS)
library. The name is **R**ust + **ABS** + rhaps-**ody**; the binary is `rabs`.

RABSody talks to the ABS HTTP API directly. It is the eventual native replacement
for shelling out to the third-party `abs-cli`, plus a home for higher-level
curation logic (ASIN identification, chapter repair, narrator/abridged audits,
field hygiene, age/AR enrichment).

## Status: reads-first

The tool is being built **reads-first** - every implemented command is read-only,
so there is currently zero risk to a live library. Write commands (metadata edits,
chapter writes, embed/encode) land only after the shared write harness (dry-run +
backup + ledger) and per-command safety gates exist.

Implemented today:

- `rabs doctor` - verify connectivity + credentials against the ABS server.
- `rabs report stats` - library summary (item count, ASIN/ISBN coverage, abridged
  count, distinct genres/tags/narrators, top genres).

Planned command families (stubbed): `asin`, `chapters`, `metadata`, `fields`.
See the **"RABSody: abs-cli parity"** milestone / the parity epic for the roadmap.

## Configuration

For now RABSody reuses `abs-cli`'s credentials at `~/.abs-cli/config.json`
(`server`, `accessToken`, `defaultLibrary`), so no separate login is required for
reads. A native `rabs login` + token refresh is on the roadmap.

## Build & run

```sh
cargo run -- doctor
cargo run -- report stats
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
