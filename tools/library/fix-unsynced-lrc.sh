#!/usr/bin/env bash
#
# fix-unsynced-lrc.sh
# Finds .lrc files that contain no LRC timestamps (i.e. unsynced/plain-text
# lyrics incorrectly saved by mxlrc-go) and renames them to .txt.
#
# A synced LRC file contains at least one line matching [mm:ss.xx].
# Any .lrc without that pattern is treated as unsynced and renamed.
#
# Usage:
#   ./fix-unsynced-lrc.sh [--dry-run] [--dir PATH]
#
# Options:
#   --dry-run     Show what would be renamed without renaming
#   --dir PATH    Root directory to search (default: current directory)

set -euo pipefail

DRY_RUN=false
SEARCH_DIR="."

usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Rename unsynced .lrc files (no timestamps) to .txt.

Options:
  --dry-run     Show what would be renamed without renaming
  --dir PATH    Root directory to search (default: current directory)
  -h, --help    Show this help
EOF
    exit 0
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run)  DRY_RUN=true; shift ;;
        --dir)      SEARCH_DIR="$2"; shift 2 ;;
        -h|--help)  usage ;;
        *)          echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

if [[ ! -d "$SEARCH_DIR" ]]; then
    echo "Error: directory '$SEARCH_DIR' not found" >&2
    exit 1
fi

# LRC timestamp pattern: [mm:ss.xx] or [mm:ss:xx] (various mxlrc variants)
LRC_TIMESTAMP_PATTERN='\[[0-9]+:[0-9]+[.:][0-9]+\]'

total=0
skipped=0

while IFS= read -r -d '' lrc_file; do
    # If the file contains at least one timestamp line, it's synced — skip it
    if grep -qP "$LRC_TIMESTAMP_PATTERN" "$lrc_file" 2>/dev/null; then
        continue
    fi

    txt_file="${lrc_file%.lrc}.txt"

    if [[ -e "$txt_file" ]]; then
        echo "SKIP (target exists): $lrc_file -> $(basename "$txt_file")"
        ((skipped++)) || true
        continue
    fi

    if $DRY_RUN; then
        echo "[dry-run] $lrc_file -> $(basename "$txt_file")"
    else
        mv -- "$lrc_file" "$txt_file"
        echo "renamed: $lrc_file -> $(basename "$txt_file")"
    fi
    ((total++)) || true

done < <(find "$SEARCH_DIR" -type f -iname "*.lrc" -print0)

echo ""
if $DRY_RUN; then
    echo "DRY RUN: $total unsynced .lrc files would be renamed ($skipped skipped — .txt already exists)"
else
    echo "Done: $total unsynced .lrc files renamed ($skipped skipped — .txt already exists)"
fi
