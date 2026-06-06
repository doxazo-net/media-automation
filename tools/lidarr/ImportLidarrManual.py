#!/usr/bin/env python3
"""
Lidarr Manual Import Script

Works around two Lidarr limitations:

1. 100-file limit: When Lidarr can't determine the artist from the top-level
   folder name and finds >100 audio files, it skips parsing entirely. This
   script bypasses that by calling the manual import endpoint per album dir.

2. Missing albums: Lidarr can only match files to albums already in its
   library. The --prepare flag scans audio file tags for MusicBrainz IDs,
   finds albums missing from the library, and adds them (refreshing existing
   artists or optionally adding new artists with --add-artists).

Also handles folder artwork (folder.jpg, cover.png, etc.) by moving it
to the imported album's destination directory.

Includes quality gating: skips imports when existing library tracks are
higher quality (e.g., won't replace FLAC with MP3). Use --ignore-quality
to bypass, or --auto-quality for automatic resolution of ambiguous cases.

After successful imports, empty source directories are cleaned up.

Requirements: Python 3.6+ (stdlib only, no pip packages needed)

Configuration (checked in order):
    1. CLI args: --url and --api-key
    2. Config file: --config /path/to/LidarrConfig.json
    3. .env file: --env-file > ${XDG_CONFIG_HOME:-~/.config}/importlidarr/.env
       > next-to-script .env > ./.env  (LIDARR_URL and LIDARR_API_KEY)
    4. Environment variables: LIDARR_URL and LIDARR_API_KEY

Usage:
    # First-time bootstrap (deps, il symlink, .env, Unraid boot script):
    python3 ImportLidarrManual.py --setup

    # Simplest: put a .env file next to this script, then just:
    python3 ImportLidarrManual.py /share/Downloads/Finished/Manual/Music

    # Prepare (add missing albums) then import:
    python3 ImportLidarrManual.py /share/Downloads/Finished/Manual/Music --prepare

    # Only scan for missing albums, don't import:
    python3 ImportLidarrManual.py /share/Downloads/Finished/Manual/Music --prepare-only

    # Also add artists not yet in library:
    python3 ImportLidarrManual.py /share/Downloads/Finished/Manual/Music --prepare --add-artists
"""

from __future__ import annotations

import io
import os
import sys
import signal
import json
import time
import shlex
import shutil
import struct
import getpass
import hashlib
import tarfile
import zipfile
import tempfile
import subprocess
import logging
import argparse
import threading
import platform as _platform_mod
import urllib.parse
import urllib.request
import urllib.error
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import PurePosixPath
from typing import TYPE_CHECKING

# Graceful-shutdown flag.  The SIGINT handler sets this instead of hard-killing
# the process so that in-progress tag writes can complete without corruption.
# Workers and the main album loop check this before starting new work.
_shutdown = threading.Event()


def _sigint_handler(*_):
    if _shutdown.is_set():
        # Second CTRL-C: user is impatient — force-kill immediately.
        os._exit(130)
    _shutdown.set()
    # Print directly; the logging system may be mid-write on another thread.
    print("\nInterrupt received — finishing current files then stopping "
          "(CTRL-C again to force-quit)…", flush=True)


signal.signal(signal.SIGINT, _sigint_handler)

if TYPE_CHECKING:
    import essentia.standard as _es

try:
    import cv2
    import numpy as np
    _HAS_CV2 = True
except ImportError:
    _HAS_CV2 = False

try:
    import essentia
    essentia.log.infoActive = False
    essentia.log.warningActive = False
    import essentia.standard as _es
    _HAS_ESSENTIA = True
except ImportError:
    _HAS_ESSENTIA = False

try:
    from mutagen.flac import FLAC
    from mutagen.mp3 import MP3
    from mutagen.mp4 import MP4
    from mutagen.oggvorbis import OggVorbis
    from mutagen.oggopus import OggOpus
    from mutagen.wavpack import WavPack
    from mutagen.apev2 import APEv2File
    from mutagen.dsf import DSF
    from mutagen.id3._frames import TBPM, TXXX
    _HAS_MUTAGEN = True
except ImportError:
    _HAS_MUTAGEN = False

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

AUDIO_EXTENSIONS = {
    '.flac', '.mp3', '.m4a', '.ogg', '.opus', '.wma', '.wav',
    '.aac', '.alac', '.wv', '.ape', '.dsf', '.dff',
}

ARTWORK_NAMES = {'folder', 'cover', 'front', 'albumart', 'album', 'art', 'thumb'}
DISCART_NAMES = {'cd', 'cdart', 'discart', 'disc'}
ARTWORK_EXTENSIONS = {'.jpg', '.jpeg', '.png', '.bmp', '.gif', '.webp', '.tiff', '.tif'}

# Lossless image formats that should be converted to standard formats via ImageMagick
CONVERTIBLE_IMAGE_FORMATS = {'.tiff', '.tif', '.webp'}

# Video formats recognised as animated album art (folder.mp4, folder.webm, etc.)
# Only the "folder" basename is matched — these are converted to looping GIF.
ANIMATED_ART_EXTENSIONS = {'.mp4', '.webm', '.mov'}

LOSSLESS_CODECS = {'flac', 'alac', 'ape', 'pcm', 'wavpack', 'wav'}
LOSSY_CODECS = {'mp3', 'mp3cbr', 'mp3vbr', 'aac', 'aacvbr', 'ogg', 'opus', 'wma'}

# Junk files that don't prevent directory cleanup
JUNK_FILES = {'thumbs.db', '.ds_store', 'desktop.ini'}

API_TIMEOUT = 120  # seconds

RSGAIN_BIN = '/mnt/vms/utils/rsgain/rsgain'
BPMTAGGER_MARKER = 'bpmtag.py'

# Directory of the real script file (realpath, not abspath, so an invocation
# via the `il` symlink resolves to where sibling files such as `.env` and
# `bin/rsgain` actually live, not /usr/local/bin).
SCRIPT_DIR = os.path.dirname(os.path.realpath(__file__))

# A binary fetched by `--setup` lands here, next to the script on the
# persistent checkout, so it survives an Unraid reboot with no boot-script
# entry (resolved via realpath, see SCRIPT_DIR).
BUNDLED_RSGAIN = os.path.join(SCRIPT_DIR, 'bin', 'rsgain')

# Platforms whose home dir (e.g. Unraid's /root) is RAM-backed and wiped on
# reboot, so config and fetched binaries must live next to the script instead.
EPHEMERAL_HOME_PLATFORMS = {'unraid'}

# Statically-runnable binaries we can fetch when no package manager carries
# them (notably rsgain on Unraid). Each entry is keyed by '<os>-<arch>' as
# produced by _platform_arch_key(). sha256 is verified against the downloaded
# bytes before anything is written; `member` is the path inside the archive to
# extract (the official rsgain releases ship a .tar.xz/.zip, not a raw binary).
#
# Source: https://github.com/complexlogic/rsgain/releases/tag/v3.7
# The macOS builds link only against system frameworks; the Linux build
# dynamically links FFmpeg, which the dependency table already installs.
FETCHABLE_BINARIES = {
    'rsgain': {
        'linux-x86_64': {
            'url': 'https://github.com/complexlogic/rsgain/releases/download/'
                   'v3.7/rsgain-3.7-Linux.tar.xz',
            'sha256': '28c529f20b822df803ab1bde981ca3256cf58276e5e1d0b8e969faf017842151',
            'member': 'rsgain-3.7-Linux/rsgain',
        },
        'darwin-arm64': {
            'url': 'https://github.com/complexlogic/rsgain/releases/download/'
                   'v3.7/rsgain-3.7-macOS-arm64.zip',
            'sha256': '481f192354723c54e9605e3c9a9cf453b7cf87eed35b8e92fed5efdfa3ac4e86',
            'member': 'rsgain-3.7-macOS-arm64/rsgain',
        },
        'darwin-x86_64': {
            'url': 'https://github.com/complexlogic/rsgain/releases/download/'
                   'v3.7/rsgain-3.7-macOS-x86_64.zip',
            'sha256': '5e7f9655ff38ece783588b7210feae3a1eef1f9f41f3e9961c42f31cd53b4f17',
            'member': 'rsgain-3.7-macOS-x86_64/rsgain',
        },
    },
}


def resolve_rsgain():
    """Return the rsgain binary to execute, or None if unavailable.

    Resolution order: a bundled `bin/rsgain` fetched by `--setup` > the
    hardcoded RSGAIN_BIN (a manually-installed binary not on PATH) > PATH.
    Used by BOTH the dependency preflight and run_rsgain so detection and
    execution agree on the same binary instead of checking PATH but running
    a different one.
    """
    for candidate in (BUNDLED_RSGAIN, RSGAIN_BIN):
        if candidate and os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    return shutil.which('rsgain')

log = logging.getLogger('lidarr-import')

# ---------------------------------------------------------------------------
# Dependency preflight
# ---------------------------------------------------------------------------


def detect_platform(platform=None, unraid_marker='/etc/unraid-version',
                    kernel_release=None):
    """Return 'mac' | 'unraid' | 'linux' | 'unknown'.

    Unraid is detected by EITHER the /etc/unraid-version marker OR an
    '-Unraid'-suffixed kernel release (its kernels carry that suffix), so a
    booted-but-marker-missing array still resolves correctly. Args are
    injectable for testing; defaults read the real environment, guarded by
    hasattr(os, 'uname') so the module stays importable on non-POSIX hosts.
    """
    plat = platform if platform is not None else sys.platform
    if plat == 'darwin':
        return 'mac'
    if kernel_release is None and hasattr(os, 'uname'):
        kernel_release = os.uname().release
    if os.path.exists(unraid_marker) or (
            kernel_release and 'unraid' in kernel_release.lower()):
        return 'unraid'
    if plat.startswith('linux'):
        return 'linux'
    return 'unknown'


class Dependency:
    """One optional tool the script can use.

    check()        -> True when the tool is already present.
    needed_when(args, scan) -> True when THIS run will use it.
    packages       -> {manager: package-name} for 'pip' / 'brew' / 'un-get'.
    optional       -> reported for information only; never auto-installed.
    """

    def __init__(self, name, kind, check, enables, packages,
                 needed_when=lambda args, scan: False, optional=False):
        self.name = name
        self.kind = kind            # 'pip' | 'binary'
        self.check = check          # () -> bool
        self.enables = enables
        self.packages = packages    # {'pip'|'brew'|'un-get': 'pkg'}
        self.needed_when = needed_when
        self.optional = optional


def _have(binary):
    return shutil.which(binary) is not None


def build_dependencies():
    """Return the static dependency table."""
    return [
        Dependency(
            'mutagen', 'pip', lambda: _HAS_MUTAGEN,
            'BPM tagging and audio tag reads',
            {'pip': 'mutagen'},
            needed_when=lambda args, scan: not args.skip_bpm,
        ),
        Dependency(
            'essentia', 'pip', lambda: _HAS_ESSENTIA,
            'BPM detection',
            {'pip': 'essentia'},
            needed_when=lambda args, scan: not args.skip_bpm,
        ),
        Dependency(
            'cv2', 'pip', lambda: _HAS_CV2,
            'higher-quality disc-art cropping (ImageMagick fallback exists)',
            {'pip': 'opencv-python-headless'},
            optional=True,
        ),
        Dependency(
            'magick', 'binary', lambda: _have('magick'),
            'TIFF/WEBP artwork conversion',
            {'brew': 'imagemagick', 'un-get': 'imagemagick'},
            needed_when=lambda args, scan: scan['convertible'],
        ),
        Dependency(
            'ffmpeg', 'binary', lambda: _have('ffmpeg'),
            'animated artwork (folder.mp4) to looping GIF',
            {'brew': 'ffmpeg', 'un-get': 'ffmpeg'},
            needed_when=lambda args, scan: scan['animated'],
        ),
        Dependency(
            'rsgain', 'binary', lambda: resolve_rsgain() is not None,
            'ReplayGain tagging',
            {'brew': 'rsgain', 'un-get': 'rsgain'},
            needed_when=lambda args, scan: not args.no_rsgain,
        ),
        Dependency(
            'git', 'binary', lambda: _have('git'),
            'repo checkout and manual updates',
            {'brew': 'git', 'un-get': 'git'},
            optional=True,
        ),
    ]


def scan_artwork_kinds(album_dirs):
    """Scan album dirs once for facts that decide which tools this run needs.

    Returns {'convertible': bool, 'animated': bool}.
    """
    result = {'convertible': False, 'animated': False}
    for directory in album_dirs:
        try:
            entries = os.listdir(directory)
        except OSError:
            continue
        for name in entries:
            base, ext = os.path.splitext(name)
            ext = ext.lower()
            if ext in CONVERTIBLE_IMAGE_FORMATS:
                result['convertible'] = True
            if ext in ANIMATED_ART_EXTENSIONS and base.lower() == 'folder':
                result['animated'] = True
        if result['convertible'] and result['animated']:
            break
    return result


def needed_dependencies(deps, args, scan):
    """Missing, non-optional deps that THIS run will actually use."""
    return [d for d in deps
            if not d.optional and not d.check() and d.needed_when(args, scan)]


def optional_missing_dependencies(deps):
    """Missing optional deps - reported for info, never auto-installed."""
    return [d for d in deps if d.optional and not d.check()]


def setup_pip_packages(deps):
    """Pip packages that `--setup` installs and boot-persists.

    Includes optional pip deps (e.g. cv2 / opencv-python-headless) on purpose:
    `--setup` is a full bootstrap, and anything it installs must also land in
    the Unraid boot script so a RAM-wiped reboot reinstalls it. Returning the
    one list both call sites use keeps install and boot-persist in lockstep.
    """
    return sorted(d.packages['pip'] for d in deps if d.kind == 'pip')


def install_command_for(dep, platform):
    """Return the argv list to install dep on platform, or None if unsupported.

    pip always uses the current interpreter so it lands where imports resolve.
    Binaries route to brew (mac) / un-get (unraid); other platforms print-only.
    """
    if dep.kind == 'pip':
        return [sys.executable, '-m', 'pip', 'install', dep.packages['pip']]
    if platform == 'mac':
        return ['brew', 'install', dep.packages.get('brew', dep.name)]
    if platform == 'unraid':
        return ['un-get', 'install', dep.packages.get('un-get', dep.name)]
    return None


