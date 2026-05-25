#!/usr/bin/env bash
#
# clean-broken-albums.sh
# Removes audio files from directories listed in broken.txt,
# preserving lyric (.lrc) and art (.jpg, .png, etc.) sidecars.
# Optionally triggers targeted Lidarr rescans per album.
#
# Usage:
#   ./clean-broken-albums.sh [--dry-run] [--lidarr-rescan] [--input FILE]
#
# Environment:
#   LIDARR_URL          Base URL for Lidarr (e.g. http://localhost:8686)
#   LIDARR_API_KEY      Lidarr API key (Settings > General)
#   LIDARR_REMOTE_PATH  Lidarr's root path prefix (default: /share)
#   LIDARR_LOCAL_PATH    Host path prefix to replace (default: /mnt/user)

set -euo pipefail

# Audio extensions to remove (case-insensitive via find -iname)
AUDIO_EXTENSIONS=(
    flac mp3 m4a ogg opus wma wav aac alac dsf dff wv ape mpc m4b
)

DRY_RUN=false
LIDARR_RESCAN=false
LIDARR_ONLY=false
INPUT_FILE="broken.txt"
LIDARR_REMOTE_PATH="${LIDARR_REMOTE_PATH:-/share}"
LIDARR_LOCAL_PATH="${LIDARR_LOCAL_PATH:-/mnt/user}"

usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Remove audio files from broken album directories, preserving sidecars.

Options:
  --dry-run         Show what would be deleted without deleting
  --lidarr-rescan   Trigger targeted Lidarr rescan + search per album
                    (requires LIDARR_URL and LIDARR_API_KEY)
  --lidarr-only     Skip file deletion, only trigger Lidarr rescan + search
  --input FILE      Path to directory list (default: broken.txt)
  -h, --help        Show this help

Environment:
  LIDARR_URL          Lidarr base URL
  LIDARR_API_KEY      Lidarr API key
  LIDARR_REMOTE_PATH  Lidarr's path prefix (default: /share)
  LIDARR_LOCAL_PATH   Host path prefix to map from (default: /mnt/user)
EOF
    exit 0
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run)       DRY_RUN=true; shift ;;
        --lidarr-rescan) LIDARR_RESCAN=true; shift ;;
        --lidarr-only)   LIDARR_ONLY=true; LIDARR_RESCAN=true; shift ;;
        --input)         INPUT_FILE="$2"; shift 2 ;;
        -h|--help)       usage ;;
        *)               echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

if [[ ! -f "$INPUT_FILE" ]]; then
    echo "Error: input file '$INPUT_FILE' not found" >&2
    exit 1
fi

if $LIDARR_RESCAN; then
    if [[ -z "${LIDARR_URL:-}" || -z "${LIDARR_API_KEY:-}" ]]; then
        echo "Error: --lidarr-rescan requires LIDARR_URL and LIDARR_API_KEY" >&2
        exit 1
    fi
fi

# --- Lidarr API helpers ---

lidarr_api() {
    local method="$1" endpoint="$2" data="${3:-}"
    local args=(
        -s -X "$method"
        "${LIDARR_URL}/api/v1${endpoint}"
        -H "X-Api-Key: ${LIDARR_API_KEY}"
        -H "Content-Type: application/json"
    )
    if [[ -n "$data" ]]; then
        args+=(-d "$data")
    fi
    curl "${args[@]}"
}

# Convert host path to Lidarr path: /mnt/user/Music/... -> /share/Music/...
to_lidarr_path() {
    local host_path="$1"
    echo "${host_path/#${LIDARR_LOCAL_PATH}/${LIDARR_REMOTE_PATH}}"
}

# Cache Lidarr artists by path for matching
declare -A ARTIST_ID_BY_PATH=()
declare -A ARTIST_NAME_BY_PATH=()

# Track which artists we've already triggered (avoid duplicate API calls)
declare -A ARTIST_REFRESHED=()

load_lidarr_artists() {
    echo "Fetching artist list from Lidarr..."
    local artists
    artists=$(lidarr_api GET "/artist")

    while IFS=$'\t' read -r artist_id artist_path artist_name; do
        artist_path="${artist_path%/}"
        ARTIST_ID_BY_PATH["$artist_path"]="$artist_id"
        ARTIST_NAME_BY_PATH["$artist_path"]="$artist_name"
    done < <(echo "$artists" | jq -r '.[] | [.id, .path, .artistName] | @tsv')

    echo "Loaded ${#ARTIST_ID_BY_PATH[@]} artists from Lidarr"
}

