#!/usr/bin/env python3
"""Repository-local context, diagnostics and verification. Python 3.9+, no dependencies."""

import argparse
import ast
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import signal
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True

SCOPES = ("auto", "docs", "tooling", "rust", "ui", "index", "helpers", "release")
GUIDES = ("AGENTS.md", "docs/agent-map.md", "docs/agent-workflow.md", "docs/tasks/README.md")
PYTHON_SOURCES = ("scripts/agent.py", "scripts/agent_delivery.py", "bench/AgentWorkflowTests.py",
                  ".codex/hooks/agent_hook.py")


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args], stderr=subprocess.PIPE)


def repo_root():
    return Path(git(Path(__file__).resolve().parent, "rev-parse", "--show-toplevel").decode().strip())


def version(root):
    match = re.search(r"^VERSION\s*:=\s*(\d+\.\d+\.\d+)\s*$", (root / "Makefile").read_text(), re.M)
    if not match:
        raise ValueError("Makefile must contain one explicit X.Y.Z VERSION")
    return match[1]


def changed_paths(root, base=None):
    if base:
        base = git(root, "rev-parse", "--verify", "--end-of-options", base + "^{commit}").decode().strip()
    else:
        try:
            base = git(root, "rev-parse", "--verify", "HEAD").decode().strip()
        except subprocess.CalledProcessError:
            base = None
    tracked = git(root, "diff", "--name-only", "--no-renames", "-z", base, "--") if base else git(root, "ls-files", "-z")
    others = git(root, "ls-files", "--others", "--exclude-standard", "-z")
    return sorted(set(os.fsdecode(p) for p in (tracked + others).split(b"\0") if p))


def fingerprint(root):
    digest = hashlib.sha256()
    files = git(root, "ls-files", "-z", "--cached", "--others", "--exclude-standard")
    for name in sorted(set(files.split(b"\0")) - {b""}):
        path = root / os.fsdecode(name)
        digest.update(name + b"\0")
        try:
            info = path.lstat()
            digest.update(str(stat.S_IMODE(info.st_mode)).encode() + b"\0")
            if path.is_symlink():
                digest.update(os.fsencode(os.readlink(path)))
            elif path.is_file():
                with path.open("rb") as stream:
                    for block in iter(lambda: stream.read(1024 * 1024), b""):
                        digest.update(block)
            else:
                digest.update(b"not-a-file")
        except FileNotFoundError:
            digest.update(b"deleted")
        digest.update(b"\0")
    return digest.hexdigest()


def select_scopes(paths, requested="auto"):
    if requested not in SCOPES:
        raise ValueError("Unknown scope: " + requested)
    if requested != "auto":
        return {requested}
    if not paths:
        raise ValueError("No changes detected. Set SCOPE explicitly or BASE to the revision being compared.")
    scopes = set()
    for path in paths:
        if path in ("Makefile", "Cargo.toml", "Cargo.lock") or path.startswith((".github/", "include/")):
            scopes.add("release")
        elif path.startswith(("scripts/", ".codex/", ".agents/")) or path == "bench/AgentWorkflowTests.py":
            scopes.add("tooling")
        elif path.startswith(("core/src/content", "core/src/semantic")):
            scopes.add("index")
        elif path.startswith("core/src/ffi") or path == "shell/Bridge.swift":
            scopes.update(("rust", "ui", "index"))
        elif path.startswith(("core/", "crates/")):
            scopes.add("rust")
        elif path.startswith("shell/"):
            scopes.add("ui")
            if any(word in path for word in ("ContentWatcher", "IndexDashboard", "Settings")):
                scopes.add("index")
        elif path.startswith("helpers/"):
            scopes.add("helpers")
        elif path.startswith("bench/"):
            scopes.add("release")
        elif path.endswith(".md") or path.startswith(("docs/", "media/")) or path == ".gitignore":
            scopes.add("docs")
        else:
            scopes.add("release")
    return scopes


def app_changes(paths):
    return any(p == "Makefile" or p.startswith(("core/", "crates/", "shell/", "helpers/", "include/", "Cargo")) for p in paths)


