# Daily container tools
Status: complete
Updated: 2026-09-18
Base: 510e5f5 plus pre-existing onboarding/container working tree; preserve all edits.

## Approved scope
- New-container environment file plus masked inline overrides; review before creation.
- Bind directories, existing named volumes, restart policy, CPU/memory limits.
- Rich overview, live resource samples every two seconds, lifecycle events.
- Searchable timestamped live logs, bounded history, pause/reconnect, copy and local export (all/filtered).
- Preserve native glass, keyboard behavior, local pinned endpoints, cancellation and privacy.
- No editing/recreating existing environment, image pulls, Compose, or volume/network management.

## Plan
1. Extend creation validation/service and add native creation sheet.
2. Add bounded monitoring service and window controls; enrich inspection.
3. Extend disposable fixtures/tests and user guide; verify glass UI.
4. Build, focused checks, complete agent-deliver and installed verification.

## Verification
Completed below. No live container mutations were performed for verification.

## Implementation milestone
Creation form/review, masked file+inline environment, mounts and resource limits implemented.
Overview/Resources/Logs/Events tabs preserve the shared glass surface. Separate cancellable
monitoring tasks retain 120 resource samples, 200 events and at most 1 MiB of live logs.
Environment uses a mode-0600 temporary file inside a mode-0700 directory, removed on normal
completion/failure/cancellation. Abrupt process/system termination can prevent cleanup; documented.
Existing volume names are rechecked before run; runtime disappearance after that check remains
an engine-level race. No image pulls, existing-container environment mutation or live test mutations.

make app passed after correcting a Swift concurrency compile error. Focused fixtures passed
219 assertions including captures before final small timestamp/export/chart refinements.
Inspected native creation/review, Resources and Logs captures; moved details into tabs to keep
controls reachable. Full release profile selected because the preserved working tree includes
Makefile and release tooling changes. Version remains intentionally 0.3.2 for this local update.
Next: fresh make agent-deliver, installed UI verification, record final outcomes.

## Delivery and installed verification
- make agent-deliver passed all 24 steps on the final app sources: full release checks,
  signing, packages/checksums, verified rollback, install, installed process/signature and
  index/preference/state preservation. Report: build/agent/deliver-9kx9f506/delivery.json.
- Final native suite: 211 container assertions, 59 onboarding assertions; all 23 registered
  command routes and Return/window visibility checks passed. Earlier capture run: 219
  assertions including eight native screenshots. No live container mutations.
- Installed UI verified through :docker + Return; four detail tabs and local Images visible.
  Opened creation form, entered a disposable name and inline variable, verified secure field
  and masked review, then cancelled. No container was created by this UI check.
- Rollback: build/agent/deliver-9kx9f506/Blindspot-before-install.zip.
- Packages: build/Blindspot-0.3.2.zip and build/SHA256SUMS.
- Guide: docs/new-features.md, Local containers. No GitHub publication.
- Limits: live runtime creation/resource streaming were fixture-tested rather than exercised
  against user workloads. Native Save-dialog export writing was not manually exercised;
  export snapshot/filter/status content is covered by fixtures. No scale/stress benchmarks.

## Next action
User review of installed container tools. No required implementation or local delivery remains.
