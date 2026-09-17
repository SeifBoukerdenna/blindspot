#!/usr/bin/env bash
# Compatibility entry point. No auto-formatting, builds or global configuration changes.
set -euo pipefail
hook_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$hook_dir/agent_hook.py" PostToolUse
