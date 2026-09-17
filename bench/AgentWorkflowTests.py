#!/usr/bin/env python3
"""Offline, disposable-fixture tests. Never signs, launches or installs a real app."""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

sys.dont_write_bytecode = True
REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO / "scripts"))
import agent
import agent_delivery as delivery


def write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def app(path, version):
    path.mkdir(parents=True, exist_ok=True)
    write(path / "Contents/Info.plist", plistlib.dumps({
        "CFBundleIdentifier": delivery.BUNDLE_ID,
        "CFBundleShortVersionString": version,
    }).decode())
    write(path / "Contents/MacOS/blindspot", "fixture executable, never launched")


def archive_app(path, bundle, prefix=""):
    with zipfile.ZipFile(path, "w") as archive:
        for source in bundle.rglob("*"):
            if source.is_file():
                archive.write(source, prefix + str(source.relative_to(bundle.parent)))
        if prefix:
            archive.writestr(prefix + "START-HERE.md", "fixture guide")


class Fixture(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="blindspot-agent-tests-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name) / "repo"
        self.root.mkdir()
        self.git("init", "-q")
        write(self.root / ".gitignore", "build/\n")
        write(self.root / "Makefile", "VERSION := 0.3.2\n")
        write(self.root / "tracked.txt", "original\n")
        self.git("add", ".")
        self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                 "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", "commit", "-qm", "fixture only")
        self.directory = self.root / "build/agent/run"
        self.directory.mkdir(parents=True)

    def git(self, *args):
        return agent.git(self.root, *args)


class RoutingTests(Fixture):
    def test_staged_unstaged_untracked_spaces_and_deleted(self):
        write(self.root / "staged.rs", "staged")
        self.git("add", "staged.rs")
        write(self.root / "staged.rs", "unstaged too")
        write(self.root / "a space.md", "untracked")
        (self.root / "tracked.txt").unlink()
        self.assertEqual(agent.changed_paths(self.root), ["a space.md", "staged.rs", "tracked.txt"])

    def test_base_and_bad_base(self):
        self.assertEqual(agent.changed_paths(self.root, "HEAD"), [])
        with self.assertRaises(subprocess.CalledProcessError):
            agent.changed_paths(self.root, "--help")

    def test_scopes(self):
        for paths, expected in [(["docs/new-features.md"], {"docs"}),
                                (["scripts/install.sh"], {"tooling"}),
                                (["core/src/content/store.rs"], {"index"}),
                                (["shell/ResultsView.swift"], {"ui"}),
                                (["Makefile"], {"release"}), (["unknown"], {"release"})]:
            self.assertEqual(agent.select_scopes(paths), expected)
        with self.assertRaises(ValueError):
            agent.select_scopes([])
        with self.assertRaises(ValueError):
            agent.select_scopes([], "typo")
        with self.assertRaises(ValueError):
            agent.check_plan({"typo"})
        self.assertEqual(agent.select_scopes([], "docs"), {"docs"})
        self.assertFalse(agent.app_changes(["docs/guide.md", "scripts/agent.py"]))

    def test_plan_builds_before_tests_and_has_no_install(self):
        commands = agent.check_plan({"release"})
        self.assertLess(commands.index(["make", "app"]), commands.index(["make", "test-content"]))
        self.assertIn(["make", "test-updater"], commands)
        self.assertNotIn(["make", "sign"], commands)
        self.assertEqual(len(agent.check_plan({"docs"})), 2)

    def test_fingerprint_and_ignored_outputs(self):
        first = agent.fingerprint(self.root)
        write(self.directory / "report", "ignored")
        self.assertEqual(agent.fingerprint(self.root), first)
        write(self.root / "tracked.txt", "changed")
        second = agent.fingerprint(self.root)
        self.assertNotEqual(second, first)
        (self.root / "tracked.txt").chmod(0o755)
        self.assertNotEqual(agent.fingerprint(self.root), second)
        (self.root / "link").symlink_to("tracked.txt")
        third = agent.fingerprint(self.root)
        (self.root / "link").unlink()
        (self.root / "link").symlink_to("Makefile")
        self.assertNotEqual(agent.fingerprint(self.root), third)

    def test_symlink_output_refused(self):
        other = self.root / "other"
        other.mkdir()
        shutil.rmtree(self.root / "build")  # Only this test's disposable fixture.
        (self.root / "build").symlink_to(other)
        with self.assertRaises(ValueError):
            agent.output_dir(self.root)

    def test_failure_report_and_skipped_steps(self):
        runner = agent.Runner(self.root, self.directory)
        commands = [[sys.executable, "-c", "raise SystemExit(7)"], ["must-not-run"]]
        with patch.object(agent, "check_plan", return_value=commands):
            with self.assertRaises(RuntimeError), contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                agent.verify(self.root, {"tooling"}, runner, [])
        report = json.loads((self.directory / "verification.json").read_text())
        self.assertEqual(report["status"], "failed")
        self.assertEqual(report["steps"][0]["exit_code"], 7)
        self.assertEqual(report["not_run"], [["must-not-run"]])

    def test_changes_during_check_invalidate_pass(self):
        runner = agent.Runner(self.root, self.directory)
        command = [sys.executable, "-c", "from pathlib import Path; Path('tracked.txt').write_text('edited')"]
        with patch.object(agent, "check_plan", return_value=[command]):
            with self.assertRaisesRegex(RuntimeError, "inputs changed"), contextlib.redirect_stdout(io.StringIO()):
                agent.verify(self.root, {"docs"}, runner, [])

    def test_runner_timeout_is_failure(self):
        runner = agent.Runner(self.root, self.directory)
        with self.assertRaises(RuntimeError), contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            runner.run([sys.executable, "-c", "import time; time.sleep(10)"], timeout=0.05)
        self.assertEqual(runner.steps[0]["status"], "failed")


