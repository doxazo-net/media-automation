#!/usr/bin/env python3
"""Repair album-artist tags stored as a single joined credit instead of a list.

Some taggers (notably a Picard setup missing ``$copy(albumartist,albumartists)``)
write the MusicBrainz *artist-credit string* into ``ALBUMARTIST`` -- e.g.
``"Beethoven; Berlin Philharmonic Orchestra, Herbert von Karajan"`` -- as a single
value, even though the release genuinely has several album artists. Downstream
consumers that split album artists on ``;`` (Emby, Jellyfin, Music Assistant) then
emit phantom artists like ``"Berlin Philharmonic Orchestra, Herbert von Karajan"``,
because they cannot split on ``,`` without shredding legitimate names such as
``"Hank Williams, Jr."``.

Defect signature -- the ONLY files this touches:

    ALBUMARTIST / TPE2 holds exactly ONE value
    AND the file carries MORE THAN ONE MusicBrainz album-artist ID.

That pairing is self-contradicting: the file asserts N album artists via MBID yet
stores a single joined name. Files whose album-artist MBID count is <= 1 are left
untouched, which is what protects genuine single artists whose name contains a
comma.

Names are resolved from the MBIDs via the MusicBrainz API (canonical form). The
joined string is never split -- that operation is ambiguous by construction.

Every write is preceded by a full per-file tag backup; each backup's absolute
path is recorded in a timestamped manifest so ``restore`` can roll a batch back
without needing to know where state lives.

Usage:
    repair-albumartist-multivalue.py --root DIR [--root DIR ...] scan
    repair-albumartist-multivalue.py --root DIR repair            # dry run
    repair-albumartist-multivalue.py --root DIR repair --apply    # write, with backups
    repair-albumartist-multivalue.py restore <manifest>           # roll back a batch

Library roots are required and taken from ``--root`` (repeatable) or the
``MUSIC_ROOTS`` environment variable (os.pathsep separated); there is no built-in
default, so the tool never scans a path you did not name. State (backups,
manifests, MBID name cache) is written under ``--state-dir`` (default:
``./albumartist-repair`` in the current directory) -- nothing is written outside
the roots you scan and the state dir you choose.
"""

import argparse
import json
import os
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path


class _DeferInterrupt:
    """SIGINT handler for the apply loop: the first Ctrl-C sets a flag (checked at
    the next file boundary) so the in-flight tag write finishes instead of being
    torn; a second Ctrl-C restores default handling and exits immediately."""

    def __init__(self):
        self.flag = False

    def __call__(self, signum, frame):
        self.flag = True
        signal.signal(signal.SIGINT, signal.SIG_DFL)  # next Ctrl-C exits now
        print("\ninterrupt: finishing the current file, then stopping "
              "(Ctrl-C again to force)...", file=sys.stderr)


