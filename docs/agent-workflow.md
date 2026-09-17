# Working on Blindspot with Codex

Use Codex CLI or IDE from the repository. Start with [AGENTS.md](../AGENTS.md), then
[the source/test map](agent-map.md). No plugin, global configuration, or additional Python
package is required. App features and syntax remain in [the feature guide](new-features.md).

## A normal session

```sh
make agent-context
make agent-doctor
make agent-check PLAN=1
```

Context shows the current revision/version, dirty files, and active task notes; it does not
read your documents or live index. Doctor checks tools, Python syntax, guide links, task-note
structure, and agreement between VERSION and the feature guide. It does not install tools,
prove signing-key access, grant permissions, or confirm hook activation.

For substantial work, copy [the task template](tasks/TEMPLATE.md) into `docs/tasks/`.
Keep the objective, decisions, existing user changes, actual verification, and next action
short. See [task-note conventions](tasks/README.md). New sessions must reconcile notes with
the current checkout; an old note is neither proof nor new permission.

Example request:

> Read AGENTS.md and docs/tasks/my-task.md. Resume the next action, preserve the existing
> edits, implement the scoped change, verify it, and deliver locally if app code changed.
> Do not commit, push, tag, or publish.

## Verify the relevant work

```sh
make agent-check                    # Routes all staged, unstaged and untracked changes
make agent-check SCOPE=tooling      # Scripts, hooks and repository instructions
make agent-check SCOPE=docs         # Documentation only
make agent-check BASE=HEAD~1        # Include already-committed changes
make agent-check SCOPE=release      # Full local validation; no install
```

Available scopes: `auto`, `docs`, `tooling`, `rust`, `ui`, `index`, `helpers`, `release`.
Use `PLAN=1` with any scope to preview the exact commands without running them. Automatic
routing is conservative: Makefile, workspace manifests, CI, unknown files and most bench
changes select release checks. It considers other people's dirty files too; when choosing
a narrower scope, explain why those unrelated changes are outside your verification claim.
A clean tree requires an explicit scope or `BASE`—it never means "already passed."

The wrapper calls the existing Makefile targets, building before native tests. Index checks
include Rust, UI and helper checks. Release adds tooling and updater fixtures. It does not
run benchmarks, use live data for tests, install the app, or invoke the release script.
Focused additional tests are still appropriate when the source/test map calls for them.

Each real check writes a unique `build/agent/check-*/verification.json` with per-command logs,
passed/failed steps and steps not run. It stops at the first failure and fingerprints tracked
and nonignored untracked inputs before and after checks. Edits during checks invalidate the
pass. There is no cached-success shortcut. Generated outputs under `build/` remain ignored.
Reports can contain local paths/test output: keep them local, and do not paste private logs
into tracked notes. No index content is included in context output.

Native fixtures may need WindowServer or tool approval. A sandbox/TCC denial is a blocked
check, not a passing test or necessarily an app defect. Never reset TCC to get a green result.

## Deliver an app change locally

```sh
make agent-deliver PLAN=1
make agent-deliver
# For an already committed app change:
make agent-deliver BASE=HEAD~1
```

Delivery is for authorized app changes, not documentation/tooling-only work. It does not
choose a version or bump it automatically. Update Makefile and feature-guide release notes
together only when the task calls for a new version. Review any earlier dirty work that will
be included in the built app before delivering.

Requirements: macOS arm64, an existing `~/Applications/Blindspot.app`, the same Developer ID
team available for signing, and access to the normal local state. Missing requirements stop
delivery; the helper does not fall back to ad-hoc signing or download anything. Standard
Cargo builds may need to fetch uncached build dependencies; runtime processing stays local.

Every delivery runs fresh full release checks, then:

1. Read-only index counts and preference hashes establish a before snapshot.
2. Existing Makefile targets sign and package the app, with strict signature, archive identity,
   guide and SHA-256 checks. Existing same-version artifacts are preserved under the report.
3. The installed app is archived in `build/agent/deliver-*/Blindspot-before-install.zip`.
   Its CRC, bundle/version and checksum are verified before any installation attempt.
4. If source inputs still match verification, the existing installer moves the new bundle
   into Applications and launches it. No duplicate build bundle should remain.
5. Installed version, signature/team, process, preference hashes, existing state-file presence,
   database file identity, and nondecreasing index counts are checked.

The report is `build/agent/deliver-*/delivery.json`. An installation attempt and a verified
installation are separate fields. A process check does not prove the UI looks or behaves
correctly: perform focused UI checks and say what you actually exercised.

Preservation checks are not exhaustive integrity proofs. Legitimate concurrent indexing can
decrease counts or change preferences; that stops the success claim for review, without
automatically repairing or restoring anything. Clipboard/history file presence is checked,
not byte equality, because the running app can legitimately update them.

### Rollback and failures

The rollback ZIP contains **the previous app only**, not a database backup. `make clean`
removes `build/`, including reports and these archives; copy a needed rollback somewhere safe
before cleaning. Migration work still requires dedicated disposable database fixtures and an
explicit live-data plan. An older app may not support a newer database schema: do not blindly
restore an old app or any old database.

If delivery fails, inspect the report and the last step's log. Before the installation step,
the installed app is unchanged. After an attempted install, inspect actual installed state;
do not assume the old app is still present. The helper deliberately does not automatically
roll back a potentially migrated index.

Once compatibility is established, quit Blindspot, move any failed installed app aside to a
clearly named recovery location, extract the exact recorded rollback ZIP into
`~/Applications`, verify it with `codesign --verify --deep --strict`, and relaunch. Preserve
all user state. A fresh install with no existing app remains a manual workflow.

## Optional Codex hooks

[Codex discovers AGENTS.md](https://learn.chatgpt.com/docs/agent-configuration/agents-md)
when a session starts; start a new session after changing instructions. The root guide
explicitly routes to relevant docs rather than assuming every nested instruction is loaded.

The repository's [hook configuration](../.codex/hooks.json) uses portable commands and the
documented `apply_patch` payload (`tool_input.command`). Hooks add startup context and simple
pre-tool tripwires for protected files and obviously hazardous commands. They **do not**
auto-format, build after every edit, grant permissions, or certify correctness. Shell parsing
is deliberately limited: these are reminders/guardrails, not a security sandbox.

Per the [official hook documentation](https://learn.chatgpt.com/docs/hooks), project hook
definitions require review/trust; changed definitions may be skipped until reviewed. Use
`/hooks` in a supporting Codex CLI to inspect them. Do not edit global config or bypass trust
to force activation. CLI/IDE support can vary: the Make commands work without hooks.
Fixture tests validate our payload handling, not that a particular client has invoked hooks.

Legacy hook filenames remain as harmless compatibility wrappers; Rust/Swift post-edit
wrappers only remind the agent to run explicit verification. Their former auto-formatting
and Claude-only environment assumptions are gone.

## Publication stays yours

No helper stages, commits, stashes, resets, tags, pushes, publishes, configures secrets, or
changes repository settings. `scripts/release.sh` is a **user-run publication workflow**, not
an agent delivery shortcut. See [releasing](releasing.md). This work itself needs no app
version bump or local reinstall.