def install_dependency(dep, platform):
    """Install dep; return True on success. Never raises - failure is logged."""
    cmd = install_command_for(dep, platform)
    if cmd is None:
        log.info("  No package manager configured for %s on %s - install manually",
                 dep.name, platform)
        return False
    printable = ' '.join(cmd)
    log.info("  Installing %s: %s", dep.name, printable)
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=600)
    except FileNotFoundError:
        log.warning("  %s not found - install %s manually: %s",
                    cmd[0], dep.name, printable)
        return False
    except subprocess.TimeoutExpired:
        log.warning("  install of %s timed out", dep.name)
        return False
    if result.returncode != 0:
        log.warning("  install of %s failed: %s",
                    dep.name, (result.stderr or '').strip())
        if platform == 'unraid' and dep.kind == 'binary':
            log.warning("  un-get may not carry %s - install via NerdTools or "
                        "another plugin", dep.name)
        return False
    return True


def _verify_sha256(data, expected_sha256):
    """True when sha256(data) matches expected (case-insensitive hex). Pure."""
    return hashlib.sha256(data).hexdigest() == expected_sha256.strip().lower()


def _extract_member(data, url, member):
    """Return one member's bytes from an in-memory archive, or None.

    Archive type is inferred from the URL suffix. Supports .zip and tar
    variants (.tar.xz etc.); tarfile's 'r:*' auto-detects the compression.
    """
    lower = url.lower()
    if lower.endswith('.zip'):
        with zipfile.ZipFile(io.BytesIO(data)) as zf:
            return zf.read(member)
    if '.tar' in lower:
        with tarfile.open(fileobj=io.BytesIO(data), mode='r:*') as tf:
            extracted = tf.extractfile(member)
            return extracted.read() if extracted is not None else None
    return None


def download_binary(name, url, sha256, dest_dir, member=None):
    """Fetch a binary to dest_dir/name, verifying sha256 first. Non-fatal.

    Downloads to memory, verifies the checksum against the raw bytes BEFORE
    touching disk, then (for archives) extracts `member` and writes it
    atomically with the executable bit set. Returns the destination path on
    success or False on any download/checksum/extraction failure.
    """
    try:
        with urllib.request.urlopen(url, timeout=120) as resp:  # noqa: S310
            data = resp.read()
    except Exception as exc:
        log.warning("  Could not download %s from %s: %s", name, url, exc)
        return False
    if not _verify_sha256(data, sha256):
        log.error("  Checksum mismatch for %s - discarding download "
                  "(expected %s, got %s, %d bytes)",
                  name, sha256.strip().lower(), hashlib.sha256(data).hexdigest(),
                  len(data))
        return False
    try:
        payload = _extract_member(data, url, member) if member else data
    except (tarfile.TarError, zipfile.BadZipFile, KeyError) as exc:
        log.error("  Could not extract %s from %s archive: %s", member, name, exc)
        return False
    if payload is None:
        log.error("  Archive for %s did not contain %s", name, member)
        return False
    dest = os.path.join(dest_dir, name)
    try:
        os.makedirs(dest_dir, exist_ok=True)
        fd, tmp = tempfile.mkstemp(dir=dest_dir)
        try:
            with os.fdopen(fd, 'wb') as f:
                f.write(payload)
            os.chmod(tmp, 0o755)
            os.replace(tmp, dest)
        except OSError:
            # Don't leave a half-written temp file in the persistent bin/ dir.
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise
    except OSError as exc:
        log.warning("  Could not install fetched %s: %s", name, exc)
        return False
    return dest


def _platform_arch_key():
    """'<os>-<arch>' key into FETCHABLE_BINARIES, e.g. 'linux-x86_64'."""
    os_part = 'darwin' if sys.platform == 'darwin' else 'linux'
    return '%s-%s' % (os_part, _platform_mod.machine())


def fetch_binary_dependency(name):
    """Fetch a binary dep from FETCHABLE_BINARIES into SCRIPT_DIR/bin. Non-fatal.

    Returns the installed path, or False when there is no fetchable build for
    this platform/arch or the download/verify fails.
    """
    entry = FETCHABLE_BINARIES.get(name)
    if not entry:
        return False
    key = _platform_arch_key()
    spec = entry.get(key)
    if not spec:
        log.info("  No fetchable %s build for %s", name, key)
        return False
    return download_binary(name, spec['url'], spec['sha256'],
                           os.path.join(SCRIPT_DIR, 'bin'),
                           member=spec.get('member'))


def write_idempotent_block(path, marker, body):
    """Insert or replace a fenced block in a file, identified by marker.

    Re-running with the same marker replaces the prior block rather than
    appending a duplicate. Creates the file if absent.
    """
    start = "# >>> %s >>>" % marker
    end = "# <<< %s <<<" % marker
    block = "%s\n%s\n%s" % (start, body, end)
    try:
        with open(path, 'r') as f:
            content = f.read()
    except FileNotFoundError:
        content = ''
    if start in content and end in content and content.index(start) < content.index(end):
        i = content.index(start)
        j = content.index(end) + len(end)
        new_content = content[:i] + block + content[j:]
    else:
        prefix = content if (content == '' or content.endswith('\n')) else content + '\n'
        new_content = prefix + block + '\n'
    with open(path, 'w') as f:
        f.write(new_content)


USER_SCRIPTS_DIR = '/boot/config/plugins/user.scripts/scripts'
GO_FILE = '/boot/config/go'
BOOT_MARKER = 'ImportLidarrManual boot setup'
IL_LINK_PATH = '/usr/local/bin/il'


def boot_script_body(pip_packages, script_path):
    """Lines a boot script runs: reinstall wiped pip libs, recreate il symlink."""
    lines = []
    if pip_packages:
        lines.append('python3 -m pip install ' + ' '.join(sorted(pip_packages)))
    lines.append('ln -sf %s %s' % (shlex.quote(script_path), shlex.quote(IL_LINK_PATH)))
    return '\n'.join(lines)


def write_boot_persistence(pip_packages, script_path,
                           user_scripts_dir=USER_SCRIPTS_DIR, go_file=GO_FILE):
    """Make pip libs + il symlink survive an Unraid reboot.

    Prefer the User Scripts plugin dir; fall back to the go file. Returns the
    path written.
    """
    body = boot_script_body(pip_packages, script_path)
    if os.path.isdir(user_scripts_dir):
        dest_dir = os.path.join(user_scripts_dir, 'importlidarr-boot')
        os.makedirs(dest_dir, exist_ok=True)
        dest = os.path.join(dest_dir, 'script')
        content = ("#!/bin/bash\n"
                   "#name=importlidarr-boot\n"
                   "#description=Reinstall ImportLidarrManual pip deps and il symlink\n"
                   "\n" + body + "\n")
        # This file is fully managed by smpr: rewritten verbatim each run.
        # (Unlike the go-file path, which only replaces its fenced block.)
        with open(dest, 'w') as f:
            f.write(content)
        os.chmod(dest, 0o755)
        log.info("  Wrote User Scripts entry: %s", dest)
        log.info("  One-time: set it to 'At Startup of Array' in the Unraid "
                 "User Scripts UI.")
        return dest
    write_idempotent_block(go_file, BOOT_MARKER, body)
    log.info("  Updated %s with a boot block (pip deps + il symlink)", go_file)
    return go_file


def ensure_il_symlink(script_path, link_path=IL_LINK_PATH):
    """Point link_path at script_path. Idempotent; never raises."""
    try:
        if os.path.islink(link_path) or os.path.exists(link_path):
            if os.path.realpath(link_path) == os.path.realpath(script_path):
                return True
            os.remove(link_path)
        os.symlink(script_path, link_path)
        log.info("  Linked %s -> %s", link_path, script_path)
        return True
    except OSError as exc:
        log.warning("  Could not create %s -> %s: %s", link_path, script_path, exc)
        return False


def preflight_dependencies(args, album_dirs, interactive=None):
    """Report missing tools this run needs and offer to install them.

    Non-fatal throughout: declining leaves a feature disabled and the run
    continues. On Unraid, after reinstalling wiped pip libs, offers to write a
    reboot-survivable boot script and the `il` symlink.
    """
    if getattr(args, 'no_preflight', False):
        return

    platform = detect_platform()
    scan = scan_artwork_kinds(album_dirs)
    deps = build_dependencies()
    needed = needed_dependencies(deps, args, scan)
    optional_missing = optional_missing_dependencies(deps)

    if not needed and not optional_missing:
        log.debug("All dependencies present")
        return

    log.info("")
    log.info("=" * 60)
    log.info("DEPENDENCY PREFLIGHT (platform: %s)", platform)
    log.info("=" * 60)

    if interactive is None:
        interactive = sys.stdin.isatty()
    installed_pip = []

    for dep in needed:
        cmd = install_command_for(dep, platform)
        log.info("Missing: %s - enables %s", dep.name, dep.enables)
        if cmd is None:
            log.info("  Install %s manually on this platform (no package manager configured).",
                     dep.name)
            continue
        log.info("  Install: %s", ' '.join(cmd))
        if not interactive:
            log.info("  Non-interactive run; run the command above to install.")
            continue
        try:
            answer = input("  Install %s now? [y/N] " % dep.name).strip().lower()
        except EOFError:
            answer = 'n'
        if answer in ('y', 'yes'):
            if install_dependency(dep, platform):
                log.info("  Installed %s", dep.name)
                if dep.kind == 'pip':
                    installed_pip.append(dep.packages['pip'])
            else:
                log.warning("  Could not install %s - continuing without it", dep.name)
        else:
            log.info("  Skipped %s (%s disabled)", dep.name, dep.enables)

    for dep in optional_missing:
        log.info("Optional (absent): %s - would enable %s", dep.name, dep.enables)

    if installed_pip and platform == 'unraid' and interactive:
        try:
            answer = input("  Make reinstalled pip libs survive reboot "
                           "(write boot script + il symlink)? [y/N] ").strip().lower()
        except EOFError:
            answer = 'n'
        if answer in ('y', 'yes'):
            script_path = os.path.realpath(__file__)
            write_boot_persistence(installed_pip, script_path)
            ensure_il_symlink(script_path)

    log.info("=" * 60)


def run_setup(args):
    """Stand up persistence independently of any import run.

    Installs the non-optional dependencies (fetching a verified binary when no
    package manager carries it), scaffolds `.env`, creates the `il` symlink,
    and on Unraid writes the reboot-survivable boot script. Idempotent and
    non-fatal: an individual failure is logged and the rest still runs. Unlike
    the import-time preflight, the symlink and boot persistence are NOT gated
    on whether pip packages were just installed.
    """
    interactive = sys.stdin.isatty()
    platform = detect_platform()
    script_path = os.path.realpath(__file__)

    log.info("=" * 60)
    log.info("SETUP (platform: %s)", platform)
    log.info("=" * 60)

    deps = build_dependencies()
    # --setup is a full bootstrap: install ALL deps, including optional ones
    # (cv2 for higher-quality disc-art cropping, git). pip deps a reboot would
    # wipe - including cv2 - go to the boot script via setup_pip_packages so
    # install and boot-persist stay in lockstep.
    pip_packages = setup_pip_packages(deps)

    for dep in deps:
        if dep.check():
            log.info("Present: %s", dep.name)
            continue
        log.info("Missing: %s - enables %s", dep.name, dep.enables)
        if install_dependency(dep, platform):
            log.info("  Installed %s", dep.name)
            continue
        if dep.kind == 'binary':
            fetched = fetch_binary_dependency(dep.name)
            if fetched:
                log.info("  Fetched %s -> %s", dep.name, fetched)
            else:
                log.warning("  Could not install or fetch %s - continuing without it",
                            dep.name)
        else:
            log.warning("  Could not install %s - continuing without it", dep.name)

    env_path = os.path.join(get_config_dir(platform), '.env')
    scaffold_env_file(env_path, args, interactive)

    # Symlink and boot persistence run unconditionally (decoupled from the
    # pip-install gate that the import-time preflight uses).
    ensure_il_symlink(script_path)
    if platform == 'unraid':
        write_boot_persistence(pip_packages, script_path)
    else:
        log.info("Boot persistence: skipped (only needed on Unraid)")

    log.info("Setup complete. Run `il <music-dir>` (or "
             "`python3 %s <music-dir>`) to import.", script_path)
    log.info("=" * 60)


# ---------------------------------------------------------------------------
# Lidarr API client
# ---------------------------------------------------------------------------

class LidarrClient:
    def __init__(self, url: str, api_key: str):
        self.base_url = url.rstrip('/')
        self.api_key = api_key

    def _request(self, method: str, endpoint: str, data=None, params=None,
                 timeout=API_TIMEOUT):
        url = f"{self.base_url}/api/v1/{endpoint}"
        if params:
            url += '?' + urllib.parse.urlencode(params)

        headers = {
            'X-Api-Key': self.api_key,
            'Content-Type': 'application/json',
            'Accept': 'application/json',
        }

        body = json.dumps(data).encode('utf-8') if data is not None else None
        req = urllib.request.Request(url, data=body, headers=headers, method=method)

        max_retries = 3
        for attempt in range(max_retries):
            try:
                with urllib.request.urlopen(req, timeout=timeout) as resp:
                    raw = resp.read().decode('utf-8')
                    return json.loads(raw) if raw else None
            except urllib.error.HTTPError as exc:
                if exc.code in (502, 503, 504) and attempt < max_retries - 1:
                    wait = 10 * (attempt + 1)
                    log.warning("API %s %s -> %s (retrying in %ds, attempt "
                                "%d/%d)", method, endpoint, exc.code, wait,
                                attempt + 1, max_retries)
                    time.sleep(wait)
                    # Rebuild request (consumed by previous attempt)
                    body = json.dumps(data).encode('utf-8') if data is not None else None
                    req = urllib.request.Request(url, data=body,
                                                headers=headers, method=method)
                    continue
                error_body = ''
                try:
                    error_body = exc.read().decode('utf-8', errors='replace')[:500]
                except Exception:
                    pass
                log.error("API %s %s -> %s %s  %s", method, endpoint, exc.code,
                          exc.reason, error_body)
                raise
            except (urllib.error.URLError, TimeoutError, OSError) as exc:
                if attempt < max_retries - 1:
                    wait = 10 * (attempt + 1)
                    reason = getattr(exc, 'reason', str(exc))
                    log.warning("API %s %s -> %s (retrying in %ds, attempt "
                                "%d/%d)", method, endpoint, reason, wait,
                                attempt + 1, max_retries)
                    time.sleep(wait)
                    body = json.dumps(data).encode('utf-8') if data is not None else None
                    req = urllib.request.Request(url, data=body,
                                                headers=headers, method=method)
                    continue
                log.error("API %s %s -> connection error: %s", method, endpoint,
                          getattr(exc, 'reason', str(exc)))
                raise

    def get(self, endpoint, params=None, **kw):
        return self._request('GET', endpoint, params=params, **kw)

    def post(self, endpoint, data, **kw):
        return self._request('POST', endpoint, data=data, **kw)

    def put(self, endpoint, data, **kw):
        return self._request('PUT', endpoint, data=data, **kw)

    def delete(self, endpoint, params=None, **kw):
        return self._request('DELETE', endpoint, params=params, **kw)

    # --- Convenience wrappers ---

    def system_status(self):
        return self.get('system/status')

    def manual_import_preview(self, folder: str, filter_existing=True,
                              replace_existing=True, artist_id: int = 0):
        """GET /api/v1/manualimport  -- scan a folder and return file info."""
        params: dict[str, str | int] = {
            'folder': folder,
            'filterExistingFiles': str(filter_existing).lower(),
            'replaceExistingFiles': str(replace_existing).lower(),
        }
        if artist_id:
            params['artistId'] = artist_id
        return self.get('manualimport', params=params)

    def manual_import_execute(self, items: list, import_mode: str = 'move',
                              replace_existing: bool = True):
        """Queue a ManualImport command to actually move/copy the files."""
        return self.run_command('ManualImport',
                                files=items,
                                importMode=import_mode,
                                replaceExistingFiles=replace_existing)

    def artist_lookup(self, term: str):
        return self.get('artist/lookup', params={'term': term})

    def get_all_artists(self):
        """Get all artists in the Lidarr library."""
        return self.get('artist')

    def get_artist(self, artist_id: int):
        return self.get(f'artist/{artist_id}')

    def get_album(self, album_id: int):
        return self.get(f'album/{album_id}')

    def get_track_files(self, album_id: int):
        return self.get('trackfile', params={'albumId': album_id})

    def album_lookup(self, term: str):
        """Look up an album by name or lidarr:MBID."""
        return self.get('album/lookup', params={'term': term})

    def get_albums(self, artist_id: int | None = None):
        """Get albums, optionally filtered by artist."""
        params = {'artistId': artist_id} if artist_id is not None else None
        return self.get('album', params=params)

    def add_artist(self, artist_data: dict):
        """Add a new artist to the library."""
        return self.post('artist', data=artist_data)

    def update_album(self, album_id: int, album_data: dict):
        """Update an album (e.g., set monitored)."""
        return self.put(f'album/{album_id}', data=album_data)

    def get_command(self, command_id: int):
        """Get command status."""
        return self.get(f'command/{command_id}')

    def run_command(self, name: str, **kwargs):
        """Run a Lidarr command (e.g., RefreshArtist)."""
        payload = {'name': name}
        payload.update(kwargs)
        return self.post('command', data=payload)

