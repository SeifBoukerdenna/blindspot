# Blindspot — engineering handoff (0.2.4)

## Mission and working agreement

Own this mature macOS launcher and evolve it into a universal local command interface.
Preserve working abstractions; do not rewrite it. Priorities: correctness, responsiveness,
reliability, privacy, security, maintainability, extensibility, then feature richness.

The user wants implemented features in a signed build running on this Mac, with a complete
Markdown cheatsheet and real workflows. Work autonomously through implementation, focused
validation, packaging and installation. Ask only for genuine ambiguity or consequential
operations outside the authorized scope. Do not stop at a design document. Do not push,
publish or discard existing work without authorization.

Keep usage low. Use targeted reads and bounded tasks. If delegating, choose a cheaper coding
model available in your environment. The prior Codex preference was Astra orchestration and
Luna coding; do not pretend those model names are available inside Claude Code. Avoid extensive
repeated tests and huge benchmarks during this feature-focused iteration. Always build and
run meaningful focused checks for changed behavior. Report what was actually verified.

## Verified handoff state — September 15, 2026 (0.2.8)

- 0.2.8 was built, signed, packaged, installed at ~/Applications/Blindspot.app by moving
  build/Blindspot.app (no backup, no staging copy) and launched. It is the only copy on disk and
  the only LaunchServices entry; stale build/ registrations were unregistered. Downloads holds
  Blindspot-0.2.8.zip, Blindspot-0.2.8-Cheatsheet.md and Blindspot-How-To-Use.md; the 0.2.7 zip
  and cheatsheet were removed. The next install has no rollback copy unless one is made first.
- Passed: cargo test (339 core tests), clippy --all-targets (no warnings), make test-actions,
  smoke-panel, codesign --verify --deep --strict on the installed app. Index preserved across
  install: schema 7, 14,480 documents, 14,449 embeddings, before and after.
- Office import safety: DOCX/DOC/RTF/ODT fixtures referencing remote images, templates,
  hyperlinks and INCLUDEPICTURE made zero network connections through NSAttributedString;
  the extract helper additionally applies sandbox_init("no-network") before parsing.
- Not verified by Claude: the live Settings/panel UI (no Computer Use); running any system
  command (deliberately not exercised on the user's Mac: lock/sleep/restart/empty Trash/dark
  mode/mute would change real state); whether loginwindow Apple Events need an Automation
  prompt; the Calendar permission prompt and live events; AI commands and fallback rows with a
  live model; the two-copy dialog, live Compact, agent rename/move/trash, and snippet paste.
- Live index: schema 7, ~14.5k documents after excluding ~/hl/data; database file ~736–779 MB
  until Compact is run. Historical snapshot, not a fixed count.
- Claude and Codex sessions both work in this tree. Many files are modified or untracked;
  inspect git status, diffs and file mtimes. Never reset, clean or overwrite them.