refresh_and_search_artist() {
    local artist_id="$1" artist_name="$2"

    # Skip if already refreshed this artist
    if [[ -n "${ARTIST_REFRESHED[$artist_id]:-}" ]]; then
        return
    fi
    ARTIST_REFRESHED["$artist_id"]=1

    # RefreshArtist rescans disk for this artist
    if ! lidarr_api POST "/command" \
        "{\"name\": \"RefreshArtist\", \"artistId\": ${artist_id}}" \
        >/dev/null 2>&1; then
        echo "  [lidarr] WARNING: RefreshArtist failed for $artist_name (id=$artist_id)" >&2
        ((LIDARR_FAILURES++)) || true
    fi

    # MissingAlbumSearch triggers download for any missing albums
    if ! lidarr_api POST "/command" \
        "{\"name\": \"MissingAlbumSearch\", \"artistId\": ${artist_id}}" \
        >/dev/null 2>&1; then
        echo "  [lidarr] WARNING: MissingAlbumSearch failed for $artist_name (id=$artist_id)" >&2
        ((LIDARR_FAILURES++)) || true
    fi

    echo "  [lidarr] queued refresh + missing search for: $artist_name (id=$artist_id)"
}

# --- File deletion ---

# Build the find predicates once, as an array. Array form (not a flat string +
# eval) is required so paths in broken.txt with shell metacharacters can never
# be evaluated as commands.
FIND_ARGS=()
for i in "${!AUDIO_EXTENSIONS[@]}"; do
    if [[ $i -gt 0 ]]; then
        FIND_ARGS+=(-o)
    fi
    FIND_ARGS+=(-iname "*.${AUDIO_EXTENSIONS[$i]}")
done

LIDARR_FAILURES=0

# Pre-load Lidarr data if we'll need it
if $LIDARR_RESCAN; then
    load_lidarr_artists
fi

total_dirs=0
total_files=0
skipped_dirs=0
lidarr_matched=0
lidarr_missed=0

while IFS= read -r dir; do
    [[ -z "$dir" ]] && continue
    dir="${dir%/}"

    ((total_dirs++)) || true

    if ! $LIDARR_ONLY; then
        if [[ ! -d "$dir" ]]; then
            echo "SKIP (not found): $dir"
            ((skipped_dirs++)) || true
            continue
        fi

        mapfile -t audio_files < <(find "$dir" -type f \( "${FIND_ARGS[@]}" \))

        if [[ ${#audio_files[@]} -eq 0 ]]; then
            echo "SKIP (no audio): $dir"
            continue
        fi

        echo "--- $dir (${#audio_files[@]} audio files)"

        for f in "${audio_files[@]}"; do
            if $DRY_RUN; then
                echo "  [dry-run] would delete: $(basename "$f")"
            else
                rm -- "$f"
                echo "  deleted: $(basename "$f")"
            fi
            ((total_files++)) || true
        done
    fi

    # Lidarr targeted refresh + search at the artist level
    if $LIDARR_RESCAN && ! $DRY_RUN; then
        # Extract artist path: /mnt/user/Music/Artist/Album -> /share/Music/Artist
        artist_local="${dir%/*}"
        artist_lidarr=$(to_lidarr_path "$artist_local")
        artist_id="${ARTIST_ID_BY_PATH[$artist_lidarr]:-}"

        if [[ -n "$artist_id" ]]; then
            artist_name="${ARTIST_NAME_BY_PATH[$artist_lidarr]}"
            refresh_and_search_artist "$artist_id" "$artist_name"
            ((lidarr_matched++)) || true
        else
            echo "  [lidarr] WARNING: no artist found for path: $artist_lidarr"
            ((lidarr_missed++)) || true
        fi
    fi
done < "$INPUT_FILE"

echo ""
if $LIDARR_ONLY; then
    echo "Lidarr-only: processed $total_dirs directories"
    echo "Lidarr: $lidarr_matched albums queued for rescan+search, $lidarr_missed not found in Lidarr"
elif $DRY_RUN; then
    echo "DRY RUN complete: $total_files audio files would be removed across $total_dirs directories ($skipped_dirs skipped)"
    if $LIDARR_RESCAN; then
        echo "(Lidarr rescans skipped in dry-run mode)"
    fi
else
    echo "Done: $total_files audio files removed across $total_dirs directories ($skipped_dirs skipped)"
    if $LIDARR_RESCAN; then
        echo "Lidarr: $lidarr_matched albums queued for rescan+search, $lidarr_missed not found in Lidarr"
    fi
fi

if (( LIDARR_FAILURES > 0 )); then
    echo "Lidarr: $LIDARR_FAILURES API call(s) failed (see warnings above)" >&2
    exit 1
fi