# ---------------------------------------------------------------------------
# Filesystem helpers
# ---------------------------------------------------------------------------

def remap_path(local_path: str, local_prefix: str, remote_prefix: str) -> str:
    """Remap a local (host) path to a remote (Docker container) path."""
    if local_prefix and remote_prefix and local_path.startswith(local_prefix):
        return remote_prefix + local_path[len(local_prefix):]
    return local_path


def find_album_dirs(root: str) -> list[str]:
    """Return directories that directly contain audio files (album-level dirs)."""
    album_dirs = set()
    for dirpath, _dirnames, filenames in os.walk(root):
        for f in filenames:
            if os.path.splitext(f)[1].lower() in AUDIO_EXTENSIONS:
                album_dirs.add(dirpath)
                break  # one audio file is enough to qualify
    return sorted(album_dirs)


def find_artwork(directory: str) -> list[str]:
    """Find artwork files in a directory (folder.jpg, cover.png, etc.)."""
    artwork = []
    try:
        for f in os.listdir(directory):
            name, ext = os.path.splitext(f)
            if (ext.lower() in ARTWORK_EXTENSIONS
                    and name.lower() in ARTWORK_NAMES):
                artwork.append(os.path.join(directory, f))
    except OSError:
        pass
    return artwork


def find_animated_artwork(directory: str) -> str | None:
    """Find a folder.mp4 / folder.webm / folder.mov in *directory*.

    Only the ``folder`` basename is matched — animated art from other names
    (cover, front, etc.) is not expected in the wild.  Returns the first
    match or None.
    """
    try:
        for f in os.listdir(directory):
            name, ext = os.path.splitext(f)
            if name.lower() == 'folder' and ext.lower() in ANIMATED_ART_EXTENSIONS:
                return os.path.join(directory, f)
    except OSError:
        pass
    return None


def count_audio_files(directory: str) -> int:
    """Count total audio files recursively under a directory."""
    count = 0
    for _dirpath, _dirnames, filenames in os.walk(directory):
        for f in filenames:
            if os.path.splitext(f)[1].lower() in AUDIO_EXTENSIONS:
                count += 1
    return count


def get_image_dimensions(filepath: str) -> tuple[int, int] | None:
    """
    Read image dimensions from file headers without external libraries.
    Supports JPEG, PNG, GIF, BMP, and WebP.
    Returns (width, height) or None if dimensions cannot be determined.
    """
    try:
        with open(filepath, 'rb') as f:
            header = f.read(32)
            if len(header) < 8:
                return None

            # PNG: 8-byte signature, then IHDR chunk with width/height at bytes 16-24
            if header[:8] == b'\x89PNG\r\n\x1a\n':
                w, h = struct.unpack('>II', header[16:24])
                return (w, h)

            # GIF: 'GIF87a' or 'GIF89a', width/height as little-endian at bytes 6-10
            if header[:6] in (b'GIF87a', b'GIF89a'):
                w, h = struct.unpack('<HH', header[6:10])
                return (w, h)

            # BMP: 'BM' signature, width/height as little-endian int32 at bytes 18-26
            if header[:2] == b'BM' and len(header) >= 26:
                w, h = struct.unpack('<ii', header[18:26])
                return (abs(w), abs(h))

            # TIFF: 'II' (little-endian) or 'MM' (big-endian), then magic 42
            if header[:2] in (b'II', b'MM'):
                bo = '>' if header[:2] == b'MM' else '<'
                magic = struct.unpack(f'{bo}H', header[2:4])[0]
                if magic == 42:
                    ifd_off = struct.unpack(f'{bo}I', header[4:8])[0]
                    f.seek(ifd_off)
                    raw = f.read(2)
                    if len(raw) == 2:
                        num = struct.unpack(f'{bo}H', raw)[0]
                        w = h = None
                        for _ in range(min(num, 256)):
                            entry = f.read(12)
                            if len(entry) < 12:
                                break
                            tag = struct.unpack(f'{bo}H', entry[0:2])[0]
                            typ = struct.unpack(f'{bo}H', entry[2:4])[0]
                            if typ == 3:  # SHORT
                                val = struct.unpack(f'{bo}H', entry[8:10])[0]
                            elif typ == 4:  # LONG
                                val = struct.unpack(f'{bo}I', entry[8:12])[0]
                            else:
                                continue
                            if tag == 256:
                                w = val
                            elif tag == 257:
                                h = val
                            if w is not None and h is not None:
                                return (w, h)

            # WebP: 'RIFF....WEBP', then VP8 chunk
            if header[:4] == b'RIFF' and header[8:12] == b'WEBP':
                f.seek(12)
                while True:
                    chunk_hdr = f.read(8)
                    if len(chunk_hdr) < 8:
                        break
                    fourcc = chunk_hdr[:4]
                    chunk_size = struct.unpack('<I', chunk_hdr[4:8])[0]
                    if fourcc == b'VP8 ' and chunk_size >= 10:
                        data = f.read(10)
                        if len(data) >= 10 and data[3:6] == b'\x9d\x01\x2a':
                            w = struct.unpack('<H', data[6:8])[0] & 0x3FFF
                            h = struct.unpack('<H', data[8:10])[0] & 0x3FFF
                            return (w, h)
                        break
                    elif fourcc == b'VP8L' and chunk_size >= 5:
                        data = f.read(5)
                        if len(data) >= 5 and data[0:1] == b'\x2f':
                            bits = struct.unpack('<I', data[1:5])[0]
                            w = (bits & 0x3FFF) + 1
                            h = ((bits >> 14) & 0x3FFF) + 1
                            return (w, h)
                        break
                    else:
                        f.seek(chunk_size + (chunk_size % 2), 1)

            # JPEG: scan for SOF markers
            if header[:2] == b'\xff\xd8':
                f.seek(2)
                while True:
                    marker = f.read(2)
                    if len(marker) < 2 or marker[0:1] != b'\xff':
                        break
                    marker_type = marker[1]
                    # SOF markers: 0xC0-0xC3, 0xC5-0xC7, 0xC9-0xCB, 0xCD-0xCF
                    if marker_type in (0xC0, 0xC1, 0xC2, 0xC3,
                                       0xC5, 0xC6, 0xC7,
                                       0xC9, 0xCA, 0xCB,
                                       0xCD, 0xCE, 0xCF):
                        sof = f.read(7)
                        if len(sof) >= 7:
                            h, w = struct.unpack('>HH', sof[3:7])
                            return (w, h)
                        break
                    # Skip non-SOF segments
                    length_data = f.read(2)
                    if len(length_data) < 2:
                        break
                    seg_len = struct.unpack('>H', length_data)[0]
                    if seg_len < 2:
                        break
                    f.seek(seg_len - 2, 1)

    except OSError:
        pass
    return None


def should_replace_artwork(src: str, dest: str) -> tuple[bool, str]:
    """
    Decide whether source artwork should replace an existing destination file.

    Rules:
      1. If destination doesn't exist -> replace (new file)
      2. If source has larger dimensions (pixel area) -> replace
      3. If same dimensions but source has larger filesize -> replace
      4. Otherwise -> keep existing

    Returns (should_replace, reason).
    """
    if not os.path.exists(dest):
        return True, 'new file'

    src_dims = get_image_dimensions(src)
    dest_dims = get_image_dimensions(dest)
    src_size = os.path.getsize(src)
    dest_size = os.path.getsize(dest)

    if src_dims and dest_dims:
        src_area = src_dims[0] * src_dims[1]
        dest_area = dest_dims[0] * dest_dims[1]
        if src_area > dest_area:
            return True, (f'larger dimensions: {src_dims[0]}x{src_dims[1]} '
                          f'vs {dest_dims[0]}x{dest_dims[1]}')
        if src_area < dest_area:
            return False, (f'existing is larger: {dest_dims[0]}x{dest_dims[1]} '
                           f'vs {src_dims[0]}x{src_dims[1]}')
        # Same dimensions - compare filesize
        if src_size > dest_size:
            return True, (f'same dimensions ({src_dims[0]}x{src_dims[1]}), '
                          f'larger file: {src_size} vs {dest_size} bytes')
        if src_size == dest_size:
            return False, (f'identical dimensions and size '
                           f'({src_dims[0]}x{src_dims[1]}, {src_size} bytes)')
        return False, (f'same dimensions ({dest_dims[0]}x{dest_dims[1]}), '
                       f'existing file larger: {dest_size} vs {src_size} bytes')

    # Couldn't read dimensions for one or both - fall back to filesize only
    if src_size > dest_size:
        return True, f'larger file: {src_size} vs {dest_size} bytes (dimensions unknown)'
    if src_size == dest_size:
        return False, f'same filesize: {src_size} bytes (dimensions unknown)'
    return False, f'existing file larger: {dest_size} vs {src_size} bytes (dimensions unknown)'


def convert_image(src: str, dest: str) -> bool:
    """Convert image using ImageMagick 7.x (magick command). Returns True on success."""
    try:
        result = subprocess.run(
            ['magick', src, dest],
            capture_output=True, timeout=30,
        )
        if result.returncode != 0:
            stderr = result.stderr.decode(errors='replace').strip()
            if stderr:
                log.warning("  ImageMagick error: %s", stderr)
            return False
        return True
    except FileNotFoundError:
        log.warning("  ImageMagick not found (install ImageMagick 7.x for format conversion)")
        return False
    except subprocess.TimeoutExpired:
        log.warning("  ImageMagick conversion timed out")
        return False


def convert_video_to_gif(src: str, dest: str, fps: int = 15) -> bool:
    """Convert a video file to a looping GIF using ffmpeg's two-pass palette method.

    Pass 1: ``palettegen`` analyses the whole video to build an optimal
    256-colour palette.  Pass 2: ``paletteuse`` re-encodes with that palette
    for much better colour fidelity than a single-pass conversion.

    The output keeps the source resolution, runs at *fps* frames per second,
    uses the full original duration, and loops infinitely.

    Returns True on success.  On any failure (ffmpeg missing, unsupported
    format, timeout) returns False — the caller should treat this as
    "animated art not available" and skip rather than fall back to copying
    the source video.
    """
    import tempfile

    palette = None
    try:
        # Palette lives in a tempfile so concurrent conversions don't collide.
        fd, palette = tempfile.mkstemp(suffix='.png', prefix='gifpal_')
        os.close(fd)

        vf_palette = f'fps={fps},palettegen=stats_mode=full'
        vf_use = f'fps={fps},paletteuse=dither=sierra2_4a'

        # Pass 1: generate palette
        r1 = subprocess.run(
            ['ffmpeg', '-y', '-i', src,
             '-vf', vf_palette, palette],
            capture_output=True, timeout=120,
        )
        if r1.returncode != 0:
            stderr = r1.stderr.decode(errors='replace').strip()
            log.warning("  ffmpeg palette generation failed for %s: %s",
                        os.path.basename(src), stderr[-300:] if stderr else '(no output)')
            return False

        # Pass 2: encode GIF using the palette
        r2 = subprocess.run(
            ['ffmpeg', '-y', '-i', src, '-i', palette,
             '-lavfi', vf_use,
             '-loop', '0',  # 0 = loop forever
             dest],
            capture_output=True, timeout=300,
        )
        if r2.returncode != 0:
            stderr = r2.stderr.decode(errors='replace').strip()
            log.warning("  ffmpeg GIF encoding failed for %s: %s",
                        os.path.basename(src), stderr[-300:] if stderr else '(no output)')
            # Clean up partial output
            if os.path.exists(dest):
                os.remove(dest)
            return False

        return True

    except FileNotFoundError:
        log.warning("  ffmpeg not found — cannot convert animated artwork "
                    "(install ffmpeg for folder.mp4 → folder.gif conversion)")
        return False
    except subprocess.TimeoutExpired:
        log.warning("  ffmpeg timed out converting %s", os.path.basename(src))
        if os.path.exists(dest):
            os.remove(dest)
        return False
    finally:
        if palette and os.path.exists(palette):
            os.remove(palette)


def find_discart(directory: str) -> list[str]:
    """Find disc art files in a directory (cd.jpg, cdart.png, discart.jpg, etc.)."""
    discart = []
    try:
        for f in os.listdir(directory):
            name, ext = os.path.splitext(f)
            if (ext.lower() in ARTWORK_EXTENSIONS
                    and name.lower() in DISCART_NAMES):
                discart.append(os.path.join(directory, f))
    except OSError:
        pass
    return discart


