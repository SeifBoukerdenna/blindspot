#!/usr/bin/env python3
"""Prints the GitHub release body for one version: `release-notes.py docs/new-features.md X.Y.Z`.

The body is the guide's "**X.Y.Z changes**" list, so the notes are written once, reviewed in
the commit that bumps the version, and cannot drift from the guide shipped inside the zip.
A missing or empty entry exits non-zero, which stops a release whose notes were forgotten.
"""

import re
import sys
from pathlib import Path

HEADING = re.compile(r"^\*\*(\d+\.\d+\.\d+) changes\*\*\s*$")

FOOTER = """
### Install

Requires macOS 26 or later on Apple silicon.

1. Download `Blindspot-{version}.zip` and unzip it.
2. Move `Blindspot.app` to your Applications folder and open it.
3. Blindspot is not notarized by Apple, so macOS blocks the first launch. Open **System Settings →
   Privacy & Security**, find the message about Blindspot and click **Open Anyway**. You only do
   this once per download.
4. Follow the welcome guide, or press **⌘⇧Space**, type an app name and press Return.
   Optional setup guides document indexing, Accessibility, Ollama and a confirmed first-model download.

Choose the app ZIP, not GitHub's Source code archives. `~/Applications` is a writable installation
location for built-in updates. `START-HERE.md` is the short quick start; `FEATURE-GUIDE.md` and the
attached cheatsheet are the full reference. No account or AI model is needed to begin.

To verify all checksums, download the ZIP, standalone cheatsheet and checksum file into the same
folder, then run `shasum -a 256 -c Blindspot-{version}-SHA256SUMS.txt`.
"""


def changes(guide: str, version: str) -> str:
    lines = guide.splitlines()
    starts = [at for at, line in enumerate(lines)
              if (match := HEADING.match(line)) and match.group(1) == version]
    if not starts:
        raise SystemExit(f"error: no **{version} changes** entry in the guide")
    body = []
    for line in lines[starts[0] + 1:]:
        # An entry is a bullet list; the next heading, entry or plain paragraph ends it, so
        # prose that follows the oldest entry never leaks into its notes.
        if HEADING.match(line) or line.startswith("#"):
            break
        if line.strip() and not line.startswith(("-", " ")) and any(part.strip() for part in body):
            break
        body.append(line)
    text = "\n".join(body).strip()
    if not text:
        raise SystemExit(f"error: the **{version} changes** entry is empty")
    return text


def main() -> int:
    if len(sys.argv) != 3:
        raise SystemExit("usage: release-notes.py GUIDE VERSION")
    guide, version = Path(sys.argv[1]).read_text(encoding="utf-8"), sys.argv[2]
    print(f"## Blindspot {version}\n\n{changes(guide, version)}\n{FOOTER.format(version=version)}", end="")
    return 0


if __name__ == "__main__":
    sys.exit(main())
