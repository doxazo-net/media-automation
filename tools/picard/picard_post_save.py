#!/usr/bin/env python3
"""
Picard post-tagging action: FLAC rip quality gate + cover art fetch.

Combined workflow:
    1. If CORRUPT_RIP.log already exists, delete the audio and exit WITHOUT
       re-testing anything (the log is a tombstone -- an empty one is enough,
       so a re-rip into the same folder is condemned until it is removed)
    2. Verify `flac` is available (if not, skip check, still run covit)
    3. Test every FLAC in the album directory for stream corruption
    4. If ANY confirmed corrupt: log CORRUPT_RIP.log, delete all audio, exit
    5. If ALL clean: run covit to fetch cover art

Hidden and macOS AppleDouble (`._*`) entries are ignored throughout: they are
never real audio, and treating one as such deletes the album it sits next to.

Picard setup (Post Tagging Actions plugin):
    Command:   python3 /config/tools/picard_post_save.py -n 8 "%folderpath%"
    Execute for: albums
    Wait for process to finish: yes

DO NOT pass "%albumartist%" / "%album%" on the command line. The plugin
interpolates variables into the command *string* and only then runs
``shlex.split`` on the result, so a double quote in the metadata is re-parsed
as a shell quote: an even number is silently swallowed (covit then queries the
wrong title) and an odd number raises "No closing quotation" so the action
never runs at all. There is no escaping that survives this -- the values must
not cross the command line. The album directory is safe to pass as long as
Picard's ``windows_compatibility`` option is enabled (Options - File Naming),
which replaces ``"`` with ``_`` in paths on disk. A quote reaching the path
would break this the same way.

Instead of passing metadata, covit is handed ``--input <audio file>`` and reads
the query out of the tags itself. That is better than extracting the tags here
and passing them back as strings: covit also uses barcode, catalog number and
TOC when present (far more precise than artist/album), and a multi-value artist
credit stays intact rather than being flattened to its first value. The
positional arguments remain only as a fallback for a directory with no readable
audio file, and are never needed from Picard.

SAFETY: Files are NEVER deleted due to tool errors (missing flac binary,
timeouts, etc). Only confirmed stream corruption triggers deletion.

Environment variable overrides:
    RIPCHECK_LOG_ONLY=1     Log corruption but don't delete files
    COVIT_BIN               Path to covit binary (default: /config/covit)
    COVIT_ADDRESS            Cover art server (default: covers.musichoarders.xyz)
"""

from __future__ import annotations

import os
import sys
import shlex
import shutil
import subprocess
import time
import argparse
from enum import Enum
from concurrent.futures import ThreadPoolExecutor, as_completed

AUDIO_EXTENSIONS = {
    '.flac', '.mp3', '.m4a', '.ogg', '.opus', '.wav', '.wv',
    '.ape', '.dsf', '.dff', '.aac', '.alac',
}

LOG_FILENAME = 'CORRUPT_RIP.log'
LOG_ONLY = os.environ.get('RIPCHECK_LOG_ONLY', '').strip() in ('1', 'true', 'yes')
COVIT_BIN = os.environ.get('COVIT_BIN', '/config/covit')
COVIT_ADDRESS = os.environ.get('COVIT_ADDRESS', 'covers.musichoarders.xyz')


def _log(msg: str, error: bool = False) -> None:
    """Print a log message. Picard shows stderr as 'Action error', so only
    actual problems should go there. Info/progress goes to stdout."""
    dest = sys.stderr if error else sys.stdout
    print(f"[post_save] {msg}", file=dest, flush=True)


# ---------------------------------------------------------------------------
# Stream testing
# ---------------------------------------------------------------------------

class StreamStatus(Enum):
    OK = 'ok'
    CORRUPT = 'corrupt'
    TOOL_ERROR = 'tool_error'


