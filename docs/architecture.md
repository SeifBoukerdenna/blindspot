# Blindspot architecture

Blindspot is a local macOS launcher with a Rust workspace static library and a Swift 6 shell
using AppKit and SwiftUI. [Makefile](../Makefile) owns the version, arm64 macOS deployment target,
compiler flags, helpers, signing and packaging. There is no Xcode project.

## Native shell and Rust core

`AppDelegate` prepares first-run settings before initializing `Core`, owns app lifecycle and
installs the menu bar/global shortcuts. `Panel` and `ResultsView` provide the launcher and pooled
result rows. `Bridge.swift` translates the generated C ABI from `core/src/ffi.rs` and `ffi/`.
Rust owns its ABI allocations and matching free functions; the shell owns native windows,
keyboard focus, previews and OS interaction. Expensive queries and process work stay off the UI
thread, with cancellation and stale-result guards.

| Area | Owners | Behavior |
|---|---|---|
| Query routing and ranking | `core/src/query.rs`, `commands.rs`, `match.rs`, `relevance.rs`, `ffi/` | Prefix modes, deterministic matching/ranking, provider composition |
| Apps and filenames | `core/src/index/`, `files.rs`, `recent.rs` | App snapshots and bounded Spotlight-backed file discovery, separate from content indexing |
| Document search | `core/src/content/`, `content_indexer.rs`, `content_service/`, `crates/retrieval/` | SQLite WAL/FTS, bounded passage extraction, word/semantic fusion and authoritative scope validation |
| Embeddings and vectors | `core/src/semantic/`, `helpers/SemanticWorker.swift`, `helpers/vector-worker/` | Installed local embedding models, generation isolation and derived vector shards |
| Document extraction | `helpers/ExtractWorker.swift`, `OfficeExtract.swift` | Bounded local PDF/Office/OCR work in an isolated no-network helper |
| Local AI | `core/src/agent/`, `exec.rs`, `shell/LocalRequest.swift` | Explicit loopback Ollama requests, cited document answers and validated typed actions; no arbitrary model-generated shell execution |
| Actions and context | `shell/Actions.swift`, `Context.swift`, `PassagePreview.swift`, `Preview.swift` | ⌘K actions, permission-aware selected text, passage reader and native previews |
| Clipboard and saved data | `core/src/clips.rs`, `store.rs`, `shortcuts.rs`, `shell/ClipboardWatcher.swift` | Local history, pins, retention, frecency and shortcuts; sensitive pasteboard markers excluded |
| Settings and indexing UI | `core/src/settings.rs`, `shell/Settings.swift`, `IndexDashboard.swift`, `ContentWatcher.swift` | Layered settings, FSEvents reconciliation, power policy, read-only diagnostics and explicit maintenance |
| Onboarding | `shell/Onboarding*.swift` | Optional first-run setup, hardware-based model suggestions, explicit downloads, local verification and resumable progress |
| Containers | `shell/Containers.swift`, `ContainerCreation.swift`, `ContainerMonitoring.swift`, `ContainerWindow.swift` | Fixed-argument Docker/Podman CLIs pinned to local Unix sockets; images, reviewed creation, lifecycle controls, inspection, monitoring and logs |
| Native appearance and presentation | `shell/Theme.swift` | Shared transparent material/palette, accessibility fallbacks and consistent auxiliary-window activation |
| Updates | `shell/Updater.swift` | User-triggered GitHub checks, checksum/signature verification and replacement/relaunch |

## State and privacy

User configuration at `~/.config/blindspot/config.toml` is read, not rewritten by the app's settings
UI. Overrides and local stores live under `~/.local/share/blindspot/`. Clipboard/frecency stores,
the content database, embeddings and command history have separate lifecycles. Colon history
stores registered command names, not arguments or queries.

New installations start with indexing, clipboard capture and login startup off until chosen in
setup. Existing installations keep their choices. Normal upgrades preserve data; migration
failure never authorizes erasing or rebuilding a live store. Vector caches are derived data,
with source identities/generations revalidated before returning results.

Document processing and inference stay local. Ollama endpoints are loopback-only; ordinary
inference uses installed models. Onboarding can download a model only after confirmation.
GitHub updates, opening download pages, browser links and explicit web searches are deliberate
network actions. Container environment values are masked in review and excluded from argv and
saved preferences; transient private env files and their cleanup limits are documented in the
[feature guide](new-features.md#local-containers). Logs/events/metrics are bounded in memory;
log export is an explicit local action.

## Platform boundaries and verification

Accessibility, Screen Recording, Calendars and Automation are requested for relevant features.
Missing access degrades those features; ordinary launching still works. App-specific context,
Spotlight metadata, process visibility and runtime metrics may be unavailable. There is no
untrusted in-process plugin mechanism, remote container control, or guarantee of hard indexing
resource quotas or multi-million-document retrieval quality.

[The source map](agent-map.md) routes changes to focused checks. Rust tests cover parsers,
storage and worker contracts; native harnesses cover UI, actions, helpers, onboarding and
container fixtures. Fixtures do not operate on live workloads. `make agent-deliver` adds full
release checks, signing, rollback, installation and state-preservation verification.

Earlier design and measurement checkpoints are catalogued in [the documentation index](README.md#historical-reports).
