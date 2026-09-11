import Foundation

/// One row of results. A value type, so nothing the panel holds points into memory
/// that Rust owns.
enum MatchKind {
    case app
    case file
    /// A calculated answer. Has no path — there is nothing on disk — and Enter copies it.
    case calc
    /// Clipboard history. No path either; Enter puts the content back on the pasteboard.
    case clipText
    case clipImage

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
                height: Int(result.height)
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
        case UInt8(BS_KIND_CLIP_TEXT): return .clipText
        case UInt8(BS_KIND_CLIP_IMAGE): return .clipImage
        default: return .app
        }
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
