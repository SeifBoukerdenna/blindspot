# Task: make Blindspot easier for coding agents to maintain

Status: complete
Updated: 2026-09-16
Base: 8f28d36, plus pre-existing uncommitted 0.3.2 app work

## Objective and acceptance

- [x] Codex-first CLI/IDE instructions, short source/test map and resumable Markdown notes.
- [x] Explicit changed-area checks; fresh full checks before app delivery; honest local reports.
- [x] Signed package and validated app rollback before install, with post-install state checks.
- [x] Portable optional hooks; no automatic formatting or hidden success assumptions.
- [x] Offline failure-path tests, command smoke checks and final preservation review.

## Scope and existing work

Owned: AGENTS.md, docs/agent-*.md, docs/tasks/, historical handoff archive, scripts/agent*.py,
bench/AgentWorkflowTests.py, .codex/hooks*, new Makefile targets and two CI check steps.
Preserve existing edits in Makefile (VERSION 0.3.2), shell/Actions.swift,
bench/ActionTests.swift and docs/new-features.md. No app behavior change, new version,
live install/index mutation, global Codex configuration, Git commits or publication.

## Decisions

- Source and fresh observations outrank versioned handoff claims. Archive the old AGENTS.md
  intact rather than maintaining stale live counts and old "next steps" in startup context.
- Root instructions route to focused docs; do not depend on nested instruction auto-loading.
- Standard-library Python wraps existing Makefile targets. No second build system, plugin,
  dependency installation, auto-trust or hard dependency on hooks.
- Every delivery checks fresh inputs; a source fingerprint invalidates a pass after edits.
- App rollback is not a database backup. State ambiguity stops the success claim; no automatic
  repair or blind rollback across migrations. Real migration tasks still need fixture testing.
- Documentation/tooling-only work is verified but does not bump or reinstall the app.

## Progress

Implemented instructions/map/task conventions, routing/reports, local delivery with rollback,
hook replacements and CI entry points. All 25 offline workflow/hook tests pass, including real
Codex patch payloads, portable commands from nested checkouts, legacy wrappers, failed checks,
rollback validation, source changes and post-install preservation failures. Tests exposed a
macOS /var versus /private/var path-alias bug; resolving both paths fixed it.
Legacy post-edit wrappers were observed running in this session; that does not verify new
CLI/IDE hook discovery or trust.

## Verification

`PYTHONPYCACHEPREFIX=build/agent/pycache python3 -m py_compile scripts/agent.py scripts/agent_delivery.py bench/AgentWorkflowTests.py .codex/hooks/agent_hook.py`
passed. `make agent-check SCOPE=tooling` passed lint, diff whitespace checks and all 25 fixture
tests; report: build/agent/check-bkyycdlt/verification.json (before this final note update).
Signing, packaging, installation and live-state access are simulated by a fake command runner.
The tooling scope is deliberate: automatic routing also sees pre-existing app changes and
the Makefile/CI edits, but this task only adds agent entry points, not native app behavior.

`make agent-context`, `make agent-doctor`, `make agent-check PLAN=1` and
`make agent-deliver PLAN=1` passed. Doctor found the required tools and Codex CLI 0.154.0;
it does not prove signing/TCC permissions. Both workflow YAML files parsed with Ruby YAML;
all four legacy shell wrappers passed `bash -n`. `git diff --check` passed.
Pre-existing shell/Actions.swift, bench/ActionTests.swift and docs/new-features.md SHA-256
hashes match their starting values; VERSION remains 0.3.2. Makefile changes only add the
agent targets beside the user's existing version change.

Not run: full app suite, real local delivery, remote CI and CLI/IDE hook trust activation.
The task changes repository tooling, not app source; no live install is warranted.

## Next action

No implementation work remains for this task. The user can review/commit the diff and use
docs/agent-workflow.md. In the next supporting CLI session, review optional hooks with
`/hooks`; the Make commands do not require hooks. First real app delivery still needs its
normal full checks and installed-state validation; the fixture pass is not a live-install claim.
