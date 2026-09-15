#!/usr/bin/env bash
# PostToolUse: keep the Swift shell honest after edits.
set -uo pipefail

input=$(cat)
file=$(printf '%s' "$input" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("tool_input",{}).get("file_path",""))' 2>/dev/null)

[[ "$file" == *.swift ]] || exit 0
[[ -f "$file" ]] || exit 0

if command -v swift-format >/dev/null 2>&1; then
  swift-format --in-place "$file" 2>/dev/null
fi

# Cheap syntax gate. A full xcodebuild here would be too slow for a per-edit hook —
# run that from /verify instead.
if ! out=$(swiftc -parse "$file" 2>&1); then
  echo "Swift does not parse: $file" >&2
  printf '%s\n' "$out" | head -30 >&2
  exit 2
fi

exit 0
