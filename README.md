# Kanistr

Copies photos and videos from a mounted card into a dated folder tree. It only reads the card.

## Run

```bash
cargo run --release
```

Plug the card in, set the destination folder and date format in Settings, press Import.

## What it does

It scans `/run/media/<user>`, `/media/<user>` and `/mnt` for JPEG, HEIC, CR3, NEF, ARW, DNG, MP4 and MOV files. Folders named `MISC` and files starting with `._` are ignored.

Each file lands at `<destination>/<date>/<date>_<original name>`. The date is the photo's EXIF `DateTimeOriginal`, the video's `CreateDate`, or the file's modified time when neither is present. Format tokens are `YYYY`, `MM`, `DD`, `HH` and `SS`. `MM` is the month until the first `HH`, then minutes.

Nothing gets overwritten. An identical file is skipped; a different file with the same name gets a `-1` suffix.

Files copy in parallel, 1 to 10 at a time, set in Settings.

## Settings

Settings are stored in `$XDG_CONFIG_HOME/kanistr/settings.conf`, or `~/.config/kanistr/settings.conf` when that variable is unset:

```ini
dest_root=/home/you/Pictures/Imports
date_format=YYYYMMDD
concurrency=3
usb_power_fix=false
```

The USB power fix is off by default. When on, it disables USB autosuspend before each import so an idle card reader does not drop off mid-copy. It runs only when needed, uses whatever authentication sudo is set up for, and lasts until reboot.

## Develop

```bash
cargo test
```
