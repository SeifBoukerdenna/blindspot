# Task: Native guided onboarding

Status: complete
Updated: 2026-09-17
Base: 510e5f5

## Objective and acceptance

- [x] Native first-run quick start and resumable optional setup.
- [x] Explicit folder/clipboard/startup choices before fresh-install background work.
- [x] Guided Ollama install, hardware recommendation, explicit pull, local model test.
- [x] Guided indexing, Accessibility and useful first actions.
- [x] Updated download guide and packaging; fixtures and visual inspection.
- [x] Required build/tests/sign/package/rollback/local delivery with state preservation.

## Scope and existing work

Pre-existing untracked docs/tasks/onboarding-experience.md is the approved plan; preserve it.
Owned: onboarding shell modules/integration, related fixtures, packaging and guides.
No publication, commits, TCC changes, live model downloads or live data repair.

## Decisions

Use AppKit window hosting native SwiftUI controls. Keep existing settings/FFI contracts.
Prepare fresh-only overrides before Core initialization; never rewrite existing config or overrides.
Assume app minimum platform requirements. Model tiers are conservative suggestions, not measured
latency guarantees. Explicit setup download is separate from installed-only inference.
Keep current version for this local iteration; no release publication requested.
Fresh overrides and setup record are published together from a sibling staging directory;
startup stops on preparation errors. Existing data/config is preserved conservatively.
Model downloads require confirmation and use bounded loopback requests, reject redirects,
verify local metadata and test a fixed sample before applying settings. Host changes during
a test require retesting. Existing manual model choices retain precedence; the guide explains
choosing Automatic in the launcher model picker to use the configured setup model.

## Progress

Completed: seven native guided pages, menu/Settings entry points, fresh-install gate,
Ollama installation/download/test flow, memory-tier recommendations, indexing controls,
Accessibility guidance and controlled sample test, short download guide and packaging split.
Inspected native window captures in light/dark, including model selection and active download.
Light-mode inactive prominent buttons disappeared in captures; switched to native bordered
buttons and retained the default keyboard action. Final captures verified the correction.
Completed full release verification, signed package, rollback and local installation.
Opened setup through the installed app's Settings entry point and inspected its welcome window;
left it open for review. This creates only the guide's progress record, not feature overrides.

## Verification

make agent-context: 510e5f5; implementation working tree plus preserved prior planning note.
make app test-onboarding: passed, 59 assertions plus disposable real-Core startup child.
Coverage: lifecycle, legacy preservation, interrupted setup, recommendation tiers, bounded
requests, cloud alias/redirect rejection, successful/truncated/cancelled pulls, light/dark pages.
make test-agent-tools: passed, 26 tests including required packaged guides.
git diff --check: passed before final visual adjustments; delivery repeats it.
make agent-deliver PLAN=1: full release profile; no scope narrowing.
No live model download/inference, permission grant, cross-app paste or quarantined first-launch
trial performed. Fixture screenshots use only synthetic setup content. Model recommendations
have not been benchmarked across hardware. No live preferences or data changed by fixtures.

make agent-deliver: passed all 24 steps. Release checks included 382 core tests (8 ignored),
9 retrieval tests, 26 tooling tests, 59 onboarding assertions, native action/panel/dashboard,
content, passages, semantic/vector and updater checks. Signed with the existing Developer ID;
installed version/process/signature, preference hashes, existing state files, database identity
and index-count preservation checks passed. No commits, tags, pushes or publication.
Report: build/agent/deliver-dtli9b8h/delivery.json.
Rollback: build/agent/deliver-dtli9b8h/Blindspot-before-install.zip.
Artifacts: build/Blindspot-0.3.2.zip and build/Blindspot-0.3.2-SHA256SUMS.txt.
Visual fixtures: build/onboarding-screens/. Guide: docs/quick-start.md.
This final evidence-only note update follows delivery; app/build inputs remain the tested ones.

## Next action

User review of the installed guide; publication remains user-owned. Before a public release,
manually exercise a fresh quarantined download, a real model pull on supported hardware and
the macOS permission round trip; fixtures do not prove those external-system interactions.

## Follow-up: replay from menu bar

User requested a menu-bar control to rerun onboarding. Added Restart onboarding… alongside
Set up Blindspot…: restart navigates to Welcome and opens the same guide; resume keeps the
current page. Neither resets feature settings, models, indexing or an in-flight download.
Quick-start guide updated. make app and make agent-deliver passed for this follow-up: all
24 delivery steps, including the full release checks, signing, rollback, local installation and
state preservation. Installed app verified running. Report and rollback are under
build/agent/deliver-05dcqf8o/. No new mirrored unit tests for this simple menu callback; existing
onboarding/native smoke checks passed. The menu action itself was not clicked through UI automation.
