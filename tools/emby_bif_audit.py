#!/usr/bin/env python3
"""
emby_bif_audit.py

Identifies Emby video items with missing chapter thumbnail ImageTags
(blank/missing trickplay frames), optionally refreshes their metadata
to reset Emby's internal processing state, and triggers the Video
Preview Thumbnail Extraction scheduled task.

Usage:
    # Dry run (report only, no API changes):
    python3 emby_bif_audit.py --api-key YOUR_KEY --dry-run

    # Audit and trigger extraction task:
    python3 emby_bif_audit.py --api-key YOUR_KEY

    # Custom server URL:
    python3 emby_bif_audit.py --server http://192.168.1.10:8096 --api-key YOUR_KEY

    # Force metadata refresh to reset trickplay state (dry run first):
    python3 emby_bif_audit.py --api-key YOUR_KEY --force-refresh --dry-run

    # Force refresh with slower delay for a loaded server:
    python3 emby_bif_audit.py --api-key YOUR_KEY --force-refresh --refresh-delay 0.5
"""

import argparse
import sys
import time
import requests

# ── Default config ────────────────────────────────────────────────────────────

DEFAULT_SERVER = "http://localhost:8096"
PAGE_SIZE = 500


# ── Emby API ──────────────────────────────────────────────────────────────────

def _auth_headers(api_key: str) -> dict:
    return {"X-Emby-Token": api_key}


def get_items(server: str, api_key: str) -> list[dict]:
    """
    Fetch all Episode items that have chapter data,
    including their Chapters and Path fields. Paginated.
    """
    items = []
    start_index = 0

    while True:
        resp = requests.get(
            f"{server}/emby/Items",
            headers=_auth_headers(api_key),
            params={
                "IncludeItemTypes": "Episode",
                "Recursive": "true",
                "Fields": "Chapters,Path",
                "HasChapters": "true",
                "StartIndex": start_index,
                "Limit": PAGE_SIZE,
            },
            timeout=30,
        )
        resp.raise_for_status()
        data = resp.json()

        batch = data.get("Items", [])
        items.extend(batch)

        total = data.get("TotalRecordCount", 0)
        start_index += len(batch)

        print(f"  Fetched {start_index}/{total} items...", end="\r", flush=True)

        if start_index >= total or not batch:
            break

    print()  # newline after progress line
    return items


def get_extraction_task_id(server: str, api_key: str) -> str | None:
    """Find the scheduled task ID for Video Preview Thumbnail Extraction."""
    resp = requests.get(
        f"{server}/emby/ScheduledTasks",
        headers=_auth_headers(api_key),
        timeout=10,
    )
    resp.raise_for_status()
    for task in resp.json():
        if "preview thumbnail" in task.get("Name", "").lower():
            return task["Id"]
    return None


def trigger_extraction_task(server: str, api_key: str, task_id: str) -> bool:
    """POST to trigger the extraction scheduled task. Returns True on success."""
    resp = requests.post(
        f"{server}/emby/ScheduledTasks/Running/{task_id}",
        headers=_auth_headers(api_key),
        timeout=10,
    )
    return resp.status_code == 204


def refresh_item(server: str, api_key: str, item_id: str) -> bool:
    """
    POST a metadata refresh for a single item to reset Emby's internal
    trickplay processing state. Uses conservative defaults to avoid
    replacing artwork or metadata.
    """
    resp = requests.post(
        f"{server}/emby/Items/{item_id}/Refresh",
        headers=_auth_headers(api_key),
        params={
            "MetadataRefreshMode": "Default",
            "ReplaceAllImages": "false",
            "ReplaceAllMetadata": "false",
        },
        timeout=30,
    )
    return resp.status_code in (200, 204)


# ── Detection ─────────────────────────────────────────────────────────────────

def missing_image_tag_count(item: dict) -> int:
    """Return the number of chapters missing an ImageTag."""
    return sum(1 for c in item.get("Chapters", []) if not c.get("ImageTag"))


# ── Main ──────────────────────────────────────────────────────────────────────

def _non_negative_float(value: str) -> float:
    v = float(value)
    if v < 0:
        raise argparse.ArgumentTypeError("must be >= 0")
    return v


