# media-automation

Automation tools for home media servers — Emby, Jellyfin, Lidarr, comics.

## Layout

```text
.
├── smpr/             Rust CLI for parental ratings (the main app)
├── tools/
│   ├── lidarr/       Lidarr manual-import workaround (+ setup guide)
│   ├── emby/         Emby maintenance tools
│   └── library/      Music/library maintenance shell utilities
└── docs/specs/       Specifications for unbuilt tools
```

## Tools

### [smpr](smpr/) — Set Music Parental Rating

Automatically rate music tracks on [Emby](https://emby.media/) and
[Jellyfin](https://jellyfin.org/) servers based on lyrics content. Fetches
lyrics from your server, detects explicit language using tiered word detection
(R / PG-13 / G), and sets `OfficialRating` on each track.

- Interactive setup wizard and TUI config editor
- Multi-server support (Emby and Jellyfin simultaneously)
- Per-library and per-location force-rating overrides
- Customizable detection word lists and genre allow-lists
- CSV reporting and dry-run mode
- Cross-platform: Linux, macOS, Windows

Pre-built binaries on [GitHub Releases](https://github.com/sydlexius/media-automation/releases).

### [tools/lidarr/ImportLidarrManual.py](tools/lidarr/ImportLidarrManual.py) — Lidarr Manual Import Workaround

Bypasses two Lidarr limitations:

1. **100-file limit:** when Lidarr can't determine the artist from the
   top-level folder name and finds >100 audio files, it skips parsing
   entirely. This script invokes the manual-import endpoint per album
   directory, sidestepping the cap.
2. **Missing albums:** Lidarr only matches files to albums already in its
   library. With `--prepare`, the script scans audio tags for MusicBrainz
   IDs, finds albums absent from the library, refreshes existing artists,
   and (with `--add-artists`) adds new ones.

Also moves folder artwork (`folder.jpg`, `cover.png`, etc.) to the imported
album's destination and runs quality-gating so FLAC isn't replaced by MP3
(`--ignore-quality` to bypass, `--auto-quality` to auto-resolve ambiguous
cases). Empty source directories are cleaned up after a successful import.

Stdlib-only Python 3.6+ — no `pip install` needed. Optional tools (ImageMagick,
ffmpeg, rsgain, and pip libraries) are detected at startup by a dependency
preflight that can install them for you; see
[tools/lidarr/README.md](tools/lidarr/README.md) for macOS and Unraid setup.

### [tools/emby/emby_bif_audit.py](tools/emby/emby_bif_audit.py) — Emby Chapter Thumbnail Audit

Identifies Emby video items with missing chapter thumbnail `ImageTags`
(blank or missing trickplay frames). Optionally refreshes their metadata
to reset Emby's internal trickplay state, then triggers the *Video Preview
Thumbnail Extraction* scheduled task.

Useful when Emby has cached a "failed extraction" state for a video and
won't retry on its own.

Requires `requests`.

## Library maintenance scripts

### [tools/library/clean-broken-albums.sh](tools/library/clean-broken-albums.sh)

Removes audio files from album directories listed in a plain-text file
(`broken.txt` by default), while preserving sidecar files (`.lrc`, `.jpg`,
`cover.png`, etc.). Optionally triggers a targeted Lidarr rescan per
album so the library reflects the deletions.

Format the input file like [tools/library/broken.txt.example](tools/library/broken.txt.example).
Your own `broken.txt` is gitignored.

```bash
# dry run, no Lidarr rescan
./tools/library/clean-broken-albums.sh --dry-run

# real run, plus per-album Lidarr rescans
LIDARR_URL=http://localhost:8686 LIDARR_API_KEY=... \
  ./tools/library/clean-broken-albums.sh --lidarr-rescan
```

### [tools/library/fix-unsynced-lrc.sh](tools/library/fix-unsynced-lrc.sh)

Renames `.lrc` files that contain no LRC timestamps (i.e. plain-text
lyrics incorrectly saved with a `.lrc` extension by tools like `mxlrc-go`)
to `.txt`. A synced LRC file has at least one `[mm:ss.xx]` timestamp line;
anything without that is treated as unsynced and renamed.

```bash
./tools/library/fix-unsynced-lrc.sh --dry-run --dir /mnt/user/Music
```

### [tools/library/findage.sh](tools/library/findage.sh)

Quick stats script — walks a directory of CBZ/CBR comic archives,
reads each `ComicInfo.xml`, and reports how many have a usable `AgeRating`
tag versus none/unknown. Useful as a pre-flight check before tagging
work or as input to the planned age-rating backfill tool (see
[docs/specs/age-rater-spec.md](docs/specs/age-rater-spec.md)).

## Specifications

### [docs/specs/age-rater-spec.md](docs/specs/age-rater-spec.md)

Spec for a future Python CLI that backfills the `AgeRating` field in
`ComicInfo.xml` inside CBZ archives via tiered lookup (Amazon scraping
through FlareSolverr → publisher/imprint heuristic). Not yet implemented;
[findage.sh](tools/library/findage.sh) is the matching coverage-check tool.

## Security

See [SECURITY.md](SECURITY.md) for vulnerability reporting.
