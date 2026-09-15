# Engineering report — 2026-09-13

This is a validated foundational iteration, **not completion of the full product roadmap**.
Existing uncommitted work was preserved. No production installation or user database was migrated.

1. **Architecture found.** Rust static library, C ABI, Swift 6/AppKit shell; immutable app snapshots,
   nucleo matching, deliberate file relevance tiers, redb frecency/clipboard stores, Spotlight file
   discovery, and an external loopback Ollama service. Qwen weights are not embedded in the app.
   Settings UI exists; no distinct system-settings search provider was found. See [architecture map](architecture.md).

2. **Changes.** Added a latest-request worker with coalescing, cancellation and bounded native-process
   capture; replaced file/port worker ownership; bounded large-corpus candidate retention; hardened
   HTTP parsing and clipboard writes; introduced native AI intents, a first-party action registry,
   a keyboard action menu, and ephemeral selected-text context.

3. **New capabilities.** `:ports`, `:listening`, `:localhost`, and listener-name filters such as
   `:node`/`:python`/`:docker`; throttled refresh while visible. ⌘K exposes file/app actions including
   Copy Filename, Trash and Force Quit. Explicit `>rewrite`, `>summarize`, `>explain`, and `>translate`
   submissions can include authorized selected text. AI creation/listing plans execute natively.
   These console filters describe **TCP listeners**, not every running process or UDP endpoint.

4. **Decisions.** Preserve AppKit pooling, snapshots, existing relevance policies, redb and Spotlight.
   Use one worker per active provider with at most one queued replacement, rather than spawn an
   independent job for every keystroke. Native action providers own their dispatch; results carry
   action descriptions, not UI closures. Untrusted model text never reaches the legacy shell runner.
   Existing shell-shaped replies are translated only for a narrow supported subset.

5. **Significant modules.** `process_job.rs`, `files.rs`, `ports.rs`, `match.rs`, `clips.rs`,
   `agent/{http,mod,session,intent}.rs`, `ffi.rs`, `exec.rs`, generated `blindspot.h`,
   `Actions.swift`, `Context.swift`, `Panel.swift`, `Bridge.swift`, `ClipboardWatcher.swift`,
   `agent/history.rs`, Makefile, and new Rust/Swift validation harnesses. libc became a direct dependency; it was
   already present transitively. No new framework dependency or cloud service was introduced.

6. **Database changes.** None to schema or on-disk format. Clipboard mutations now serialize
   database writes and publication without holding the query-state lock during commits. Clear
   deletes successfully before clearing memory. Existing history/legacy-image tests still pass.

7. **Security fixes.** Removed model access to unrestricted `zsh` execution. Creation uses `openat`,
   `mkdirat`, `O_NOFOLLOW` and exclusive creation; rejects traversal, sensitive directories including
   case variants, symlinks and overwrites. Fixed HTTP integer-overflow and Unicode-panic paths,
   cumulative response limits and transport-level loopback enforcement. Removed sensitive debug
   log payloads and paths from launch-error logs. Fixed caller-forged confirmation metadata and
   added port-termination confirmation. Serialized clipboard mutations to prevent duplicate
   publication; bounded thumbnail/OCR input and image dimensions before decoding.

8. **Performance.** Measurements are single-machine observations, not a general speedup claim.

   | Measurement | Before | After |
   |---|---:|---:|
   | 620-record typical rank query | 9.088 µs | 9.045 µs |
   | 620-record broad single-character query | ~10.08 µs | 10.40 µs |
   | Shell harness median / p99 | 4.355 / 7.846 ms | 4.310 / 6.661 ms |
   | Shell frames over 16 ms | 6 / 3000 | 5 / 3000 |
   | 10M broad rank query | 264.680 ms | 273.395 ms |
   | 10M selective fuzzy query | 380.071 ms | 378.067 ms |

   Ranking candidate storage is now O(result limit) for large corpora, rather than retaining all
   matches. The input corpus is still resident and scanning is still O(N). The broad-query cost
   increased slightly; the change is retained for bounded scratch memory, not latency. Typical
   app-query timing is unchanged within measurement noise; Criterion flags a small broad-query
   regression. Shell runs had sandbox-limited persistent stores and long maximum outliers
   (184 ms before, 137 ms after); their lower p99 does not prove launcher-wide improvement.
   Initial-load changes are cache-sensitive and are not claimed as an optimization.