def test_flac_stream(file_path: str) -> tuple[StreamStatus, str]:
    """Run flac -t to test audio stream integrity.

    Returns (status, detail):
        OK:         stream verified clean
        CORRUPT:    flac -t confirmed stream errors
        TOOL_ERROR: inconclusive — tool failure, NOT evidence of corruption

    A CORRUPT verdict condemns the WHOLE album, so flac's exit code is trusted
    only when BOTH preconditions hold: the bytes were readable, and the process
    ran to completion. flac reports "cannot read this" and "read it, the audio
    is bad" through the same exit code, and a process killed before it finished
    reports nothing at all -- neither is evidence about the audio.
    """
    # An empty file is never a bad rip -- it is an interrupted copy or a
    # download still in flight, which is routine in an import pipeline.
    #
    # Deliberately NOT paired with an mtime "still being written" window:
    # Picard has just rewritten the tags on every file here, so their mtimes
    # are always seconds old, and any such window would skip the rip check on
    # every real run -- a gate that passes because it never looked.
    try:
        if os.path.getsize(file_path) == 0:
            return StreamStatus.TOOL_ERROR, 'file is empty (incomplete copy?)'
    except OSError as exc:
        return StreamStatus.TOOL_ERROR, f'could not stat file: {exc}'

    # Open the file HERE and hand flac the descriptor, rather than letting it
    # open the path itself. That is what makes the "readable" precondition
    # actually hold: a separate probe followed by a separate open is two
    # instants, and a share that flickers in between (uid remap, SMB
    # reconnect, ESTALE) lets flac fail to open a file the probe just read.
    # flac announces that with exit 1 and a populated stderr, so it survives
    # every guard below and condemns the album. One handle, one instant, and
    # the whole "could flac open it?" question stops existing -- an open
    # failure is an OSError here, never a verdict about the audio.
    #
    # Verified across nine corruption shapes (MD5 mismatch, header,
    # STREAMINFO, mid-stream, truncation, non-audio) that reading from stdin
    # returns the same status as the path form, so this buys the safety
    # without weakening detection.
    try:
        source = open(file_path, 'rb')
    except OSError as exc:
        return StreamStatus.TOOL_ERROR, f'file is not readable: {exc}'

    try:
        with source:
            # --silent (not --totally-silent) is load-bearing: the empty-stderr
            # guard below treats a reasonless failure as inconclusive, and
            # --totally-silent exits 1 with no stderr at all, which would turn
            # every corruption verdict into a skip and disable the gate.
            result = subprocess.run(
                ['flac', '-t', '--silent', '-'],
                stdin=source, capture_output=True, text=True, timeout=300,
            )
        if result.returncode == 0:
            return StreamStatus.OK, ''

        # A negative returncode means flac was killed by a signal, not that it
        # finished and disagreed with the audio. With -n 8 in a container an
        # OOM kill (SIGKILL) or a container stop (SIGTERM) hits workers
        # mid-flight; the files are fine. flac uses exit 1 for genuine stream
        # errors, so this can never mask a real bad rip.
        if result.returncode < 0:
            return StreamStatus.TOOL_ERROR, (
                f'flac killed by signal {-result.returncode} before it finished'
            )

        stderr = (result.stderr or '').strip()
        if not stderr:
            # Refuse to condemn an album on an assertion with nothing behind
            # it: a real corruption verdict always comes with flac's diagnosis.
            return StreamStatus.TOOL_ERROR, (
                f'flac exited {result.returncode} without reporting a reason'
            )

        return StreamStatus.CORRUPT, stderr
    except FileNotFoundError:
        return StreamStatus.TOOL_ERROR, 'flac binary not found in PATH'
    except subprocess.TimeoutExpired:
        return StreamStatus.TOOL_ERROR, 'flac -t timed out (>300s)'
    # A file-level PermissionError is raised by the open() above, not here:
    # subprocess only fails when the process itself cannot be spawned. These
    # arms cover that, plus any read error on the handed-over descriptor.
    except OSError as exc:
        return StreamStatus.TOOL_ERROR, f'OS error: {exc}'
    except Exception as exc:
        return StreamStatus.TOOL_ERROR, f'unexpected error: {exc}'


def preflight_flac() -> bool:
    """Verify `flac` binary exists and runs. Returns True if usable."""
    if not shutil.which('flac'):
        return False
    try:
        result = subprocess.run(
            ['flac', '--version'],
            capture_output=True, text=True, timeout=10,
        )
        return result.returncode == 0
    except Exception:
        return False


# ---------------------------------------------------------------------------
# Filesystem helpers
# ---------------------------------------------------------------------------

