# Documentation and media refresh
Status: complete
Updated: 2026-09-18
Base: 510e5f5 plus preserved onboarding/container/app/tooling edits.

## Scope
Audit public entry points, setup/build/release instructions, feature guide and media against
current source. Preserve historical measurements as dated evidence, with clear current-guide links.
Refresh demo captures using isolated fixtures only. No commits, pushes, tags, publication or
live-state changes. Local v0.3.2 tag already exists; release.sh auto mode will choose the next
patch after fetching tags. Do not change the version or rewrite existing published notes.

## Plan
Update README, setup/architecture/release docs and navigation; document unreleased work;
refresh isolated media; validate links, release-note extraction, tooling and captures.
Documentation-only check scope excludes pre-existing app changes delivered in the preceding task.

## Delivered
Updated the README, contributor setup, architecture, release instructions, quick starts and
feature guide. Added a documentation index and preserved the original architecture in history;
older reports now identify their historical scope and point to current guides. Prepared 0.3.3
release notes without changing the build version. Publication uses ./scripts/release.sh.

Refreshed 14 screenshots and both recordings using isolated demo data, including onboarding,
container overview, creation review and logs. Fixed the capture harness's obsolete settings-tab
lookup, onboarding window stacking and launcher recording bounds. Container scenes use fixtures
and never call a real runtime; the document-answer scene uses an already installed local model.

## Verification
- make media passed after updating the settings navigation; inspected all screenshots and
  representative frames of both recordings. Retook onboarding and launcher after visual review
  found an obscured window and clipped recording bounds; inspected the corrected outputs.
- make agent-check SCOPE=tooling passed all three steps: lint, diff check and workflow fixtures.
  Report: build/agent/check-qnsv_4uj. Scope excludes pre-existing app changes, already delivered
  in the preceding task; this task changes documentation and capture/release wording only.
- Checked 193 local Markdown references and heading anchors across 39 files; all exist.
- Release-note extraction for 0.3.3 and bash -n scripts/release.sh passed. The release script
  itself was not executed. Python syntax was checked; the generated cache was removed.

## Next action
User review, commit/push (including new source, guide and media files), then publication using
./scripts/release.sh. No version bump, app installation or GitHub publication in this task.
