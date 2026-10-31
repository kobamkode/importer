#!/bin/sh
# Builds Kanistr.app and Kanistr-<version>-<arch>.dmg from a native release
# build. Runs on macOS only (uses sips, iconutil, hdiutil).
set -eu
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
arch=$(uname -m) # arm64 or x86_64
appdir=Kanistr.app

cargo build --release

rm -rf "$appdir"
mkdir -p "$appdir/Contents/MacOS" "$appdir/Contents/Resources"
cp target/release/kanistr "$appdir/Contents/MacOS/kanistr"
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$appdir/Contents/Info.plist"

iconset=$(mktemp -d)/kanistr.iconset
mkdir -p "$iconset"
for spec in \
    "16x16 16" "16x16@2x 32" \
    "32x32 32" "32x32@2x 64" \
    "128x128 128" "128x128@2x 256" \
    "256x256 256" "256x256@2x 512" \
    "512x512 512" "512x512@2x 1024"; do
    name=${spec% *}
    size=${spec#* }
    sips -z "$size" "$size" packaging/kanistr-1024.png \
        --out "$iconset/icon_$name.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$appdir/Contents/Resources/kanistr.icns"

hdiutil create -volname Kanistr -srcfolder "$appdir" -ov -format UDZO \
    "Kanistr-$version-$arch.dmg"
