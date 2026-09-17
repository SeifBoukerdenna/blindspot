import Foundation

/// One row of results. A value type, so nothing the panel holds points into memory
/// that Rust owns.
enum MatchKind {
    case app
    case file
    /// A calculated answer. Has no path — there is nothing on disk — and Enter copies it.
    case calc
    /// An SRE tool's answer — an epoch, a unit, an encoding. Copied on Enter, like `calc`.
    case tool
    /// Clipboard history. No path either; Enter puts the content back on the pasteboard.
    case clipText
    case clipImage
    /// A welcome-screen section title. Never selected, never acted on.
    case header
    /// A process listening on the port you asked about. `id` is its pid.
    case port
    case command
    case setting
    case shortcut
    case quickLink
    case snippet
    case system
    case prompt
    case event
    /// The agent's "ask the local model" row. Enter submits the request.
    case agentPrompt
    /// A command the agent proposes. Enter runs the plan it belongs to.
    case agentStep
    /// A command blindspot refuses to run, or an error from the agent.
    case agentBlocked
    /// A command that ran and succeeded, or failed.
    case agentOk
    case agentFailed
    /// Prose: the model answered a question. Wraps over as many lines as it needs.
    case agentAnswer
    /// One model in the picker.
    case agentModel
    /// The command running right now: Enter leaves it running, Esc stops it.
    case agentRunning
    /// A past request from the agent's history.
    case agentPast

    /// Rows that came out of the agent, which Enter treats differently from a search result.
    var isAgent: Bool {
        switch self {
        case .agentPrompt, .agentStep, .agentBlocked, .agentOk, .agentFailed, .agentAnswer,
            .agentModel, .agentRunning, .agentPast:
            true
        default: false
        }
    }

    var isClip: Bool { self == .clipText || self == .clipImage }
}

struct Match: Identifiable, Equatable {
    let id: UInt64
    let name: String
    let kind: MatchKind
    /// The bundle path. Used twice — for the icon and for the launch — which is why it
    /// travels in the result struct rather than being re-derived from `id`.
    let path: String
    let score: UInt32
    /// Unix seconds a clip was last copied; zero for everything else.
    let timestamp: UInt64
    /// Pixel size of an image clip; zero for everything else.
    let width: Int
    let height: Int
    /// A tool row's form — "binary", "ISO 8601". Empty for everything else.
    let detail: String
    /// Which characters of `name` the query matched, as Unicode scalar offsets. Empty
    /// for every row nothing was typed at.
    let highlights: [Int]
    /// Where a content passage sits in its file: page for an extracted document, line for text
    /// and code. Zero for every row that is not a passage.
    var page: Int = 0
    var line: Int = 0
    var processPID: UInt32 = 0
    var portNumber: UInt16 = 0
    var targetPID: UInt64 { processPID == 0 ? id : UInt64(processPID) }

    /// The containing folder, shown as the row's second line.
    ///
    /// Spotlight puts a category here, but every result in blindspot is an application,
    /// so a constant "Application" would be decoration. The folder actually earns its
    /// line: it is what tells two same-named bundles apart, and what reveals that the
    /// Xcode you are about to launch is the one in `~/Applications`.
    var subtitle: String {
        switch kind {
        case .calc:
            return "Press ↩ to copy"
        case .tool:
            // An epoch's local time is rendered by `ResultRow`, which owns the formatter.
            return detail
        case .header:
            // Empty for the welcome screen's sections; the agent's say where it will run.
            return detail
        case .agentPrompt, .agentStep, .agentBlocked, .agentOk, .agentFailed, .agentAnswer,
            .agentModel, .agentRunning, .agentPast:
            // The core writes these: the reason, the model's why, the last line of output, or
            // a model's size.
            return detail
        case .clipText, .clipImage:
            // Relative time, which `ResultRow` renders because the formatter is main-actor
            // state and this is a plain value type.
            return ""
        case .port, .command, .setting, .shortcut, .quickLink, .snippet, .system, .prompt, .event:
            // "pid 4821 · 127.0.0.1:3000" — written by the core, which knows both.
            return detail
        case .app, .file:
            let parent = (path as NSString).deletingLastPathComponent
            let folder = (parent as NSString).abbreviatingWithTildeInPath
            return detail.isEmpty ? folder : "\(detail) · \(folder)"
        }
    }
}

