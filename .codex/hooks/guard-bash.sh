#!/usr/bin/env bash
# PreToolUse on Bash. Exit 2 blocks the call and returns stderr to Claude.
set -uo pipefail

input=$(cat)
cmd=$(printf '%s' "$input" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("tool_input",{}).get("command",""))' 2>/dev/null)

block() { echo "BLOCKED: $1" >&2; exit 2; }

case "$cmd" in
  *"rm -rf /"*|*"rm -rf ~"*|*"rm -fr /"*)
    block "recursive delete of a root or home path." ;;
  *"git push --force"*|*"git push -f"*)
    block "force push. Push normally, or ask me first." ;;
  *"git reset --hard"*)
    block "hard reset discards uncommitted work. Stash instead, or ask me." ;;
  *"tccutil reset"*)
    block "TCC reset wipes granted permissions. I'll run this by hand when I want it." ;;
  *"defaults write com.apple.symbolichotkeys"*)
    block "do not touch Spotlight's hotkey binding. That is a manual user step by design." ;;
esac

exit 0