def is_hidden_or_sidecar(fname: str) -> bool:
    """True for dotfiles and macOS AppleDouble resource-fork sidecars.

    A Mac writing to an SMB/AFP share leaves a `._<name>` twin next to every
    real file, carrying the same extension while containing no audio. Both
    matter here and in opposite directions: `flac -t` reports `._01.flac` as
    CONFIRMED CORRUPT (it is not a FLAC at all), which would condemn and delete
    the entire album; and `._01.flac` sorts before any digit, so it would also
    win the covit query-file pick. Skip anything starting with a dot.
    """
    return fname.startswith('.')


def find_flacs(directory: str) -> list[str]:
    """Find all FLAC files in a directory (non-recursive)."""
    try:
        return sorted(
            os.path.join(directory, f)
            for f in os.listdir(directory)
            if f.lower().endswith('.flac')
            and not is_hidden_or_sidecar(f)
            and os.path.isfile(os.path.join(directory, f))
        )
    except OSError:
        return []


def log_corruption(album_dir: str, file_path: str, error: str) -> None:
    """Append a corruption entry to CORRUPT_RIP.log."""
    log_path = os.path.join(album_dir, LOG_FILENAME)
    timestamp = time.strftime('%Y-%m-%d %H:%M:%S')
    basename = os.path.basename(file_path)

    entry = (
        f"[{timestamp}] CORRUPT STREAM: {basename}\n"
        f"  Path:  {file_path}\n"
        f"  Error: {error}\n"
        f"\n"
    )

    try:
        with open(log_path, 'a') as fh:
            fh.write(entry)
    except OSError as exc:
        _log(f"ERROR: Could not write log: {exc}", error=True)


def delete_audio_files(album_dir: str) -> list[str]:
    """Delete all audio files in a directory. Returns list of deleted paths.

    Hidden and AppleDouble entries are left alone. They are never the audio
    this is meant to remove, and deleting on a name match is exactly the
    conflation that let a `._01.flac` sidecar condemn a whole album.
    """
    deleted = []
    try:
        for fname in os.listdir(album_dir):
            fpath = os.path.join(album_dir, fname)
            if not os.path.isfile(fpath) or is_hidden_or_sidecar(fname):
                continue
            ext = os.path.splitext(fname)[1].lower()
            if ext in AUDIO_EXTENSIONS:
                try:
                    os.remove(fpath)
                    deleted.append(fpath)
                except OSError as exc:
                    _log(f"WARNING: Could not delete {fpath}: {exc}", error=True)
    except OSError as exc:
        _log(f"WARNING: Could not list {album_dir}: {exc}", error=True)
    return deleted


# ---------------------------------------------------------------------------
# Query file selection
# ---------------------------------------------------------------------------

# covit reads the query straight out of an audio file when given --input, and it
# understands more than artist/album: barcode, catalog number and TOC all make
# for far more precise matches. Handing it a file therefore beats extracting
# tags here and passing them back in as strings -- and it sidesteps the
# multi-value problem entirely (an album credited to two artists has two
# ALBUMARTIST values, and picking just the first one silently queries for the
# wrong, partial credit).
COVIT_INPUT_EXTENSIONS = {
    '.aiff', '.ape', '.dsf', '.flac', '.mp3', '.m4a',
    '.mp4', '.ogg', '.opus', '.tak', '.wav', '.wv',
}


def find_query_file(directory: str) -> str | None:
    """Return an audio file for covit to read the query from, or None.

    Picks the alphabetically first real file covit can parse, which for a
    normal track-numbered album is track 1. Every track on an album carries the
    same album-level tags, so the choice only matters when the directory holds
    something unrelated. Hidden and AppleDouble entries are skipped -- a `._01`
    twin sorts ahead of every digit and would otherwise always win the pick.
    """
    try:
        entries = sorted(os.listdir(directory))
    except OSError as exc:
        _log(f"WARNING: Could not list {directory}: {exc}", error=True)
        return None

    for fname in entries:
        if is_hidden_or_sidecar(fname):
            continue
        if os.path.splitext(fname)[1].lower() not in COVIT_INPUT_EXTENSIONS:
            continue
        fpath = os.path.join(directory, fname)
        if os.path.isfile(fpath):
            return fpath
    return None


# ---------------------------------------------------------------------------
# Covit
# ---------------------------------------------------------------------------