/// One row of the settings window, as the core describes it.
///
/// Everything the window needs to draw and validate a knob comes from here — including
/// where the value came from and whether changing it needs a restart — so the shell holds
/// no schema of its own.
struct Setting: Identifiable, Equatable {
    enum Kind {
        case flag, count, number, text, chord, paths, readonly

        init(_ raw: UInt8) {
            switch raw {
            case UInt8(BS_SETTING_COUNT): self = .count
            case UInt8(BS_SETTING_NUMBER): self = .number
            case UInt8(BS_SETTING_TEXT): self = .text
            case UInt8(BS_SETTING_CHORD): self = .chord
            case UInt8(BS_SETTING_PATHS): self = .paths
            case UInt8(BS_SETTING_READONLY): self = .readonly
            default: self = .flag
            }
        }
    }

    /// Which of the three places the value in force came from. The row says so, because
    /// "is this mine or the file's" is the question a layered config raises.
    enum Source {
        case builtIn, configFile, window

        init(_ raw: UInt8) {
            switch raw {
            case UInt8(BS_SOURCE_CONFIG): self = .configFile
            case UInt8(BS_SOURCE_OVERRIDE): self = .window
            default: self = .builtIn
            }
        }
    }

    let key: String
    let section: String
    let label: String
    let help: String
    /// Scalars in their canonical text form. A `.paths` value is NUL-separated — see
    /// `items` — because NUL is the one byte a pathname cannot contain.
    let value: String
    /// What this would be with the window's value removed, which is what resetting
    /// restores and is not always the built-in default.
    let fallback: String
    let kind: Kind
    let source: Source
    /// False means the change needs a restart, which the row says.
    let live: Bool
    let min: Double
    let max: Double

    var id: String { key }

    /// A `.paths` value as its members.
    var items: [String] {
        value.isEmpty ? [] : value.components(separatedBy: "\0")
    }

    /// The inverse, for handing an edited list back to the core.
    static func joined(_ items: [String]) -> String {
        items.joined(separator: "\0")
    }
}

/// Swift's side of the C ABI, and the only place in the shell that names a `bs_`
/// symbol. Owns the `BsHandle` for the process lifetime.
@MainActor
final class Core {
    private var handle: OpaquePointer { clipSink.owner.handle }

    /// The one entry point safe to call off the main thread. See `ClipSink`.
    nonisolated let clipSink: ClipSink
    nonisolated var passageReader: PassageReader { PassageReader(owner: clipSink.owner) }

    /// Fails only if the core could not initialise at all. A malformed config is not a
    /// failure — Rust falls back to defaults and logs, because a typo in a TOML file
    /// must not cost the user their launcher.
    init?(configPath: String? = nil) {
        let created: OpaquePointer? =
            if let configPath {
                configPath.withCString { bs_init($0) }
            } else {
                bs_init(nil)
            }
        guard let created else { return nil }
        clipSink = ClipSink(owner: NativeCoreOwner(handle: created))
    }

    /// The configured row count, read from the core rather than hardcoded here so that
    /// `max_results` in config.toml means something.
    ///
    /// `BsHandle` is an opaque C struct, so Swift imports both `BsHandle *` and
    /// `const BsHandle *` as the same `OpaquePointer` — no cast needed.
    var maxResults: Int {
        Int(bs_max_results(handle))
    }

