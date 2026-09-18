# Task: Container workspace and command recents

Status: complete
Updated: 2026-09-18
Base: 510e5f5

## Scope

Preserve all prior working-tree edits. Add separate local Images view (user screenshot compares
Docker Images with Blindspot Containers), adjustable log tail, persistent window with explicit
Command-W close and keep-on-top control, and recent-first colon command navigation.
User chose inspection/resource usage/local port links and creation from local images. No live workload mutations,
downloads, remote engines or private inspect/log output in task notes.

## Decisions

Reuse pinned Unix-socket CLI adapters, bounded output and generation guards. Command history stores
only registered invocations, never search text or command arguments. Fixtures use temporary stores.
Keep 0.3.2 for local review. User owns publication.

## Verification

make app passed after correcting a static preferences-file reference. Native fixture run passed
184 assertions including captures: grouped/untagged images, exact local image creation, no pulls,
loopback binding, invalid name/port rejection, removed-image refusal, bounded logs, metadata filtering,
deactivation persistence, Command-W and creation cancellation. Inspected Images and container details
captures. Read-only harness on installed engines: Docker active endpoint reports 3 images and 2
containers; other local Docker/Podman endpoints report empty inventories. Image listing and container
inspection passed. No live logs, creation or lifecycle changes. Full delivery pending.
Final focused container run: 187 assertions with captures, including confirmed UI creation and
cancellation against the fake CLI. Initial delivery stopped in the existing launcher fixture's
immediate !NSApp.isActive assertion; diagnostic build identified PanelSmoke.swift:113. macOS
activation is asynchronous and deactivation may not be granted. The fixture now waits boundedly
and reports unavailable inactive setup while retaining Return and actual visibility assertions.
Rerun make agent-deliver passed all 24 steps, with 182 container assertions without captures,
59 onboarding assertions and all launcher checks. No unavailable-inactive-setup message occurred
on the successful run. Includes signing, packages/checksums, verified rollback, local install and
installed signature/process/state checks. Report: build/agent/deliver-cdla_jxk/delivery.json;
rollback archive in the same directory. Installed UI :docker → Return succeeded; Images showed
all three local images corresponding to the user's screenshot. Left Images open. No live creation,
log reading or lifecycle mutations. Resource snapshots were fixture-tested; no running live
container was available for that check. Cmd-W cancels pending requests; engines may already have
accepted a mutation, so refresh after reopening before retrying.

## Next action

User review. Creation uses default image command, name and one optional localhost port mapping;
custom commands, environment, mounts, Compose, pulls and deletion remain outside this slice.
Artifacts: build/Blindspot-0.3.2.zip and build/Blindspot-0.3.2-SHA256SUMS.txt. No publication.