def detect_disc_circle(img_path: str) -> tuple[int, int, int] | None:
    """
    Detect the largest circle in an image using OpenCV HoughCircles.
    Returns (center_x, center_y, radius) or None if no circle found.
    Requires opencv-python-headless.
    """
    if not _HAS_CV2:
        return None

    img = cv2.imread(img_path)
    if img is None:
        return None

    gray = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
    gray = cv2.GaussianBlur(gray, (9, 9), 2)
    h, w = gray.shape
    min_dim = min(h, w)

    # Disc should be a substantial portion of the image
    min_radius = min_dim // 6
    max_radius = max(h, w) // 2

    # Try with moderate sensitivity first, then relax if nothing found
    for param2 in (30, 15):
        circles = cv2.HoughCircles(
            gray, cv2.HOUGH_GRADIENT,
            dp=1.2,
            minDist=min_dim // 4,
            param1=100,
            param2=param2,
            minRadius=min_radius,
            maxRadius=max_radius,
        )
        if circles is not None:
            best = max(circles[0], key=lambda c: c[2])
            return int(best[0]), int(best[1]), int(best[2])

    return None


def _crop_disc_cv2(src: str, dest: str) -> bool:
    """
    Crop disc art to a circle with transparent background using OpenCV.
    Detects the disc circle automatically; falls back to center crop if
    detection fails. Returns True on success.
    """
    img = cv2.imread(src, cv2.IMREAD_UNCHANGED)
    if img is None:
        return False

    h, w = img.shape[:2]
    circle = detect_disc_circle(src)

    if circle:
        cx, cy, r = circle
        log.debug("  Detected disc circle: center=(%d,%d) radius=%d", cx, cy, r)
    else:
        cx, cy = w // 2, h // 2
        r = min(w, h) // 2
        log.debug("  No circle detected, using center crop")

    # Convert to BGRA for transparency
    if len(img.shape) == 2:
        img = cv2.cvtColor(img, cv2.COLOR_GRAY2BGRA)
    elif img.shape[2] == 3:
        img = cv2.cvtColor(img, cv2.COLOR_BGR2BGRA)

    # Create square output canvas (2r x 2r), fully transparent
    size = 2 * r
    output = np.zeros((size, size, 4), dtype=np.uint8)

    # Copy source pixels into canvas, handling edge clipping
    src_x1, src_y1 = max(cx - r, 0), max(cy - r, 0)
    src_x2, src_y2 = min(cx + r, w), min(cy + r, h)
    dst_x1, dst_y1 = src_x1 - (cx - r), src_y1 - (cy - r)
    dst_x2 = dst_x1 + (src_x2 - src_x1)
    dst_y2 = dst_y1 + (src_y2 - src_y1)
    output[dst_y1:dst_y2, dst_x1:dst_x2] = img[src_y1:src_y2, src_x1:src_x2]

    # Apply circular mask to alpha channel
    mask = np.zeros((size, size), dtype=np.uint8)
    cv2.circle(mask, (r, r), r, 255, -1)
    output[:, :, 3] = cv2.bitwise_and(output[:, :, 3], mask)

    return cv2.imwrite(dest, output)


def _crop_disc_magick(src: str, dest: str) -> bool:
    """Fallback: crop disc art using ImageMagick naive center-crop."""
    try:
        result = subprocess.run(
            ['magick', src, '-alpha', 'set',
             '(', '+clone', '-alpha', 'transparent',
             '-fill', 'white',
             '-draw', 'circle %[fx:w/2],%[fx:h/2] %[fx:w/2],0',
             ')', '-compose', 'DstIn', '-composite', dest],
            capture_output=True, timeout=60,
        )
        if result.returncode != 0:
            stderr = result.stderr.decode(errors='replace').strip()
            if stderr:
                log.warning("  ImageMagick disc art error: %s", stderr)
            return False
        return True
    except FileNotFoundError:
        log.warning("  ImageMagick not found (install ImageMagick 7.x for disc art conversion)")
        return False
    except subprocess.TimeoutExpired:
        log.warning("  ImageMagick disc art conversion timed out")
        return False


def convert_discart(src: str, dest: str) -> bool:
    """
    Crop disc art to a circle with transparent background.
    Uses OpenCV for circle detection when available, falls back to
    ImageMagick naive center-crop.
    Output is always PNG (transparency requires it).
    Returns True on success.
    """
    if _HAS_CV2:
        return _crop_disc_cv2(src, dest)
    return _crop_disc_magick(src, dest)


def find_matching_dest_artwork(dest_dir: str, base_name: str) -> str | None:
    """Find existing artwork in dest_dir matching base_name with any image extension."""
    # Prefer standard formats first
    for ext in ('.jpg', '.jpeg', '.png'):
        candidate = os.path.join(dest_dir, base_name + ext)
        if os.path.exists(candidate):
            return candidate
    # Then any image format
    for ext in ARTWORK_EXTENSIONS:
        candidate = os.path.join(dest_dir, base_name + ext)
        if os.path.exists(candidate):
            return candidate
    return None


def cleanup_empty_dir(directory: str, import_root: str, dry_run: bool = False) -> int:
    """
    Remove a directory if empty (or only junk files), then walk up
    removing empty parents up to (but not including) import_root.

    Returns the number of directories removed.
    """
    import_root = os.path.normpath(os.path.abspath(import_root))
    directory = os.path.normpath(os.path.abspath(directory))
    removed = 0

    current = directory
    while current != import_root and current.startswith(import_root):
        try:
            entries = os.listdir(current)
        except OSError:
            break

        # Filter out junk files
        real_entries = [e for e in entries if e.lower() not in JUNK_FILES]

        if real_entries:
            break  # Directory has real files, stop

        if dry_run:
            log.info("  Would remove empty directory: %s",
                     os.path.relpath(current, import_root))
            removed += 1
        else:
            # Delete junk files first
            for junk in entries:
                try:
                    os.remove(os.path.join(current, junk))
                except OSError:
                    pass
            try:
                os.rmdir(current)
                log.info("  Removed empty directory: %s",
                         os.path.relpath(current, import_root))
                removed += 1
            except OSError:
                break

        # Move up to parent
        current = os.path.dirname(current)

    return removed


# ---------------------------------------------------------------------------
# Quality comparison helpers
# ---------------------------------------------------------------------------

def _safe_int(value) -> int:
    """Convert a value to int, returning 0 on failure."""
    if value is None:
        return 0
    try:
        return int(value)
    except (ValueError, TypeError):
        return 0


def _extract_quality_info(quality_obj: dict, audio_tags: dict | None = None,
                          media_info: dict | None = None) -> dict:
    """
    Extract normalized quality info from Lidarr API objects.

    Works with both manual import items (quality + audioTags) and
    track file objects (quality + mediaInfo).

    Returns dict with keys: codec, lossless, bit_depth, sample_rate
    """
    codec = ''
    bit_depth = 0
    sample_rate = 0

    # Get codec name from quality object
    q = quality_obj.get('quality', {}) if quality_obj else {}
    codec = (q.get('name') or '').lower().replace('-', '').replace(' ', '')

    # Determine lossless from codec name
    lossless = any(lc in codec for lc in LOSSLESS_CODECS)

    # Try to get bit depth and sample rate from audioTags.mediaInfo first
    if audio_tags:
        tag_mi = audio_tags.get('mediaInfo') or {}
        bit_depth = _safe_int(tag_mi.get('audioBits') or tag_mi.get('audioBitDepth'))
        sample_rate = _safe_int(tag_mi.get('audioSampleRate'))

    # Fall back to top-level mediaInfo (track files use this format)
    if media_info and not (bit_depth and sample_rate):
        # mediaInfo.audioBits is formatted as "16bit" or "24bit"
        bits_str = media_info.get('audioBits') or ''
        bit_depth = bit_depth or _safe_int(bits_str.replace('bit', '').strip())
        # mediaInfo.audioSampleRate is formatted as "44.1kHz" or "96kHz"
        sr_str = media_info.get('audioSampleRate') or ''
        sr_str = sr_str.lower().replace('khz', '').strip()
        if sr_str and not sample_rate:
            try:
                sample_rate = int(float(sr_str) * 1000)
            except (ValueError, TypeError):
                pass
        # Also check audioCodec for lossless determination
        mi_codec = (media_info.get('audioCodec') or '').lower()
        if mi_codec and not lossless:
            lossless = any(lc in mi_codec for lc in LOSSLESS_CODECS)

    return {
        'codec': codec,
        'lossless': lossless,
        'bit_depth': bit_depth,
        'sample_rate': sample_rate,
    }


def _quality_tier(info: dict) -> int:
    """
    Assign a quality tier:
      0 = lossy
      1 = lossless 16-bit (or unknown bit depth)
      2 = lossless 24-bit+
    """
    if not info['lossless']:
        return 0
    if info['bit_depth'] >= 24:
        return 2
    return 1


def compare_quality(incoming: dict, existing: dict,
                    auto_score: bool = False) -> tuple[str, str]:
    """
    Compare incoming vs existing quality info.

    Returns (decision, reason) where decision is one of:
      'import'  - incoming is better, proceed
      'skip'    - existing is better, skip
      'prompt'  - ambiguous, ask user
      'equal'   - same quality, skip
    """
    inc_tier = _quality_tier(incoming)
    ext_tier = _quality_tier(existing)

    tier_names = {0: 'lossy', 1: 'lossless 16-bit', 2: 'lossless 24-bit'}

    if inc_tier > ext_tier:
        return ('import',
                f'upgrade: {tier_names.get(inc_tier, "?")} '
                f'over {tier_names.get(ext_tier, "?")}')

    if inc_tier < ext_tier:
        return ('skip',
                f'existing is {tier_names.get(ext_tier, "?")} '
                f'vs incoming {tier_names.get(inc_tier, "?")}')

    # Same tier - compare bit depth and sample rate
    inc_bd = incoming['bit_depth']
    inc_sr = incoming['sample_rate']
    ext_bd = existing['bit_depth']
    ext_sr = existing['sample_rate']

    bd_better = inc_bd > ext_bd
    bd_worse = inc_bd < ext_bd
    sr_better = inc_sr > ext_sr
    sr_worse = inc_sr < ext_sr

    detail = (f'incoming: {inc_bd}bit/{inc_sr}Hz, '
              f'existing: {ext_bd}bit/{ext_sr}Hz')

    # Both metrics better or equal
    if (bd_better or inc_bd == ext_bd) and (sr_better or inc_sr == ext_sr):
        if not bd_better and not sr_better:
            return ('equal', f'same quality ({detail})')
        return ('import', f'higher quality: {detail}')

    # Both metrics worse or equal
    if (bd_worse or inc_bd == ext_bd) and (sr_worse or inc_sr == ext_sr):
        return ('skip', f'existing is higher quality: {detail}')

    # Conflicting - one metric better, other worse
    if auto_score:
        inc_score = inc_bd * inc_sr if inc_bd and inc_sr else 0
        ext_score = ext_bd * ext_sr if ext_bd and ext_sr else 0
        if inc_score > ext_score:
            return ('import', f'higher combined score: {detail} '
                    f'({inc_score:,} vs {ext_score:,})')
        if inc_score < ext_score:
            return ('skip', f'existing has higher combined score: {detail} '
                    f'({ext_score:,} vs {inc_score:,})')
        return ('equal', f'same combined score: {detail}')

    return ('prompt', detail)


# ---------------------------------------------------------------------------
# Prepare (pre-flight) logic
# ---------------------------------------------------------------------------


def _tag_value(tags: dict, *keys) -> str:
    """Extract a value from audioTags, trying multiple possible key names."""
    for key in keys:
        val = tags.get(key)
        if val:
            return str(val)
    return ''