class FakeRunner:
    """Simulates external tools; no command here is executed."""
    def __init__(self, fixture, fault=None):
        self.root, self.directory, self.home = fixture.root, fixture.directory, fixture.home
        self.steps, self.commands, self.fault, self.reads = [], [], fault, 0

    def run(self, command, timeout=1200):
        command = list(map(str, command))
        self.commands.append(command)
        self.steps.append({"command": command, "status": "passed"})
        built = self.root / "build/Blindspot.app"
        if command == ["make", "sign"]:
            app(built, "9.9.9" if self.fault == "version" else "0.3.2")
        elif command == ["make", "package"]:
            archive_app(self.root / "build/Blindspot-0.3.2.zip", built, "Blindspot-0.3.2/")
            write(self.root / "build/Blindspot-0.3.2-Cheatsheet.md", "fixture guide")
        elif command[0] == "codesign" and "-dv" in command:
            team = "OTHER" if self.fault == "team" and command[-1] == str(built) else "TEST123"
            return "TeamIdentifier=" + team + "\n"
        elif command[0] == "ditto":
            if self.fault != "missing-rollback":
                archive_app(Path(command[-1]), Path(command[-2]))
            if self.fault == "corrupt-rollback":
                Path(command[-1]).write_bytes(b"not a zip")
            if self.fault == "changed-input":
                write(self.root / "tracked.txt", "changed after verification")
        elif command[:2] == ["bash", "scripts/install.sh"]:
            if self.fault == "install":
                raise RuntimeError("fixture install failed")
            installed = self.home / "Applications/Blindspot.app"
            shutil.rmtree(installed)  # Disposable app fixture only.
            built.rename(installed)
            if self.fault == "preferences":
                write(self.home / ".config/blindspot/config.toml", "changed")
        elif command[0].endswith("/content_inspect"):
            self.reads += 1
            count = 1 if self.fault == "counts" and self.reads == 2 else 10
            self.assert_read_only = len(command) == 2
            return f"schema=10 documents={count} legacy_embeddings=2 passages=20 passage_embeddings=15\n"
        return ""


