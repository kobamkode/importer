# Kanistr

Copies photos and videos from a mounted card into a dated folder tree. It only reads the card.

## Download

Prebuilt binaries are on the [releases page](https://github.com/kobamkode/kanistr/releases):

- **Linux** — `Kanistr-<version>-x86_64.AppImage`
- **Windows** — `Kanistr-<version>-windows-x86_64.exe`
- **macOS** — `Kanistr-<version>-arm64.dmg` (Apple Silicon) or `Kanistr-<version>-x86_64.dmg` (Intel)

The Windows and macOS builds are unsigned: expect a SmartScreen warning on Windows, and right-click → Open the first time on macOS.

## Run from source

```bash
cargo run --release
```

Plug the card in, set the destination folder and date format in Settings, press Import.

## What it does

It scans the places removable media appears on each system — `/run/media/<user>`, `/media/<user>` and `/mnt` on Linux, `/Volumes` on macOS, removable drive letters on Windows — for JPEG, HEIC, CR3, NEF, ARW, DNG, RAF, MP4 and MOV files. Folders named `MISC` and files starting with `._` are ignored.

Each file lands at `<destination>/<date>/<date>_<original name>`. The date is the photo's EXIF `DateTimeOriginal`, the video's `CreateDate`, or the file's modified time when neither is present. Format tokens are `YYYY`, `MM`, `DD`, `HH` and `SS`. `MM` is the month until the first `HH`, then minutes.

Nothing gets overwritten. An identical file is skipped; a different file with the same name gets a `-1` suffix.

Files copy in parallel, 1 to 10 at a time, set in Settings.

## Settings

Settings are stored in `kanistr/settings.conf` under the platform's config folder — `$XDG_CONFIG_HOME` (or `~/.config`) on Linux, `~/Library/Application Support` on macOS, `%APPDATA%` on Windows. On Linux the file looks like:

```ini
dest_root=/home/you/Pictures/Imports
date_format=YYYYMMDD
concurrency=3
usb_power_fix=false
```

The USB power fix is Linux-only and off by default. When on, it disables USB autosuspend before each import so an idle card reader does not drop off mid-copy. It runs only when needed, asks for authentication with sudo or polkit (whichever the launch allows), and lasts until reboot.

## Develop

```bash
cargo test
```
