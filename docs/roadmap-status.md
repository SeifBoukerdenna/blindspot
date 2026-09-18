# Roadmap implementation status

> **Historical record.** The implementation status, measurements and release instructions below
> describe their recorded revision, not the current app. See the [documentation index](README.md),
> [current feature guide](new-features.md) and [current release workflow](releasing.md).

The user requested completion of the roadmap, excluding custom third-party extension
setup, followed by a testable latest build and a short usage guide. This is a work log,
not a claim that the entire roadmap is complete. The original baseline and foundational
iteration are documented in `architecture.md` and `engineering-report.md`.

## September 14 release checkpoint — 0.2.0

The sections below are historical checkpoints. The current release integrates semantic
indexing/retrieval and its setting, independent conventional/semantic search lifetimes,
clipboard pinning and atomic retention, asynchronous clear with ingestion draining,
ZIP and URL actions, typed default actions, TCP/UDP socket identities, process command
line/children/related-port navigation, safe application restart, Finder/browser/terminal
context and deterministic natural-language search/process routes. PDF extraction now runs
in a bounded separate helper. Filesystem updates coalesce without cancelling active scans,
reconcile affected configured roots, and retry failed monitoring with full reconciliation.
Clipboard files are private and reject final symlinks/hard links. Model diagnostic host
checks no longer resolve or connect to non-loopback names.

Selected-root reconciliation still scans the affected root: it is not a per-file mutation
queue. Browser content, working directory and selection availability remain constrained by
permissions and app support. Arbitrary process restart and broad AI automation are deliberately
unavailable. No third-party extension setup is included. The 10M production-scale claim is
not established. See `new-features.md` for the user-facing supported command set.

The user requested a live build ahead of further extensive testing. Release validation is
limited to compilation, action/parser checks, focused clipboard/command/process checks,
signing verification and a launch smoke check. Earlier test/benchmark results below are
historical, not a rerun of the entire final tree. Local Qwen/Ollama service responded and
reported five installed models. Packaging/install results are recorded separately at delivery.

## Implemented in the continuation

- `Bridge.swift`: shared native-handle ownership keeps background clipboard work alive
  safely; asynchronous action providers use the retained `ClipSink` for content/history work.
- `ClipboardWatcher.swift`: one ingestion worker; at most 16 waiting clips and 32 MiB of
  queued input, plus the active item. Stop cancels work and clears the backlog. Old queued
  copies are dropped when a burst exceeds the budget.
- `Actions.swift`: typed presentation outcomes, native file Open/Quick Look/Rename/Move/
  Duplicate, bounded PDF/text extraction and local summaries. Clipboard Copy/Delete/
  Rewrite/Summarize/Translate/Ask actions; process Inspect/Copy PID/Reveal/Open Directory/
  Terminate/Force Terminate. Registry confirmation validation remains authoritative.
- `ffi/process_native.rs`: public libproc APIs for PID/start identity, executable, parent,
  resident memory, cumulative CPU time, working directory, and process enumeration.
  Signal actions refuse special/self/foreign-owner processes and stale identities.
  The old PID-only ABI returns unavailable rather than signaling an unverified target.
- `ports.rs`: `:processes` and name filters now include non-listening processes. TCP
  listener queries retain the bounded lsof worker and throttled refresh.
- `files.rs` / `ffi.rs`: ordinary multi-character searches include files. Calculator and
  utility shapes keep their deterministic path. Home-directory reads run off-main and
  cache for five seconds; previous snapshots remain usable during refresh.
- `clips.rs`: atomic per-item deletion before in-memory publication; reopen regression.
- `Context.swift` / `Panel.swift`: AX focused-window/document URL snapshots and normal
  contextual text commands. Documents are read only on explicit submission. No page
  fetching, browser database scraping, or automatic permission prompt was introduced.
- `agent/session.rs`: reference-text transformations cannot yield executable plans,
  including during streaming. A malicious-reference regression verifies rejection.
- `recent.rs` / `usage.rs`: metadata subprocess output now uses bounded capture with a
  deadline. `Diagnostics.swift` adds privacy-safe Instruments signposts for launcher show
  and search refresh; signposts contain no query, path, clipboard, or document payload.
