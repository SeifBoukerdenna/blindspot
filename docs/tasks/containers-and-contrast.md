# Task: Local containers and readable launcher surfaces

Status: complete
Updated: 2026-09-18
Base: 510e5f5

## Objective

Add local Docker/Podman container status, logs, published ports and confirmed start/stop/restart.
Fix launcher text washing out over bright backgrounds. Keep runtime work local, bounded and
explicit; no AI dependency. No live workload mutations for verification.

## Scope and decisions

Preserve all onboarding working-tree changes and its completed task notes.
New native container browser, launcher/menu entry points, CLI adapters, isolated fixtures and
surface contrast regression checks. Use installed CLIs with argv (no shell), sanitized runtime
environment and pinned local Unix socket endpoints; Docker contexts and Podman machine sockets.
No remote engines/registries, pulls, pruning, deletion, exec shells or Compose mutations.
Log reading is explicit, bounded and transient. Lifecycle changes confirm exact engine/container.
Keep version unchanged for local review; user owns publication.

## Verification

make app: passed. Final make test-containers: 139 assertions passed, including active-context
preference. Earlier native window capture run included three extra capture assertions.
Fixtures cover full-ID/state checks, removed targets,
fixed argv, sanitized environment, endpoint rejection, stdout/stderr logs, limits, timeout and
cancellation; all lifecycle mutations use a disposable fake CLI.
BLINDSPOT_CAPTURE_CONTAINERS=1 make app test-containers smoke-panel: passed, including the
59 onboarding assertions and existing launcher/navigation/passage checks. Native captures of all
six palettes over synthetic white/black backgrounds live in build/container-screens/; inspected
Nocturne, Graphite and Cool paper captures plus light/dark container browser and empty/error state.
Text contrast checks use actual SurfaceView tint and both selected/unselected extreme backdrops,
requiring 4.5:1 for ink, secondary text, hints and muted labels. Accessibility fallback stays opaque.
cargo test --locked --manifest-path core/Cargo.toml commands::tests: 3 passed.
Read-only compiled harness: local endpoint discovery and container metadata listing succeeded
against both installed Docker and Podman. No live lifecycle commands or log reads performed.
make agent-deliver PLAN=1: full release profile; no narrowing.
make agent-deliver: all 24 steps passed, including build, full release tests, existing Developer ID
signing, packaging/checksums, verified rollback, local installation and signature/process/state
preservation. Report: build/agent/deliver-ypczu_et/delivery.json; rollback in the same directory.
Installed launcher :containers → Return was exercised through native UI automation. Browser
opened with the active local Docker context; real metadata and appropriate stopped-container
controls appeared. No live log reads or lifecycle mutations. Window left open for user review.
Artifacts: build/Blindspot-0.3.2.zip and build/Blindspot-0.3.2-SHA256SUMS.txt.
Documentation-only evidence update follows delivery; application inputs remain the tested ones.

## Implementation

Container window/menu and :containers/:docker/:podman launcher entries are complete. CLI discovery
and actions are async and bounded; local endpoint plus immutable ID/state pinned before mutation.
Docker's active local context is preferred. Logs are explicit/transient. Documented that :docker
now opens containers and :com.docker remains available for backend process filtering.
Surface tint increased from 32–40% to 96%, with stronger secondary/hint palette colors. Existing
appearance choice, native blur, accessibility fallback and launcher abstractions are retained.
Scope remains local container controls; remote/registry/Compose workflows are not included.

## Next action

User requested the previous transparent appearance after reviewing the installed dense tint.
Restore the original 32% dark / 40% light tint, retaining stronger foreground colors. Contrast
assertions now apply to the opaque accessibility modes; normal glass deliberately depends on
the backdrop and is checked for translucency and restoration after accessibility changes.
Earlier verification above describes the superseded 96% tint. Follow-up verification:
make app passed. BLINDSPOT_CAPTURE_CONTAINERS=1 make test-containers smoke-panel passed:
160 container/appearance assertions (including capture checks), 59 onboarding assertions and
launcher smoke checks. Inspected Nocturne bright/dark captures; translucency is restored.
Normal glass does not guarantee 4.5:1 contrast over every backdrop; opaque accessibility modes
retain that tested contrast requirement. The first delivery stopped at task-note status lint;
corrected the status to the supported value and reran. make agent-deliver then passed all 24 steps,
installed and verified 0.3.2, including signature/process/state checks and verified rollback.
Report: build/agent/deliver-mzoce0nu/delivery.json. Rollback archive is in the same directory.
Updated packages remain build/Blindspot-0.3.2.zip and build/Blindspot-0.3.2-SHA256SUMS.txt.

User review. Live start/stop/restart was intentionally not exercised; these operations were
verified against disposable fixtures. Remote engines, Compose orchestration and registry actions
remain outside the requested local scope. No publication, commits or version bump.