0.2.8: system commands (core/src/system.rs catalog, BS_KIND_SYSTEM rows spliced after apps in
file_query and `:system`; shell SystemActions with confirmations for restart/shut down/log out/
empty Trash/quit all); fallback rows when a settled search has <3 results (fallback_rows,
`fallback_search` setting validated as http(s) with {query}); `:schedule`/`my schedule` via
shell/Schedule.swift EventKit provider and EventActions (meeting links only for known hosts,
forced https; NSCalendars*UsageDescription; no hardened runtime so no entitlement needed);
AI commands (shortcuts.rs BUILT_IN_PROMPTS and saved `prompts`, `:prompt`/`:prompts`/
`:unprompt`, saved ones only run as `ai KEYWORD`; bs_prompt_resolve for the shell, and
bs_agent_submit substitutes the instruction via submit_with_context's `instruction`).
0.2.7: `>docs` answers from indexed passages with source rows (content_service Retriever,
agent session submit_documents); content snippets and :content kind/size/modified filters;
Word/RTF/ODT indexing; Compact (ContentStore::compact, Phase::Compacting); typed rename_path,
move_path, trash_path intents (RENAME_EXCL, no-follow opens, confirmed in Panel); quick links
and snippets (core/src/shortcuts.rs, shortcuts.toml, BS_KIND_SHORTCUT/LINK/SNIPPET). 0.2.6
contents (Index tab, churn fixes, single-instance guard) and 0.2.5 contents remain. macOS's
/usr/bin/sqlite3 -readonly cannot always open the idle WAL index but the bundled reader can; do
not "fix" readers based on the CLI. Still open: OCR, spreadsheets/presentations, copy/delete
agent actions, system-wide snippet expansion, hard quotas, calendar event creation.

## Read first; source remains authoritative

- docs/new-features.md: complete current feature guide, supported syntax and workflows.
- docs/architecture.md: architecture background.
- docs/roadmap-status.md and docs/semantic-search.md: historical checkpoints/measurements.
  Several pending/completion statements are stale. Verify each against source before acting.
- docs/claude-pre-024-historical.md: preserved former CLAUDE.md, historical reference only.
  Its model-generated shell design, no-extension rule, Swift line cap and early milestones
  do not govern current work.
- Inspect applicable .claude/rules and other local instructions; reconcile obsolete product
  assumptions with current user requirements and source.

## Architecture and ownership

Makefile is the authoritative build/version/signing configuration: Rust static library plus
Swift 6/AppKit, arm64, macOS deployment target 26.0. There is no Xcode project.

- core/src/ffi.rs, core/src/ffi/, shell/Bridge.swift, include/blindspot.h: C ABI and generated
  cbindgen header. Preserve allocation/free, error handling and thread contracts.
- core/src/query.rs, commands.rs, match.rs, files.rs, ports.rs: parsing, commands, ranking,
  Spotlight file search and process/port discovery.
- core/src/content.rs, content/, content_indexer.rs, content_service.rs: SQLite WAL/FTS
  content storage, incremental indexing and search.
- core/src/semantic.rs, semantic/, helpers/SemanticWorker.swift, helpers/vector-worker:
  installed Apple local English embeddings and isolated USearch retrieval with bounded,
  persistent shards. Embeddings are distinct from Ollama generative models.
- core/src/agent/intent.rs, agent/, exec.rs, process_job.rs: validated AI intents, local
  transport and trusted execution. Never restore unrestricted model-generated shell commands.
- shell/Actions.swift, Context.swift, LocalRequest.swift, Preview.swift: typed native actions,
  context acquisition, local requests and previews.
- shell/AppDelegate.swift, Panel.swift, ResultsView.swift, Theme.swift: lifecycle, shortcuts,
  focus, keyboard navigation and rendering. Preserve pooled rows, icon caches and preview guards.
- shell/Settings.swift, ContentWatcher.swift, Diagnostics.swift; core/src/settings.rs,
  config.rs: settings, background scheduling and diagnostics.
- core/src/clips.rs, shell/ClipboardWatcher.swift: clipboard persistence, pins, retention,
  polling and local OCR. Preserve concealed/transient pasteboard exclusions.
- helpers/ExtractWorker.swift: bounded isolated extraction for file actions. Supported action
  extraction does not imply support in background content indexing.

Keep expensive work off the main thread. Propagate cancellation, reject stale results, bound
queues/caches and isolate failure. Preserve native API boundaries; avoid giant managers.

## Next useful implementation order

First inspect current behavior and produce a short source-backed gap list. Then implement
complete vertical slices, in this suggested order, adjusting to actual evidence:

1. Smoke-check 0.2.4 shortcuts, Settings, folder controls and search; fix actual regressions.
2. Improve document discovery end to end. The user's motivating query is
   `documents about genetec`. Background indexing currently supports bounded UTF-8 text/code,
   not PDF/DOCX bodies. Filename search and explicit PDF extraction actions are separate.
   Extend indexing safely where practical, with explicit unsupported/scanned/locked/oversized
   states. Do not silently upload documents or silently broaden indexing scope.
3. Improve indexing visibility: phase, current work, counts, backlog, actual index disk size,
   failures and useful resource controls. Current bytes mean source bytes updated this pass,
   not stored index size or RAM. Current resource diagnostics are sampled app RSS and cumulative
   CPU, exclude helpers/Ollama, and are not live CPU percentage or hard OS quotas.
4. Broaden typed AI search/process workflows and permission-safe context. Current typed
   mutation intents are narrow: create directory, create empty file and list directory.
   Validate schemas/targets in trusted code and require destructive-action confirmation.
5. Complete first-party provider/action/command integration where source reveals gaps.
   Keep future extension boundaries clean. Do NOT build custom third-party extension setup,
   distribution, or untrusted in-process plugin loading.

The broader roadmap still includes deterministic hybrid ranking, indexing rename/move/delete
reliability, privacy/resource controls, cancellation, graceful degradation and scale assessment.
Scale tools exist, but millions of records and ten-million-vector production readiness have
not been established. Do not launch costly stress runs by default for this iteration.

`?"python" used:>180d` is valid syntax but depends on Spotlight last-opened metadata. Prior
read-only checks on this Mac found no usable last-opened dates, even for matching Python files.
Reindexing Blindspot content cannot supply that metadata. Explain the limitation; never silently
drop the filter or equate used with modified.

## Data, privacy and macOS constraints

- Preserve user config, overrides, clipboard, history, content database and embeddings.
  Compatible upgrades must reuse the index. Test migrations on fixtures; never erase state
  as a migration fallback. Do not use the user's live data for destructive tests.
- ~/.config/blindspot/config.toml belongs to the user. Settings write overrides beneath
  ~/.local/share/blindspot/. Do not rewrite the user's config file.
- Desktop/Downloads are default roots. Preserve explicit roots/exclusions. Folder selection
  uses NSOpenPanel. The user chooses any additional document folders in Settings.
- Runtime processing stays local. Preserve loopback-only Ollama validation, bounded requests
  and installed-model selection. No automatic model downloads, telemetry or cloud dependencies.
- Treat indexed content and AI output as untrusted input. Avoid private paths/content/prompts
  in diagnostic logs. User-visible current-work information is not permission to log documents.
- Missing Accessibility, browser data or process permissions must degrade gracefully.
  Do not use private APIs or reset TCC to make tests pass. PID start-time checks reduce but
  cannot atomically eliminate the macOS PID-reuse race.

## Build, verify and deliver

Inspect Makefile before changing build configuration. Typical commands:

```sh
CLANG_MODULE_CACHE_PATH=/tmp/blindspot-clang-cache make app
cargo test --manifest-path core/Cargo.toml <relevant_test_filter>
git diff --check
CLANG_MODULE_CACHE_PATH=/tmp/blindspot-clang-cache make sign
codesign --verify --deep --strict build/Blindspot.app
```

Optional focused targets: test-actions, test-content, test-semantic, test-vectors, smoke-panel.
Distinguish sandbox/TCC failures from product failures. Request necessary tool permissions
honestly. Signing uses the existing Developer ID and explicitly signs nested helpers. Keep
bundle ID stable. This is a locally signed build, not a notarized distribution.

Releases: the user runs `scripts/release.sh`, which sets VERSION, adds a `**X.Y.Z changes**`
entry to docs/new-features.md from commit messages when missing, runs check-header/check/test/
test-actions, asks once, then commits, tags vX.Y.Z and pushes. `make install` builds, signs,
moves the bundle to ~/Applications/Blindspot.app (single copy, build path unregistered) and
relaunches. Verify installed version/process/signature and preserved index. Preserve all user
state. Do not claim delivered or installed until it actually happened.

CI and GitHub releases (docs/releasing.md):
- **CI:** .github/workflows/ci.yml runs on push/PR on `macos-26` with Rust 1.95.0 and
  Xcode 26.6: check-header, clippy, tests, ad-hoc build, action/content/helper/smoke tests, and
  the package step.
- **Release:** .github/workflows/release.yml runs on a `vX.Y.Z` tag. It verifies tag = VERSION
  and that the commit is on main, runs the tests, signs, and publishes the release.
  - Signing uses a Developer ID only if the optional MACOS_CERTIFICATE_P12_BASE64 and
    MACOS_CERTIFICATE_PASSWORD secrets exist; otherwise ad hoc.
- **In-app updates:** shell/Updater.swift, driven from Settings → Status → Updates, user-initiated only.
  - Reads the latest release of Info.plist `BlindspotReleaseRepository` (Makefile `RELEASE_REPO`).
  - Checks: repo-hosted asset URLs; the SHA-256 file; zip entries under `Blindspot-X.Y.Z/` before
    extraction; valid strict code signature, same bundle ID, tag version.
  - Signing trust: same team installs directly; ad hoc asks with a warning; a different team is refused.
  - Swaps via replaceItemAt beside the app and relaunches after exit.
  - Tests: `make test-updater` (offline fixture apps; in CI and release).
- **No Apple services:** releases are GitHub Releases only; the user does not want notarization,
  App Store Connect or Apple-server steps. Signing keeps --timestamp=none and no hardened runtime.
- **The user does all of these, never Claude:** commits, pushes, tags, secrets, repository
  settings and releases. Scripts they run may do them.

Finish with concise changes, actual validation, artifact/guide paths and remaining limitations.
Keep the cheatsheet comprehensive and honest, with real workflows for major new capabilities.
