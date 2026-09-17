# Agent source map

Read the section relevant to the change, then the source and adjacent tests. This is a
routing map, not a second feature inventory. [The feature guide](new-features.md) describes
shipping behavior; [Makefile](../Makefile) owns the build. Update this map when moving entry points.

## Rust, search and the native boundary

| Responsibility | Entry points | Relevant checks |
|---|---|---|
| C ABI, allocation/free and Swift decoding | core/src/ffi.rs, core/src/ffi/, shell/Bridge.swift, core/cbindgen.toml | make check-header; cargo test with ffi filter; make app test-actions smoke-panel |
| Query parsing, names and ordering | core/src/query.rs, core/src/match.rs, core/src/files.rs, core/src/relevance.rs | Cargo tests for the changed module; make test-retrieval; make smoke-panel |
| Pure passage chunking/ranking | crates/retrieval/ | make test-retrieval; passage-search tests |
| Typed local AI intents and execution | core/src/agent/, core/src/exec.rs, core/src/process_job.rs | Relevant agent/intent/process tests; make test-actions |

Cargo uses the root workspace and root Cargo.lock. Link target/release/libblindspot_core.a,
never the obsolete core/target archive. Regenerate include/blindspot.h through make header;
never hand-edit it. Header generation and Swift/Rust layout changes must land together.
Keep pointer lifetimes, blob/result freeing, null/error behavior and worker-thread contracts.
An ABI mismatch can look like a Swift allocation crash rather than a Rust error.

## Indexing, embeddings and document extraction

| Responsibility | Entry points | Relevant checks |
|---|---|---|
| WAL storage, migrations, FTS, passage reads/fusion | core/src/content.rs, core/src/content/ | Cargo content and passage_search tests; fixture migrations |
| Scoped, incremental scanning/extraction | core/src/content_indexer.rs | Cargo content_indexer tests; make test-content test-passages |
| Scheduling, progress, recovery, file diagnostics | core/src/content_service.rs, core/src/content_service/ | Cargo content_service and inspection tests; make test-content test-index-dashboard |
| Passage embeddings, model generations, vector cache | core/src/semantic/, helpers/SemanticWorker.swift, helpers/vector-worker/ | Cargo semantic tests; make test-semantic test-vectors |
| Isolated PDF/Office/OCR extraction | helpers/ExtractWorker.swift, helpers/OfficeExtract.swift | make test-passages |

Filename search is separate from the content/embedding index. Passage word search and semantic
search can finish at different times. Preserve scope/exclusion/deleted-chunk validation after
retrieval, deterministic fusion, cancellation and generation checks. Embeddings are distinct
from the generative model answering questions; never compare vectors across model generations.

Preserve explicit roots, built-in privacy exclusions, no-follow traversal and cloud-placeholder
handling. Compatible upgrades reuse the index; migration tests must use fixtures. Do not erase
or repair live data because a reader fails: macOS sqlite3 -readonly can fail on the WAL index
even when the bundled reader works. The read-only diagnostic is:

```sh
cargo build --release --locked --manifest-path core/Cargo.toml --example content_inspect
target/release/examples/content_inspect /absolute/path/to/content.sqlite
```

Exactly one database argument is read-only. Extra arguments can request maintenance/migration.
Live inspection is an explicit diagnostic/delivery action, not a routine test. Do not paste
private paths or database contents into committed notes. Throughput/coverage counters are not
an ETA, hard resource quota or proof of large-library readiness.

## Swift, AppKit and user actions

| Responsibility | Entry points | Relevant checks |
|---|---|---|
| Launch/focus, keys, result polling | shell/AppDelegate.swift, shell/Panel.swift, shell/HotKey.swift, shell/MainMenu.swift | make app smoke-panel |
| Rendering and native surfaces | shell/ResultsView.swift, shell/Theme.swift | make smoke-panel test-index-dashboard; inspect fixture screenshots |
| Result actions, explanations and context | shell/Actions.swift, shell/Context.swift, shell/LocalRequest.swift | make test-actions smoke-panel |
| Settings, indexing dashboard and watcher | shell/Settings.swift, shell/IndexDashboard.swift, shell/ContentWatcher.swift | make test-index-dashboard test-content smoke-panel |
| Document/passage preview | shell/Preview.swift, shell/PassagePreview.swift | make test-passages smoke-panel |
| Clipboard, calendar, updater | shell/ClipboardWatcher.swift, shell/Schedule.swift, shell/Updater.swift | Relevant action/watcher tests; make test-updater |

Keep AppKit work on the main actor and slow I/O off it. Preserve pooled rows, icon caches,
preview/focus guards and stale-task rejection. UI direction: minimal Spotlight-like native
translucency, few borders/cards/badges, opaque accessibility fallbacks and keyboard usability.
Appearance changes need actual visual inspection; compilation alone is not UI validation.

The panel smoke fixture runs with HOME unset so it cannot initialize normal persistent stores.
Native fixtures require WindowServer; signing may require keychain access. A sandbox failure
is not permission to reset TCC. Never test restart, sleep, Trash erasure, calendar writes,
snippet pasting or agent mutations on the user's real state without explicit authorization.
Do not substitute unrestricted model-generated shell execution for validated typed actions.

## Repository tooling and delivery

scripts/agent.py selects checks and writes local reports; scripts/agent_delivery.py handles
local delivery through the existing Makefile and scripts/install.sh. bench/AgentWorkflowTests.py
uses disposable repositories and a fake command runner. .codex/hooks/agent_hook.py is optional,
tested independently of Codex and deliberately does not format files or run full builds.

Use [agent-workflow.md](agent-workflow.md) for commands and limitations. CI and GitHub releases
remain in .github/workflows/. The user alone runs scripts/release.sh and controls publication.
Do not treat dates/test counts in docs/semantic-*.md, roadmap-status.md or historical handoffs
as current feature gaps; verify them against code before proposing work.
