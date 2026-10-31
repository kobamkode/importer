# Kanistr

A tool that copies photos and videos from mounted media into a dated folder tree.

## Language

**Import**:
Copying a file from a Source into the Destination, renamed with its Taken Date prefix. The Source is never modified.
_Avoid_: transfer, sync, move

**Source**:
The mounted card or folder media is read from during an import.
_Avoid_: input, card, media

**Destination**:
The root folder where imported files land, grouped by date.
_Avoid_: output, target, library

**Card reader**:
The physical USB device a card sits in. It presents the card as a mounted Source, but is not the Source itself.
_Avoid_: card reader as a synonym for Source

**Taken Date**:
The moment a photo or video was captured, as recorded in the file's own metadata.
_Avoid_: modified date, import date

**Settings**:
User-configured preferences that persist between runs: at minimum the Destination root and the date format used for folder names and file prefixes.
_Avoid_: preferences, config

**USB power fix**:
The opt-in Settings choice that, before an Import, temporarily disables kernel USB autosuspend so an idle card reader is not powered down and cannot vanish mid-Import. It runs with sudo only when the kernel still allows autosuspend, using whatever authentication sudo is configured for, and the change lasts until reboot.
_Avoid_: USB tweak, power workaround