# Artwork filenames that covit / Picard / Lidarr might create
ARTWORK_STEMS = {'folder', 'cover', 'front', 'albumart', 'album', 'art', 'thumb'}
IMAGE_EXTENSIONS = {'.jpg', '.jpeg', '.png', '.bmp', '.gif', '.webp', '.tiff', '.tif'}


def clean_existing_artwork(directory: str) -> list[str]:
    """Remove existing cover art files to prevent covit deduplication.

    Matches files like folder.jpg, folder (1).jpg, cover.png, etc.
    Returns list of removed file paths.
    """
    removed = []
    try:
        for fname in os.listdir(directory):
            fpath = os.path.join(directory, fname)
            if not os.path.isfile(fpath):
                continue
            name, ext = os.path.splitext(fname)
            if ext.lower() not in IMAGE_EXTENSIONS:
                continue
            # Match "folder", "folder (1)", "folder (2)", "cover", etc.
            stem = name.lower().split('(')[0].rstrip()
            if stem in ARTWORK_STEMS:
                try:
                    os.remove(fpath)
                    removed.append(fpath)
                except OSError as exc:
                    _log(f"WARNING: Could not remove {fname}: {exc}", error=True)
    except OSError as exc:
        _log(f"WARNING: Could not list {directory}: {exc}", error=True)

    if removed:
        _log(f"Cleaned {len(removed)} existing artwork file(s)")
    return removed


def run_covit(
    folderpath: str,
    albumartist: str | None = None,
    album: str | None = None,
) -> bool:
    """Run covit to fetch cover art. Returns True on success.

    Prefers `--input <audio file>` so covit reads the query from the tags
    itself (barcode / catalog / TOC included, and multi-value artist credits
    stay intact). Falls back to explicit --query-* only when an artist and
    album were supplied on the command line and no readable audio file is
    present. Everything is passed as an argv list, never a shell string, so
    quotes in a value reach covit verbatim.
    """
    # Every bail-out below must come BEFORE clean_existing_artwork: deleting the
    # user's existing folder.jpg and then discovering covit cannot run leaves the
    # album with no art at all, which is worse than doing nothing.
    if not os.path.isfile(COVIT_BIN):
        _log(f"WARNING: covit not found at {COVIT_BIN}, skipping cover art")
        return False

    query_file = find_query_file(folderpath)

    if query_file:
        query_args = ['--input', query_file]
    elif albumartist and album:
        _log("No readable audio file for the query, falling back to arguments")
        query_args = ['--query-artist', albumartist, '--query-album', album]
    else:
        _log(
            "WARNING: no audio file to query from and no artist/album "
            "arguments, skipping cover art",
            error=True,
        )
        return False

    # Remove existing artwork so covit doesn't deduplicate (folder (1).jpg, etc.)
    clean_existing_artwork(folderpath)

    cmd = [
        COVIT_BIN,
        '--address', COVIT_ADDRESS,
        *query_args,
        '--primary-output', os.path.join(folderpath, 'folder'),
    ]

    try:
        # shlex.join, not ' '.join: the log line stays copy-pasteable even when
        # the album title contains quotes or spaces.
        _log(f"covit: {shlex.join(cmd)}")
        result = subprocess.run(
            cmd, capture_output=True, text=True, timeout=120,
        )
        if result.returncode > 1:
            output = (result.stderr or '').strip() or (result.stdout or '').strip()
            _log(f"WARNING: covit exited {result.returncode}", error=True)
            if output:
                for line in output.splitlines()[:10]:
                    _log(f"  {line}", error=True)
            return False
        return True
    except subprocess.TimeoutExpired:
        _log("WARNING: covit timed out (>120s)", error=True)
        return False
    except Exception as exc:
        _log(f"WARNING: covit error: {exc}", error=True)
        return False


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def parse_args(argv: list[str] | None = None):
    p = argparse.ArgumentParser(
        description='Picard post-tagging action: ripcheck + covit.',
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog='Picard command (pass the folder ONLY -- see module docstring):\n'
               '  python3 /config/tools/picard_post_save.py "%folderpath%"\n'
               '  python3 /config/tools/picard_post_save.py -n 8 "%folderpath%"')
    p.add_argument('-n', '--threads', type=int, default=1, metavar='N',
                   help='Number of parallel workers for FLAC checking (default: 1)')
    p.add_argument('folderpath', help='Album directory')
    p.add_argument('albumartist', nargs='?', default=None,
                   help='Album artist (optional fallback; tags take precedence). '
                        'Do NOT pass this from Picard -- quotes get mangled.')
    p.add_argument('album', nargs='?', default=None,
                   help='Album name (optional fallback; tags take precedence). '
                        'Do NOT pass this from Picard -- quotes get mangled.')

    return p.parse_args(argv)