def check_plan(scopes):
    scopes = set(scopes)
    if not scopes or not scopes <= set(SCOPES) - {"auto"}:
        raise ValueError("Check plan needs explicit, recognized scopes")
    if "release" in scopes:
        scopes.update(("tooling", "rust", "ui", "index", "helpers"))
    if "index" in scopes:
        scopes.update(("rust", "ui", "helpers"))
    commands = [[sys.executable, "scripts/agent.py", "lint"], ["git", "diff", "--check"]]
    if scopes & {"ui", "helpers"}:
        commands.append(["make", "app"])
    elif "rust" in scopes:
        commands.append(["make", "core"])
    if "tooling" in scopes:
        commands.append([sys.executable, "bench/AgentWorkflowTests.py"])
    if "rust" in scopes:
        commands.append(["make", "check-header", "check", "test", "test-retrieval"])
    if "ui" in scopes:
        commands.append(["make", "test-actions", "smoke-panel", "test-index-dashboard"])
    if "index" in scopes:
        commands.append(["make", "test-content"])
    if "helpers" in scopes:
        commands.append(["make", "test-passages", "test-semantic", "test-vectors"])
    if "release" in scopes:
        commands.append(["make", "test-updater"])
    return commands


def output_dir(root):
    for part in (root / "build", root / "build/agent"):
        if part.is_symlink():
            raise ValueError("Refusing a symlinked build/report directory")
        part.mkdir(exist_ok=True)
    return root / "build/agent"


def write_json(path, value):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


class Runner:
    def __init__(self, root, directory):
        self.root, self.directory = root, directory
        self.steps = []

    def run(self, command, timeout=1200):
        command = list(map(str, command))
        print("Running: " + shlex.join(command), flush=True)
        started = time.monotonic()
        environment = os.environ.copy()
        for key in ("MAKEFLAGS", "MFLAGS", "MAKELEVEL", "MAKEOVERRIDES"):
            environment.pop(key, None)
        environment["PYTHONDONTWRITEBYTECODE"] = "1"
        environment.setdefault("CLANG_MODULE_CACHE_PATH", str(self.root / "build/agent/clang-cache"))
        log = self.directory / f"step-{len(self.steps) + 1:02d}.log"
        with log.open("w+") as stream:
            try:
                process = subprocess.Popen(command, cwd=self.root, env=environment, stdout=stream,
                                           stderr=subprocess.STDOUT, start_new_session=True)
                try:
                    code = process.wait(timeout=timeout)
                except (subprocess.TimeoutExpired, KeyboardInterrupt):
                    os.killpg(process.pid, signal.SIGTERM)
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL)
                        process.wait()
                    raise
            except (OSError, subprocess.TimeoutExpired, KeyboardInterrupt) as error:
                stream.write(str(error) + "\n")
                code = -1
            stream.seek(0)
            output = stream.read()
        self.steps.append({"command": command, "status": "passed" if code == 0 else "failed",
                           "exit_code": code, "seconds": round(time.monotonic() - started, 2), "log": log.name})
        print("  " + self.steps[-1]["status"] + " · " + log.name, flush=True)
        if code:
            print(output[-6000:], file=sys.stderr)
            raise RuntimeError("Command failed; later steps were not run: " + shlex.join(command))
        return output


def verify(root, scopes, runner, paths):
    before = fingerprint(root)
    commands = check_plan(scopes)
    report = {"status": "failed", "version": version(root), "scopes": sorted(scopes),
              "changed_paths": paths, "input_fingerprint": before,
              "started": datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "steps": runner.steps, "not_run": []}
    try:
        for command in commands:
            runner.run(command)
        if fingerprint(root) != before:
            raise RuntimeError("Repository inputs changed during verification; rerun checks for the new state")
        report["status"] = "passed"
    finally:
        report["not_run"] = commands[len(runner.steps):]
        write_json(runner.directory / "verification.json", report)
    return report


