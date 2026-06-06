# ImportLidarrManual - setup

## macOS

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

1. Install git (one time) via un-get / NerdTools:

   ```bash
   un-get update && un-get install git
   ```

2. Clone onto a persistent array path (use any persistent array path; on this host anything under `/mnt/vms` persists):

   ```bash
   git clone <repo-url> /mnt/vms/dockerappdata/media-automation
   chmod +x /mnt/vms/dockerappdata/media-automation/tools/lidarr/ImportLidarrManual.py
   ```

3. Run it once; the dependency preflight detects missing tools, offers to
   install them, and (after reinstalling pip libs) offers to write a boot script
   that reinstalls them and recreates the `il` symlink on every boot:

   ```bash
   python3 /mnt/vms/dockerappdata/media-automation/tools/lidarr/ImportLidarrManual.py /path/to/music
   ```

   - If the User Scripts plugin is installed, it writes
     `.../user.scripts/scripts/importlidarr-boot/script` - set it to
     "At Startup of Array" once in the User Scripts UI.
   - Otherwise it appends an idempotent block to `/boot/config/go`.

4. `il` then works from any path (it is symlinked into `/usr/local/bin`).

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