- `cbindgen.toml`: excludes internal duplicate PREFIX constants from the public header.

Existing redb user stores retain their schemas. The content-index foundation below adds
a separate SQLite database and the bundled rusqlite dependency. No production content
database has been created or populated. Existing user edits remain intact.

### Typed file-filter slice

`query.rs` now parses quoted filename text plus `kind:`, `size:`, `modified:` and `used:`
into typed values. Sizes use checked integer arithmetic and require whole bytes; date
ranges are bounded. Duplicate or malformed filters report errors rather than silently
broadening a search. Spotlight predicates quote untrusted filename data. Filtered queries
exclude unvalidated application and home-folder candidates; ranking sees the filename
portion only. Ordinary unfiltered search retains the existing filename query path.

Validation: **252 Rust tests passed**, including a metadata-service positive control
and escaped-injection regression. Live system-app queries found 59 positive-control
matches and zero escaped-injection matches. Apple's MDQuery parser accepted all three
additional syntax probes. The sandbox produced zero positive-control matches, so that
check was rerun with metadata-service access rather than treated as a pass. Full app,
13 Swift checks, the **14-query** panel smoke test, and clippy passed. Logs:
`/tmp/blindspot-filter-tests.log` and `/tmp/blindspot-filter-build.log`.

### Command and settings discovery slice

`commands.rs` owns registered first-party command descriptions, duplicate rejection,
prefix completion, and settings lookup using the existing schema. `:`/`:help` expose
the catalog; `:po` completes to `:ports` with Tab or Return; `:settings agent.model`
opens the relevant settings section, scrolls to the row, and focuses a control.
New ABI command/setting row kinds flow through a navigation action provider and typed
presentation outcomes. Results contain no UI callbacks. Completion does not intercept
normal or `?` filename searches. This registry describes commands; asynchronous search
provider registration and isolation remain separate unfinished work.

Validation: **255 Rust tests passed**, **15 Swift checks passed**, full app and clippy
passed. The AppKit harness exercises actual Tab completion, Return setting navigation,
and the visible target settings row, alongside its 14 query cases and arrow navigation.
Sandbox Launch Services/view-service warnings remain, without a failed assertion.
Logs: `/tmp/blindspot-command-tests.log`, `/tmp/blindspot-command-validation.log`.

## Validation checkpoints

### Durable content-index foundation and application integration

`content.rs` adds a private SQLite WAL store with transactional schema versions 1/2/3/4/5,
FTS5/BM25, bounded batches/results, stale scan and embedding rejection, non-reused row
IDs, identity-preserving renames, and deletion only after complete reconciliation.
Foreign, future-version, corrupt, and failed-migration databases are preserved. No-follow
opening rejects a symlinked database while resolving macOS's `/var` parent alias.
Embeddings can be stored with model/revision/dimension validation; semantic retrieval
is **not implemented** by this storage API. Application integration is described below.

`content_indexer.rs` and `ffi/index_native.rs` add descriptor-anchored traversal and
incremental UTF-8 text/code extraction. Native directory handles resist ancestor symlink
replacement; files and directories reached through symlinks are skipped. Local-volume,
device, cloud-placeholder, size, depth, entry-count and exclusion checks bound work.
Default maximum source size is 1 MiB; at most 64 KiB of text is indexed per source.
Hidden/generated paths and common credential filenames are skipped. Interrupted scans
retain committed records; cancellation is checked between entries and batch pauses.
The application now calls these APIs on a coalescing background worker. Schema 3 adds
ctime to identity/mtime/size fingerprints; a restored-mtime edit regression passes. Hard
links choose a stable lexicographic representative per scan. PDF indexing remains unfinished.

