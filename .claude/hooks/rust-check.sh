#!/usr/bin/env bash
# PostToolUse: format + lint Rust after Claude edits a .rs file.
# Exit 2 feeds stderr back to Claude as feedback it must address.
set -uo pipefail

input=$(cat)
file=$(printf '%s' "$input" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("tool_input",{}).get("file_path",""))' 2>/dev/null)

[[ "$file" == *.rs ]] || exit 0
[[ -f "$file" ]] || exit 0

cd "$CLAUDE_PROJECT_DIR/core" || exit 0

rustfmt --edition 2024 "$file" 2>/dev/null

# Only surface problems in the crate we just touched. Keep it fast.
if ! out=$(cargo clippy --all-targets --message-format=short -- -D warnings 2>&1); then
  echo "clippy failed on $file:" >&2
  # Trim to the first 40 lines so we don't dump a wall of text into context.
  printf '%s\n' "$out" | grep -E '^(error|warning)' | head -40 >&2
  exit 2
fi

exit 0
