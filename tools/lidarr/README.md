# ImportLidarrManual - setup

## Dependencies

The script is stdlib-only at its core and degrades gracefully: every tool below
is optional, and the startup preflight only prompts for the ones the current run
actually needs (it scans the import folders to decide). Declining an install
just disables that feature and the run continues.

| Tool | Kind | Needed when | Enables |
|------|------|-------------|---------|
| `mutagen` | pip | unless `--skip-bpm` | BPM tag writes, tag reads |
| `essentia` | pip | unless `--skip-bpm` | BPM detection |
| `rsgain` | binary | unless `--no-rsgain` | ReplayGain tagging |
| `magick` (ImageMagick) | binary | TIFF/WEBP artwork present | artwork format conversion |
| `ffmpeg` | binary | animated art (`folder.mp4`) present | animated art to looping GIF |
| `cv2` (opencv) | pip | always optional | higher-quality disc-art cropping |
| `git` | binary | always optional | repo checkout and updates |

Install routing: pip libraries via `pip`, binaries via `brew` (macOS) or `un-get`
(Unraid).

## Quick start: `--setup`

`--setup` is a standalone bootstrap that needs no import path. It installs all
dependencies - including the optional ones (`cv2`/`opencv-python-headless` for
higher-quality disc-art cropping, and `git`) - creates the `il` symlink,
scaffolds a `.env` (prompting for `LIDARR_URL` / `LIDARR_API_KEY`; the key is
read without echo and the file is written `chmod 600`), and on Unraid writes the
reboot-survivable boot script. Optional pip deps (cv2) are included in that boot
script too, so they survive a reboot. It is idempotent and never overwrites an
existing `.env` without confirmation.

```bash
python3 tools/lidarr/ImportLidarrManual.py --setup        # interactive
# non-interactive (e.g. cron/CI): pass credentials as flags or LIDARR_* env vars
python3 tools/lidarr/ImportLidarrManual.py --setup --url http://localhost:8686 --api-key KEY
```

When `un-get`/`brew` has no `rsgain` package, `--setup` fetches the official
rsgain release for this platform/arch and verifies it against a pinned sha256
before extracting it to `tools/lidarr/bin/rsgain` (which `resolve_rsgain()`
checks first). That path is on the persistent checkout, so the fetched binary
survives an Unraid reboot with no boot-script entry.

### Config / `.env` location

`--setup` writes `.env` to `${XDG_CONFIG_HOME:-~/.config}/importlidarr/.env` by
default, but next to the script on Unraid (where `~/.config` is RAM-wiped on
reboot). Runtime discovery precedence: `--url`/`--api-key` > `--config` >
`--env-file` > XDG `.env` > next-to-script `.env` > `./.env` > `LIDARR_*` env
vars.

## macOS

`python3 tools/lidarr/ImportLidarrManual.py --setup` (above) handles everything.
To do it by hand instead:

```bash
brew install imagemagick ffmpeg rsgain git
python3 -m pip install mutagen essentia opencv-python-headless
# optional convenience command:
ln -sf "$PWD/tools/lidarr/ImportLidarrManual.py" /usr/local/bin/il   # or /opt/homebrew/bin
```

`brew` installs persist; nothing further needed.

## Unraid

Unraid runs its OS from RAM, so `/`, `/usr`, and pip site-packages are wiped on
every reboot. Only `/boot` (flash) and the array (`/mnt/...`) persist. Keep the
checkout on the array and let the boot script re-establish the rest.

1. Install git and python3 (one time) via un-get / NerdTools. Unraid does not
   ship python3, so it must be installed before running the script:

   ```bash
   un-get update && un-get install git python3
   ```

2. Clone onto a persistent array path (use any persistent array path; on this host anything under `/mnt/vms` persists):

   ```bash
   git clone <repo-url> /mnt/vms/dockerappdata/media-automation
   chmod +x "/mnt/vms/dockerappdata/media-automation/tools/lidarr/ImportLidarrManual.py"
   ```

3. Run `--setup` once. It installs the missing tools (fetching a checksum-verified
   `rsgain` binary if no package carries it), scaffolds `.env`, creates the `il`
   symlink, and writes a boot script that reinstalls the pip libs and recreates
   the symlink on every boot:

   ```bash
   python3 /mnt/vms/dockerappdata/media-automation/tools/lidarr/ImportLidarrManual.py --setup
   ```

   - If the User Scripts plugin is installed, it writes
     `.../user.scripts/scripts/importlidarr-boot/script` - set it to
     "At Startup of Array" once in the User Scripts UI.
   - Otherwise it appends an idempotent block to `/boot/config/go`.

4. `il` then works from any path (it is symlinked into `/usr/local/bin`):

   ```bash
   il /path/to/music
   ```

### Updates

Manual, by design:

```bash
cd /mnt/vms/dockerappdata/media-automation && git pull
```

### Notes

- un-get may not carry ImageMagick; if `un-get install imagemagick` fails,
  install it via NerdTools or another plugin. The preflight reports this and
  continues.
- Skip the preflight entirely with `--no-preflight`.

### Non-interactive / automation

In a non-interactive run (piped output, cron, CI), the preflight does not block
on a prompt: it prints the exact install command for each missing tool and
continues without installing. Pair this with `--no-preflight` to bypass the
check entirely in automated pipelines.

The Unraid boot persistence is idempotent. Re-running the install (or running
the script again after adding a dependency) rewrites the User Scripts entry, or
replaces the fenced block in `/boot/config/go`, rather than appending a
duplicate. The `il` symlink is likewise re-pointed, not stacked.

## Preserving lyric/metadata sidecars (`--preserve-sidecars`)

Lidarr imports the audio but, unless told otherwise, leaves per-track sidecar
files (synced `.lrc` / plain `.txt` lyrics, per-track `.nfo`) behind in the
source folder, so they never follow a track into the library. This is most
visible with a "deluxe" rip that lands on a different path from an edition
already in the library.

`--preserve-sidecars` makes those sidecars travel by using Lidarr's own
extra-file import (it never places or renames files itself):

```bash
il /path/to/album --preserve-sidecars
il /path/to/album --preserve-sidecars --dry-run    # report the plan, change nothing
```

What it does, per album:

- Checks Lidarr's Media Management config (`Import Extra Files` +
  `Extra File Extensions`). If your sidecar types are not enabled, it shows
  exactly what is missing and prompts before turning them on (it unions the
  extensions in, never replacing your existing list). Decline, or a
  non-interactive run, leaves the config untouched and warns.
- Carries only sidecars whose basename already matches a source audio file
  (for example `01 - Song.lrc` beside `01 - Song.flac`). Lidarr renames the
  sidecar to follow the track, so it lands correctly even when the destination
  path differs.
- Never renames or guesses. A wrong lyric on the wrong track is worse than a
  missing one (it would feed bad results into downstream tools), and deluxe
  editions reorder tracks, so anything that does not exactly match, plus every
  album-level `album.nfo` / `.cue`, is left in place and listed in the log for
  you to handle by hand. (An `album.nfo` is deliberately never carried: its
  MusicBrainz identity cannot be reliably verified, so it could mislabel the
  destination.)
- After the import, verifies the carried sidecars actually appear at the
  destination (a per-extension count, robust to Lidarr's renaming) and warns on
  any shortfall.

All of this is best-effort: a config or verification hiccup only logs a warning
and never blocks the audio import. The flag is off by default; without it,
behavior is unchanged.
