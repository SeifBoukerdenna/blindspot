#!/usr/bin/env python3
"""Collect licenses for the exact locked Rust dependencies installed locally.

This script is deliberately offline: it reads Cargo.lock and the local Cargo
registry only. Missing packages are recorded for release review instead of
being downloaded or guessed.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import shutil
import tomllib


LICENSE_PREFIXES = ("license", "notice", "copying")
SKIP_DIRS = {".git", "target"}


def dependency_dirs(lock_path: Path, cargo_home: Path) -> list[tuple[str, str, Path | None]]:
    with lock_path.open("rb") as stream:
        lock = tomllib.load(stream)
    registry_roots = sorted((cargo_home / "registry" / "src").glob("*"))
    result: list[tuple[str, str, Path | None]] = []
    seen: set[tuple[str, str]] = set()
    for package in lock.get("package", []):
        source = package.get("source", "")
        name = package.get("name")
        version = package.get("version")
        if not name or not version or not source.startswith("registry+"):
            continue
        key = (name, version)
        if key in seen:
            continue
        seen.add(key)
        directory = None
        suffix = f"{name}-{version}"
        for root in registry_roots:
            candidate = root / suffix
            if candidate.is_dir():
                directory = candidate
                break
        result.append((name, version, directory))
    return result


def license_files(package_dir: Path) -> list[Path]:
    found: list[Path] = []
    for base, dirs, files in os.walk(package_dir):
        dirs[:] = sorted(d for d in dirs if d not in SKIP_DIRS)
        for filename in sorted(files):
            if filename.lower().startswith(LICENSE_PREFIXES):
                found.append(Path(base) / filename)
    return found


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = args.root.resolve()
    requested_output = Path(os.path.abspath(args.output))
    expected_output = root / "build" / "release-licenses"
    if requested_output != expected_output:
        parser.error("output is fixed to the repository build/release-licenses directory")
    if requested_output.is_symlink():
        parser.error("refusing a symlinked release-licenses directory")
    output = expected_output
    if output.exists():
        shutil.rmtree(output)
    output.mkdir(parents=True)

    cargo_home = Path(os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")))
    lock_files = [root / "core" / "Cargo.lock", root / "helpers" / "vector-worker" / "Cargo.lock"]
    packages: dict[tuple[str, str], Path | None] = {}
    for lock_file in lock_files:
        if lock_file.exists():
            for name, version, directory in dependency_dirs(lock_file, cargo_home):
                packages.setdefault((name, version), directory)

    missing: list[str] = []
    copied = 0
    for (name, version), directory in sorted(packages.items()):
        destination = output / f"{name}-{version}"
        if directory is None:
            missing.append(f"{name} {version} (not installed in the local Cargo registry)")
            continue
        files = license_files(directory)
        if not files:
            missing.append(f"{name} {version} (no license/notice file found)")
            continue
        for source in files:
            relative = source.relative_to(directory)
            target = destination / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
            copied += 1

    supplemental = root / "docs" / "licenses"
    supplemental_packages: set[str] = set()
    if supplemental.is_dir():
        target = output / "supplemental"
        shutil.copytree(supplemental, target, dirs_exist_ok=True)
        for path in supplemental.iterdir():
            if path.is_file() and "-" in path.name:
                supplemental_packages.add(path.name.split("-", 1)[0].lower())

    covered: list[str] = []
    still_missing: list[str] = []
    for item in missing:
        package_name = item.split(" ", 1)[0].lower()
        (covered if package_name in supplemental_packages else still_missing).append(item)

    report = output / "COLLECTION.txt"
    report.write_text(
        "Blindspot release license collection\n"
        "Source: locally installed packages named by core/Cargo.lock and "
        "helpers/vector-worker/Cargo.lock\n"
        f"License files copied: {copied}\n"
        f"Packages covered by checked-in supplemental licenses: {len(covered)}\n"
        + "\n".join(f"- {item}" for item in covered)
        + ("\n" if covered else "")
        + f"Packages without a local or supplemental license file: {len(still_missing)}\n"
        + "\n".join(f"- {item}" for item in still_missing)
        + "\n",
        encoding="utf-8",
    )
    print(f"collected {copied} license files into {output}")
    if still_missing:
        print(f"recorded {len(still_missing)} missing package licenses in {report}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