    /// `pending` is true while a file search is still running, meaning more results may
    /// arrive for the same query. Always false for a query with no `?` prefix.
    func query(_ text: String, limit: Int) -> (matches: [Match], pending: Bool) {
        let results = text.withCString { bs_query(handle, $0, limit) }
        // `defer`, not a trailing call: this must run on every path out of the
        // function, and Rust owns every allocation inside `results`.
        defer { bs_free_results(results) }
        return (Self.decode(results), results.pending)
    }

    func queryDocuments(_ text: String, folder: String, limit: Int) -> (matches: [Match], pending: Bool) {
        guard !text.contains("\0"), !folder.contains("\0") else { return ([], false) }
        let results = text.withCString { query in
            folder.withCString { bs_content_query_in_folder(handle, query, $0, max(0, limit)) }
        }
        defer { bs_free_results(results) }
        return (Self.decode(results), results.pending)
    }

    /// The M3 frecency seam. A no-op in the core today; called anyway so that landing
    /// frecency needs no change on this side.
    func cancelSearch() { bs_search_cancel(handle) }

    var contentState: ContentRuntime? {
        let blob = bs_content_state(handle)
        defer { bs_free_blob(blob) }
        guard let bytes = blob.data, blob.len > 0, blob.len <= 256 * 1024 else { return nil }
        return try? JSONDecoder().decode(ContentRuntime.self, from: Data(bytes: bytes, count: blob.len))
    }

    func refreshContent() { bs_content_refresh(handle) }

    var indexControls: IndexControls? {
        let blob = bs_index_controls(handle)
        defer { bs_free_blob(blob) }
        guard let bytes = blob.data, blob.len > 0, blob.len <= 256 * 1024 else { return nil }
        return try? JSONDecoder().decode(IndexControls.self, from: Data(bytes: bytes, count: blob.len))
    }

    func indexAction(_ action: String, folder: String = "") -> String? {
        action.withCString { action in folder.withCString { Self.message(bs_index_action(handle, action, $0)) } }
    }

    func recoverContentModel() { _ = indexAction("tick") }

    /// Runs a `:link` / `:snippet` / `:unlink` / `:unsnippet` command; nil on success, else why not.
    func applyShortcut(_ command: String) -> String? {
        command.withCString { Self.message(bs_shortcut_apply(handle, $0)) }
    }

    func saveSnippet(keyword: String, text: String) -> String? {
        let bytes = Array(text.utf8)
        return keyword.withCString { name in
            bytes.withUnsafeBufferPointer { Self.message(bs_snippet_save(handle, name, $0.baseAddress, $0.count)) }
        }
    }

    func compactContent() -> String? { Self.message(bs_content_compact(handle)) }

    /// The instruction an AI command such as `fix grammar` or `ai tldr` stands for, or nil.
    func promptInstruction(for text: String) -> String? {
        text.withCString { Self.message(bs_prompt_resolve(handle, $0)) }
    }
    func refreshContent(paths: [String]) {
        guard let data = try? JSONEncoder().encode(paths), data.count <= 64 * 1024 else { refreshContent(); return }
        data.withUnsafeBytes { bytes in
            bs_content_refresh_paths(handle, bytes.bindMemory(to: UInt8.self).baseAddress, data.count)
        }
    }
    func pauseContent(_ paused: Bool, reason: ContentPauseReason = .none) {
        bs_content_pause(handle, paused, reason.rawValue)
    }
    func eraseContent(confirmed: Bool) -> String? { Self.message(bs_content_erase(handle, confirmed)) }
    func cancelContentErasure() { bs_content_erase_cancel(handle) }
    func contentEventRelevant(_ path: String) -> Bool {
        path.withCString { bs_content_event_relevant(handle, $0) }
    }

    func activate(_ id: UInt64) {
        bs_activate(handle, id)
    }

    /// Asks the local model about `request`. Returns at once: the answer arrives through the
    /// next `query`, whose `pending` stays true until the model has finished.
    func agentSubmit(_ request: String, selectedText: String? = nil) {
        request.withCString { request in
            if let selectedText {
                selectedText.withCString { bs_agent_submit_context(handle, request, $0) }
            } else { bs_agent_submit(handle, request) }
        }
    }

