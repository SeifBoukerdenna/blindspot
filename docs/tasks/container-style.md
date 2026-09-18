# Task: Match container window to Blindspot

Status: complete
Updated: 2026-09-18
Base: 510e5f5

## Scope

User requests the same appearance as the rest of Blindspot. Preserve all existing working-tree
changes, local container behavior, window handoff fix and user state. No version bump.

## Implementation

ContainerWindow uses shared SurfaceView glass and accessibility fallback, remembered palette,
transparent list background, palette text/accent/status colors and quiet native controls.
Retains native split view, list keyboard selection and confirmation sheets. Reopening after a
palette change rebuilds the presentation without resetting container data. Shorter log help copy.

## Verification

make app passed. BLINDSPOT_CAPTURE_CONTAINERS=1 make test-containers passed with 162 assertions.
Inspected final Nocturne and Parchment captures in build/container-screens/containers-dark.png
and containers-light.png: continuous glass titlebar, readable palette text, muted disabled actions,
transparent list and contained log output. No user palette changed by fixtures.
make agent-deliver PLAN=1 selected full release. make agent-deliver passed all 24 steps, including
command Return/visibility smoke checks, signed package/checksums, verified rollback, local install
and installed signature/process/state checks. Report: build/agent/deliver-svqscze5/delivery.json.
Rollback: build/agent/deliver-svqscze5/Blindspot-before-install.zip.
Packages: build/Blindspot-0.3.2.zip and build/Blindspot-0.3.2-SHA256SUMS.txt.

## Next action

User review via :containers. No publication, commits or live container mutations performed.