def _write_durable(path, text):
    """Write text and fsync it, so a crash can't leave a torn recovery file."""
    path.write_text(text, encoding="utf-8")
    try:
        fd = os.open(path, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    except OSError:
        pass  # fsync unsupported (some network mounts); the write still landed

MB_API = "https://musicbrainz.org/ws/2/artist/{}?fmt=json"
USER_AGENT = "media-automation-albumartist-repair/1.0 ( https://github.com/doxazo-net/media-automation )"
MB_RATE_LIMIT = 1.1  # seconds between calls; MusicBrainz permits ~1 req/sec

AUDIO_EXT = {".flac", ".mp3", ".ogg", ".oga", ".opus"}


# --------------------------------------------------------------- dependencies

def ensure_mutagen(allow_bootstrap=True):
    """Import mutagen, installing it into the user site on first miss."""
    try:
        import mutagen  # noqa: F401
        return
    except ImportError:
        pass
    if not allow_bootstrap:
        sys.exit("mutagen is required; install it with `pip install --user mutagen` "
                 "or drop --no-bootstrap")
    print("mutagen not found; installing into the user site...", file=sys.stderr)
    try:
        subprocess.run(
            [sys.executable, "-m", "pip", "install", "--user", "--quiet", "mutagen"],
            check=True,
        )
    except (subprocess.CalledProcessError, OSError) as exc:
        sys.exit(f"failed to bootstrap mutagen: {exc}\n"
                 "install it manually with `pip install --user mutagen`")
    try:
        import mutagen  # noqa: F401
    except ImportError:
        sys.exit("mutagen still unimportable after install; check your Python/pip pairing")


# ----------------------------------------------------------------- tag access

class Adapter:
    """Uniform read/write of album-artist names and MBIDs across containers."""

    def __init__(self, path):
        from mutagen.flac import FLAC
        from mutagen.id3 import ID3
        from mutagen.oggopus import OggOpus
        from mutagen.oggvorbis import OggVorbis

        self.path = path
        ext = path.suffix.lower()
        if ext == ".flac":
            self.kind, self.f = "vorbis", FLAC(path)
        elif ext in (".ogg", ".oga"):
            self.kind, self.f = "vorbis", OggVorbis(path)
        elif ext == ".opus":
            # Opus streams need OggOpus; OggVorbis rejects them. Both use Vorbis
            # comments, so the "vorbis" read/write paths below are identical.
            self.kind, self.f = "vorbis", OggOpus(path)
        elif ext == ".mp3":
            self.kind, self.f = "id3", ID3(path)
        else:
            raise ValueError(f"unsupported: {path}")

    def album_artists(self):
        if self.kind == "vorbis":
            return list(self.f.tags.get("albumartist", []))
        return list(self.f["TPE2"].text) if "TPE2" in self.f else []

    def album_artist_mbids(self):
        if self.kind == "vorbis":
            return list(self.f.tags.get("musicbrainz_albumartistid", []))
        out = []
        for fr in self.f.getall("TXXX"):
            if fr.desc.lower() == "musicbrainz album artist id":
                out.extend(str(x) for x in fr.text)
        # Picard may pack several MBIDs into one frame separated by "; ".
        if len(out) == 1 and ";" in out[0]:
            out = [x.strip() for x in out[0].split(";") if x.strip()]
        return out

    def snapshot(self):
        """Capture enough tag state to reconstruct what this tool changes."""
        if self.kind == "vorbis":
            return {"kind": "vorbis", "tags": {k: list(v) for k, v in self.f.tags}}
        return {
            "kind": "id3",
            "tags": {
                "TPE2": list(self.f["TPE2"].text) if "TPE2" in self.f else [],
                "TXXX:ALBUMARTISTS": [
                    str(x) for fr in self.f.getall("TXXX")
                    if fr.desc.upper() == "ALBUMARTISTS" for x in fr.text
                ],
            },
        }

    def write_album_artists(self, names):
        from mutagen.id3 import TPE2, TXXX

        if self.kind == "vorbis":
            self.f.tags["ALBUMARTIST"] = names
            self.f.tags["ALBUMARTISTS"] = names
        else:
            self.f.setall("TPE2", [TPE2(encoding=3, text=names)])
            keep = [fr for fr in self.f.getall("TXXX") if fr.desc.upper() != "ALBUMARTISTS"]
            self.f.setall("TXXX", keep + [TXXX(encoding=3, desc="ALBUMARTISTS", text=names)])
        self.f.save()

    def restore(self, snap):
        from mutagen.id3 import TPE2, TXXX

        if snap["kind"] == "vorbis":
            self.f.tags.clear()
            for key, vals in snap["tags"].items():
                self.f.tags[key] = vals
        else:
            tpe2 = snap["tags"].get("TPE2", [])
            if tpe2:
                self.f.setall("TPE2", [TPE2(encoding=3, text=tpe2)])
            else:
                self.f.delall("TPE2")
            keep = [fr for fr in self.f.getall("TXXX") if fr.desc.upper() != "ALBUMARTISTS"]
            aas = snap["tags"].get("TXXX:ALBUMARTISTS", [])
            if aas:
                keep.append(TXXX(encoding=3, desc="ALBUMARTISTS", text=aas))
            self.f.setall("TXXX", keep)
        self.f.save()


# --------------------------------------------------------- MusicBrainz names

class NameResolver:
    def __init__(self, cache_path):
        self.cache_path = cache_path
        self.cache = json.loads(cache_path.read_text()) if cache_path.exists() else {}
        self.last_call = 0.0

    def name(self, mbid):
        if mbid in self.cache:
            return self.cache[mbid]
        wait = MB_RATE_LIMIT - (time.monotonic() - self.last_call)
        if wait > 0:
            time.sleep(wait)
        req = urllib.request.Request(MB_API.format(mbid), headers={"User-Agent": USER_AGENT})
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                data = json.loads(resp.read())
        except urllib.error.HTTPError as exc:
            print(f"    ! MBID {mbid}: HTTP {exc.code}", file=sys.stderr)
            return None
        finally:
            self.last_call = time.monotonic()
        self.cache[mbid] = data["name"]
        return data["name"]

    def flush(self):
        self.cache_path.parent.mkdir(parents=True, exist_ok=True)
        self.cache_path.write_text(json.dumps(self.cache, indent=1, ensure_ascii=False))


# ------------------------------------------------------------------- passes

def walk(roots):
    # Resolve to an absolute path so manifest entries are independent of the
    # working directory, and dedupe so repeated / nested roots never yield (and
    # later re-backup) the same file twice.
    seen = set()
    for root in roots:
        for dirpath, _, files in os.walk(root):
            for fn in files:
                p = (Path(dirpath) / fn).resolve()
                if p.suffix.lower() in AUDIO_EXT and p not in seen:
                    seen.add(p)
                    yield p


def find_broken(roots):
    broken, errors = [], 0
    for p in walk(roots):
        try:
            adapter = Adapter(p)
            names = adapter.album_artists()
            # Distinct MBIDs only: a single artist whose MBID is listed twice
            # must not satisfy the >1 defect signature. dict.fromkeys keeps
            # credit order.
            mbids = list(dict.fromkeys(adapter.album_artist_mbids()))
        except Exception:  # noqa: BLE001 - unreadable file, skip and count
            errors += 1
            continue
        if len(names) == 1 and len(mbids) > 1:
            broken.append((p, names[0], mbids))
    return broken, errors


def cmd_scan(args):
    broken, errors = find_broken(args.roots)
    albums = {}
    for p, joined, mbids in broken:
        albums.setdefault((str(p.parent), joined, tuple(mbids)), []).append(p)
    unique_mbids = {m for _, _, mb in broken for m in mb}
    print(f"broken files : {len(broken)}")
    print(f"broken albums: {len(albums)}")
    print(f"unread files : {errors}")
    print(f"unique MBIDs to resolve: {len(unique_mbids)}")
    print()
    for (parent, joined, mbids), files in sorted(albums.items())[:20]:
        print(f"  {joined}")
        print(f"    {len(mbids)} album artists, {len(files)} tracks -- {parent}")
    if len(albums) > 20:
        print(f"  ... and {len(albums) - 20} more albums")


def cmd_repair(args):
    broken, errors = find_broken(args.roots)
    if not broken:
        print("nothing to repair")
        return
    print(f"{len(broken)} files match the defect ({errors} unreadable, skipped)")

    resolver = NameResolver(args.state_dir / "mb-name-cache.json")
    todo = sorted({m for _, _, mb in broken for m in mb} - set(resolver.cache))
    if todo:
        print(f"resolving {len(todo)} new MBIDs from MusicBrainz "
              f"(~{len(todo) * MB_RATE_LIMIT / 60:.1f} min at 1 req/sec)...")
        for i, mbid in enumerate(todo, 1):
            resolver.name(mbid)
            if i % 25 == 0:
                print(f"  {i}/{len(todo)}")
                resolver.flush()
        resolver.flush()

    # UUID suffix so two apply runs in the same second cannot share a manifest
    # path and clobber each other's recovery metadata.
    stamp = f"{time.strftime('%Y%m%dT%H%M%S')}-{uuid.uuid4().hex[:8]}"
    backups = args.state_dir / "backups"
    skipped, changed = 0, 0

    manifest_path = None
    manifest_f = None
    interrupt = None
    prev_handler = None
    stopped_early = False
    if args.apply:
        args.state_dir.mkdir(parents=True, exist_ok=True)
        backups.mkdir(parents=True, exist_ok=True)
        # JSON Lines, appended durably per file BEFORE that file is mutated, so an
        # interruption always leaves a restorable entry for everything touched.
        manifest_path = args.state_dir / f"manifest-{stamp}.jsonl"
        manifest_f = open(manifest_path, "a", encoding="utf-8")
        interrupt = _DeferInterrupt()
        prev_handler = signal.signal(signal.SIGINT, interrupt)

    try:
        for p, joined, mbids in broken:
            if interrupt is not None and interrupt.flag:
                stopped_early = True
                break
            names = [resolver.name(m) for m in mbids]
            if any(n is None for n in names):
                print(f"  SKIP (unresolved MBID): {p}")
                skipped += 1
                continue
            if not args.apply:
                print(f"  {joined}\n    -> {names}")
                changed += 1
                continue
            adapter = Adapter(p)
            snap = adapter.snapshot()
            bkey = f"{abs(hash(str(p))):016x}-{p.name}.json"
            backup_path = (backups / bkey).resolve()
            _write_durable(backup_path, json.dumps(
                {"path": str(p), "snapshot": snap}, indent=1, ensure_ascii=False))
            # Record recovery metadata and flush it to disk BEFORE mutating the
            # audio file -- if write_album_artists() or the process dies next, the
            # manifest still points at a valid backup for this file.
            entry = {"path": str(p), "backup": str(backup_path), "was": joined, "now": names}
            manifest_f.write(json.dumps(entry, ensure_ascii=False) + "\n")
            manifest_f.flush()
            os.fsync(manifest_f.fileno())
            adapter.write_album_artists(names)
            changed += 1
    finally:
        if manifest_f is not None:
            manifest_f.close()
        if prev_handler is not None:
            signal.signal(signal.SIGINT, prev_handler)

    if args.apply:
        verb = "stopped early after" if stopped_early else "repaired"
        print(f"\n{verb} {changed}, skipped {skipped}")
        print(f"manifest: {manifest_path}")
        print(f"restore with: {sys.argv[0]} restore {manifest_path}")
    else:
        print(f"\nDRY RUN -- {changed} files would change, {skipped} skipped. "
              f"Re-run with --apply to write.")


def cmd_restore(args):
    entries = []
    with open(args.manifest, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:  # tolerate a torn final line from an interrupted apply
                entries.append(json.loads(line))
    for entry in entries:
        snap = json.loads(Path(entry["backup"]).read_text())["snapshot"]
        Adapter(Path(entry["path"])).restore(snap)
    print(f"restored {len(entries)} files")


# --------------------------------------------------------------------- main

def resolve_roots(cli_roots):
    if cli_roots:
        return cli_roots
    env = os.environ.get("MUSIC_ROOTS")
    if env:
        return [r for r in env.split(os.pathsep) if r]
    return []


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--root", action="append", dest="roots", metavar="DIR",
                    help="library root to scan (repeatable; or set MUSIC_ROOTS). Required "
                         "for scan/repair; no built-in default.")
    ap.add_argument("--state-dir", type=Path, default=Path("albumartist-repair"),
                    help="backups/manifests/cache dir (default: ./albumartist-repair)")
    ap.add_argument("--no-bootstrap", action="store_true",
                    help="do not auto-install a missing mutagen")
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("scan", help="report matching files; write nothing").set_defaults(fn=cmd_scan)
    rep = sub.add_parser("repair", help="repair matching files (dry run without --apply)")
    rep.add_argument("--apply", action="store_true", help="actually write (default: dry run)")
    rep.set_defaults(fn=cmd_repair)
    res = sub.add_parser("restore", help="roll a batch back from its manifest")
    res.add_argument("manifest")
    res.set_defaults(fn=cmd_restore)

    args = ap.parse_args()
    args.roots = resolve_roots(args.roots)
    if args.cmd in ("scan", "repair") and not args.roots:
        ap.error("no library roots given; pass --root DIR (repeatable) or set MUSIC_ROOTS")
    ensure_mutagen(allow_bootstrap=not args.no_bootstrap)
    try:
        args.fn(args)
    except KeyboardInterrupt:
        # repair --apply defers SIGINT to a file boundary and reports itself; this
        # is the clean exit for scan / dry-run / restore (and a forced second
        # Ctrl-C), instead of a traceback.
        print(f"\n{args.cmd} aborted.", file=sys.stderr)
        sys.exit(130)


if __name__ == "__main__":
    main()