    /// Runs the commands the agent proposed. Does nothing unless a plan is waiting and every
    /// command in it passed the core's refusal rules.
    func agentRun() {
        bs_agent_run(handle)
    }

    /// Offers the models Ollama has installed, as rows in place of the results.
    func agentModels() {
        bs_agent_models(handle)
    }

    /// Picks the model at `index` from that list; 0 is "Automatic". Remembered across restarts.
    func agentChoose(_ index: Int) {
        bs_agent_choose(handle, index)
    }

    /// Stops a generation, or kills a running command.
    func agentCancel() {
        bs_agent_cancel(handle)
    }

    /// Stops waiting on the command in flight and leaves it running — for a server, which
    /// would otherwise be killed when the step times out.
    func agentDetach() {
        bs_agent_detach(handle)
    }

    /// The two hotkeys and the login-item flag, read at launch.
    var startup: BsStartup {
        bs_startup(handle)
    }

    /// Asks the process with this pid to stop. `SIGTERM`, and never pid 0 or 1.
    @discardableResult
    func stopProcess(pid: UInt64, started: UInt64) -> Bool {
        guard let pid = UInt32(exactly: pid) else { return false }
        return bs_process_signal(pid, started, false)
    }

    /// Forgets every clip, returning how many went. Clipboard history is its own file, so
    /// this cannot touch launch history.
    @discardableResult
    func clearClips() async -> Bool {
        let sink = clipSink
        return await Task.detached(priority: .userInitiated) { sink.clear() }.value
    }

    /// Asks, on a thread inside Rust, whether the agent's host answers. The answer shows
    /// up in the next `settings()`.
    func refreshDiagnostics() {
        bs_diagnostics_refresh(handle)
    }

    /// Spells a Carbon chord the way config.toml writes it, or nil for a key that has no
    /// name. The recorder sends the result straight back through `set`, so the window never
    /// has to know the vocabulary.
    func formatHotkey(keyCode: UInt32, modifiers: UInt32) -> String? {
        Self.message(bs_hotkey_format(keyCode, modifiers))
    }

    /// Every settable knob and every diagnostic, in the order the window draws them.
    ///
    /// Not on the keystroke path — this is called when the settings window opens — so it
    /// copies each row out rather than borrowing Rust's memory for the view's lifetime.
    func settings() -> [Setting] {
        let list = bs_settings_list(handle)
        // `defer` so the Rust allocation is released on every path.
        defer { bs_free_settings(list) }
        guard let items = list.items, list.len > 0 else { return [] }
        var result = UnsafeBufferPointer(start: items, count: list.len).map { row in
            Setting(
                key: Self.string(row.key, row.key_len),
                section: Self.string(row.section, row.section_len),
                label: Self.string(row.label, row.label_len),
                help: Self.string(row.help, row.help_len),
                value: Self.string(row.value, row.value_len),
                fallback: Self.string(row.fallback, row.fallback_len),
                kind: Setting.Kind(row.kind),
                source: Setting.Source(row.source),
                live: row.live,
                min: row.min,
                max: row.max
            )
        }
        result.append(Setting(
            key: "status.version", section: "Status", label: "Blindspot version",
            help: "The version of this app build.",
            value: (Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String) ?? "unknown",
            fallback: "",
            kind: .readonly, source: .builtIn, live: true, min: 0, max: 0))
        return result
    }

    /// Sets `key`, returning why it was refused — or nil if it took.
    ///
    /// A string rather than a boolean because the window has to be able to say *which*
    /// part of a chord or a host it did not like, and every one of those parsers lives in
    /// Rust so the window cannot drift from what config.toml is read with.
    func set(_ key: String, to value: String) -> String? {
        let bytes = Array(value.utf8)
        return key.withCString { name in
            bytes.withUnsafeBufferPointer { buffer in
                // An empty value has a nil base address, which the core reads as empty.
                Self.message(bs_setting_set(handle, name, buffer.baseAddress, buffer.count))
            }
        }
    }

