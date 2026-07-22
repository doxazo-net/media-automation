# picard_post_save - setup

A MusicBrainz Picard post-tagging action that gates on FLAC rip quality and then
fetches cover art with [covit](https://covers.musichoarders.xyz).

Run order per album:

1. If a `CORRUPT_RIP.log` is already present, delete the audio and stop (see
   [Stale corruption logs](#stale-corruption-logs) - this fires without
   re-checking anything)
2. Verify `flac` is available (if not, the rip check is skipped and covit still runs)
3. Test every FLAC in the album directory for stream corruption
4. If any file is confirmed corrupt: write `CORRUPT_RIP.log`, delete all audio in
   the folder, exit non-zero
5. If all clean: run covit against one of the tracks

Audio is never deleted because of a tool error (missing binary, timeout,
permission problem). Only a confirmed `flac -t` stream failure, or an existing
`CORRUPT_RIP.log`, triggers deletion.

A corruption verdict condemns the whole folder, so `flac`'s exit code is
believed only when the bytes were readable **and** the process ran to
completion. Anything else is reported as unchecked rather than corrupt:

- **Empty files** - an interrupted copy or an in-flight download, routine in an
  import pipeline. Never a bad rip.
- **Unreadable files** - a permission denial, a stalled mount or an I/O error.
  `flac` reports "cannot read this" and "read it, the audio is bad" with the
  same exit code, so the file is opened here and the descriptor handed to
  `flac` rather than letting it open the path itself. An open failure is then
  an error on this side, never a verdict about the audio. A container and a
  share disagreeing about uids is the common case, and it used to delete the
  album.
- **A `flac` killed by a signal** - reported as a negative exit code. With
  `-n 8` inside a container, an OOM kill or a container stop hits workers
  mid-run; the audio is fine. A nonzero exit carrying no error text at all is
  treated the same way, since a real verdict always comes with a diagnosis.
- **Hidden files and AppleDouble sidecars** - see below.

There is deliberately no "recently modified, may still be writing" window:
Picard has just rewritten the tags on every file in the folder, so their mtimes
are always seconds old and such a window would skip the check on every real
run. The summary line reports what was actually verified
(`1/2 FLAC(s) verified clean, 1 not checked`), so a run that skipped files
never reads as a clean bill of health.

Hidden files and macOS AppleDouble sidecars (`._01 - track.flac`, written by a
Mac onto an SMB or AFP share) are ignored everywhere. They carry a real audio
extension but hold no audio, so `flac -t` reports one as confirmed corrupt -
which would condemn and delete the entire album it sits next to.

## Picard configuration

Options - Post Tagging Actions, add one action:

| Field | Value |
|-------|-------|
| Command | `python3 /config/tools/picard_post_save.py -n 8 "%folderpath%"` |
| Execute for | albums |
| Wait for process to finish | yes |

### Pass the folder only

**Do not add `"%albumartist%"` or `"%album%"` to the command.** The
`post_tagging_actions` plugin interpolates variables into the command *string*
and only then runs `shlex.split` on the result, so a double quote inside a value
is re-parsed as a shell quote:

| Album title | What the script receives |
|-------------|--------------------------|
| `Normal Album` | `Normal Album` |
| `Say "Hello"` | `Say Hello` - quotes silently swallowed, covit queries the wrong title |
| `The 12" Mixes` | nothing - `ValueError: No closing quotation`, the action never runs |

An even number of quotes corrupts the query; an odd number kills the action
before the script starts. No escaping in the Picard command survives this,
because the value is substituted *before* the tokenizer sees it.

### Prerequisite: `windows_compatibility` must stay enabled

Passing `"%folderpath%"` is only safe because Picard replaces `"` with `_` in
paths on disk. That is the **Windows compatibility** option (Options - File
Naming); it is on by default. If it is ever turned off, an album whose title
contains a quote produces a quoted *path* and the command breaks exactly as
above. There is a test pinning this (`test_quote_in_the_folder_path_would_still_break`).

### How the cover art query is built

covit is invoked with `--input <one track from the album>` and reads the query
out of the tags itself. This is deliberate, and better than re-reading the tags
here and passing them back as `--query-artist` / `--query-album` strings:

- covit also uses **barcode, catalog number and TOC** when the tags carry them,
  which match far more precisely than artist plus album.
- A **multi-value artist credit stays intact**. A collaboration album has two
  `ALBUMARTIST` values, and anything that flattens them to a single string (or
  worse, picks just the first) queries for the wrong, partial credit.

The two positional arguments still exist as a fallback for a directory that has
no file covit can read, and are never needed when running from Picard.

## Dependencies

| Tool | Kind | Needed when | Enables |
|------|------|-------------|---------|
| `flac` | binary | always | the rip corruption check |
| `covit` | binary | always | cover art fetch |

No Python packages are required; the script is stdlib-only.

If `flac` is missing the rip check is skipped entirely and the files are left
intact - **the gate silently passes**. The `jlesage/musicbrainz-picard` image
does **not** ship a `flac` binary, so it has to be installed for step 3 to do
anything. Check which happened in the action output:

- `All N FLAC(s) verified clean` - the check ran
- `WARNING: flac not available, skipping rip check` - the check did **not** run

If `covit` is missing, cover art is skipped and any existing artwork is left
untouched.

## Stale corruption logs

`CORRUPT_RIP.log` is a tombstone: while it exists in a folder, every subsequent
run deletes the audio there immediately, without re-testing. An empty file is
enough.

This matters for the obvious recovery workflow - re-rip into the same folder,
re-run Picard - which deletes the fresh rip. **Delete `CORRUPT_RIP.log` after
re-ripping.**

## Environment overrides

| Variable | Default | Effect |
|----------|---------|--------|
| `RIPCHECK_LOG_ONLY` | unset | `1`/`true`/`yes` logs corruption but never deletes |
| `COVIT_BIN` | `/config/covit` | path to the covit binary |
| `COVIT_ADDRESS` | `covers.musichoarders.xyz` | cover art server |

### Detect-only mode

`RIPCHECK_LOG_ONLY=1` reports what would have been deleted and leaves every
file in place. It is honored on **both** deletion paths - a fresh corruption
verdict and the stale-`CORRUPT_RIP.log` fast path - so with it set the script
never removes audio at all.

This is a supported permanent posture, not only a way to trial the gate. On a
large library, detection without automated deletion is a reasonable trade: you
still get the log and the flagged folder, and you decide what to do about it.

One consequence to know: a flagged folder keeps exiting non-zero on every
later run until `CORRUPT_RIP.log` is deleted, so covit never runs there and the
album gets no cover art. If an album mysteriously has no artwork, look for that
file.

## Tests

```bash
python3 -m unittest discover -s tools/picard -v
```

The rip-check and end-to-end tests generate real FLACs and need `flac` and
`ffmpeg` on PATH; they self-skip without them. Since those are the tests
covering the code that deletes files, check they are not skipping when it
matters - a skipped suite reports `OK` too.