def wait_for_command(client: LidarrClient, command_id: int,
                     timeout: float = 60, poll: float = 2.0) -> str:
    """Poll a Lidarr command until it completes or times out."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            cmd = client.get_command(command_id)
            if not cmd:
                continue
            status = (cmd.get('status') or '').lower()
            if status in ('completed', 'failed', 'aborted'):
                return status
        except Exception:
            pass
        time.sleep(poll)
    return 'timeout'


def prepare_library(client: LidarrClient, album_dirs: list[str],
                    import_root: str,
                    local_prefix: str = '', remote_prefix: str = '',
                    add_artists: bool = False, dry_run: bool = False) -> int:
    """
    Scan import folders for albums missing from Lidarr's library and add them
    so that manual import can match tracks to the correct albums.

    Returns the number of failures.
    """
    log.info("Scanning library...")
    all_artists = client.get_all_artists() or []
    artist_by_mbid: dict[str, dict] = {}
    for a in all_artists:
        mbid = a.get('foreignArtistId', '')
        if mbid:
            artist_by_mbid[mbid] = a

    all_albums = client.get_albums() or []
    album_mbids: set[str] = set()
    for alb in all_albums:
        mbid = alb.get('foreignAlbumId', '')
        if mbid:
            album_mbids.add(mbid)

    log.info("Library: %d artists, %d albums", len(artist_by_mbid), len(album_mbids))

    # Default profile settings from first existing artist (used when adding new artists)
    default_root = ''
    default_quality_profile = 0
    default_metadata_profile = 0
    if all_artists:
        ref = all_artists[0]
        default_root = ref.get('rootFolderPath', '')
        default_quality_profile = ref.get('qualityProfileId', 0)
        default_metadata_profile = ref.get('metadataProfileId', 0)

    # Scan album dirs for MBIDs not in library
    log.info("Scanning %d album folders for missing MBIDs...", len(album_dirs))
    missing: dict[str, dict] = {}

    for album_dir in album_dirs:
        rel = os.path.relpath(album_dir, import_root)
        api_path = remap_path(album_dir, local_prefix, remote_prefix)

        try:
            items = client.manual_import_preview(api_path)
        except Exception as exc:
            log.debug("Could not scan %s: %s", rel, exc)
            continue

        if not items:
            continue

        for item in items:
            if item.get('additionalFile', False):
                continue
            tags = item.get('audioTags') or {}
            rg_mbid = _tag_value(tags, 'releaseGroupMBId', 'releaseGroupMbId',
                                 'releaseGroupMBid', 'releaseGroupMbid')
            artist_mbid = _tag_value(tags, 'artistMBId', 'artistMbId',
                                     'artistMBid', 'artistMbid')

            # Fallback: if no release group MBID, try albumMBId (release
            # MBID) and resolve to release group via Lidarr lookup
            if not rg_mbid:
                album_release_mbid = _tag_value(tags, 'albumMBId',
                                                'albumMbId', 'albumMBid')
                if album_release_mbid:
                    try:
                        lookup = client.get(
                            'album/lookup',
                            params={'term': f'lidarr:{album_release_mbid}'})
                        if lookup and isinstance(lookup, list):
                            rg_mbid = lookup[0].get('foreignAlbumId')
                            log.debug("Resolved release %s -> RG %s",
                                      album_release_mbid, rg_mbid)
                    except Exception:
                        pass

            if not rg_mbid or rg_mbid in album_mbids or rg_mbid in missing:
                continue

            missing[rg_mbid] = {
                'album_mbid': rg_mbid,
                'artist_mbid': artist_mbid,
                'album_title': _tag_value(tags, 'albumTitle', 'album') or '?',
                'artist_name': _tag_value(tags, 'artistTitle', 'artist') or '?',
                'source_dir': rel,
            }
            break  # only need MBID from first audio track per directory

    if not missing:
        log.info("All albums already in library")
        return 0

    log.info("")
    log.info("Found %d album(s) missing from library:", len(missing))
    for info in missing.values():
        log.info("  %s - %s  [%s]", info['artist_name'], info['album_title'],
                 info['album_mbid'])

    added = 0
    skipped = 0
    failed = 0

    for album_mbid, info in missing.items():
        artist_mbid = info['artist_mbid']
        artist_name = info['artist_name']
        album_title = info['album_title']

        log.info("")
        log.info("Processing: %s - %s", artist_name, album_title)

        artist_in_lib = artist_by_mbid.get(artist_mbid) if artist_mbid else None

        if artist_in_lib:
            # Artist exists but album missing - refresh artist metadata
            artist_id = artist_in_lib['id']
            log.info("  Artist in library (id %d), refreshing metadata...", artist_id)

            if dry_run:
                log.info("  DRY RUN - would refresh artist and monitor album")
                added += 1
                continue

            try:
                cmd = client.run_command('RefreshArtist', artistId=artist_id)
                cmd_id = (cmd or {}).get('id', 0)
                if cmd_id:
                    status = wait_for_command(client, cmd_id)
                    log.debug("  Refresh command %d: %s", cmd_id, status)
                else:
                    time.sleep(5)

                # Check if album appeared after refresh
                artist_albums = client.get_albums(artist_id=artist_id) or []
                target = None
                for alb in artist_albums:
                    if alb.get('foreignAlbumId') == album_mbid:
                        target = alb
                        break

                if target:
                    if not target.get('monitored', False):
                        target['monitored'] = True
                        client.update_album(target['id'], target)
                        log.info("  Monitored album: %s", album_title)
                    else:
                        log.info("  Album found after refresh: %s", album_title)
                    added += 1
                    album_mbids.add(album_mbid)
                else:
                    log.warning("  Album not found after artist refresh: %s",
                                album_mbid)
                    failed += 1
            except Exception as exc:
                log.warning("  Refresh failed: %s", exc)
                failed += 1

        elif add_artists:
            # Artist not in library - add them
            if not artist_mbid:
                log.warning("  No artist MBID in tags, cannot add")
                failed += 1
                continue

            log.info("  Adding artist to library...")

            if dry_run:
                log.info("  DRY RUN - would add artist and monitor album")
                added += 1
                continue

            try:
                lookup = client.artist_lookup(f'lidarr:{artist_mbid}')
                if not lookup:
                    log.warning("  Artist not found: %s", artist_mbid)
                    failed += 1
                    continue

                artist_data = lookup[0] if isinstance(lookup, list) else lookup
                artist_data['rootFolderPath'] = default_root
                artist_data['qualityProfileId'] = default_quality_profile
                artist_data['metadataProfileId'] = default_metadata_profile
                artist_data['monitored'] = True
                artist_data['monitorNewItems'] = 'none'
                artist_data['addOptions'] = {
                    'monitor': 'none',
                    'searchForMissingAlbums': False,
                }

                result = client.add_artist(artist_data)
                if not result:
                    log.warning("  Empty response from add_artist")
                    failed += 1
                    continue

                new_id = result.get('id', 0)
                log.info("  Added artist: %s (id %d)",
                         result.get('artistName', artist_name), new_id)

                # Monitor just the specific album we need
                time.sleep(3)
                artist_albums = client.get_albums(artist_id=new_id) or []
                for alb in artist_albums:
                    if alb.get('foreignAlbumId') == album_mbid:
                        if not alb.get('monitored', False):
                            alb['monitored'] = True
                            client.update_album(alb['id'], alb)
                            log.info("  Monitored album: %s", album_title)
                        break

                added += 1
                artist_by_mbid[artist_mbid] = result
                album_mbids.add(album_mbid)

            except urllib.error.HTTPError as exc:
                if exc.code == 400:
                    log.warning("  Artist may already exist or bad request")
                else:
                    log.warning("  Failed to add artist: %s %s",
                                exc.code, exc.reason)
                failed += 1
            except Exception as exc:
                log.warning("  Failed to add artist: %s", exc)
                failed += 1

        else:
            log.info("  Artist not in library (use --add-artists to add)")
            skipped += 1

    log.info("")
    log.info("Prepare: %d added, %d skipped, %d failed", added, skipped, failed)
    return failed


# ---------------------------------------------------------------------------
# Import logic
# ---------------------------------------------------------------------------

def build_import_update(item: dict) -> dict | None:
    """
    Convert a ManualImportResource (from GET) to a ManualImportUpdateResource
    (for POST).  Returns None if the item lacks required artist/album info.
    """
    artist = item.get('artist')
    album = item.get('album')
    if not artist or not album:
        return None

    track_ids = []
    tracks = item.get('tracks') or []
    for t in tracks:
        tid = t.get('id')
        if tid:
            track_ids.append(tid)

    return {
        'path': item['path'],
        'name': item.get('name', ''),
        'artistId': artist['id'],
        'albumId': album['id'],
        'albumReleaseId': item.get('albumReleaseId', 0),
        'trackIds': track_ids,
        'tracks': tracks,
        'quality': item.get('quality'),
        'releaseGroup': item.get('releaseGroup', ''),
        'indexerFlags': item.get('indexerFlags', 0),
        'downloadId': item.get('downloadId', ''),
        'additionalFile': item.get('additionalFile', False),
        'replaceExistingFiles': item.get('replaceExistingFiles', True),
        'disableReleaseSwitching': item.get('disableReleaseSwitching', False),
        'rejections': [],
    }


def resolve_artwork_destination(client: LidarrClient, album_id: int,
                                retries: int = 3, delay: float = 2.0) -> str | None:
    """
    After an import, find where the album's track files ended up so we
    know where to put artwork.  Retries because Lidarr may still be
    processing the import.
    """
    for attempt in range(retries):
        try:
            track_files = client.get_track_files(album_id)
            if track_files:
                # All tracks for one album should be in the same directory
                first_path = track_files[0].get('path', '')
                if first_path:
                    # Use PurePosixPath since Lidarr runs in Linux/Docker
                    return str(PurePosixPath(first_path).parent)
        except Exception:
            pass
        if attempt < retries - 1:
            time.sleep(delay)
    return None


def guess_artist_from_path(album_dir: str) -> str | None:
    """
    Guess the artist name from the folder structure.
    Expects: .../ArtistName/AlbumName/tracks  -> returns ArtistName
    """
    parts = album_dir.replace('\\', '/').rstrip('/').split('/')
    # The album dir is the last component, artist is one level up
    if len(parts) >= 2:
        return parts[-2]
    return None


def find_artist_in_library(client: LidarrClient, artist_name: str,
                           artist_cache: dict[str, int]) -> int | None:
    """
    Find an artist ID in the Lidarr library by name.
    Uses a cache to avoid repeated API calls.
    """
    # Build cache on first call
    if not artist_cache:
        try:
            all_artists = client.get_all_artists() or []
            for a in all_artists:
                name = (a.get('artistName') or '').lower()
                if name:
                    artist_cache[name] = a['id']
        except Exception as exc:
            log.warning("  Could not fetch artist library: %s", exc)
            return None

    return artist_cache.get(artist_name.lower())


def run_rsgain(import_path: str, dry_run: bool = False) -> bool:
    """Run rsgain on the entire import tree to apply ReplayGain tags.

    Returns True on success, False on failure.
    """
    binary = resolve_rsgain()
    if binary is None:
        log.error("rsgain binary not found (looked at %s and PATH)", RSGAIN_BIN)
        return False
    cmd = [binary, 'easy', '-l', '-18', '-S', '-m', 'MAX', import_path]
    if dry_run:
        log.info("[dry run] Would run: %s", ' '.join(cmd))
        return True
    log.info("Running rsgain on import path...")
    log.debug("Command: %s", ' '.join(cmd))
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=1800)
        if result.returncode != 0:
            log.warning("rsgain failed (exit %d): %s",
                        result.returncode, result.stderr.strip())
            return False
        if result.stdout.strip():
            log.debug("rsgain output: %s", result.stdout.strip())
        log.info("rsgain completed successfully")
        return True
    except FileNotFoundError:
        log.error("rsgain binary not found at %s", binary)
        return False
    except subprocess.TimeoutExpired:
        log.warning("rsgain timed out after 1800s")
        return False
    except Exception as exc:
        log.warning("rsgain error: %s", exc)
        return False


def detect_bpm(file_path: str) -> tuple[float, float] | None:
    """Detect BPM using essentia's RhythmExtractor2013 (multifeature method).

    Returns (bpm, confidence) or None on failure.
    """
    if not _HAS_ESSENTIA:
        log.warning("essentia not installed — skipping BPM detection")
        return None

    try:
        audio = _es.MonoLoader(filename=file_path, sampleRate=44100)()
        rhythm = _es.RhythmExtractor2013(method='multifeature')
        bpm, _, confidence, _, _ = rhythm(audio)
        return round(float(bpm), 1), round(float(confidence), 3)
    except Exception as exc:
        log.warning("  BPM detection failed for %s: %s",
                    os.path.basename(file_path), exc)
        return None


def _has_bpm_marker(file_path: str) -> bool:
    """Check whether this tool already tagged the file (via BPMTAGGER marker)."""
    if not _HAS_MUTAGEN:
        return False
    ext = os.path.splitext(file_path)[1].lower()
    try:
        if ext == '.flac':
            return bool(FLAC(file_path).get('bpmtagger'))
        if ext == '.mp3':
            f = MP3(file_path)
            return f.tags is not None and any(
                k == 'TXXX:BPMTAGGER' for k in f.tags)
        if ext in ('.m4a', '.aac', '.alac'):
            f = MP4(file_path)
            return bool(f.tags and f.tags.get('----:com.bpmtag:tagger'))
        if ext == '.ogg':
            return bool(OggVorbis(file_path).get('bpmtagger'))
        if ext == '.opus':
            return bool(OggOpus(file_path).get('bpmtagger'))
        if ext == '.wv':
            return bool(WavPack(file_path).get('bpmtagger'))
        if ext in ('.ape',):
            return bool(APEv2File(file_path).get('bpmtagger'))
        if ext in ('.dsf', '.dff'):
            f = DSF(file_path)
            return f.tags is not None and any(
                k == 'TXXX:BPMTAGGER' for k in f.tags)
    except Exception:
        pass
    return False


def _write_bpm_tag(file_path: str, bpm: float) -> bool:
    """Write a BPM tag to an audio file using mutagen.

    Returns True on success.
    """
    ext = os.path.splitext(file_path)[1].lower()
    bpm_str = str(int(round(bpm)))
    try:
        if ext == '.flac':
            f = FLAC(file_path)
            f['bpm'] = bpm_str
            f['bpmtagger'] = BPMTAGGER_MARKER
            f.save()
            return True
        if ext == '.mp3':
            f = MP3(file_path)
            if f.tags is None:
                f.add_tags()
            assert f.tags is not None
            f.tags.add(TBPM(encoding=3, text=[bpm_str]))
            f.tags.add(TXXX(encoding=3, desc='BPMTAGGER', text=[BPMTAGGER_MARKER]))
            f.save()
            return True
        if ext in ('.m4a', '.aac', '.alac'):
            f = MP4(file_path)
            if f.tags is None:
                f.add_tags()
            assert f.tags is not None
            f.tags['tmpo'] = [int(round(bpm))]
            f.tags['----:com.bpmtag:tagger'] = [BPMTAGGER_MARKER.encode()]
            f.save()
            return True
        if ext == '.ogg':
            f = OggVorbis(file_path)
            f['bpm'] = bpm_str
            f['bpmtagger'] = BPMTAGGER_MARKER
            f.save()
            return True
        if ext == '.opus':
            f = OggOpus(file_path)
            f['bpm'] = bpm_str
            f['bpmtagger'] = BPMTAGGER_MARKER
            f.save()
            return True
        if ext == '.wv':
            f = WavPack(file_path)
            f['bpm'] = bpm_str
            f['bpmtagger'] = BPMTAGGER_MARKER
            f.save()
            return True
        if ext in ('.ape',):
            f = APEv2File(file_path)
            f['bpm'] = bpm_str
            f['bpmtagger'] = BPMTAGGER_MARKER
            f.save()
            return True
        if ext in ('.dsf', '.dff'):
            f = DSF(file_path)
            if f.tags is None:
                f.add_tags()
            assert f.tags is not None
            f.tags.add(TBPM(encoding=3, text=[bpm_str]))
            f.tags.add(TXXX(encoding=3, desc='BPMTAGGER', text=[BPMTAGGER_MARKER]))
            f.save()
            return True
        log.debug("  BPM tagging not supported for %s files", ext)
        return False
    except Exception as exc:
        log.warning("  Failed to write BPM tag to %s: %s",
                    os.path.basename(file_path), exc)
        return False


def _write_bpm_marker(file_path: str) -> bool:
    """Write only the BPMTAGGER marker (no BPM value) to mark a file as evaluated."""
    ext = os.path.splitext(file_path)[1].lower()
    try:
        if ext == '.flac':
            f = FLAC(file_path)
            f['bpmtagger'] = BPMTAGGER_MARKER
            f.save()
            return True
        if ext == '.mp3':
            f = MP3(file_path)
            if f.tags is None:
                f.add_tags()
            assert f.tags is not None
            f.tags.add(TXXX(encoding=3, desc='BPMTAGGER', text=[BPMTAGGER_MARKER]))
            f.save()
            return True
        if ext in ('.m4a', '.aac', '.alac'):
            f = MP4(file_path)
            if f.tags is None:
                f.add_tags()
            assert f.tags is not None
            f.tags['----:com.bpmtag:tagger'] = [BPMTAGGER_MARKER.encode()]
            f.save()
            return True
        if ext == '.ogg':
            f = OggVorbis(file_path)
            f['bpmtagger'] = BPMTAGGER_MARKER
            f.save()
            return True
        if ext == '.opus':
            f = OggOpus(file_path)
            f['bpmtagger'] = BPMTAGGER_MARKER
            f.save()
            return True
        if ext == '.wv':
            f = WavPack(file_path)
            f['bpmtagger'] = BPMTAGGER_MARKER
            f.save()
            return True
        if ext in ('.ape',):
            f = APEv2File(file_path)
            f['bpmtagger'] = BPMTAGGER_MARKER
            f.save()
            return True
        if ext in ('.dsf', '.dff'):
            f = DSF(file_path)
            if f.tags is None:
                f.add_tags()
            assert f.tags is not None
            f.tags.add(TXXX(encoding=3, desc='BPMTAGGER', text=[BPMTAGGER_MARKER]))
            f.save()
            return True
    except Exception as exc:
        log.warning("  Failed to write BPM marker to %s: %s",
                    os.path.basename(file_path), exc)
    return False


def _process_single_bpm_file(fpath: str, fname: str,
                              force_bpm: bool,
                              min_confidence: float) -> str:
    """Detect and write BPM for one audio file.

    Returns one of: 'tagged', 'skipped', 'marker_only', 'low_confidence',
    'write_failed', or 'error'.  All logging happens inside so the caller
    only needs to aggregate the returned status strings.

    Each call is fully self-contained (reads its own file, writes its own
    tags) so multiple calls can run concurrently without shared state.
    Mutagen opens, modifies, and saves each file independently; there is no
    cross-file locking needed as long as no two workers touch the same path.
    """
    try:
        # Check before doing any I/O so a pending shutdown never starts a new
        # file.  We do NOT check mid-write; a write that has already begun is
        # always allowed to finish to avoid corrupting the tag block.
        if _shutdown.is_set():
            return 'cancelled'

        if not force_bpm and _has_bpm_marker(fpath):
            log.debug("  BPM already evaluated: %s", fname)
            return 'skipped'

        result = detect_bpm(fpath)
        if result is None:
            _write_bpm_marker(fpath)
            return 'marker_only'

        bpm, confidence = result
        if confidence < min_confidence:
            log.info("  BPM skip %s: confidence %.3f < %.3f (tempo unstable)",
                     fname, confidence, min_confidence)
            _write_bpm_marker(fpath)
            return 'low_confidence'

        if _write_bpm_tag(fpath, bpm):
            log.info("  BPM %s -> %d (confidence %.3f)", fname,
                     int(round(bpm)), confidence)
            return 'tagged'

        log.warning("  BPM detected %.1f but could not write tag: %s",
                    bpm, fname)
        return 'write_failed'

    except Exception as exc:  # noqa: BLE001 — worker must not crash the pool
        log.warning("  BPM unexpected error for %s: %s", fname, exc)
        return 'error'


def run_bpm_tagging(dest_dir: str, dry_run: bool = False,
                    force_bpm: bool = False,
                    min_confidence: float = 0.5,
                    max_workers: int | None = None) -> None:
    """Detect BPM for all audio files in dest_dir and write tags.

    Files are processed in parallel using a ThreadPoolExecutor.  essentia's
    RhythmExtractor2013 is a C++ DSP routine that releases the GIL, so
    threads achieve real CPU concurrency for the heavy analysis step while
    keeping memory overhead much lower than a ProcessPoolExecutor (no inter-
    process serialisation or separate Python interpreters).

    max_workers defaults to min(cpu_count, 4) to avoid loading too many large
    audio files into RAM at once on high-core-count machines.
    """
    if not _HAS_MUTAGEN:
        log.warning("  mutagen not installed — skipping BPM tagging")
        return

    try:
        entries = os.listdir(dest_dir)
    except OSError as exc:
        log.warning("  Cannot list destination for BPM tagging: %s", exc)
        return

    audio_files = sorted(
        f for f in entries
        if os.path.splitext(f)[1].lower() in AUDIO_EXTENSIONS
    )
    if not audio_files:
        return

    # Dry-run: nothing to parallelise — just log and count.
    if dry_run:
        for fname in audio_files:
            fpath = os.path.join(dest_dir, fname)
            if not force_bpm and _has_bpm_marker(fpath):
                continue
            log.info("  [dry run] Would detect and tag BPM: %s", fname)
        return

    # Default: cap at 4 to avoid saturating RAM with large decoded audio arrays
    # (essentia loads the entire file as a float32 array).  Caller can override
    # via --bpm-workers.
    if max_workers is None:
        max_workers = min(os.cpu_count() or 1, 4)

    tagged = 0
    skipped = 0
    cancelled = 0

    # Submit one future per file; collect results as they complete so that a
    # slow file does not delay logging from faster ones.
    with ThreadPoolExecutor(max_workers=max_workers) as pool:
        future_to_name = {
            pool.submit(
                _process_single_bpm_file,
                os.path.join(dest_dir, fname),
                fname,
                force_bpm,
                min_confidence,
            ): fname
            for fname in audio_files
        }

        for future in as_completed(future_to_name):
            status = future.result()  # exceptions are caught inside the worker
            if status == 'tagged':
                tagged += 1
            elif status == 'skipped':
                skipped += 1
            elif status == 'cancelled':
                cancelled += 1

    if tagged:
        log.info("  BPM tagged %d file(s)", tagged)
    if skipped:
        log.debug("  BPM skipped %d file(s) (already evaluated)", skipped)
    if cancelled:
        log.info("  BPM cancelled %d file(s) (shutdown requested)", cancelled)


def process_album_dir(client: LidarrClient, album_dir: str,
                      copy_art: bool, dry_run: bool,
                      local_prefix: str = '', remote_prefix: str = '',
                      artist_cache: dict[str, int] | None = None,
                      force: bool = False,
                      confirm_artwork: bool = False,
                      ignore_quality: bool = False,
                      auto_quality: bool = False,
                      delete_equal: bool = False,
                      skip_bpm: bool = False,
                      force_bpm: bool = False,
                      bpm_confidence: float = 0.5,
                      bpm_workers: int | None = None) -> str:
    """
    Import a single album directory.

    Returns: 'success', 'skipped', 'failed', 'dry_run', 'quality_skip', or 'deleted'.
    """
    if artist_cache is None:
        artist_cache = {}

    # Step 1: Get Lidarr's assessment of the folder
    api_path = remap_path(album_dir, local_prefix, remote_prefix)
    try:
        items = client.manual_import_preview(api_path)
    except Exception as exc:
        log.warning("  API error scanning folder: %s", exc)
        return 'failed'

    if not items:
        log.info("  No importable files found")
        return 'skipped'

    # Separate audio items from additional files
    audio_items = [it for it in items if not it.get('additionalFile', False)]
    if not audio_items:
        log.info("  No audio files found (%d additional files only)", len(items))
        return 'skipped'

    # Step 2: Check identification status
    identified = [it for it in audio_items
                  if it.get('artist') and it.get('album')]
    unidentified = [it for it in audio_items
                    if not it.get('artist') or not it.get('album')]

    # Step 2b: If nothing identified, retry with artist hint from folder name
    if not identified:
        artist_name = guess_artist_from_path(api_path)
        if artist_name:
            artist_id = find_artist_in_library(client, artist_name, artist_cache)
            if artist_id:
                log.info("  Retrying with artist hint: %s (id %d)", artist_name, artist_id)
                try:
                    items = client.manual_import_preview(api_path, artist_id=artist_id)
                    audio_items = [it for it in (items or [])
                                   if not it.get('additionalFile', False)]
                    identified = [it for it in audio_items
                                  if it.get('artist') and it.get('album')]
                    unidentified = [it for it in audio_items
                                    if not it.get('artist') or not it.get('album')]
                except Exception as exc:
                    log.warning("  Retry with artist hint failed: %s", exc)

    if not identified:
        log.warning("  Could not identify any of %d audio files", len(audio_items))
        for item in audio_items[:3]:
            for r in (item.get('rejections') or []):
                reason = r.get('reason', str(r)) if isinstance(r, dict) else str(r)
                log.warning("    Rejection: %s", reason)
        return 'failed'

    # Summarize what was identified
    artists = set()
    albums = set()
    album_ids = set()
    for it in identified:
        if it.get('artist'):
            artists.add(it['artist'].get('artistName', '?'))
        if it.get('album'):
            albums.add(it['album'].get('title', '?'))
            album_ids.add(it['album']['id'])

    log.info("  Matched: %s - %s  (%d/%d tracks)",
             ', '.join(artists), ', '.join(albums),
             len(identified), len(audio_items))

    if unidentified:
        log.warning("  %d tracks not identified (will be skipped)", len(unidentified))

    # Collect unique rejections across all identified items
    rejection_reasons: set[str] = set()
    rejected_items = []
    clean_items = []
    for it in identified:
        item_rejections = it.get('rejections') or []
        if item_rejections:
            rejected_items.append(it)
            for r in item_rejections:
                reason = r.get('reason', str(r)) if isinstance(r, dict) else str(r)
                rejection_reasons.add(reason)
        else:
            clean_items.append(it)

    if rejection_reasons:
        for reason in sorted(rejection_reasons):
            log.warning("    Rejection: %s", reason)

        if not force:
            log.warning("  Skipping: %d/%d tracks have rejections (use --force to override)",
                        len(rejected_items), len(identified))
            # Import only the clean items, if any
            identified = clean_items
            if not identified:
                return 'rejected'

    # Step 2c: Quality gate - check if existing tracks are higher quality
    skip_import = False
    if not ignore_quality and album_ids:
        for aid in album_ids:
            try:
                existing_files = client.get_track_files(aid)
            except Exception:
                existing_files = []

            if not existing_files:
                continue  # No existing tracks, import freely

            # Get quality info from first existing track file
            ext_file = existing_files[0]
            ext_quality = _extract_quality_info(
                ext_file.get('quality'),
                audio_tags=ext_file.get('audioTags'),
                media_info=ext_file.get('mediaInfo'),
            )

            # Get quality info from first incoming identified track
            inc_item = identified[0]
            inc_quality = _extract_quality_info(
                inc_item.get('quality'),
                audio_tags=inc_item.get('audioTags'),
            )

            decision, reason = compare_quality(inc_quality, ext_quality,
                                               auto_score=auto_quality)

            if decision == 'equal' and delete_equal:
                log.info("  Equal quality: %s — deleting source audio files", reason)
                audio_files = [f for f in os.listdir(album_dir)
                               if os.path.splitext(f)[1].lower() in AUDIO_EXTENSIONS]
                for af in audio_files:
                    fp = os.path.join(album_dir, af)
                    if dry_run:
                        log.info("  DRY RUN - would delete: %s", af)
                    else:
                        os.remove(fp)
                        log.info("  Deleted: %s", af)
                skip_import = True
                # Fall through to artwork handling
                break

            if decision in ('skip', 'equal'):
                log.info("  Quality skip: %s", reason)
                return 'quality_skip'

            if decision == 'prompt':
                log.info("  Quality ambiguous: %s", reason)
                try:
                    answer = input("  Import anyway? [y/N] ").strip().lower()
                except EOFError:
                    answer = 'n'
                if answer not in ('y', 'yes'):
                    log.info("  Skipped by user")
                    return 'quality_skip'

            if decision == 'import':
                log.info("  Quality upgrade: %s", reason)

    if dry_run and not skip_import:
        log.info("  DRY RUN - would import %d tracks", len(identified))
        return 'dry_run'

    if not skip_import:
        # Step 3: Build POST payload
        updates = []
        for item in identified:
            update = build_import_update(item)
            if update:
                updates.append(update)

        if not updates:
            log.warning("  No valid items to import after building payload")
            return 'failed'

        # Step 4: Execute import via ManualImport command
        try:
            result = client.manual_import_execute(updates)
            cmd_id = (result or {}).get('id', 0)
            log.info("  Import command queued (%d tracks, command %d)", len(updates), cmd_id)
        except Exception as exc:
            log.error("  Import failed: %s", exc)
            return 'failed'

        # Wait for the import command to complete
        if cmd_id:
            status = wait_for_command(client, cmd_id, timeout=120)
            if status == 'completed':
                log.info("  Import completed")
            else:
                log.warning("  Import command status: %s", status)
        else:
            log.warning("  No command ID returned, waiting 5s...")
            time.sleep(5)

        # Check import success before artwork handling
        import_ok = (cmd_id and status == 'completed') or (not cmd_id)
        if not import_ok:
            log.warning("  Skipping artwork: import did not complete successfully (status: %s)",
                        status if cmd_id else 'unknown')
            return 'failed'

    # Step 5: BPM tagging on destination files
    if not skip_import and not skip_bpm and album_ids:
        for aid in album_ids:
            bpm_dest = resolve_artwork_destination(client, aid)
            if bpm_dest:
                bpm_dest = remap_path(bpm_dest, remote_prefix, local_prefix)
                run_bpm_tagging(bpm_dest, dry_run=dry_run, force_bpm=force_bpm,
                                min_confidence=bpm_confidence,
                                max_workers=bpm_workers)

    # Step 6: Handle artwork
    artwork_files = find_artwork(album_dir)
    artwork_updated = False
    if artwork_files and album_ids:
        action_verb = 'Copied' if copy_art else 'Moved'

        for aid in album_ids:
            dest_dir = resolve_artwork_destination(client, aid)
            if dest_dir:
                # API returns container path - reverse-map to host path
                dest_dir = remap_path(dest_dir, remote_prefix, local_prefix)
                for art_src in artwork_files:
                    art_name = os.path.basename(art_src)
                    art_base, art_ext = os.path.splitext(art_name)
                    needs_convert = art_ext.lower() in CONVERTIBLE_IMAGE_FORMATS

                    # For convertible formats, find existing artwork to match format
                    if needs_convert:
                        dest_match = find_matching_dest_artwork(dest_dir, art_base)
                        if dest_match:
                            dest_match_ext = os.path.splitext(dest_match)[1].lower()
                            # If existing is also convertible, target PNG
                            target_ext = ('.png' if dest_match_ext in CONVERTIBLE_IMAGE_FORMATS
                                          else dest_match_ext)
                            compare_target = dest_match
                        else:
                            target_ext = '.png'
                            compare_target = None
                        art_dest = os.path.join(dest_dir, art_base + target_ext)
                    else:
                        art_dest = os.path.join(dest_dir, art_name)
                        compare_target = art_dest

                    # Smart comparison: only replace if source is better
                    if compare_target and os.path.exists(compare_target):
                        replace, reason = should_replace_artwork(art_src, compare_target)
                    else:
                        replace, reason = True, 'new file'

                    if not replace:
                        log.info("  Skipped artwork %s: %s", art_name, reason)
                        continue

                    if confirm_artwork and compare_target and os.path.exists(compare_target):
                        log.info("  %s: %s", art_name, reason)
                        try:
                            prompt = 'Copy' if copy_art else 'Move'
                            answer = input(f"  {prompt} {art_name}? [y/N] ").strip().lower()
                        except EOFError:
                            answer = 'n'
                        if answer not in ('y', 'yes'):
                            log.info("  Skipped artwork %s (user declined)", art_name)
                            continue

                    if dry_run:
                        if needs_convert:
                            log.info("  DRY RUN - would convert %s -> %s",
                                     art_name, art_base + target_ext)
                        else:
                            log.info("  DRY RUN - would %s artwork: %s -> %s",
                                     'copy' if copy_art else 'move', art_name, dest_dir)
                        artwork_updated = True
                        continue

                    try:
                        if needs_convert:
                            if convert_image(art_src, art_dest):
                                log.info("  Converted artwork: %s -> %s (%s)",
                                         art_name, art_base + target_ext, reason)
                                if not copy_art:
                                    os.remove(art_src)
                                # Remove old format file if conversion changed the extension
                                if (compare_target
                                        and os.path.abspath(compare_target) != os.path.abspath(art_dest)
                                        and os.path.exists(compare_target)):
                                    os.remove(compare_target)
                                    log.info("  Removed old format: %s",
                                             os.path.basename(compare_target))
                                artwork_updated = True
                            else:
                                # Conversion failed - fall back to direct copy/move
                                log.warning("  Conversion failed, copying %s as-is", art_name)
                                art_dest_raw = os.path.join(dest_dir, art_name)
                                if copy_art:
                                    shutil.copy2(art_src, art_dest_raw)
                                else:
                                    shutil.move(art_src, art_dest_raw)
                                artwork_updated = True
                        else:
                            if copy_art:
                                shutil.copy2(art_src, art_dest)
                            else:
                                shutil.move(art_src, art_dest)
                            log.info("  %s artwork: %s -> %s (%s)",
                                     action_verb, art_name, dest_dir, reason)
                            artwork_updated = True
                    except OSError as exc:
                        log.warning("  Artwork transfer failed for %s: %s",
                                    art_name, exc)
            else:
                log.warning("  Could not determine destination for artwork "
                            "(album id %d)", aid)

    # Step 6: Handle disc art (cd.jpg, cdart.jpg, etc. -> discart.png with circle crop)
    discart_files = find_discart(album_dir)
    if discart_files and album_ids:
        action_verb = 'Copied' if copy_art else 'Moved'

        for aid in album_ids:
            dest_dir = resolve_artwork_destination(client, aid)
            if dest_dir:
                dest_dir = remap_path(dest_dir, remote_prefix, local_prefix)
                dest_discart = os.path.join(dest_dir, 'discart.png')

                # Pick the best source disc art file (largest dimensions)
                best_src = discart_files[0]
                best_area = 0
                for da in discart_files:
                    dims = get_image_dimensions(da)
                    area = dims[0] * dims[1] if dims else 0
                    if area > best_area:
                        best_area = area
                        best_src = da

                src_name = os.path.basename(best_src)
                src_base, src_ext = os.path.splitext(src_name)
                is_already_png = (src_base.lower() == 'discart'
                                  and src_ext.lower() == '.png')

                # Compare against existing destination discart.png
                if os.path.exists(dest_discart):
                    replace, reason = should_replace_artwork(best_src, dest_discart)
                else:
                    replace, reason = True, 'new file'

                if not replace:
                    log.info("  Skipped disc art %s: %s", src_name, reason)
                elif confirm_artwork and os.path.exists(dest_discart):
                    log.info("  Disc art %s: %s", src_name, reason)
                    try:
                        prompt = 'Copy' if copy_art else 'Move'
                        answer = input(f"  {prompt} disc art? [y/N] ").strip().lower()
                    except EOFError:
                        answer = 'n'
                    if answer not in ('y', 'yes'):
                        log.info("  Skipped disc art (user declined)")
                        replace = False

                if replace:
                    if dry_run:
                        if is_already_png:
                            log.info("  DRY RUN - would %s disc art: %s -> %s",
                                     'copy' if copy_art else 'move', src_name,
                                     dest_dir)
                        else:
                            log.info("  DRY RUN - would convert disc art: "
                                     "%s -> discart.png", src_name)
                        artwork_updated = True
                    else:
                        try:
                            if is_already_png:
                                # Already processed - just copy/move
                                if copy_art:
                                    shutil.copy2(best_src, dest_discart)
                                else:
                                    shutil.move(best_src, dest_discart)
                                log.info("  %s disc art: %s -> %s (%s)",
                                         action_verb, src_name, dest_dir, reason)
                                artwork_updated = True
                            else:
                                # Crop to circle + transparent background
                                if convert_discart(best_src, dest_discart):
                                    log.info("  Converted disc art: %s -> "
                                             "discart.png (%s)", src_name, reason)
                                    if not copy_art:
                                        os.remove(best_src)
                                    artwork_updated = True
                                else:
                                    # Conversion failed - copy/move as-is
                                    log.warning("  Disc art conversion failed, "
                                                "copying %s as-is", src_name)
                                    art_dest_raw = os.path.join(dest_dir, src_name)
                                    if copy_art:
                                        shutil.copy2(best_src, art_dest_raw)
                                    else:
                                        shutil.move(best_src, art_dest_raw)
                                    artwork_updated = True
                        except OSError as exc:
                            log.warning("  Disc art transfer failed for %s: %s",
                                        src_name, exc)

                # Clean up remaining disc art source files (non-best)
                if not copy_art:
                    for da in discart_files:
                        if da != best_src and os.path.exists(da):
                            try:
                                os.remove(da)
                                log.debug("  Removed extra disc art: %s",
                                          os.path.basename(da))
                            except OSError:
                                pass
            else:
                log.warning("  Could not determine destination for disc art "
                            "(album id %d)", aid)

    # Step 7: Handle animated artwork (folder.mp4 / .webm / .mov -> folder.gif)
    # Animated art coexists with static artwork — it only replaces an existing
    # folder.gif at the destination.  The source video is always removed on
    # success; on failure (ffmpeg missing / unsupported format) the source is
    # left in place but NOT copied, since the raw video isn't useful as artwork.
    anim_src = find_animated_artwork(album_dir)
    if anim_src and album_ids:
        anim_name = os.path.basename(anim_src)
        anim_converted = False
        for aid in album_ids:
            dest_dir = resolve_artwork_destination(client, aid)
            if dest_dir:
                dest_dir = remap_path(dest_dir, remote_prefix, local_prefix)
                dest_gif = os.path.join(dest_dir, 'folder.gif')

                # Only compare against an existing animated image — never
                # against a static folder.jpg/png which is a separate artifact.
                if os.path.exists(dest_gif):
                    src_size = os.path.getsize(anim_src)
                    dest_size = os.path.getsize(dest_gif)
                    if src_size <= dest_size:
                        log.info("  Skipped animated art %s: existing GIF is "
                                 "same size or larger (%d vs %d bytes)",
                                 anim_name, dest_size, src_size)
                        continue
                    reason = f'larger source: {src_size} vs {dest_size} bytes'
                else:
                    reason = 'new file'

                if dry_run:
                    log.info("  DRY RUN - would convert animated art: "
                             "%s -> folder.gif", anim_name)
                    artwork_updated = True
                    continue

                if convert_video_to_gif(anim_src, dest_gif):
                    log.info("  Converted animated art: %s -> folder.gif (%s)",
                             anim_name, reason)
                    artwork_updated = True
                    anim_converted = True
                else:
                    log.warning("  Animated art conversion failed — skipping %s",
                                anim_name)
            else:
                log.warning("  Could not determine destination for animated art "
                            "(album id %d)", aid)

        # Remove the source video once at least one conversion succeeded.
        # On failure the source stays — the format may simply not be
        # supported, so we leave it for manual handling or a future retry.
        if anim_converted and not dry_run and os.path.exists(anim_src):
            try:
                os.remove(anim_src)
                log.debug("  Removed animated art source: %s", anim_name)
            except OSError as exc:
                log.warning("  Could not remove %s: %s", anim_name, exc)

    # delete-equal cleanup: if no artwork was worth saving, remove all remaining files
    if skip_import and not artwork_updated:
        try:
            remaining = [f for f in os.listdir(album_dir)
                         if os.path.isfile(os.path.join(album_dir, f))]
        except OSError:
            remaining = []
        for rf in remaining:
            fp = os.path.join(album_dir, rf)
            if dry_run:
                log.info("  DRY RUN - would delete: %s", rf)
            else:
                try:
                    os.remove(fp)
                    log.info("  Deleted (no artwork upgrade): %s", rf)
                except OSError as exc:
                    log.warning("  Could not delete %s: %s", rf, exc)

    if skip_import:
        return 'dry_run' if dry_run else 'deleted'
    return 'success'

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def redact_url(url: str) -> str:
    """Return a log-safe URL string with credentials removed.

    A Lidarr URL normally carries no credentials (auth is the X-Api-Key header),
    but a user could embed `user:pass@` in the configured URL. Strip it before
    the URL reaches any log sink so embedded credentials never appear in output.

    This helper is intentionally fail-closed: if parsing fails, never return the
    original input because it may contain sensitive data.
    """
    try:
        parts = urllib.parse.urlsplit(url)
    except ValueError:
        return '[redacted-url]'

    host = parts.hostname or ''
    if parts.port is not None:
        host = '%s:%d' % (host, parts.port)

    return urllib.parse.urlunsplit(
        (parts.scheme, host, parts.path, parts.query, parts.fragment))


def load_dotenv(env_path: str) -> dict[str, str]:
    """Parse a .env file into a dict. Handles KEY=VALUE, quotes, comments."""
    env = {}
    try:
        with open(env_path) as f:
            for line in f:
                line = line.strip()
                if not line or line.startswith('#'):
                    continue
                if '=' not in line:
                    continue
                key, _, value = line.partition('=')
                key = key.strip()
                value = value.strip()
                # Strip surrounding quotes
                if len(value) >= 2 and value[0] == value[-1] and value[0] in ('"', "'"):
                    value = value[1:-1]
                env[key] = value
    except FileNotFoundError:
        pass
    return env


def _script_dir(invoked_path: str) -> str:
    """Directory of the real script file.

    Uses realpath (not abspath) so an invocation via a symlink (e.g.
    /usr/local/bin/il -> the real script) resolves to the real file's
    directory, where sibling files such as .env actually live.
    """
    return os.path.dirname(os.path.realpath(invoked_path))


def load_config(config_path: str) -> tuple[str, str]:
    """Load LidarrUrl and ApiKey from a JSON config file."""
    with open(config_path) as f:
        cfg = json.load(f)
    url = cfg.get('LidarrUrl', '').rstrip('/')
    key = cfg.get('ApiKey', '')
    if not url or not key:
        raise ValueError(f"Config file must contain LidarrUrl and ApiKey: {config_path}")
    return url, key


def xdg_config_dir() -> str:
    """${XDG_CONFIG_HOME:-~/.config}/importlidarr."""
    base = os.environ.get('XDG_CONFIG_HOME') or os.path.expanduser('~/.config')
    return os.path.join(base, 'importlidarr')


def get_config_dir(platform: str) -> str:
    """Where `--setup` should write `.env`.

    The XDG config dir by default, but next to the script on ephemeral-home
    platforms (Unraid), where ~/.config is RAM-wiped on reboot and only the
    persistent checkout survives.
    """
    if platform in EPHEMERAL_HOME_PLATFORMS:
        return SCRIPT_DIR
    return xdg_config_dir()


def env_discovery_paths(args) -> list[str]:
    """Ordered `.env` candidates, highest precedence first.

    --env-file > XDG `.env` > next-to-script `.env` > CWD `.env`.
    """
    paths = []
    env_file = getattr(args, 'env_file', None)
    if env_file:
        paths.append(env_file)
    paths.append(os.path.join(xdg_config_dir(), '.env'))
    paths.append(os.path.join(_script_dir(__file__), '.env'))
    paths.append(os.path.join(os.getcwd(), '.env'))
    return paths


def scaffold_env_file(env_path: str, args, interactive: bool) -> bool:
    """Create `env_path` with Lidarr credentials. Returns True when written.

    Reads the API key without echo (getpass) and never logs/prints it; writes
    the file 0600 from creation (os.open) so the secret is never world-readable
    even briefly. Non-interactive runs take values from --url/--api-key or the
    LIDARR_* env vars and skip (rather than hang) when they are missing. An
    existing `.env` is never clobbered without explicit confirmation.
    """
    if os.path.exists(env_path):
        if not interactive:
            log.info("  .env already exists at %s - leaving it untouched", env_path)
            return False
        try:
            answer = input("  .env exists at %s - overwrite? [y/N] "
                           % env_path).strip().lower()
        except EOFError:
            answer = 'n'
        if answer not in ('y', 'yes'):
            log.info("  Keeping existing .env")
            return False

    url = getattr(args, 'url', None) or os.environ.get('LIDARR_URL', '')
    api_key = getattr(args, 'api_key', None) or os.environ.get('LIDARR_API_KEY', '')
    local_path = getattr(args, 'local_path', None) or os.environ.get('LIDARR_LOCAL_PATH', '')
    remote_path = getattr(args, 'remote_path', None) or os.environ.get('LIDARR_REMOTE_PATH', '')

    if interactive:
        if not url:
            try:
                url = input("  LIDARR_URL (e.g. http://localhost:8686): ").strip()
            except EOFError:
                url = ''
        if not api_key:
            try:
                api_key = getpass.getpass("  LIDARR_API_KEY (input hidden): ").strip()
            except EOFError:
                api_key = ''
        if not local_path:
            try:
                local_path = input("  LIDARR_LOCAL_PATH (optional, blank to skip): ").strip()
            except EOFError:
                local_path = ''
        if not remote_path:
            try:
                remote_path = input("  LIDARR_REMOTE_PATH (optional, blank to skip): ").strip()
            except EOFError:
                remote_path = ''

    if not url or not api_key:
        log.info("  Skipping .env scaffold: need both a URL and an API key "
                 "(pass --url/--api-key or set LIDARR_URL/LIDARR_API_KEY)")
        return False

    lines = ['LIDARR_URL=%s' % url, 'LIDARR_API_KEY=%s' % api_key]
    if local_path:
        lines.append('LIDARR_LOCAL_PATH=%s' % local_path)
    if remote_path:
        lines.append('LIDARR_REMOTE_PATH=%s' % remote_path)

    try:
        parent = os.path.dirname(env_path)
        if parent:
            os.makedirs(parent, exist_ok=True)
        fd = os.open(env_path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, 'w') as f:
            f.write('\n'.join(lines) + '\n')
        os.chmod(env_path, 0o600)  # tighten even if the file pre-existed with looser perms
    except OSError as exc:
        # Stay non-fatal: a config-dir write failure must not abort the rest of
        # --setup (symlink, boot persistence).
        log.warning("  Could not write .env at %s: %s", env_path, exc)
        return False
    log.info("  Wrote %s (chmod 600)", env_path)  # never logs the API key
    return True


def resolve_settings(args) -> dict[str, str]:
    """
    Resolve all settings in priority order:
    1. CLI args (--url / --api-key)
    2. Config file (--config JSON)
    3. .env file: --env-file > XDG > next-to-script > CWD
    4. Environment variables
    """
    url = args.url
    api_key = args.api_key
    local_path = getattr(args, 'local_path', None) or ''
    remote_path = getattr(args, 'remote_path', None) or ''

    # From --config JSON file
    if not (url and api_key) and args.config:
        url, api_key = load_config(args.config)

    # From the first existing .env in the discovery order
    env = {}
    for candidate in env_discovery_paths(args):
        if os.path.isfile(candidate):
            env = load_dotenv(candidate)
            break

    url = url or env.get('LIDARR_URL', '') or os.environ.get('LIDARR_URL', '')
    api_key = (api_key or env.get('LIDARR_API_KEY', '')
               or os.environ.get('LIDARR_API_KEY', ''))
    local_path = (local_path or env.get('LIDARR_LOCAL_PATH', '')
                  or os.environ.get('LIDARR_LOCAL_PATH', ''))
    remote_path = (remote_path or env.get('LIDARR_REMOTE_PATH', '')
                   or os.environ.get('LIDARR_REMOTE_PATH', ''))

    return {
        'url': url.rstrip('/'),
        'api_key': api_key,
        'local_path': local_path.rstrip('/'),
        'remote_path': remote_path.rstrip('/'),
    }


EXAMPLES = """\
First-time setup (no import path needed):
  %(prog)s --setup                                        # install deps, il symlink, boot script, .env
  %(prog)s --setup --url http://localhost:8686 --api-key KEY  # non-interactive .env scaffold