    /// Forgets what the window set for `key`, back to config.toml and then the built-in.
    func reset(_ key: String) -> String? {
        key.withCString { Self.message(bs_setting_reset(handle, $0)) }
    }

    /// A refusal from the core, or nil for "it worked".
    private static func message(_ blob: BsBlob) -> String? {
        defer { bs_free_blob(blob) }
        guard let data = blob.data, blob.len > 0 else { return nil }
        return String(decoding: UnsafeBufferPointer(start: data, count: blob.len), as: UTF8.self)
    }

    /// Returns immediately; the walk happens on a thread inside Rust.
    func reindex() {
        bs_reindex(handle)
    }

    /// A clip's full content or its thumbnail, copied out of Rust-owned memory.
    func clipContent(_ id: UInt64, part: ClipPart) -> Data? {
        let blob = bs_clip_content(handle, id, part.rawValue)
        // `defer` so the Rust allocation is released on every path, including `nil`.
        defer { bs_free_blob(blob) }
        guard let data = blob.data, blob.len > 0 else { return nil }
        return Data(bytes: data, count: blob.len)
    }

    /// Every indexed bundle path, for warming caches off the keystroke path.
    ///
    /// An empty query is documented in `ffi.rs` to return the head of the index in order,
    /// so a limit past any plausible index size returns all of it — no extra FFI entry
    /// point and no header regeneration needed.
    func allPaths() -> [String] {
        query("", limit: 4096).matches.map(\.path)
    }

    private static func decode(_ results: BsResults) -> [Match] {
        guard let items = results.items, results.len > 0 else { return [] }
        return UnsafeBufferPointer(start: items, count: results.len).map { result in
            Match(
                id: result.id,
                name: string(result.name, result.name_len),
                kind: kind(result.kind),
                path: string(result.path, result.path_len),
                score: result.score,
                timestamp: result.timestamp,
                width: Int(result.width),
                height: Int(result.height),
                detail: string(result.detail, result.detail_len),
                highlights: offsets(result.highlights, result.highlights_len),
                page: Int(result.page), line: Int(result.line),
                processPID: result.process_pid, portNumber: result.network_port
            )
        }
    }

    /// The core hands over pointer + length rather than a NUL-terminated string,
    /// because a macOS path is bytes. Decoding is lossy in principle; in practice APFS
    /// requires filenames to be valid UTF-8, so the replacement path is unreachable.
    private static func kind(_ raw: UInt8) -> MatchKind {
        switch raw {
        case UInt8(BS_KIND_FILE): return .file
        case UInt8(BS_KIND_CALC): return .calc
        case UInt8(BS_KIND_TOOL): return .tool
        case UInt8(BS_KIND_HEADER): return .header
        case UInt8(BS_KIND_AGENT_PROMPT): return .agentPrompt
        case UInt8(BS_KIND_AGENT_STEP): return .agentStep
        case UInt8(BS_KIND_AGENT_BLOCKED): return .agentBlocked
        case UInt8(BS_KIND_AGENT_OK): return .agentOk
        case UInt8(BS_KIND_AGENT_FAILED): return .agentFailed
        case UInt8(BS_KIND_AGENT_ANSWER): return .agentAnswer
        case UInt8(BS_KIND_AGENT_MODEL): return .agentModel
        case UInt8(BS_KIND_AGENT_RUNNING): return .agentRunning
        case UInt8(BS_KIND_AGENT_PAST): return .agentPast
        case UInt8(BS_KIND_CLIP_TEXT): return .clipText
        case UInt8(BS_KIND_CLIP_IMAGE): return .clipImage
        case UInt8(BS_KIND_PORT): return .port
        case UInt8(BS_KIND_COMMAND): return .command
        case UInt8(BS_KIND_SETTING): return .setting
        case UInt8(BS_KIND_SHORTCUT): return .shortcut
        case UInt8(BS_KIND_LINK): return .quickLink
        case UInt8(BS_KIND_SNIPPET): return .snippet
        case UInt8(BS_KIND_SYSTEM): return .system
        case UInt8(BS_KIND_PROMPT): return .prompt
        default: return .app
        }
    }