class DeliveryTests(Fixture):
    def setUp(self):
        super().setUp()
        self.home = Path(self.temporary.name) / "fixture-home"
        app(self.home / "Applications/Blindspot.app", "0.3.1")
        write(self.home / ".config/blindspot/config.toml", "original config")
        write(self.home / ".local/share/blindspot/content.sqlite", "not a database; fake reader only")
        self.addCleanup(patch.stopall)
        patch.object(delivery.platform, "system", return_value="Darwin").start()
        patch.object(delivery.platform, "machine", return_value="arm64").start()

    def deliver(self, runner, verifier=None, paths=None):
        if verifier is None:
            verifier = lambda root, scopes, runner, paths: {"status": "passed", "input_fingerprint": agent.fingerprint(root)}
        with contextlib.redirect_stdout(io.StringIO()):
            delivery.deliver(self.root, runner, ["shell/Panel.swift"] if paths is None else paths,
                             home=self.home, verifier=verifier)

    def test_success_has_rollback_then_install_and_state_checks(self):
        runner = FakeRunner(self)
        self.deliver(runner)
        report = json.loads((self.directory / "delivery.json").read_text())
        self.assertEqual(report["status"], "passed")
        self.assertTrue(report["installation_attempted"])
        self.assertTrue(report["preferences_preserved"])
        self.assertEqual(report["previous_version"], "0.3.1")
        self.assertTrue((self.root / report["rollback"]).is_file())
        self.assertTrue(runner.assert_read_only)
        self.assertLess(next(i for i, c in enumerate(runner.commands) if c[0] == "ditto"),
                        runner.commands.index(["bash", "scripts/install.sh", "build/Blindspot.app"]))

    def test_preinstall_failures_never_install(self):
        for fault in ("version", "team", "missing-rollback", "corrupt-rollback", "changed-input"):
            with self.subTest(fault=fault):
                run = self.directory / fault
                run.mkdir()
                runner = FakeRunner(self, fault)
                runner.directory = run
                with self.assertRaises((RuntimeError, ValueError, zipfile.BadZipFile)):
                    self.deliver(runner)
                self.assertFalse(any(c[:2] == ["bash", "scripts/install.sh"] for c in runner.commands))
                self.assertFalse(json.loads((run / "delivery.json").read_text())["installation_attempted"])

    def test_failed_verification_stops_every_external_step(self):
        runner = FakeRunner(self)
        with self.assertRaises(RuntimeError):
            self.deliver(runner, verifier=lambda *args: {"status": "failed"})
        self.assertEqual(runner.commands, [])

    def test_documentation_only_refuses_delivery(self):
        runner = FakeRunner(self)
        with self.assertRaises(ValueError):
            self.deliver(runner, paths=["docs/guide.md"])
        self.assertEqual(runner.commands, [])

    def test_postinstall_change_is_not_claimed_preserved(self):
        for fault in ("counts", "preferences", "install"):
            with self.subTest(fault=fault):
                app(self.home / "Applications/Blindspot.app", "0.3.1")
                write(self.home / ".config/blindspot/config.toml", "original config")
                run = self.directory / fault
                run.mkdir()
                runner = FakeRunner(self, fault)
                runner.directory = run
                with self.assertRaises(RuntimeError):
                    self.deliver(runner)
                report = json.loads((run / "delivery.json").read_text())
                self.assertEqual(report["status"], "failed")
                self.assertTrue(report["installation_attempted"])
                self.assertIsNotNone(report["rollback"])

    def test_archive_path_traversal_refused(self):
        archive = self.directory / "bad.zip"
        with zipfile.ZipFile(archive, "w") as stream:
            stream.writestr("Blindspot-0.3.2/../escape", "bad")
        with self.assertRaisesRegex(RuntimeError, "archive path"):
            delivery.verify_archive(archive, "0.3.2")

    def test_symlinked_delivery_lock_refused(self):
        (self.home / "Applications/.blindspot-delivery.lock").symlink_to(self.root / "tracked.txt")
        with self.assertRaises(OSError):
            self.deliver(FakeRunner(self))
        self.assertEqual((self.root / "tracked.txt").read_text(), "original\n")


