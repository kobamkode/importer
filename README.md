# Importer

A Linux desktop app that copies photos and videos from a mounted Source into a dated Destination tree. The Source is never modified.

## Run

```bash
cargo run --release
```

Plug the card or USB drive in, choose a Destination in Settings, then press Import.

## What an Import does

- Finds media under `/run/media/$USER`, `/media/$USER`, and `/mnt`.
- Copies JPEG, HEIC, RAW (CR3, NEF, ARW, DNG), and MP4/MOV. `MISC/` folders and `._*` sidecars are ignored.
- Writes each file to `<Destination>/<DATE>/<DATE>_<original name>`.
- `DATE` comes from the Taken Date: EXIF `DateTimeOriginal`, video `CreateDate`, falling back to the file's modified time. Format tokens: `YYYY`, `MM`, `DD`, `HH`, `SS` (`MM` is month before the first `HH`, minute after).
- Never overwrites: identical content is skipped, different content gets a `-1` suffix before the extension.
- Copies up to the concurrency setting (1–10) at once. Each file is independent; one failure does not stop the run.

## Settings

Saved at `$XDG_CONFIG_HOME/importer/settings.conf` (default `~/.config/importer/settings.conf`) as flat `key=value` lines:

```ini
dest_root=/home/you/Pictures/Imports
date_format=YYYYMMDD
concurrency=3
usb_power_fix=false
```

The USB power fix is opt-in. Before an Import it disables kernel USB autosuspend so an idle card reader is not powered down and cannot vanish mid-Import. It runs with sudo only when the kernel still allows autosuspend, uses your usual sudo authentication, and lasts until reboot.

## Develop

```bash
cargo test   # core and integration tests, no GUI needed
cargo run
```