    /// Scalar offsets, as the core counts them. Left as offsets rather than converted to
    /// UTF-16 here: only the handful of rows actually on screen are ever drawn, and this
    /// runs for all fifty on every keystroke.
    private static func offsets(_ values: UnsafePointer<UInt32>?, _ count: Int) -> [Int] {
        guard let values, count > 0 else { return [] }
        return UnsafeBufferPointer(start: values, count: count).map(Int.init)
    }

    private static func string(_ bytes: UnsafePointer<UInt8>?, _ count: Int) -> String {
        guard let bytes, count > 0 else { return "" }
        return String(decoding: UnsafeBufferPointer(start: bytes, count: count), as: UTF8.self)
    }
}

enum ClipPart: UInt8 {
    case full = 0
    case thumbnail = 1
}

struct ContentRuntime: Decodable, Sendable {
    let enabled: Bool
    let onBattery: Bool
    let roots: [String]
    let watchRoots: [String]
    let indexing: Bool
    let status: String
    var erasing = false
    var erased = false
    var eraseFailed = false
    var phase: String? = nil
    var visited: UInt64? = nil
    var indexed: UInt64? = nil
    var unchanged: UInt64? = nil
    var skipped: UInt64? = nil
    var failed: UInt64? = nil
    var removed: UInt64? = nil
    var currentPath: String? = nil
    var databaseBytes: UInt64? = nil
    var walBytes: UInt64? = nil
    var shmBytes: UInt64? = nil
    var vectorCatalogBytes: UInt64? = nil
    var documents: UInt64? = nil
    var embeddings: UInt64? = nil
    var statsSampled: Bool = false
    var extractionCounts: [UInt64]? = nil
    var overview: IndexOverview? = nil
}

/// Structured Index-page state from `bs_content_state`. Counts keep the core's meanings; the
/// shell only formats them.
struct IndexOverview: Decodable, Sendable, Equatable {
    struct Pass: Decodable, Sendable, Equatable {
        let checked, updated, sourceBytes, unchanged, skipped, unreadable, removed: UInt64
    }
    struct Semantic: Decodable, Sendable, Equatable {
        let enabled: Bool
        let embedded, eligible, passEmbedded, passFailed: UInt64?
    }
    struct Disk: Decodable, Sendable, Equatable {
        let database: UInt64
        let cache: UInt64?
    }
    struct Process: Decodable, Sendable, Equatable {
        let name: String
        let memory: UInt64
        let cpu: Double?
    }
    struct Pacing: Decodable, Sendable, Equatable {
        let lowImpact, battery, documents: Bool
        let documentLimitMB: UInt64
    }
    struct Busy: Decodable, Sendable, Equatable {
        let folder, path: String
        let changes: UInt64
    }
    struct Share: Decodable, Sendable, Equatable {
        let label: String
        let count, bytes: UInt64
    }
    struct Folder: Decodable, Sendable, Equatable {
        let folder, path: String
        let count, bytes: UInt64
    }
    struct Attention: Decodable, Sendable, Equatable {
        let name, folder, path, reason: String
    }
    struct Recent: Decodable, Sendable, Equatable {
        let name, folder, path: String
        let modified: Int64
    }
    struct Compact: Decodable, Sendable, Equatable {
        let before, after, removed: UInt64
    }
    let state, stage, message: String
    let compact: Compact?
    let compactError: String?
    let reclaimable: UInt64?
    let stageSeconds, lastPassAgo: UInt64?
    let lastPassSeconds: Double?
    let currentFolder: String?
    let pass: Pass
    let roots: [String]
    let semantic: Semantic
    let documents, pdfText: UInt64?
    var partialDocuments: UInt64? = nil
    var budgetBytes: UInt64? = nil
    let pdfIssues: [UInt64]?
    let disk: Disk
    let processes: [Process]
    let pacing: Pacing
    let busy: [Busy]
    let sampled: Bool
    let sampleAgo: UInt64?
    let kinds: [Share]
    let folders: [Folder]
    let attention: [Attention]
    let recent: [Recent]
}