Basic import:
  %(prog)s /path/to/music                                 # import (uses .env)
  %(prog)s /path/to/music --dry-run                       # preview only

Prepare (add missing albums to Lidarr before import):
  %(prog)s /path/to/music --prepare                       # add missing albums, then import
  %(prog)s /path/to/music --prepare-only --dry-run        # scan only, don't import
  %(prog)s /path/to/music --prepare --add-artists         # also add new artists

Artwork:
  %(prog)s /path/to/music --copy-art                      # copy artwork instead of move
  %(prog)s /path/to/music --confirm-artwork               # prompt before replacing artwork

Quality control:
  %(prog)s /path/to/music --force                         # ignore rejections
  %(prog)s /path/to/music --ignore-quality                # import regardless of quality
  %(prog)s /path/to/music --auto-quality                  # auto-resolve ambiguous quality
  %(prog)s /path/to/music --delete-equal                  # delete source if quality matches

Audio processing:
  %(prog)s /path/to/music --no-rsgain                     # skip ReplayGain tagging
  %(prog)s /path/to/music --skip-bpm                      # skip BPM detection
  %(prog)s /path/to/music --force-bpm                     # re-detect BPM even if tagged
  %(prog)s /path/to/music --bpm-confidence 0.7            # stricter tempo stability filter
  %(prog)s /path/to/music --bpm-workers 2                 # limit BPM threads (default: min(cpu_count, 4))

