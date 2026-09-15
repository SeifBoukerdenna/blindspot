#!/bin/bash
# Renders the app icon (scripts/make-icon.swift) and packs shell/AppIcon.icns, plus media/icon.png
# for the README. Both outputs are committed; rerun only when the drawing changes.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

master="$work/icon-1024.png"
swift scripts/make-icon.swift "$master"

iconset="$work/AppIcon.iconset"
mkdir "$iconset"
# iconutil reads the iconset by these exact file names.
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$master" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z $((size * 2)) $((size * 2)) "$master" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil --convert icns "$iconset" --output shell/AppIcon.icns

mkdir -p media
sips -z 512 512 "$master" --out media/icon.png >/dev/null
echo "built shell/AppIcon.icns ($(du -h shell/AppIcon.icns | cut -f1)) and media/icon.png"
