#!/usr/bin/env python3
"""Optional Codex tripwires, not a sandbox. No formatting, builds or permission changes."""

import json
from pathlib import Path
import re
import shlex
import sys

ROOT = Path(__file__).resolve().parents[2]
LIMIT = 1024 * 1024


def edit_paths(payload):
    value = payload.get("tool_input", {})
    if not isinstance(value, dict):
        return []
    paths = []
    if isinstance(value.get("file_path"), str):
        paths.append(value["file_path"])
    command = value.get("command", "")
    if isinstance(command, str):
        paths.extend(re.findall(r"^\*\*\* (?:Add File|Update File|Delete File|Move to): (.+)$", command, re.M))
    return paths


def path_reason(name, cwd, root):
    path = Path(name)
    path = (cwd / path).resolve() if not path.is_absolute() else path.resolve()
    parts = {p.lower() for p in path.parts}
    leaf = path.name.lower()
    if leaf == ".env" or leaf.startswith(".env.") or leaf.endswith((".p12", ".mobileprovision")) or "id_rsa" in leaf or "secrets" in parts:
        return "Credential/signing files are not agent-editable. Ask the user to handle them."
    if not path.is_relative_to(root):
        return "The edit leaves this repository. Confirm its scope with the user first."
    relative = path.relative_to(root)
    if any(p.lower() in ("target", "build", ".build", "deriveddata") for p in relative.parts):
        return "Edit source files, not generated build artifacts."
    if str(relative) == "include/blindspot.h":
        return "The ABI header is generated. Edit Rust FFI sources and run make header."
    return None


def bash_reason(command, root):
    # Deliberately a small tripwire, not a shell interpreter. Never emits an allow
    # decision: all ordinary Codex permissions remain in effect.
    try:
        lexer = shlex.shlex(command, posix=True, punctuation_chars=";&|()\n")
        lexer.whitespace = " \t\r"
        lexer.whitespace_split = True
        tokens = list(lexer)
    except ValueError:
        return None
    segments, current = [], []
    for token in tokens + [";"]:
        if token and all(c in ";&|()\n" for c in token):
            if current:
                segments.append(current)
            current = []
        else:
            current.append(token)
    for words in segments:
        # Skip common wrappers and variable assignments; this does not attempt expansions.
        while words and (words[0] in ("sudo", "command", "env") or re.match(r"^\w+=", words[0])):
            words = words[1:]
        if not words:
            continue
        executable = Path(words[0]).name
        args = words[1:]
        if executable == "git":
            while args and args[0].startswith("-"):
                args = args[2:] if args[0] in ("-C", "-c", "--git-dir", "--work-tree") else args[1:]
            if args and args[0] in ("add", "commit", "push", "tag", "reset", "clean", "stash"):
                return "Git staging, history/publication and destructive worktree operations belong to the user."
            if args and args[0] in ("checkout", "restore") and "--" in args:
                return "Do not discard existing work with checkout/restore."
        if executable == "release.sh" or (executable in ("bash", "sh", "zsh") and any(Path(a).name == "release.sh" for a in args)):
            return "scripts/release.sh publishes; only the user runs it. Use make agent-deliver for local delivery."
        if executable == "tccutil" and "reset" in args:
            return "Never reset TCC to unblock tests."
        if executable == "defaults" and "write" in args and "com.apple.symbolichotkeys" in args:
            return "Changing Spotlight's system shortcut is a manual user operation."
        if executable == "rm" and any(a in ("--recursive",) or (a.startswith("-") and "r" in a.lower()) for a in args):
            broad = {"/", ".", "..", "~", "$HOME", "${HOME}", str(Path.home()), str(root)}
            if any(a.rstrip("/") in {p.rstrip("/") for p in broad} for a in args):
                return "Recursive removal of a root, home or workspace is forbidden."
    return None


def evaluate(event, payload, root=ROOT):
    root = root.resolve()
    if event == "SessionStart":
        return {"hookSpecificOutput": {"hookEventName": event, "additionalContext":
            "Blindspot: read AGENTS.md; run make agent-context; read the relevant docs/agent-map.md "
            "section and the task note selected by the user in docs/tasks/. Reconcile notes with "
            "the current diff. Preserve existing edits. Use make agent-check; app delivery is "
            "make agent-deliver with rollback. No commits, pushes, tags or publication. "
            "These hooks do not establish that checks passed."}}
    if event == "PostToolUse":
        return {"hookSpecificOutput": {"hookEventName": event, "additionalContext":
            "Legacy post-edit hook: no files formatted or checks run. Verify the completed slice "
            "with make agent-check and report the actual results."}} if edit_paths(payload) else {}
    if event != "PreToolUse":
        return {}
    cwd = Path(payload.get("cwd") or root).resolve()
    reason = None
    for name in edit_paths(payload):
        reason = path_reason(name, cwd, root)
        if reason:
            break
    value = payload.get("tool_input", {})
    if not reason and payload.get("tool_name") in ("Bash", "bash", "exec_command") and isinstance(value, dict):
        command = value.get("command", value.get("cmd", ""))
        if isinstance(command, str):
            reason = bash_reason(command, root)
    if reason:
        return {"hookSpecificOutput": {"hookEventName": event, "permissionDecision": "deny", "permissionDecisionReason": reason}}
    return {}


def main():
    raw = sys.stdin.read(LIMIT + 1)
    try:
        if len(raw) > LIMIT:
            raise ValueError("payload too large")
        payload = json.loads(raw)
        if not isinstance(payload, dict):
            raise ValueError("expected an object")
        event = sys.argv[1] if len(sys.argv) > 1 else payload.get("hook_event_name", "")
        result = evaluate(event, payload)
    except (ValueError, TypeError, OSError):
        # Don't print raw payloads (which may contain private content).
        print("Hook input could not be checked; no verification or permission approval was granted.", file=sys.stderr)
        return 1
    if result:
        print(json.dumps(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