def main():
    parser = argparse.ArgumentParser(
        description=(
            "Audit Emby items for missing chapter thumbnail ImageTags "
            "and trigger re-extraction."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    parser.add_argument(
        "--server",
        default=DEFAULT_SERVER,
        help=f"Emby server base URL (default: {DEFAULT_SERVER})",
    )
    parser.add_argument(
        "--api-key",
        required=True,
        help="Emby API key (from Dashboard → API Keys)",
    )
    parser.add_argument(
        "--force-refresh",
        action="store_true",
        help=(
            "POST a metadata refresh for each culprit item to reset Emby's "
            "internal trickplay processing state before triggering extraction."
        ),
    )
    parser.add_argument(
        "--refresh-delay",
        type=_non_negative_float,
        default=0.1,
        metavar="SECONDS",
        help=(
            "Delay in seconds between metadata refresh requests. "
            "Only used with --force-refresh. Default: 0.1"
        ),
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Report findings without making any API changes",
    )
    args = parser.parse_args()

    if args.refresh_delay != 0.1 and not args.force_refresh:
        parser.error("--refresh-delay requires --force-refresh")

    print(f"Server:  {args.server}")
    print(f"Dry run: {args.dry_run}")
    if args.force_refresh:
        print(f"Refresh: enabled (delay: {args.refresh_delay}s)")
    print()

    # ── Fetch items ───────────────────────────────────────────────────────────
    print("Fetching chaptered items from Emby...")
    try:
        items = get_items(args.server, args.api_key)
    except requests.RequestException as e:
        print(f"Error fetching items: {e}", file=sys.stderr)
        sys.exit(1)

    print(f"Fetched {len(items)} items with chapter data.")

    # ── Identify culprits ─────────────────────────────────────────────────────
    culprits = []
    for item in items:
        missing = missing_image_tag_count(item)
        if missing == 0:
            continue

        media_path = item.get("Path", "")
        total = len(item.get("Chapters", []))

        culprits.append({
            "id": item.get("Id", "?"),
            "name": item.get("Name", "Unknown"),
            "media_path": media_path,
            "missing": missing,
            "total": total,
        })

    if not culprits:
        print("No items with missing chapter thumbnails found. Nothing to do.")
        return

    # ── Report ────────────────────────────────────────────────────────────────
    print(f"\n{len(culprits)} item(s) with missing chapter ImageTags:\n")
    for c in culprits:
        print(
            f"  {c['missing']}/{c['total']} chapters missing  —  "
            f"{c['name']}"
        )
        print(f"    {c['media_path']}")

    if args.dry_run:
        if args.force_refresh:
            print(f"\n{len(culprits)} item(s) would be refreshed (--force-refresh).")
        print("\nDry-run mode — no refreshes sent, no tasks triggered.")
        return

    # ── Force-refresh metadata ────────────────────────────────────────────────
    if args.force_refresh:
        total = len(culprits)
        failed = 0
        print(f"\nRefreshing metadata for {total} item(s) to reset trickplay state...")
        for i, c in enumerate(culprits, 1):
            print(f"  Refreshing item {i}/{total}...", end="\r", flush=True)
            try:
                ok = refresh_item(args.server, args.api_key, c["id"])
                if not ok:
                    failed += 1
                    print(f"\n  ⚠ Refresh failed for: {c['name']} (ID: {c['id']})")
            except requests.RequestException as e:
                failed += 1
                print(f"\n  ⚠ Refresh error for: {c['name']} (ID: {c['id']}): {e}")
            if i < total:
                time.sleep(args.refresh_delay)
        print(f"  Refreshed {total - failed}/{total} item(s)"
              f"{f', {failed} failed' if failed else ''}.")
        print("\nWaiting 5 seconds for server to process refresh queue...")
        time.sleep(5)

    # ── Trigger extraction task ───────────────────────────────────────────────
    print("\nLooking up 'Video Preview Thumbnail Extraction' scheduled task...")
    try:
        task_id = get_extraction_task_id(args.server, args.api_key)
    except requests.RequestException as e:
        print(f"Error fetching scheduled tasks: {e}", file=sys.stderr)
        sys.exit(1)

    if not task_id:
        print(
            "Could not find the extraction task by name.\n"
            "Trigger 'Video Preview Thumbnail Extraction' manually from the Emby dashboard."
        )
        return

    try:
        success = trigger_extraction_task(args.server, args.api_key, task_id)
    except requests.RequestException as e:
        print(f"Error triggering extraction task: {e}", file=sys.stderr)
        sys.exit(1)
    if success:
        print(f"✓ Extraction task triggered successfully (ID: {task_id})")
    else:
        print(
            f"✗ Failed to trigger task (ID: {task_id})\n"
            "The task may already be running, or check your server logs."
        )


if __name__ == "__main__":
    main()