class HookTests(Fixture):
    def setUp(self):
        super().setUp()
        spec = importlib.util.spec_from_file_location("blindspot_agent_hook", REPO / ".codex/hooks/agent_hook.py")
        self.hook = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.hook)

    def decision(self, command, tool="apply_patch"):
        payload = {"cwd": str(self.root), "tool_name": tool, "tool_input": {"command": command}}
        return self.hook.evaluate("PreToolUse", payload, root=self.root).get("hookSpecificOutput", {}).get("permissionDecision")

    def test_real_patch_payload_and_multiple_paths(self):
        self.assertIsNone(self.decision("*** Begin Patch\n*** Update File: shell/Panel.swift\n@@\n-a\n+b\n*** End Patch"))
        self.assertEqual(self.decision("*** Begin Patch\n*** Add File: okay.md\n+ok\n*** Update File: include/blindspot.h\n@@\n-a\n+b\n*** End Patch"), "deny")

    def test_move_destination_and_secret_extensions(self):
        for name in (".env", ".env.local", "secrets/key", "keys/signing.p12", "build/generated.swift", "target/a.rs", "../outside.md"):
            with self.subTest(name=name):
                self.assertEqual(self.decision("*** Update File: okay.md\n*** Move to: " + name), "deny")

    def test_spaces_unicode_and_legacy_payload(self):
        self.assertIsNone(self.decision("*** Add File: docs/café examples.md\n+example"))
        payload = {"cwd": str(self.root), "tool_input": {"file_path": "include/blindspot.h"}}
        self.assertEqual(self.hook.evaluate("PreToolUse", payload, self.root)["hookSpecificOutput"]["permissionDecision"], "deny")

    def test_symlink_escape(self):
        (self.root / "outside").symlink_to(self.root.parent)
        self.assertEqual(self.decision("*** Add File: outside/file.md\n+example"), "deny")

    def test_bash_tripwires(self):
        for command in ("git push", "git -C repo commit -m feature", "git reset --hard", "git add .",
                        "bash scripts/release.sh", "tccutil reset All", "rm -rf /", "rm -fr ~",
                        "defaults write com.apple.symbolichotkeys value"):
            with self.subTest(command=command):
                self.assertEqual(self.decision(command, "Bash"), "deny")
        for command in ("git diff --check", "git status --short", "git log --grep commit", "make agent-check",
                        "printf '%s' 'git push'", "rg 'rm -rf /' docs"):
            with self.subTest(command=command):
                self.assertIsNone(self.decision(command, "Bash"))

    def test_startup_is_bounded_and_never_claims_pass(self):
        result = self.hook.evaluate("SessionStart", {}, self.root)
        text = result["hookSpecificOutput"]["additionalContext"]
        self.assertLess(len(text), 1000)
        self.assertIn("make agent-context", text)
        self.assertNotIn("permissionDecision", json.dumps(result))
        self.assertEqual(self.hook.evaluate("PreToolUse", {}, self.root), {})

    def test_portable_config_from_nested_checkout_directory(self):
        shutil.copytree(REPO / ".codex/hooks", self.root / ".codex/hooks")
        directory = self.root / "nested folder"
        directory.mkdir()
        config = json.loads((REPO / ".codex/hooks.json").read_text())
        self.assertNotIn("PostToolUse", config["hooks"])
        for event, entries in config["hooks"].items():
            for entry in entries:
                for handler in entry["hooks"]:
                    command = handler["command"]
                    self.assertNotIn("/Users/", command)
                    payload = {"cwd": str(self.root), "tool_name": "apply_patch",
                               "tool_input": {"command": "*** Delete File: include/blindspot.h"}}
                    result = subprocess.run(["bash", "-c", command], cwd=directory, input=json.dumps(payload),
                                            text=True, capture_output=True, timeout=10)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(json.loads(result.stdout)["hookSpecificOutput"]["hookEventName"], event)

    def test_malformed_input_fails_without_echoing_secrets(self):
        result = subprocess.run([sys.executable, str(REPO / ".codex/hooks/agent_hook.py"), "PreToolUse"],
                                input="private-invalid-json", text=True, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 1)
        self.assertNotIn("private-invalid-json", result.stderr + result.stdout)

    def test_legacy_post_wrappers_do_not_format_or_need_claude_env(self):
        source = self.root / "test.rs"
        write(source, "intentionally not Rust or Swift")
        environment = os.environ.copy()
        environment.pop("CLAUDE_PROJECT_DIR", None)
        for name in ("rust-check.sh", "swift-check.sh"):
            result = subprocess.run(["bash", str(REPO / ".codex/hooks" / name)],
                                    input=json.dumps({"tool_input": {"file_path": str(source)}}),
                                    env=environment, text=True, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("no files formatted", result.stdout)
        self.assertEqual(source.read_text(), "intentionally not Rust or Swift")


if __name__ == "__main__":
    unittest.main(verbosity=2)
