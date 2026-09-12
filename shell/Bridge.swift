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
        case .app, .file:
            let parent = (path as NSString).deletingLastPathComponent
            return (parent as NSString).abbreviatingWithTildeInPath
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
    private let handle: OpaquePointer

    /// The one entry point safe to call off the main thread. See `ClipSink`.
    nonisolated let clipSink: ClipSink

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
        handle = created
        clipSink = ClipSink(handle: created)
    }

    /// `isolated` so this runs on the main actor: `handle` is an `OpaquePointer` and
    /// therefore not `Sendable`, which a nonisolated deinit may not touch under Swift 6.
    ///
    /// A rescan thread inside Rust may still be walking when this fires. That is safe by
    /// construction — it holds its own `Arc` clones — which is precisely why
    /// `bs_shutdown` is documented as taking the handle away from Swift, not from Rust.
    isolated deinit {
        bs_shutdown(handle)
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

    /// The M3 frecency seam. A no-op in the core today; called anyway so that landing
    /// frecency needs no change on this side.
    func activate(_ id: UInt64) {
        bs_activate(handle, id)
    }

    /// Asks the local model about `request`. Returns at once: the answer arrives through the
    /// next `query`, whose `pending` stays true until the model has finished.
    func agentSubmit(_ request: String) {
        request.withCString { bs_agent_submit(handle, $0) }
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

    /// Forgets every clip, returning how many went. Clipboard history is its own file, so
    /// this cannot touch launch history.
    @discardableResult
    func clearClips() -> Int {
        Int(bs_clips_clear(handle))
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
        return UnsafeBufferPointer(start: items, count: list.len).map { row in
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
                highlights: offsets(result.highlights, result.highlights_len)
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

/// Hands clips to Rust from any thread.
///
/// The only part of `Core` callable off the main actor, and deliberately narrow: recording
/// a clip is where the slow work lives — a redb fsync behind a multi-megabyte screenshot —
/// so it must not run on the thread that answers the hotkey. `@unchecked` because the
/// pointer is only ever passed to `bs_clip_add`, which Rust documents as thread-safe: it
/// locks brief in-memory sections and does its disk write outside them.
struct ClipSink: @unchecked Sendable {
    fileprivate let handle: OpaquePointer

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
                    bs_clip_add(handle, &clip)
                }
            }
        }
    }
}
