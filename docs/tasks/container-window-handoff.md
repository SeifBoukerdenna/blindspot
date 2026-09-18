# Task: Command window handoff

Status: complete
Updated: 2026-09-18
Base: 510e5f5

## Scope

User confirms Return dismisses the launcher but the container window does not come forward.
Active-app Return works in installed UI automation; previous smoke tests called delegates directly.
Preserve all existing onboarding, container and transparency changes and live user state.

## Changes

Shared WindowPresentation in Theme.swift (already linked by native fixtures) explicitly brings
Containers and Settings forward, restores minimized windows and moves them onto the active Space.
Use public AppKit activation and ordering APIs, without changing global settings or window levels.
Regression checks dispatch Return through NSApplication, exercise all registered command navigation,
container aliases, actual container visibility and minimized-window recovery. No live engine calls
or destructive commands in these fixtures. Version stays 0.3.2 for local review.

## Verification

make app and make smoke-panel passed: 59 onboarding assertions, 157 container assertions, all
23 registered command routes, four container aliases, Settings and visible/minimized window checks.
Explicit inactive-app handoff coverage also passed in the delivery run. make agent-deliver passed
all 24 steps: full release checks, signing, packaging/checksums, verified rollback, installation,
signature/process/state checks. Report: build/agent/deliver-i8d9khvh/delivery.json; rollback archive
in the same directory. Installed 0.3.2 :docker → Return was verified through native UI automation:
Containers became the focused window with local metadata. No live workloads changed.
The first delivery stopped on the task note's missing Next action heading; corrected before rerun.
Installed active-app Return on :docker
worked before changes. Reproducing from Zed via computer use was unavailable because app access
was not approved; user clarification identifies the missing window handoff rather than a dead key.

## Next action

User review from their original desktop/Space. Multi-Space switching was not automated; fixtures
assert the presentation policy and actual visibility on the active Space. No publication or live
container mutations. Packages: build/Blindspot-0.3.2.zip and build/Blindspot-0.3.2-SHA256SUMS.txt.