struct IndexControls: Decodable, Sendable, Equatable {
    struct Health: Decodable, Sendable, Equatable {
        let available: Bool
        let model: String
        let dimensions: Int?
        let milliseconds: UInt64
        let message: String
    }
    let manualPause, policyPause, canPause, canRun, checking, recoveryNeeded: Bool
    let health: Health?
    let retrySeconds, embeddingTotal, embeddingRemaining: UInt64?
    let embeddingDone: UInt64
    let roots: [String]
}

enum ContentPauseReason: UInt8, Sendable {
    case none = 0
    case lowPower = 1
    case thermal = 2
    case battery = 3
    case unavailable = 4
}

struct ProcessDetails: Decodable, Sendable {
    let pid: UInt32
    let parentPid: UInt32
    let started: UInt64
    let name: String
    let executable: String
    let workingDirectory: String?
    let residentBytes: UInt64?
    let cpuTimeNs: UInt64?
    let userId: UInt32
    let commandLine: String?
    let children: [UInt32]

    var description: String {
        let memory = residentBytes.map { "\($0 / 1_048_576) MiB" } ?? "Unavailable"
        let cpu = cpuTimeNs.map { String(format: "%.2f seconds", Double($0) / 1_000_000_000) } ?? "Unavailable"
        let uptime = max(0, Date().timeIntervalSince1970 - Double(started) / 1_000_000)
        return "PID: \(pid)\nParent PID: \(parentPid)\nUser ID: \(userId)\nUptime: \(Int(uptime)) seconds\nResident memory: \(memory)\nCPU time (total): \(cpu)\nExecutable: \(executable.isEmpty ? "Unavailable" : executable)\nWorking directory: \(workingDirectory ?? "Unavailable")\nCommand: \(commandLine ?? "Unavailable")\nChildren: \(children.map(String.init).joined(separator: ", "))"
    }
}

enum NativeProcessBridge {
    private enum Failure: Error { case unavailable }
    static func inspect(pid: UInt64, started: UInt64) async throws -> ProcessDetails {
        let worker = Task.detached(priority: .userInitiated) {
            try Task.checkCancellation()
            guard let pid = UInt32(exactly: pid) else { throw Failure.unavailable }
            let blob = bs_process_inspect(pid, started)
            defer { bs_free_blob(blob) }
            guard let bytes = blob.data, blob.len > 0 else { throw Failure.unavailable }
            let decoder = JSONDecoder()
            decoder.keyDecodingStrategy = .convertFromSnakeCase
            let details = try decoder.decode(ProcessDetails.self, from: Data(bytes: bytes, count: blob.len))
            try Task.checkCancellation()
            return details
        }
        return try await withTaskCancellationHandler { try await worker.value } onCancel: { worker.cancel() }
    }

    static func signal(pid: UInt64, started: UInt64, force: Bool) async throws {
        try Task.checkCancellation()
        guard let pid = UInt32(exactly: pid), bs_process_signal(pid, started, force) else {
            throw Failure.unavailable
        }
    }
}

