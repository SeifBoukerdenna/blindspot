#!/bin/bash
# `make install`: replaces ~/Applications/Blindspot.app with the build that was just signed and
# relaunches it. The bundle is moved, not copied, and its build path is unregistered, so exactly
# one Blindspot stays on disk: two copies indexing the same folders once kept the index
# permanently busy.
set -euo pipefail

app=${1:?usage: scripts/install.sh build/Blindspot.app}
target="$HOME/Applications/Blindspot.app"
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister

[[ -d $app ]] || { echo "error: $app is missing; run make sign first" >&2; exit 1; }
codesign --verify --deep --strict "$app"
source_path="$(cd "$(dirname "$app")" && pwd)/$(basename "$app")"
bundle_id=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app/Contents/Info.plist")
version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist")
running() { pgrep -f "Blindspot.app/Contents/MacOS/blindspot" >/dev/null; }

if running; then
    osascript -e "tell application id \"$bundle_id\" to quit" >/dev/null 2>&1 || true
    for _ in $(seq 1 50); do running || break; sleep 0.2; done
    if running; then
        pkill -f "Blindspot.app/Contents/MacOS/blindspot" || true
        sleep 1
    fi
fi

mkdir -p "$HOME/Applications"
rm -rf "$target"
mv "$app" "$target"
"$lsregister" -u "$source_path" >/dev/null 2>&1 || true
"$lsregister" -f "$target" >/dev/null 2>&1 || true

open "$target"
for _ in $(seq 1 50); do pgrep -f "$target/Contents/MacOS/blindspot" >/dev/null && break; sleep 0.2; done
pgrep -f "$target/Contents/MacOS/blindspot" >/dev/null || { echo "error: Blindspot did not start" >&2; exit 1; }
echo "Installed Blindspot $version at $target and relaunched it."
