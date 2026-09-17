# Task: Folder-scoped document search

Status: complete
Updated: 2026-09-17
Base: 8f28d36, with pre-existing working-tree changes

## Objective and acceptance

- [x] Documents offers indexed roots and a subfolder chooser, visible selected scope, and All indexed folders to clear it.
- [x] Scope survives query edits and previews; other launcher modes are unaffected.
- [x] Word and meaning retrieval apply scope before candidate limits, retaining kind/date/size filters and exclusions.
- [x] Invalid, excluded, unavailable and symlink subfolders never broaden the search; changing scope rejects stale work.
- [x] Disposable regression fixtures, native UI inspection, full checks and verified local delivery complete.

## Scope and existing work

Changed areas: core/src/content_service.rs, core/src/content_service/passage_engine.rs, core/src/content/passage_search.rs, core/src/content/passage_vectors.rs, core/src/ffi/passage.rs, generated include/blindspot.h, shell/Bridge.swift, shell/Panel.swift, shell/DocumentScope.swift, bench/PanelSmoke.swift, docs/new-features.md and this note.

Preserved pre-existing work in AGENTS.md, Makefile, shell/Actions.swift, bench/ActionTests.swift, docs/new-features.md, CI/release workflows, Codex hooks, agent workflow scripts/docs/tests and historical/task notes. The full release checks covered the combined checkout; no staging or publication was performed.

Out of scope: saved searches, scoped AI answers, indexing root changes, migrations and model downloads.

## Decisions

- Folder selection is session-only UI state, applied only to explicit Documents queries. No new syntax or persistent settings.
- Reuse configured canonical watched roots and exclusions; choosing a folder does not index or rescan it.
- Scoped semantic ranking streams eligible stored vectors with bounded work and memory (32,768 vectors, 250 ms ranking/read budget after embedding). Failures show an explicit word-only fallback rather than global candidates.
- Kept version 0.3.2 for this local development delivery. No release/version bump requested.
- The apparent cancellation failure was in the test driver: the main-queue callback did not run inside the modal chooser. A diagnostic observed response 1 (OK) and unchanged scope before applying that accepted selection. A Timer registered in modal-panel and default run-loop modes now performs actual cancellation; the original scope-preservation assertion passes. The product chooser implementation did not need changing.
- User-requested manual mode: `BLINDSPOT_TEST_FOLDER="$HOME/Desktop" make smoke-panel`. It opens the chooser on Desktop and waits for the user, with HOME still unset, no live index, and no automatic cancellation or 45-second watchdog. It does not count as an automated test pass. Escape/dismissal ends the manual fixture. Unset this variable for verification/delivery.

## Verification

Final code verified at the base revision plus the combined dirty tree. Only this handoff note was updated after successful delivery.

- `make app smoke-panel` with manual mode unset and repo-local compiler caches: passed, including folder selection, actual chooser cancellation, query/filter retention, mode isolation, clearing, focus, FFI validation and layout.
- Inspected fresh build/ui-03/launcher-folder-scope.png: visible folder footer, readable rows and no overlap.
- `make agent-deliver PLAN=1`: full release profile reviewed; no narrowing.
- `env -u BLINDSPOT_TEST_FOLDER CLANG_MODULE_CACHE_PATH=<repo>/build/folder-clang-cache SWIFT_MODULECACHE_PATH=<repo>/build/folder-swift-cache make agent-deliver`: passed all 24 steps.
- Release checks: build, header consistency, Clippy with warnings denied, 25 workflow tests, 382 core tests, 9 retrieval tests, actions, panel, index dashboard, content watcher, passage/Office extraction, semantic/vector helpers and updater fixtures passed.
- Standard Cargo suite ignored 8 native-only tests. Separately ran the relevant `native_semantic_service_returns_text_early_cancels_stale_queries_and_erases_helpers_data` test with the built vector helper: 1 passed, including scoped meaning-only retrieval. The other 7 opt-in tests and scale/stress benchmarks were not run.
- Default compiler cache was sandbox-denied; caches under build/ resolved that environment issue. Native tests/signing/install ran with required tool permissions.
- Earlier stopped delivery: build/agent/deliver-saq3tel3; it never attempted installation. Superseded by the successful report below.

## Delivery and artifacts

Installed and verified locally: Blindspot 0.3.2. Developer ID/team, installed version, running process, preference hashes, pre-existing state files, live index file identity and index-preservation checks passed. No index rebuild or repair.

- Verification: build/agent/deliver-tg3ggtnt/verification.json
- Delivery: build/agent/deliver-tg3ggtnt/delivery.json
- Verified rollback: build/agent/deliver-tg3ggtnt/Blindspot-before-install.zip
- Package: build/Blindspot-0.3.2.zip
- Checksums: build/Blindspot-0.3.2-SHA256SUMS.txt
- Guide: docs/new-features.md (also packaged as the release cheatsheet)

## Next action

None required for this slice. Use Apps → Documents, then the folder menu in the footer (or Command-L), and enter search words. The folder must already be within indexed scope. Publication remains user-owned and was not requested.