Synthetic tooling: `core/examples/content_scale.rs` (up to 10M rows),
`core/examples/index_scan.rs` (bounded real fixture files), and
`bench/EmbeddingProbe.swift` (Apple's optional local English sentence embedding).
The model probe returned 512 dimensions, top-1 **6/8** and top-3 **8/8** on its small
synthetic corpus; this is a feasibility probe, not a general relevance evaluation.

Measurement found that SQLite BM25 enumerates matching documents to calculate each
phrase's IDF. A 10M corpus therefore timed out on a ubiquitous phrase even after limiting
candidates. Search now probes each term up to 10,001 matches; common terms use bounded
index-ID ordering and return `SearchPage.limited=true`. This is an explicit relevance
tradeoff. The app labels degraded ranking and asks for query refinement.
Selective queries retain BM25. See `content-index.md` for results and remaining limits.

Validation: **270 Rust tests passed**, including 10 storage tests, 4 filesystem scanner
tests, and one native descriptor race/symlink test. Full app, 15 Swift checks, the
14-query AppKit smoke harness and clippy passed at the foundation checkpoint. SQLite's
build deployment target is pinned to macOS 26.0. The 10M fallback run completed: 430.212 s
indexing, 23.99 MB peak RSS; selective query medians 0.110/16.632/0.805 ms, broad queries
0.477 ms with explicitly degraded ranking and no timeouts. These measurements precede
ctime, hard-link and secure-delete changes; see `content-index.md` for scope and logs.

### Opt-in content search and privacy slice

`content_service.rs` owns cancellable background indexing and read-only search, with
configuration epochs and scope validation before publishing hits. `ContentWatcher.swift`
uses public FSEvents with burst coalescing and native power/thermal notifications. Settings
expose roots, exclusions, size and battery policy. `:content words` searches excerpts;
ordinary searches append deduplicated content hits while preserving filename ordering.
Defaults create no index. Tests use fixtures and have not enabled personal-file indexing.

Integration validation: **275 Rust tests**, **15 Swift checks**, the **14-query** AppKit
smoke harness and native FSEvents exclusion/coalescing/stop/owner-release checks passed.
The native harness uses `realpath` to match FSEvents' `/private/var` paths. Shell latency:
median **4.577 ms**, p99 **6.674 ms**, max **151.449 ms**, **7/3000** over 16 ms; no claimed
improvement. Logs: `/tmp/blindspot-content-live-tests.log`,
`/tmp/blindspot-content-live-validation.log`, `/tmp/blindspot-content-monitor-tests.log`.

Privacy review reproduced a deleted term remaining in FTS segments despite core SQLite
secure deletion. Enabling the separate FTS5 secure-delete setting fixes the regression.
Removed roots are pruned after all current roots reconcile successfully; temporarily
unavailable roots preserve data. Schema 4 uses a monotonic scan clock to reject stale scans even
when a root is re-added; obsolete scope metadata can be removed. Source files are untouched. **13 storage tests**, **3 service
tests**, full app build and clippy passed after this fix. A current-code 100K benchmark
indexed in **4.304 s**, selective query medians **0.087/0.186/0.091 ms**, broad fallback
**0.352 ms**, no timeouts. Full post-fix suite: **278 passed**, zero failures,
`/tmp/blindspot-content-privacy-tests.log`. The extended 10K filesystem harness passed
initial/unchanged/1K-update/1K-delete counts and integrity checks: **837.466 / 463.002 /
759.089 / 561.521 ms**, without batch pauses. No prior update/delete baseline exists.

### Confirmed content erasure and reliable settings saves

Content settings now offer **Erase index**, confirmation, background progress, and Stop.
The checked settings write disables indexing before erasure. Content-setting changes are
refused while erasure is queued/running; power and refresh notifications cannot replace it.
The indexing worker serializes erasure after cancelled scans. Bounded transactions remove
excerpts/embeddings, a final FTS clear removes old segments, and WAL truncation gates success.
Cancellation or an active snapshot preventing journal cleanup produces an incomplete status;
retry resumes. Source files stay intact. Missing indexes do not create a database.

Schema 4 introduces a monotonic scan clock, retaining no prior root names after full erase.
It prevents stale scan tokens becoming valid when roots are re-added. Existing record IDs
also remain monotonic, preventing stale embedding attachment. Migration and interrupted
reset tests pass; corrupt/foreign databases remain preserved on failure.

Settings writes now report errors through the existing ABI, use a private atomic temporary
file, and publish effective values only after successful replacement. Invalid/oversized or
unreadable overrides are preserved and block saving; loaded values receive schema validation.
Logs no longer include malformed TOML text. Failed toggles revert visually and reset errors
are shown. Directory sync is best effort; settings saves remain synchronous user actions.

Validation: **284 Rust tests**, **15 Swift checks**, app build, clippy and the **14-query**
AppKit smoke test passed. The latter opens the real erase sheet and presses Cancel only;
macOS view-service access was needed to inspect its controls. Logs:
`/tmp/blindspot-erasure-full-tests.log`, `/tmp/blindspot-erasure-native-panel.log`,
`/tmp/blindspot-erasure-action-tests.log`, `/tmp/blindspot-erasure-clippy.log`.
The extended filesystem benchmark erased 9K remaining records in **1,350.924 ms**,
including journal truncation and erase batch pauses; source counts and integrity passed.
Schema-4 scan timings are recorded in `content-index.md`, without an improvement claim.

Outstanding in the content slice: watcher startup failure reporting, selective per-path
updates, semantic retrieval, and more performance validation. Disabling indexing or removing
all roots retains stored data until explicit erasure; the UI/guide say so explicitly.

- Full Rust suite after clipboard deletion: **244 passed**, zero failures.
- New reference-text action-escalation regression: **1 passed** (245 total tests now).
- Swift action/file safety checks: **11 passed**; context/permission checks: **2 passed**.
- AppKit smoke: show/reopen, **11 queries**, bounded rows and keyboard navigation passed.
- Full application build and clippy with warnings denied passed before the final
  reference-only guard; subsequent final build/check runs are recorded in the tool logs.
- Shell benchmark: median **4.606 ms**, p99 **7.084 ms**, max **127.043 ms**, **6/3000**
  renders over 16 ms. Earlier foundational run: 4.310 / 6.661 ms, 5/3000 over budget.
  This is not evidence of an improvement. Harness persistent stores fall back in the
  tool sandbox and Launch Services reports a notification-registration warning.
- Tests use isolated fixtures for writes; unit-test agent history stays disabled.
  No installed application or private history cleanup was performed.

## Next implementation groups (still required)

### Semantic boundary and scale evaluation checkpoint

The bundle now contains an isolated native embedding helper using Apple's installed local
English sentence model. Its bounded, versioned protocol accepts only probe/embedding
requests. `semantic.rs` validates responses, owns/reaps the child and propagates cancellation
through deadline-controlled local socket I/O. Six transport tests and a separate positive
native-model integration test pass. Helper protocol tests cover interactive framing,
eleven malformed/oversized cases, recovery, privacy, termination and unavailable-model
fallback. A reproduced Foundation buffered-input stall was fixed with bounded POSIX reads.
Semantic retrieval is **still not connected to launcher results**.

The separate `bench/ann-probe` evaluates USearch 2.26.2 without adding it to production
dependencies. At 1M synthetic clustered 512-dimensional vectors, a monolithic graph used
1.27 GB peak RSS and took 965.2 seconds to build. Sixteen bounded shards used 87.1 MB peak
RSS and took 238.5 seconds; query median including mapping was 39.447 ms, recall@10 1.000
over twenty fixture queries. These are synthetic approximation measurements, not a
production relevance claim or a ten-million-vector validation. Details, limitations and
reproduction commands are in `semantic-search.md`.

Adversarial review reproduced a shared subprocess-capture hang: a descendant inheriting
stdout kept the reader join blocked after its parent exited. Socket reads with cancellation
and deadlines remove that join. The regression took 2.01 seconds before the fix; the three
initial capture/queue tests completed in 0.06 seconds afterward. Direct in-flight cancellation
and complete-line truncation tests were also added. No arbitrary shell execution was added
to production; the shell regression uses a fixed synthetic fixture command only.

Final checkpoint: **293 Rust tests passed**, zero failures; the one normally ignored native
embedding integration test passed separately. Full app build and clippy passed. The initial
sandbox suite could not bind fixture loopback ports or access Spotlight; the native rerun
passed rather than treating those unavailable capabilities as successful tests. Logs:
`/tmp/blindspot-semantic-transport-native-tests.log` and `/tmp/blindspot-capture-build.log`.
The newly built bundle has not yet been packaged or delivered as the final test build.

### Incremental embedding pass checkpoint

`ContentStore::embedding_page` enumerates at most 128 primary-key-ordered records, skips
loading current excerpts, and bounds pending Unicode text to 4 KiB. No schema change.
`semantic/indexing.rs` embeds batches of four, checks model identity across the pass,
commits only current document revisions, preserves completed work on cancellation/restart,
and isolates unembeddable text through individual retries. Escaped JSON overflow uses the
same fallback; blank text reports failure instead of falsely counting as current. The owner
supplies scope filtering. Automatic scheduling and semantic retrieval remain unconnected.

Native fixture benchmarks now cover real inference plus persistence: 1K fresh/unchanged/
100-edits **7,462.659 / 0.452 / 777.973 ms**; 10K fresh/unchanged/1K-edits
**74,307.584 / 9.563 / 7,604.124 ms**. Counts and integrity checks passed. The 10K run
partly overlapped compilation, so these are feasibility measurements. Unchanged checks
still scan metadata in bounded pages; large-scale dirty-work scheduling remains to assess.
See `semantic-search.md` and `core/examples/embedding_scan.rs`.

Validation: **299 Rust tests passed**, zero failures; **both native-model integration
tests passed separately**. Full app build and clippy passed. Logs:
`/tmp/blindspot-embedding-pass-tests.log`, `/tmp/blindspot-embedding-pass-native-tests.log`,
`/tmp/blindspot-embedding-pass-build.log`, `/tmp/blindspot-embedding-pass-clippy.log`.

### Derived vector identity and isolated worker checkpoint

Schema 5 gives embeddings independent AUTOINCREMENT identities. Replacement is transactional;
old keys remain invalid after edits, deletion, erasure and reopen. Migration preserves
existing vectors and rolls back on a reproduced conflict. It copies the embedding table,
so temporary disk space and background execution are required. `content/vectors.rs` adds
bounded export and authoritative candidate resolution, including vector/path/model/revision
validation. Twenty-one storage tests pass.

`helpers/vector-worker` is now built into the application bundle and included in explicit
nested signing. Its USearch 2.26.2 dependency is isolated from the launcher process. The
bounded build/query protocol uses private cache tokens, no-follow descriptor access,
SHA-256 verification before parsing, bounded fingerprint caching, and graceful per-shard
failure. Native integration tests pass for build/reopen/query, corruption fallback,
traversal/symlink/hard-link rejection, strict schemas, frame limits and existing-file
preservation. Tests caught extra fields being accepted on serde unit variants; strict
empty-object variants fix it. Tool-sandbox descriptor denial required a native fixture run.

Schema-5 native 1K fresh/unchanged/100-replacement timings were **7,518.539 / 0.538 /
773.955 ms**; all counts and integrity checks passed. Single runs do not establish a
significant performance change. The earlier vector scale measurements do not include
the new helper protocol or checksum costs; those still need measurement.

Validation: **302 Rust tests passed**, both native embedding tests passed separately,
the bundled vector-worker harness passed, full app build passed, and core/helper clippy
passed. Logs: `/tmp/blindspot-vector-identity-tests.log`,
`/tmp/blindspot-vector-identity-native-tests.log`, `/tmp/blindspot-vector-worker-tests.log`,
`/tmp/blindspot-vector-worker-build.log`, `/tmp/blindspot-vector-identity-clippy.log`.
Launcher-side vector transport, cache publication/recovery/erasure, semantic result merging,
resource scheduling and dependency redistribution notices remain unfinished. No personal
content index or final downloadable package has been created.

### Core vector transport and shard assembly checkpoint

`semantic/vectors.rs` adds typed build/query transport over the existing cancellable socket
connection. It validates build counts, artifact metadata and candidate responses, and closes
the helper on cancellation, deadline or malformed output. `semantic/indexing.rs::build_shard`
counts a bounded key range and streams 64-vector pages from SQLite, with progress and batch
pauses. Empty ranges create no artifact. Native tests verify cancellation before publication,
successful retry, reopening/querying, and rejection of graph keys made stale by document edits.

The end-to-end `vector_roundtrip` benchmark includes SQLite export, IPC, graph construction,
checksums, worker restart and candidate resolution. At 10K records: **4,622.690 ms** build,
**23.869 ms** first query, **0.566 ms** warm median. At 100K: **44,735.997 ms** two-shard build,
**195.723 ms** first query, **2.613 ms** warm median. Counts, indexed-query retrieval checks
and integrity passed. No compilation overlapped these runs. Sentence inference and UI
rendering are excluded. Cold cost confirms that conventional and semantic results need
independent query lifetimes. Logs: `/tmp/blindspot-vector-roundtrip-10k.log` and
`/tmp/blindspot-vector-roundtrip-100k.log`.

Validation: **310 Rust tests passed with all four native-helper tests included**, zero ignored
or failed. Full app build and clippy passed. Logs: `/tmp/blindspot-vector-client-tests.log`,
`/tmp/blindspot-vector-client-build.log`, `/tmp/blindspot-vector-client-clippy.log`.
The cache lifecycle is now implemented (schema 6): bounded SQLite catalog publication,
current-key verification inside a transaction, sparse range discovery, native checksum
validation before reuse, automatic rebuilding of corrupt/missing artifacts, and anchored
orphan cleanup. Explicit erase-index now removes cached vector files even if the database
is missing, reports unsafe directories as incomplete, and supports retry. The maintenance
pass is not yet scheduled by the app, and semantic results/settings remain unfinished.

Latest validation: **318 Rust tests passed**, including all five native-helper tests, with
zero failures or ignored tests. Release app build and all-target clippy passed. Logs:
`/tmp/blindspot-cache-tests.log`, `/tmp/blindspot-cache-build.log`,
`/tmp/blindspot-cache-final-clippy.log`. New tests cover failed schema-6 migration,
publication rollback, stale vectors, catalog limits/reopening/erase, symlink-safe deletion,
orphan cleanup without a database, and native cancellation/reuse/corruption recovery.

The lifecycle benchmark passed at 10K and 100K synthetic records. At 100K: build/publication
38,882.565 ms, unchanged-cache validation 265.450 ms, cold query 202.223 ms, warm median
3.119 ms, erase 9,675.905 ms. See `semantic-search.md` for both datasets, scope and earlier
measurements. No claim of a statistically demonstrated optimization is made. Cold semantic
work must remain independent of conventional first results.

1. Finish action integration: clipboard pins and retention reliability, file compression,
   URL actions, consistent default-action routing, useful errors and progress. Extend
   process results with stable socket identities, protocols, related ports/children and
   command line; document unsupported restart and the remaining atomic PID race.
2. Complete the typed query grammar across command categories and asynchronous first-party
   search-provider registration. Command registration, completion, settings search, typed
   file modifiers and quoted file values are implemented; provider execution/lifecycle
   and isolation still need implementation.
3. Complete the integrated content slice with watch-failure reporting,
   selective per-path updates and semantic retrieval. Measure recall using the optional
   local embedding model and test unavailable-model fallback. Keep large datasets off
   the main thread and out of resident full-scan pools.
4. Broaden safe AI intents for search/process workflows and complete context integration
   where public macOS APIs and permissions allow it. Keep transformation responses read-only.
5. Complete privacy/resource controls, battery/thermal scheduling, diagnostics, adversarial
   review, scale tests, final build/tests/benchmarks and relevant manual smoke checks.
6. Package and place the latest build where the user can test it, with `new-features.md`.
   Do not describe a build as delivered until the actual artifact has been produced.

## Platform and current implementation limits

- macOS has no public pidfd-style atomic signal-by-process-instance operation. Start-time
  checks narrow the exit/PID-reuse race but cannot eliminate it atomically.
- Working directory, process visibility and Accessibility attributes depend on permissions
  and application support. Focused document URL is not Finder selection or browser content.
- File copy/move/trash cannot be rolled back by task cancellation after the OS commits.
- File extraction is bounded and follows no final-component symlink; PDF parsing still
  depends on PDFKit. Unsupported/locked/scanned PDFs need an explicit degraded state.
- TCP sockets still share PID-based row IDs; UDP and complete process relationships remain.
- Clipboard pinning and transactional eviction reliability remain; clear-all settings can
  still block behind a disk mutation. Opt-in content scanning/settings/search now work;
  vector retrieval remains unfinished; explicit content erasure is now implemented.
- Raw Spotlight line output remains ambiguous for newline-containing filenames.
- No third-party extension distribution, custom setup, or untrusted in-process plugins.