/// Hands clips to Rust from any thread.
///
/// The only part of `Core` callable off the main actor, and deliberately narrow: recording
/// a clip is where the slow work lives — a redb fsync behind a multi-megabyte screenshot —
/// so it must not run on the thread that answers the hotkey. `@unchecked` because the
/// pointer is only ever passed to `bs_clip_add`, which Rust documents as thread-safe: it
/// locks brief in-memory sections and does its disk write outside them.
private final class NativeCoreOwner: @unchecked Sendable {
    let handle: OpaquePointer
    init(handle: OpaquePointer) { self.handle = handle }
    deinit { bs_shutdown(handle) }
}

struct ClipSink: Sendable {
    fileprivate let owner: NativeCoreOwner
    private func withHandle<T>(_ body: (OpaquePointer) -> T) -> T {
        withExtendedLifetime(owner) { body(owner.handle) }
    }

    func content(_ id: UInt64) -> Data? {
        withHandle { handle in
            let blob = bs_clip_content(handle, id, 0)
            defer { bs_free_blob(blob) }
            guard let bytes = blob.data, blob.len > 0, blob.len <= 25 * 1024 * 1024 else { return nil }
            return Data(bytes: bytes, count: blob.len)
        }
    }

    func remove(_ id: UInt64) -> Bool { withHandle { bs_clip_remove($0, id) } }
    func pinned(_ id: UInt64) -> Bool { withHandle { bs_clip_pinned($0, id) } }
    func pin(_ id: UInt64, _ pinned: Bool) -> Bool { withHandle { bs_clip_pin($0, id, pinned) } }
    func clear() -> Bool { withHandle { bs_clips_clear_checked($0) } }

    func touch(_ id: UInt64) { withHandle { _ = bs_clip_touch($0, id) } }

    func add(
        image: Bool, content: Data, thumbnail: Data = Data(), text: Data = Data(),
        width: Int = 0, height: Int = 0
    ) {
        let kind = UInt8(image ? BS_KIND_CLIP_IMAGE : BS_KIND_CLIP_TEXT)
        // Nested so every buffer is alive for the whole call; Rust copies what it keeps.
        content.withUnsafeBytes { body in
            thumbnail.withUnsafeBytes { thumb in
                text.withUnsafeBytes { words in
                    var clip = BsClip(
                        kind: kind,
                        content: body.bindMemory(to: UInt8.self).baseAddress,
                        content_len: body.count,
                        thumbnail: thumb.bindMemory(to: UInt8.self).baseAddress,
                        thumbnail_len: thumb.count,
                        text: words.bindMemory(to: UInt8.self).baseAddress,
                        text_len: words.count,
                        width: UInt32(clamping: width),
                        height: UInt32(clamping: height))
                    withHandle { bs_clip_add($0, &clip) }
                }
            }
        }
    }
}

struct PassageReader: Sendable {
    fileprivate let owner: NativeCoreOwner
    func inspect(path: String) -> FileInspection? {
        guard !Task.isCancelled, path.utf8.count <= 4096, !path.contains("\0") else { return nil }
        return withExtendedLifetime(owner) {
            path.withCString { path in
                let blob = bs_content_inspect(owner.handle, path)
                defer { bs_free_blob(blob) }
                guard !Task.isCancelled, let bytes = blob.data, blob.len > 0, blob.len <= 32 * 1024 else { return nil }
                return try? JSONDecoder().decode(FileInspection.self, from: Data(bytes: bytes, count: blob.len))
            }
        }
    }

    func text(rowID: UInt64, path: String) -> String? {
        withExtendedLifetime(owner) {
            path.withCString { path in
                let blob = bs_content_passage(owner.handle, rowID, path)
                defer { bs_free_blob(blob) }
                guard let bytes = blob.data, blob.len > 0, blob.len <= 8192 else { return nil }
                return String(decoding: UnsafeBufferPointer(start: bytes, count: blob.len), as: UTF8.self)
            }
        }
    }
}

struct FileInspection: Decodable, Sendable {
    let title, detail, next, setting: String
    let root: String?
    let passages, embedded: UInt64?
}