def main():
    args = parse_args()

    folderpath = args.folderpath
    albumartist = args.albumartist
    album = args.album
    threads = max(1, args.threads)

    if not os.path.isdir(folderpath):
        _log(f"Not a directory: {folderpath}", error=True)
        sys.exit(1)

    album_dir = os.path.abspath(folderpath)
    log_path = os.path.join(album_dir, LOG_FILENAME)

    # --- Fast path: already flagged by a previous run ---
    if os.path.isfile(log_path):
        if not LOG_ONLY:
            deleted = delete_audio_files(album_dir)
            if deleted:
                _log(f"Deleted {len(deleted)} file(s) (dir already flagged)", error=True)
        sys.exit(1)

    # =================================================================
    # STEP 1: Rip check
    # =================================================================
    flac_files = find_flacs(album_dir)

    if flac_files and preflight_flac():
        confirmed_corrupt: list[tuple[str, str]] = []
        tool_errors: list[tuple[str, str]] = []

        if threads == 1 or len(flac_files) == 1:
            for fpath in flac_files:
                status, detail = test_flac_stream(fpath)
                if status == StreamStatus.CORRUPT:
                    confirmed_corrupt.append((fpath, detail))
                elif status == StreamStatus.TOOL_ERROR:
                    tool_errors.append((fpath, detail))
        else:
            workers = min(threads, len(flac_files))
            _log(f"Checking {len(flac_files)} FLACs with {workers} workers")
            with ThreadPoolExecutor(max_workers=workers) as pool:
                futures = {
                    pool.submit(test_flac_stream, fpath): fpath
                    for fpath in flac_files
                }
                for future in as_completed(futures):
                    fpath = futures[future]
                    try:
                        status, detail = future.result()
                    except Exception as exc:
                        status = StreamStatus.TOOL_ERROR
                        detail = str(exc)
                    if status == StreamStatus.CORRUPT:
                        confirmed_corrupt.append((fpath, detail))
                    elif status == StreamStatus.TOOL_ERROR:
                        tool_errors.append((fpath, detail))

        # Tool errors: warn but NEVER delete
        if tool_errors:
            _log(f"WARNING: {len(tool_errors)} file(s) could not be tested (tool error, NOT corruption):", error=True)
            for fpath, detail in tool_errors:
                _log(f"  {os.path.basename(fpath)}: {detail}", error=True)

        # Confirmed corruption: log, delete, bail
        if confirmed_corrupt:
            for fpath, error in confirmed_corrupt:
                basename = os.path.basename(fpath)
                _log(f"CORRUPT RIP: {basename}", error=True)
                _log(f"  {error}", error=True)
                log_corruption(album_dir, fpath, error)

            _log(f"{len(confirmed_corrupt)}/{len(flac_files)} file(s) CONFIRMED corrupt", error=True)

            if not LOG_ONLY:
                deleted = delete_audio_files(album_dir)
                if deleted:
                    _log(f"Deleted {len(deleted)} audio file(s)", error=True)

            sys.exit(1)

        # All clean (or only tool errors, which we don't act on)
        # Report what was actually verified, not the file count: a run that
        # skipped files reads as a clean bill of health otherwise.
        verified = len(flac_files) - len(tool_errors)
        if tool_errors:
            _log(f"{verified}/{len(flac_files)} FLAC(s) verified clean, "
                 f"{len(tool_errors)} not checked")
        else:
            _log(f"All {verified} FLAC(s) verified clean")

    elif flac_files and not preflight_flac():
        _log("WARNING: flac not available, skipping rip check. Files left intact.")
    # else: no FLACs, nothing to check

    # =================================================================
    # STEP 2: Cover art (only reached if rip is clean)
    # =================================================================
    # Only reached on the clean path, which matters: covit reads its query from
    # one of these files, and on the corrupt path they have already been deleted.
    run_covit(album_dir, albumartist, album)

    sys.exit(0)


if __name__ == '__main__':
    main()
