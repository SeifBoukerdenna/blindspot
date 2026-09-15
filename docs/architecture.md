# Command layer engineering map

## Source baseline (2026-09-13)

The application is Rust (static library and testable rlib) plus Swift 6/AppKit,
built directly with Cargo, cbindgen and swiftc. It targets arm64 macOS 26.
There is no application sandbox entitlement or XPC service. The existing working
tree contains user changes; this iteration builds on them without resetting them.

Dependency direction: AppDelegate → Panel/ResultsView → Bridge → C ABI (ffi.rs)
→ independent core providers. Rust allocates/frees all ABI buffers. AppKit owns
activation, Carbon hotkeys, focus restoration, Quick Look, pasteboard and Vision.

| Subsystem | Owner and behavior | Constraint / risk |
|---|---|---|
| Query routing | ffi.rs, prefix modes, calculator/tools | Provider composition lives in the ABI implementation |
| Apps | index/apps.rs, depth-limited plist scan; Arc snapshots | Initial scan synchronous; rescan background/throttled |
| Matching | match.rs, reusable nucleo scratch | Full candidate sort and retained capacity; linear scan |
| Ranking | relevance.rs, frecency.rs, usage.rs | Deliberate lexicographic file policy; bounded frequency boost |
| Files | files.rs, Spotlight mdfind, 500 retained / 20K read | Line format ambiguous for newline filenames; caller waits during cancellation |
| Recents | recent.rs, cached Spotlight query | Whole subprocess output buffered before truncation |
| Persistence | store.rs/redb visits; clips.rs/redb metadata and blobs | Corrupt stores preserved, session fallback; no content/embedding index |
| Clipboard | ClipboardWatcher, ImageClip, Clips | Sensitive markers excluded; detached encoding can accumulate; clear/add ordering needs audit |
| Settings | settings.rs layered TOML; Settings.swift | User config is never overwritten; overrides separate; no system-settings search provider found |
| Ports | ports.rs lsof fields and ps executable resolution | Superseded worker can take newer child; stale cache across launcher opens |
| AI | agent HTTP/session/history, selectable loopback Ollama | Model is external Ollama, not embedded weights; default question model Qwen 3.8 27B |
| Execution | exec.rs lexical denylist followed by zsh -c | Confirmation does not make arbitrary interpreter execution safe |
| UI | pooled NSTableView rows, Panel, ActionHint | Repeated refresh creates unowned poll Tasks; action switches spread across shell |
| Privacy | loopback checks, local stores, clip markers | agent.log includes prompts and commands; history persists explicitly; open errors log paths |
| Tests | Rust unit + mock HTTP + real redb tests, Criterion, Swift latency harness | No standard Swift XCTest target; no semantic/migration/plugin tests |

Important existing abstractions to preserve: immutable index snapshots, core-owned
allocation, deterministic relevance tiers, capped clipboard blobs, AppKit row pooling,
prewarmed panel, explicit local AI submission, layered settings and separate stores.

## Safe implementation order

1. Baseline build, tests and existing microbenchmarks. Add reproducible synthetic scale tooling.
2. Fix worker ownership, cancellation, bounded subprocess output and HTTP framing.
3. Introduce validated native AI intents; retain answer streaming and confirmation UI.
   Never pass model text to a shell. Unsupported chores must explain the limitation.
4. Typed developer-console grammar with live, throttled snapshots and graceful failures.
5. Typed action/provider contracts with a native keyboard action surface, then context.
6. Opt-in content indexing only after durable job/migration/exclusion design and measured
   retrieval needs. Preserve Spotlight fallback. Embeddings require an explicit model
   contract (dimension/version) and evaluation corpus before claiming semantic quality.

No migration of existing redb files is needed for the first groups. Do not silently
recreate corrupt files. External plugins must eventually execute out of process; an
in-process Swift/Rust registry is for trusted first-party code only and cannot isolate
a crash or forcibly cancel arbitrary code. Do not promise that a timeout does so.

## Platform boundaries

Frontmost app is available through NSWorkspace. Selected text/focused window require
Accessibility and cannot be assumed to exist in every app. Finder selections and
browser tabs generally require app-specific Apple Events and Automation permission.
Do not scrape UI or read browser databases as substitutes. Terminal working directory
and editor project state have no universal macOS API. Process paths and sockets are
best effort, constrained by ownership/TCC; PID identity must be revalidated before
destructive actions. Restarting a process safely requires its original environment,
arguments and supervisor contract and is not inferable from its display name.

## Baseline

230 Rust tests pass with loopback permitted. In the restricted sandbox 21 tests fail
because socket binding/process inspection is denied; these are environment failures.
Swift requires a writable CLANG_MODULE_CACHE_PATH in this environment.
620-record Criterion typical query: 9.048–9.123 µs; two-character: 10.409–10.440 µs.
These are ranker measurements, not end-to-end launcher latency.
