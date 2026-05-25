#!/usr/bin/env bash
COMICS_DIR="${1:-/mnt/user/Downloads/Finished/Comics}"

python3 - "$COMICS_DIR" <<'PY'
import os
import sys
import zipfile
import xml.etree.ElementTree as ET

comics_dir = sys.argv[1]

total = 0
has_rating = 0
no_rating = 0
no_comicinfo = 0
cbr_skipped = 0   # .cbr is RAR, not ZIP; not supported by stdlib
errors = 0       # processing errors (permissions, corrupt, encoding...)

for root, dirs, files in os.walk(comics_dir):
    for f in files:
        ext = f.lower()
        if not ext.endswith((".cbz", ".cbr")):
            continue
        if ext.endswith(".cbr"):
            cbr_skipped += 1
            continue
        total += 1
        path = os.path.join(root, f)
        try:
            with zipfile.ZipFile(path) as z:
                if "ComicInfo.xml" not in z.namelist():
                    no_comicinfo += 1
                    continue
                xml = z.read("ComicInfo.xml").decode("utf-8")
                tree = ET.fromstring(xml)
                age = tree.findtext("AgeRating")
                if age and age.lower() not in ("", "unknown", "rating pending"):
                    has_rating += 1
                else:
                    no_rating += 1
        except (zipfile.BadZipFile, ET.ParseError, OSError, UnicodeDecodeError) as exc:
            errors += 1
            print(f"  error: {path}: {exc}", file=sys.stderr)

print(f"Total CBZ files:    {total}")
print(f"Has age rating:     {has_rating} ({100 * has_rating // max(total, 1)}%)")
print(f"No/unknown rating:  {no_rating}")
print(f"No ComicInfo.xml:   {no_comicinfo}")
print(f"Errors:             {errors}")
print(f"CBR skipped (RAR):  {cbr_skipped}")
PY