9. **Validation.** Full Rust suite: **241 passed**, zero failures; clippy with warnings denied passed.
   Swift action harness: **6 passed**, including confirmation forgery and unavailable actions.
   AppKit smoke: show/reopen, seven query modes, bounded rows and arrow routing passed. Full app
   compiled. Synthetic rank runs completed at 10K, 100K, 1M and 10M records. A real TCP socket
   discovery integration test passed. Actual Qwen 3.8 27B returned a validated typed plan, an
   answer, and a safe fallback to a destructive request, in 20.326 / 3.700 / 8.599 seconds;
   no model-proposed actions were executed. Initial sandbox test failures were environmental;
   loopback-enabled runs passed. Accessibility-granted selection capture and destructive native
   menu operations were not manually exercised against personal applications/files.

10. **Known limitations.** AI intents currently cover create-directory, create-file and list-directory;
    arbitrary shell chores are now blocked. No NL process termination planner, semantic index,
    embeddings, full-text content retrieval, Finder/browser integration, universal normal-query
    file search, full action catalog, or public extension SDK was delivered. Existing `?` file mode
    remains. Search-provider registration is not yet generalized. Action progress is indeterminate;
    cancellation cannot interrupt an OS operation after it has committed. Current context is a
    launcher-open snapshot and can become stale. AX permission is never requested automatically.

11. **Technical debt.** Clipboard ingestion tasks can still accumulate; clipboard activation/clear
    can wait on disk mutations and should move behind an owned asynchronous service. Agent history
    remains local plaintext and can contain answers derived from selected text; existing old logs
    are not deleted. Recent-file/usage queries still buffer subprocess output separately. File
    output uses a line format ambiguous for newline filenames. Process termination still uses a
    PID and needs start-time identity verification against PID reuse. The legacy shell executor
    remains in the crate for compatibility/tests but is unreachable from model plans. Full idle
    CPU, battery, thermal, memory-pressure, migration-failure and long-session profiling remains.

    **Test side effect:** the pre-existing session tests wrote to real agent history during this
    task's loopback-enabled runs. Unit-test builds now disable real history reads/writes; the
    dedicated persistence tests continue using scratch files. 38 identifiable synthetic rows
    remain in the real history. Four malformed lines were also observed; their origin is not
    established. No cleanup or backup was performed: automatic approval review rejected the
    proposed backed-up removal of test rows because it modifies private state outside the repo.
    Cleanup requires user approval. Existing history uses automatic retention, so its earlier
    contents cannot be proven unchanged without a pre-task backup.

12. **Intentionally withheld.** Unrestricted model shell execution, inferred process restart,
    permission-bypassing context collection and in-process untrusted plugins. These are unsafe
    contracts. Semantic search and the other missing roadmap features are outstanding work, not
    features declared unnecessary or impossible.

13. **Next iteration.** First finish owned clipboard lifetimes and process identity. Then move the
    remaining default actions into the registry, add provider registration and unified parsing,
    and implement opt-in durable content indexing with tested exclusions, migrations and a lexical
    fallback. Evaluate a local embedding model against a relevance corpus before choosing vector
    storage. Use measured candidate retrieval limits rather than sending millions of rows to nucleo.

14. **macOS limits.** Accessibility-selected text is app-dependent and permission-gated; secure
    fields are excluded. Finder/browser Apple Events need explicit Automation integration.
    There is no universal working-directory/project API across terminals/editors. Filesystem,
    process and socket visibility is permission-dependent. AppKit smoke emitted a sandbox-related
    Launch Services notification warning; an unrestricted manual interaction pass remains useful.

15. **Readiness assessment.** Millions of records: current resident linear scan is insufficient for
    interactive search despite bounded result memory. Third-party extensions: not ready for untrusted
    code; first-party action registration is a useful seam. Additional local models: existing model
    selection and loopback transport work, subject to schema-conformance testing. Automation:
    typed plans and native execution provide a safer starting point; durable workflows, object
    identity and broader permission-aware actions still need implementation.

## Reproduce

```sh
make test check
CLANG_MODULE_CACHE_PATH=/tmp/blindspot-clang-cache make app test-actions smoke-panel
CLANG_MODULE_CACHE_PATH=/tmp/blindspot-clang-cache make bench-shell
cargo bench --manifest-path core/Cargo.toml --bench query
cargo run --release --manifest-path core/Cargo.toml --example scale -- 10000000
cargo run --release --manifest-path core/Cargo.toml --example agent_smoke
```

Tests and real-model checks need permitted loopback access. UI harnesses need a macOS GUI session.
`agent_smoke` asks synthetic questions only and never executes plans. `scale` intentionally allocates
a synthetic resident corpus; its timing is not a database or semantic-retrieval benchmark.