Path mapping (when Lidarr runs in Docker):
  %(prog)s /mnt/user/Music --local-path /mnt/user --remote-path /share

Configuration (checked in order):
  1. CLI args: --url and --api-key
  2. Config file: --config /path/to/LidarrConfig.json
  3. .env file: --env-file > ${XDG_CONFIG_HOME:-~/.config}/importlidarr/.env
     > next-to-script .env > ./.env  (LIDARR_URL and LIDARR_API_KEY)
  4. Environment variables: LIDARR_URL and LIDARR_API_KEY
"""


def parse_args():
    p = argparse.ArgumentParser(
        description='Lidarr batch manual import - imports one album at a time '
                    'to work around the 100-file limit. '
                    'Use --examples for detailed usage examples.',
        formatter_class=argparse.RawDescriptionHelpFormatter)

    p.add_argument('import_path', nargs='?', default=None,
                   help='Root folder containing Artist/Album/Track subfolders')
    p.add_argument('--config', '-c', metavar='FILE',
                   help='Path to LidarrConfig.json (with LidarrUrl and ApiKey)')
    p.add_argument('--env-file', metavar='FILE',
                   help='Path to a .env file (overrides .env discovery)')
    p.add_argument('--url', '-u', metavar='URL',
                   help='Lidarr base URL (e.g. http://localhost:8686)')
    p.add_argument('--api-key', '-k', metavar='KEY',
                   help='Lidarr API key')
    p.add_argument('--setup', action='store_true',
                   help='Set up persistence (install deps, create the il symlink, '
                        'write the Unraid boot script, scaffold .env) and exit; '
                        'no import path required')
    p.add_argument('--copy-art', action='store_true',
                   help='Copy artwork instead of moving it to the destination')
    p.add_argument('--confirm-artwork', action='store_true',
                   help='Prompt before replacing existing artwork (default: auto-replace '
                        'when source has larger dimensions or filesize)')
    p.add_argument('--ignore-quality', action='store_true',
                   help='Bypass quality comparison, import regardless of existing quality')
    p.add_argument('--auto-quality', action='store_true',
                   help='Resolve ambiguous quality comparisons using bit_depth*sample_rate '
                        'score instead of prompting')
    p.add_argument('--delete-equal', action='store_true',
                   help='When source quality equals existing library: check artwork for an '
                        'upgrade (converting TIFF/WEBP via ImageMagick if needed); if artwork '
                        'is upgraded keep it, then delete remaining source files; if artwork '
                        'is not worth keeping, delete the entire source directory contents '
                        '(music, lyrics, images)')
    p.add_argument('--dry-run', '-n', action='store_true',
                   help='Preview what would be imported without making changes')
    p.add_argument('--force', '-f', action='store_true',
                   help='Import even when Lidarr reports rejections (low match quality, etc.)')
    p.add_argument('--prepare', '-p', action='store_true',
                   help='Scan for missing albums and add to Lidarr before importing')
    p.add_argument('--prepare-only', '-P', action='store_true',
                   help='Only scan and add missing albums (no import)')
    p.add_argument('--add-artists', action='store_true',
                   help='Also add missing artists to library (default: skip)')
    p.add_argument('--delay', '-d', type=float, default=2.0,
                   help='Seconds to wait between album imports (default: 2)')
    p.add_argument('--local-path', metavar='PREFIX',
                   help='Local (host) path prefix, e.g. /mnt/user')
    p.add_argument('--remote-path', metavar='PREFIX',
                   help='Remote (Lidarr/Docker) path prefix, e.g. /share')
    p.add_argument('--no-rsgain', action='store_true',
                   help='Skip rsgain ReplayGain tagging (enabled by default)')
    p.add_argument('--no-preflight', action='store_true',
                   help='Skip the startup dependency check/installer')
    p.add_argument('--skip-bpm', action='store_true',
                   help='Skip BPM detection and tagging (enabled by default)')
    p.add_argument('--force-bpm', action='store_true',
                   help='Overwrite existing BPM tags (default: skip files that have one)')
    p.add_argument('--bpm-workers', type=int, default=None, metavar='N',
                   help='number of parallel threads for BPM detection '
                        '(default: min(cpu_count, 4))')
    p.add_argument('--bpm-confidence', type=float, default=0.5, metavar='N',
                   help='Minimum confidence (0.0-1.0) to accept a BPM result; '
                        'tracks with unstable tempo score lower (default: 0.5)')
    p.add_argument('--verbose', '-v', action='store_true',
                   help='Enable verbose/debug logging')
    p.add_argument('--examples', action='store_true',
                   help='Show detailed usage examples and exit')

    args = p.parse_args()

    if args.examples:
        print(EXAMPLES % {'prog': p.prog})
        sys.exit(0)

    # --setup is a standalone bootstrap: it needs neither an import path nor
    # resolved Lidarr credentials. main() dispatches to run_setup() once
    # logging is configured.
    if args.setup:
        return args

    if not args.import_path:
        p.error('import_path is required (or use --examples)')

    # Resolve all settings from CLI / config / .env / environment
    settings = resolve_settings(args)
    args.url = settings['url']
    args.api_key = settings['api_key']
    args.local_path = settings['local_path']
    args.remote_path = settings['remote_path']

    if not args.url or not args.api_key:
        p.error('Lidarr URL and API key required. Provide via:\n'
                '  --url / --api-key flags\n'
                '  --config LidarrConfig.json\n'
                '  .env file (LIDARR_URL, LIDARR_API_KEY)\n'
                '  Environment variables (LIDARR_URL, LIDARR_API_KEY)')

    return args


def main():
    args = parse_args()

    # Setup logging
    level = logging.DEBUG if args.verbose else logging.INFO
    logging.basicConfig(
        level=level,
        format='%(asctime)s [%(levelname)s] %(message)s',
        datefmt='%H:%M:%S',
    )

    if getattr(args, 'setup', False):
        run_setup(args)
        sys.exit(0)

    import_path = os.path.abspath(args.import_path)
    if not os.path.isdir(import_path):
        log.error("Import path does not exist: %s", import_path)
        sys.exit(1)
    log.info("Import path: %s", import_path)

    if args.local_path and args.remote_path:
        log.info("Path mapping: %s -> %s", args.local_path, args.remote_path)

    # Show file count to confirm the >100 file issue
    total_audio = count_audio_files(import_path)
    log.info("Total audio files under %s: %d", import_path, total_audio)
    if total_audio > 100:
        log.info("This exceeds Lidarr's 100-file limit for manual import - "
                 "the per-album approach will work around it")

    # Find album directories (needed for the run-aware dependency preflight)
    album_dirs = find_album_dirs(import_path)
    if not album_dirs:
        log.warning("No album directories with audio files found under %s",
                    import_path)
        sys.exit(0)
    log.info("Found %d album directories to process", len(album_dirs))

    # Dependency preflight before any network work
    preflight_dependencies(args, album_dirs)

    client = LidarrClient(args.url, args.api_key)

    # Verify connectivity
    try:
        status = client.system_status()
        if not status:
            log.error("Empty response from Lidarr at %s", redact_url(args.url))
            sys.exit(1)
        log.info("Connected to Lidarr %s (%s)", status.get('version'),
                 status.get('branch'))
    except Exception as exc:
        log.error("Cannot connect to Lidarr at %s: %s", redact_url(args.url), exc)
        sys.exit(1)

    # Pre-flight: add missing albums to library
    if args.prepare or args.prepare_only:
        log.info("")
        log.info("=" * 60)
        log.info("PREPARE: Scanning for missing albums...")
        log.info("=" * 60)

        prep_failures = prepare_library(
            client, album_dirs, import_path,
            args.local_path, args.remote_path,
            args.add_artists, args.dry_run,
        )

        if args.prepare_only:
            sys.exit(1 if prep_failures > 0 else 0)

        if prep_failures:
            log.warning("Some albums could not be prepared - continuing with import")

        log.info("")
        log.info("=" * 60)
        log.info("IMPORT: Processing album directories...")
        log.info("=" * 60)

    if args.dry_run:
        log.info("*** DRY RUN MODE - no files will be imported ***")

    # Apply ReplayGain tags before import
    if not args.no_rsgain:
        log.info("")
        log.info("=" * 60)
        log.info("RSGAIN: Applying ReplayGain tags...")
        log.info("=" * 60)
        run_rsgain(import_path, args.dry_run)

    # Process each album directory
    counts = {'success': 0, 'skipped': 0, 'failed': 0, 'dry_run': 0,
              'rejected': 0, 'quality_skip': 0, 'deleted': 0}
    failed_dirs = []
    artist_cache: dict[str, int] = {}  # shared across all albums

    for i, album_dir in enumerate(album_dirs, 1):
        rel = os.path.relpath(album_dir, import_path)
        log.info("")
        log.info("[%d/%d] %s", i, len(album_dirs), rel)

        if _shutdown.is_set():
            log.info("Shutdown requested — stopping after %d/%d album(s)",
                     i - 1, len(album_dirs))
            break

        result = process_album_dir(client, album_dir, args.copy_art, args.dry_run,
                                   args.local_path, args.remote_path,
                                   artist_cache, args.force,
                                   args.confirm_artwork,
                                   args.ignore_quality,
                                   args.auto_quality,
                                   args.delete_equal,
                                   args.skip_bpm,
                                   args.force_bpm,
                                   args.bpm_confidence,
                                   args.bpm_workers)
        counts[result] = counts.get(result, 0) + 1

        if result in ('failed', 'rejected'):
            failed_dirs.append(rel)

        if result in ('success', 'deleted', 'dry_run'):
            cleaned = cleanup_empty_dir(album_dir, import_path, args.dry_run)
            if cleaned:
                log.debug("  Cleaned up %d empty director%s",
                          cleaned, 'y' if cleaned == 1 else 'ies')

        # Delay between albums to avoid overwhelming Lidarr/MusicBrainz
        if args.delay > 0 and i < len(album_dirs):
            time.sleep(args.delay)

    # Summary
    log.info("")
    log.info("=" * 60)
    log.info("RESULTS")
    log.info("  Imported:  %d", counts['success'])
    if counts['dry_run']:
        log.info("  Would import (dry run): %d", counts['dry_run'])
    log.info("  Skipped:   %d", counts['skipped'])
    if counts['rejected']:
        log.info("  Rejected:  %d (use --force to override)", counts['rejected'])
    if counts['deleted']:
        log.info("  Deleted (equal quality): %d", counts['deleted'])
    if counts['quality_skip']:
        log.info("  Quality skip: %d (use --ignore-quality to override)",
                 counts['quality_skip'])
    log.info("  Failed:    %d", counts['failed'])
    log.info("=" * 60)

    if failed_dirs:
        log.info("")
        log.info("Failed directories:")
        for d in failed_dirs:
            log.info("  - %s", d)

    if _shutdown.is_set():
        sys.exit(130)
    sys.exit(1 if counts['failed'] > 0 else 0)


if __name__ == '__main__':
    main()