def lint(root):
    for relative in PYTHON_SOURCES:
        path = root / relative
        ast.parse(path.read_text(), filename=relative)
    for relative in GUIDES:
        path = root / relative
        text = path.read_text()
        for link in re.findall(r"\]\(([^)]+)\)", text):
            if re.match(r"[a-zA-Z]+:", link) or link.startswith("#"):
                continue
            if not (path.parent / link.split("#")[0]).exists():
                raise ValueError(f"Broken local link in {relative}: {link}")
    guide = (root / "docs/new-features.md").read_text()
    current = version(root)
    if f"**Release: {current}.**" not in guide or f"**{current} changes**" not in guide:
        raise ValueError("Makefile version and feature-guide header/release notes disagree")
    for task in sorted((root / "docs/tasks").glob("*.md")):
        if task.name in ("README.md", "TEMPLATE.md"):
            continue
        text = task.read_text()
        if not re.search(r"^Status: (active|blocked|complete)$", text, re.M):
            raise ValueError(f"Task status missing/invalid: {task.name}")
        if not re.search(r"^Updated: \d{4}-\d{2}-\d{2}$", text, re.M) or "## Next action" not in text:
            raise ValueError(f"Task date or next action missing: {task.name}")
    print("Python syntax, agent guide links, task-note structure and version/notes consistency: passed")


def context(root):
    print("Blindspot " + version(root) + " · " + git(root, "rev-parse", "--short", "HEAD").decode().strip())
    print("Working tree (preserve existing edits):")
    print(git(root, "status", "--short").decode().strip() or "clean")
    print("Read: " + ", ".join(GUIDES))
    print("Active/blocked task notes (read the selected note; do not infer authorization):")
    for task in sorted((root / "docs/tasks").glob("*.md")):
        if task.name == "TEMPLATE.md":
            continue
        if re.search(r"^Status: (active|blocked)$", task.read_text(), re.M):
            print("  " + str(task.relative_to(root)))
    print("No live app state was inspected. Historical reports do not establish current status.")


def doctor(root):
    missing = []
    for name in ("git", "make", "python3", "cargo", "rustc", "swiftc", "cbindgen", "codesign", "ditto", "unzip", "pgrep"):
        present = bool(shutil.which(name))
        print(f"{name}: {'available' if present else 'missing'}")
        if not present:
            missing.append(name)
    native = platform.system() == "Darwin" and platform.machine() == "arm64"
    print("Native build platform: " + ("macOS arm64" if native else "unsupported for native app checks"))
    codex = shutil.which("codex")
    if codex:
        result = subprocess.run([codex, "--version"], capture_output=True, text=True, timeout=10)
        print(result.stdout.strip() or "Codex version unavailable")
    print("Hook trust, WindowServer/TCC and signing-key access: NOT verified by doctor.")
    print("No tools installed, permissions changed, hooks trusted, or live stores opened.")
    lint(root)
    return 0 if native and not missing else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("context", "doctor", "lint", "check", "deliver"))
    parser.add_argument("--scope", choices=SCOPES, default=os.environ.get("SCOPE") or "auto")
    parser.add_argument("--base", default=os.environ.get("BASE") or None)
    parser.add_argument("--dry-run", action="store_true", default=os.environ.get("PLAN") == "1")
    args = parser.parse_args()
    root = repo_root()
    if args.command == "context":
        context(root)
    elif args.command == "doctor":
        return doctor(root)
    elif args.command == "lint":
        lint(root)
    else:
        paths = changed_paths(root, args.base)
        scopes = {"release"} if args.command == "deliver" else select_scopes(paths, args.scope)
        if args.dry_run:
            print("Plan only; no checks, signing, packaging or installation ran.")
            print("Scopes: " + ", ".join(sorted(scopes)))
            for command in check_plan(scopes):
                print(shlex.join(command))
            if args.command == "deliver":
                print("Then: sign → package/checksums → verified rollback → install → installed/state checks")
            return 0
        directory = Path(tempfile.mkdtemp(prefix=args.command + "-", dir=output_dir(root)))
        runner = Runner(root, directory)
        print("Reports: " + str(directory.relative_to(root)))
        if args.command == "deliver":
            from agent_delivery import deliver
            deliver(root, runner, paths)
        else:
            verify(root, scopes, runner, paths)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, RuntimeError, OSError, subprocess.SubprocessError) as error:
        print("Stopped: " + str(error), file=sys.stderr)
        sys.exit(1)
