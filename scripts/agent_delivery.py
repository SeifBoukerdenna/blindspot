"""Local delivery only; Git publication and live-index maintenance are never invoked."""

import fcntl
import hashlib
import os
from pathlib import Path, PurePosixPath
import platform
import plistlib
import re
import shutil
import stat
import zipfile

from agent import app_changes, fingerprint, verify, version, write_json


BUNDLE_ID = "com.seifboukerdenna.blindspot"


def digest(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise ValueError("Expected a regular file")
        result = hashlib.sha256()
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(block)
        return result.hexdigest()


def bundle_info(path):
    if path.is_symlink() or path.parent.is_symlink():
        raise ValueError("Refusing a symlinked app or Applications directory")
    with (path / "Contents/Info.plist").open("rb") as stream:
        info = plistlib.load(stream)
    if info.get("CFBundleIdentifier") != BUNDLE_ID:
        raise ValueError("Unexpected app bundle identifier")
    return info


def state_snapshot(home):
    state = home / ".local/share/blindspot"
    config = home / ".config/blindspot/config.toml"
    paths = {"config": config, "overrides": state / "overrides.toml", "palette": state / "palette"}
    preferences = {name: digest(path) if path.exists() else None for name, path in paths.items()}
    files = sorted(p.name for p in state.iterdir() if p.is_file()) if state.exists() else []
    database = state / "content.sqlite"
    identity = None
    if database.exists():
        info = database.lstat()
        if not stat.S_ISREG(info.st_mode):
            raise ValueError("Live index must be a regular non-symlink file")
        identity = [info.st_dev, info.st_ino]
    return {"preferences": preferences, "files": files, "index_identity": identity}


def assert_preserved(before, after):
    if before["preferences"] != after["preferences"]:
        raise RuntimeError("Preferences changed during delivery; review before claiming preservation")
    missing = set(before["files"]) - set(after["files"])
    missing -= {"content.sqlite-wal", "content.sqlite-shm"}
    if missing:
        raise RuntimeError("A pre-existing state file is missing; stop and review, do not repair automatically")
    if before["index_identity"] and before["index_identity"] != after["index_identity"]:
        raise RuntimeError("The live index file was replaced or disappeared; stop and review")


def counts(runner, root, home):
    database = home / ".local/share/blindspot/content.sqlite"
    if not database.exists():
        return {}
    output = runner.run([root / "target/release/examples/content_inspect", database])
    result = {name: int(value) for name, value in re.findall(r"([a-z_]+)=(\d+)", output)}
    if not {"schema", "documents", "legacy_embeddings"} <= result.keys():
        raise RuntimeError("Read-only index counts unavailable; delivery cannot validate the live index")
    return result


def signature(runner, path):
    runner.run(["codesign", "--verify", "--deep", "--strict", path])
    output = runner.run(["codesign", "-dv", "--verbose=4", path])
    team = re.search(r"^TeamIdentifier=([A-Z0-9]+)$", output, re.M)
    if not team:
        raise RuntimeError("Local automatic delivery requires a Developer ID team, not ad-hoc signing")
    return team[1]


def verify_archive(path, current):
    prefix = f"Blindspot-{current}/"
    with zipfile.ZipFile(path) as archive:
        for name in archive.namelist():
            parts = PurePosixPath(name)
            if parts.is_absolute() or ".." in parts.parts or not name.startswith((prefix, "__MACOSX/")):
                raise RuntimeError("Unexpected archive path")
        if archive.testzip() is not None:
            raise RuntimeError("Archive CRC verification failed")
        info = plistlib.loads(archive.read(prefix + "Blindspot.app/Contents/Info.plist"))
        if info.get("CFBundleIdentifier") != BUNDLE_ID or info.get("CFBundleShortVersionString") != current:
            raise RuntimeError("Archive version/identity mismatch")
        for guide in ("START-HERE.md", "FEATURE-GUIDE.md"):
            if prefix + guide not in archive.namelist():
                raise RuntimeError("Archive is missing " + guide)


def verify_rollback(path, previous):
    with zipfile.ZipFile(path) as archive:
        for name in archive.namelist():
            parts = PurePosixPath(name)
            if parts.is_absolute() or ".." in parts.parts or not name.startswith(("Blindspot.app/", "__MACOSX/")):
                raise RuntimeError("Unexpected rollback archive path")
        if archive.testzip() is not None:
            raise RuntimeError("Rollback archive CRC verification failed")
        info = plistlib.loads(archive.read("Blindspot.app/Contents/Info.plist"))
        if info.get("CFBundleIdentifier") != BUNDLE_ID or info.get("CFBundleShortVersionString") != previous:
            raise RuntimeError("Rollback archive identity/version mismatch")


def deliver(root, runner, paths, home=None, verifier=verify):
    home = Path.home() if home is None else home
    installed = home / "Applications/Blindspot.app"
    bundle = root / "build/Blindspot.app"
    report = {"status": "failed", "installation_attempted": False, "installed": False,
              "rollback": None, "steps": runner.steps}
    try:
        if not app_changes(paths):
            raise ValueError("No app/build changes detected; documentation/tooling tasks do not need installation. Use BASE for committed app changes.")
        if platform.system() != "Darwin" or platform.machine() != "arm64":
            raise ValueError("Local delivery requires macOS arm64")
        old_info = bundle_info(installed)
        lock_path = installed.parent / ".blindspot-delivery.lock"
        descriptor = os.open(lock_path, os.O_WRONLY | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
        with os.fdopen(descriptor, "w") as lock:
            if not stat.S_ISREG(os.fstat(lock.fileno()).st_mode):
                raise ValueError("Delivery lock must be a regular file")
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as error:
                raise RuntimeError("Another local delivery is running") from error
            verified = verifier(root, {"release"}, runner, paths)
            if verified["status"] != "passed":
                raise RuntimeError("Release verification did not pass")
            before = state_snapshot(home)
            runner.run(["cargo", "build", "--release", "--locked", "--manifest-path", "core/Cargo.toml", "--example", "content_inspect"])
            before_counts = counts(runner, root, home)
            old_team = signature(runner, installed)
            current = version(root)
            runner.run(["make", "sign"])
            new_info = bundle_info(bundle)
            if new_info.get("CFBundleShortVersionString") != current or signature(runner, bundle) != old_team:
                raise RuntimeError("Built version or signing team does not match the delivery requirements")

            previous = runner.directory / "previous-artifacts"
            previous.mkdir()
            for suffix in (".zip", "-Cheatsheet.md", "-SHA256SUMS.txt", ""):
                artifact = root / "build" / f"Blindspot-{current}{suffix}"
                if artifact.is_symlink():
                    raise ValueError("Refusing a symlinked release artifact")
                if artifact.is_dir():
                    artifact.rename(previous / artifact.name)
                elif artifact.exists():
                    shutil.copy2(artifact, previous / artifact.name)
            runner.run(["make", "package"])
            archive = root / "build" / f"Blindspot-{current}.zip"
            guide = root / "build" / f"Blindspot-{current}-Cheatsheet.md"
            verify_archive(archive, current)
            sums = {p.name: digest(p) for p in (archive, guide)}
            checksum_file = root / "build" / f"Blindspot-{current}-SHA256SUMS.txt"
            checksum_file.write_text("".join(f"{checksum}  {name}\n" for name, checksum in sums.items()))
            report["artifacts"] = sums

            rollback = runner.directory / "Blindspot-before-install.zip"
            runner.run(["ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", installed, rollback])
            runner.run(["unzip", "-tq", rollback])
            if not rollback.is_file() or rollback.stat().st_size == 0:
                raise RuntimeError("No rollback archive was created; installation refused")
            verify_rollback(rollback, old_info.get("CFBundleShortVersionString"))
            report["rollback"] = str(rollback.relative_to(root))
            report["rollback_sha256"] = digest(rollback)
            report["previous_version"] = old_info.get("CFBundleShortVersionString")
            write_json(runner.directory / "delivery.json", report)
            if fingerprint(root) != verified["input_fingerprint"]:
                raise RuntimeError("Repository inputs changed after verification; installation refused")
            report["installation_attempted"] = True
            write_json(runner.directory / "delivery.json", report)
            runner.run(["bash", "scripts/install.sh", "build/Blindspot.app"])
            report["installed"] = True
            if bundle_info(installed).get("CFBundleShortVersionString") != current:
                raise RuntimeError("Installed version mismatch")
            if signature(runner, installed) != old_team:
                raise RuntimeError("Installed signing team mismatch")
            runner.run(["pgrep", "-f", re.escape(str(installed / "Contents/MacOS/blindspot"))])
            if bundle.exists():
                raise RuntimeError("A second build bundle remains after installation")
            after = state_snapshot(home)
            assert_preserved(before, after)
            after_counts = counts(runner, root, home)
            for key in ("documents", "passages", "passage_embeddings", "legacy_embeddings"):
                if after_counts.get(key, 0) < before_counts.get(key, 0):
                    raise RuntimeError("Index counts decreased during delivery. This may be concurrent indexing; review before claiming preservation. No automatic repair was attempted.")
            report.update(status="passed", version=current, index_before=before_counts,
                          index_after=after_counts, preferences_preserved=True, existing_state_files_preserved=True)
            print(f"Installed and verified Blindspot {current}. Rollback: {report['rollback']}")
    finally:
        write_json(runner.directory / "delivery.json", report)
        if report["status"] != "passed":
            print("Delivery stopped. Check delivery.json and its logs; no Git publication or index repair was performed.")
            if report["rollback"]:
                print("App rollback available: " + report["rollback"])
