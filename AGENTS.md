# Blindspot — agent working agreement

Evolve this mature local macOS launcher; preserve its working abstractions.
Priorities: correctness, responsiveness, reliability, privacy/security, maintainability,
extensibility, then features. Source and observed checks outrank historical reports.

## Start or resume

1. Read this file, then run `make agent-context` (or inspect git status and Makefile if
   the helper is unavailable). Preserve all pre-existing edits, including untracked files.
2. Read the relevant section of [docs/agent-map.md](docs/agent-map.md), the target
   source and its callers/tests. Read only the feature-guide sections needed for the task.
3. For substantial work, create or resume a note using
   [docs/tasks/README.md](docs/tasks/README.md). Reconcile the note with HEAD and the
   working tree; checked boxes and old test results are not proof of current correctness.
4. State a bounded plan, implement a complete slice, build, and run meaningful checks.
   Keep the note current at milestones and before handing off. Record decisions and
   remaining work, not a transcript or speculation presented as fact.

The user primarily uses Codex CLI/IDE. Keep instructions useful without any specific
model, optional hook, global skill, plugin or previous chat. Do not change global
Codex configuration, permissions, hook trust or personal skills as part of repo work.
Only delegate when the user or applicable instructions explicitly request it.

## Sources of truth

- [Makefile](Makefile): version, deployment target, build/sign/package commands.
  Rust workspace static library + Swift 6/AppKit; no Xcode project.
- [docs/new-features.md](docs/new-features.md): implemented features and usage.
- [docs/agent-map.md](docs/agent-map.md): source ownership, invariants and check routing.
- [docs/agent-workflow.md](docs/agent-workflow.md): agent commands, delivery and recovery.
- [docs/tasks/](docs/tasks/): task-specific scope, decisions and handoffs.
- [docs/releasing.md](docs/releasing.md): user-run GitHub publication.
- docs/architecture.md provides background. Roadmaps, semantic measurement reports,
  docs/history/ and docs/*historical* are checkpoints, not current instructions.

Do not duplicate the current version, live index counts or a rolling changelog here.
Update the feature guide when behavior changes. Use fresh examples in new documentation.

## Safety boundaries

- The user owns commits, pushes, tags, releases, secrets and repository settings.
  Never run scripts/release.sh yourself: it commits, tags and pushes. Do not stage,
  stash, reset, clean or discard work to make a check pass.
- Preserve config, overrides, clipboard, history, content database and embeddings.
  Test migrations/repair/erasure on disposable fixtures. A live repair or rebuild
  needs explicit task-specific authorization; it is never an upgrade fallback.
- User config at ~/.config/blindspot/config.toml is not ours to rewrite. Settings
  write overrides under ~/.local/share/blindspot/. Do not broaden indexing roots.
- Runtime processing stays local: loopback-only Ollama, bounded requests, installed
  models only. No silent downloads, telemetry, cloud processing or untrusted plugins.
- Indexed content and model output are untrusted. Keep private content, prompts and
  paths out of diagnostic logs and committed task notes.
- Use public macOS APIs; degrade gracefully when permissions are missing. Never
  reset TCC or change system settings to unblock tests. Distinguish sandbox/TCC
  limitations from product failures and request tool permissions honestly.
- Preserve FFI ownership/free/thread contracts, cancellation, stale-result rejection
  and bounded work. Keep expensive work off the main thread.
- Signing uses the existing Developer ID, stable bundle ID and --timestamp=none.
  No notarization, App Store Connect, Apple-server steps or hardened-runtime changes.

## Verify and deliver

Use `make agent-check` for changed-area routing, or explicit focused Makefile/Cargo
checks described in the map. Inspect `make agent-check PLAN=1` before an expensive run.
Unknown build/config changes select the full release profile; narrow it only with
an explicit rationale in the task note. Never claim skipped checks passed.
Do not run scale/stress benchmarks unless requested.

For app-code changes, complete build → tests → signed package/checksums → rollback
archive → local install → installed version/process/signature/state checks with
`make agent-deliver`. This does not publish. Stop delivery on failed checks,
missing rollback, or uncertain live-state preservation. Version changes must be
intentional; the helper does not choose or bump a version.

Documentation/tooling-only tasks do not require a version bump or app installation.
Validate changed tooling with its own fixtures and build relevant affected targets.
Hooks are optional best-effort reminders/guards, never a substitute for verification.

Finish with concise changes, actual check outcomes, artifact/guide paths, and explicit
limitations or remaining work. A saved report is evidence for its recorded inputs,
not a promise that later edits were tested.
