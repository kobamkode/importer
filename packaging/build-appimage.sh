#!/bin/sh
# Builds Kanistr-$version-x86_64.AppImage from a release binary.
# Needs cargo, and appimagetool on PATH or in APPIMAGETOOL.
set -eu
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
tool=${APPIMAGETOOL:-appimagetool}
work=$(mktemp -d)
appdir=$work/Kanistr.AppDir
trap 'rm -rf "$work"' EXIT

cargo build --release

install -Dm755 target/release/kanistr "$appdir/usr/bin/kanistr"
install -Dm755 packaging/AppRun "$appdir/AppRun"
install -Dm644 packaging/kanistr.desktop "$appdir/kanistr.desktop"
install -Dm644 packaging/kanistr.desktop "$appdir/usr/share/applications/kanistr.desktop"
install -Dm644 packaging/kanistr.png "$appdir/kanistr.png"
install -Dm644 packaging/kanistr.png \
    "$appdir/usr/share/icons/hicolor/512x512/apps/kanistr.png"

ARCH=x86_64 "$tool" --appimage-extract-and-run "$appdir" "Kanistr-$version-x86_64.AppImage"
