import AppKit
import CoreImage

/// Bundle-path-keyed icon cache.
///
/// Two separate costs are avoided here, and both were found by measurement rather than
/// guessed at.
///
/// The *lookup* is cached because `NSWorkspace.icon(forFile:)` touches the filesystem. A
/// genuinely cold call runs a median of 0.23ms but reaches 79ms, and the eight slowest
/// summed to 108ms — about seven frames, paid on the first invocation after login.
///
/// The *image* is flattened because a workspace icon carries many representations, and
/// handing one to `NSImageView.image` makes AppKit re-derive and rescale a
/// representation on every assignment. Measured across eight rows: 1685us for raw icons
/// against 17us for flattened ones, which was roughly three quarters of the entire
/// keystroke path.
///
/// `.icns` is still never parsed here. AppKit produces the icon; this only caches a
/// rendered form of what it handed back.
@MainActor
enum IconCache {
    /// Every icon twice: in colour, and drained of it.
    ///
    /// The plate prints application icons grey except the one you are about to launch,
    /// which is what makes the selected row the only coloured thing in the list. Both
    /// forms are built together and cached together — draining one costs a single
    /// Core Image render of a 48-pixel bitmap, and doing it lazily would put that render
    /// on the keystroke that first selects the row.
    private static var cache: [String: NSImage] = [:]
    private static var drained: [String: NSImage] = [:]

    static func icon(for path: String, colour: Bool = true) -> NSImage {
        if let hit = colour ? cache[path] : drained[path] { return hit }
        let flattened = flatten(NSWorkspace.shared.icon(forFile: path))
        cache[path] = flattened
        let grey = drain(flattened)
        drained[path] = grey
        return colour ? flattened : grey
    }

    /// The same image with the colour taken out of it, as one flat bitmap.
    ///
    /// Through `flatten` at the end rather than handing an `NSCIImageRep` to an image
    /// view: a CI-backed representation is re-rendered on every draw, which is the exact
    /// cost `flatten` exists to remove.
    private static func drain(_ image: NSImage) -> NSImage {
        guard let tiff = image.tiffRepresentation,
            let source = CIImage(data: tiff),
            let filter = CIFilter(name: "CIColorControls")
        else { return image }
        filter.setValue(source, forKey: kCIInputImageKey)
        filter.setValue(0, forKey: kCIInputSaturationKey)
        guard let output = filter.outputImage else { return image }
        let grey = NSImage(size: image.size)
        grey.addRepresentation(NSCIImageRep(ciImage: output))
        return flatten(grey)
    }

    private static var pending: [String] = []
    private static var warming = false

    /// Queues `paths` to be warmed, one icon per main-loop turn.
    ///
    /// One at a time, and not in a loop, because a genuinely cold
    /// `NSWorkspace.icon(forFile:)` measured about 7ms on this machine — nearly half a
    /// frame each. Warming the whole index in one pass took 977ms, which is a full
    /// second of dead main thread at login. Yielding between every icon means the hotkey
    /// stays responsive the entire time it is warming, at the cost of the cache taking a
    /// few seconds to fill instead of arriving all at once.
    ///
    /// Paths already held are filtered out here, so calling this on every dismiss costs a
    /// dictionary lookup each and only does real work for apps that appeared since the
    /// last rescan.
    static func prewarm(_ paths: [String]) {
        // Deduped against what is already queued as well as what is already cached:
        // this runs on every dismiss, and without it a few dismisses during the initial
        // warm would queue the same hundred paths over and over.
        let queued = Set(pending)
        pending.append(contentsOf: paths.filter { cache[$0] == nil && !queued.contains($0) })
        guard !warming, !pending.isEmpty else { return }
        warming = true
        warmNext()
    }

    private static func warmNext() {
        // Anything warmed by a real query since being queued is already done.
        while let next = pending.first, cache[next] != nil {
            pending.removeFirst()
        }
        guard !pending.isEmpty else {
            warming = false
            return
        }
        let path = pending.removeFirst()
        _ = icon(for: path)
        Task { @MainActor in warmNext() }
    }

    /// Warms everything synchronously.
    ///
    /// Only for the latency harness, which needs a known-warm cache before it can measure
    /// steady state. The app deliberately uses the incremental path above instead.
    static func warmNow(_ paths: [String]) {
        for path in paths where cache[path] == nil {
            _ = icon(for: path)
        }
    }

    /// The agent's two icons. Its commands lead with a numbered rail and its outcomes
    /// with a mark, so only the rows that are requests rather than commands need one.
    static let agentPrompt: NSImage? = symbol("terminal", "Ask the local model")
    static let agentPast: NSImage? = symbol("clock.arrow.circlepath", "Earlier request")

    private static func symbol(_ name: String, _ description: String) -> NSImage? {
        NSImage(systemSymbolName: name, accessibilityDescription: description)
    }

    /// Where clip thumbnails come from. Set once at launch; left `nil` by the latency
    /// harness, which never shows a clip.
    static var clipThumbnails: ((UInt64) -> Data?)?
    private static var clipCache: [UInt64: NSImage] = [:]
    private static var clipDrained: [UInt64: NSImage] = [:]

    /// The icon for a text clip.
    static let textClip: NSImage? = {
        let image = NSImage(
            systemSymbolName: "doc.on.clipboard", accessibilityDescription: "Copied text")
        image?.size = NSSize(width: ResultsView.iconSize, height: ResultsView.iconSize)
        return image
    }()

    /// An image clip's stored thumbnail, flattened like every app icon — the same
    /// 1685µs-to-17µs saving on `NSImageView.image` applies to a PNG as to a workspace
    /// icon. Falls back to the text-clip symbol if the thumbnail is missing.
    static func thumbnail(forClip id: UInt64, colour: Bool = true) -> NSImage? {
        if let hit = colour ? clipCache[id] : clipDrained[id] { return hit }
        guard let data = clipThumbnails?(id), let image = NSImage(data: data) else {
            return textClip
        }
        // Bounded, because clips come and go while this process lives for weeks and the
        // cache would otherwise hold every thumbnail ever shown. History itself caps at
        // 200 clips, so dropping everything past that costs a re-decode at worst.
        if clipCache.count >= 256 {
            clipCache.removeAll(keepingCapacity: true)
            clipDrained.removeAll(keepingCapacity: true)
        }
        let flattened = flatten(image)
        clipCache[id] = flattened
        let grey = drain(flattened)
        clipDrained[id] = grey
        return colour ? flattened : grey
    }

    /// The largest backing scale across every attached display, not the current screen's.
    /// The panel follows the pointer between displays, so caching at the maximum means
    /// moving onto a Retina screen can never reveal a blurry icon. Being generous costs a
    /// few hundred kilobytes.
    private static var scale: CGFloat {
        NSScreen.screens.map(\.backingScaleFactor).max() ?? 2
    }

    private static func flatten(_ icon: NSImage) -> NSImage {
        let side = ResultsView.iconSize
        let pixels = Int((side * scale).rounded())
        guard pixels > 0,
            let rep = NSBitmapImageRep(
                bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels,
                bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)
        else {
            // Allocation failed. A correctly sized slow icon beats no icon.
            icon.size = NSSize(width: side, height: side)
            return icon
        }
        rep.size = NSSize(width: side, height: side)

        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
        icon.draw(in: NSRect(x: 0, y: 0, width: side, height: side))
        NSGraphicsContext.restoreGraphicsState()

        let flattened = NSImage(size: NSSize(width: side, height: side))
        flattened.addRepresentation(rep)
        return flattened
    }
}

/// An epoch's instant in the viewer's own zone, in the same shape as the UTC row above it so
/// the two read side by side.
@MainActor
private enum LocalTime {
    private static let formatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd HH:mm:ss zzz"
        return formatter
    }()

    static func describe(_ timestamp: UInt64) -> String {
        formatter.string(from: Date(timeIntervalSince1970: TimeInterval(timestamp)))
    }
}

/// What the selected row shows in place of its folder: the keys that act on it beyond ↩.
/// Only on the selected row — on every row it would be the same line fifty times over.
@MainActor
enum ActionHint {
    /// Bundle paths of running apps, seeded once and then kept current. Not asked for on
    /// each show: building this set there measured 5.2ms median, a third of the frame, for
    /// one line of hint text.
    private static var running: Set<String> = []
    private static var observation: NSKeyValueObservation?

    static func startTracking() {
        guard observation == nil else { return }
        let workspace = NSWorkspace.shared
        running = Set(workspace.runningApplications.compactMap { $0.bundleURL?.path })
        // KVO, not the launch and terminate notifications: measured, those are posted only
        // for Dock apps, and the index deliberately holds menu-bar ones like Docker and
        // Ollama. KVO reported both kinds, as deltas, within 130ms.
        observation = workspace.observe(\.runningApplications, options: [.new, .old]) { _, change in
            let added = (change.newValue ?? []).compactMap { $0.bundleURL?.path }
            let removed = (change.oldValue ?? []).compactMap { $0.bundleURL?.path }
            let apply: @MainActor () -> Void = {
                running.subtract(removed)
                running.formUnion(added)
            }
            // AppKit documents this property as changing only while the main run loop runs,
            // so this is the main thread — checked rather than assumed, as in `HotKey`.
            if Thread.isMainThread {
                MainActor.assumeIsolated(apply)
            } else {
                Task { @MainActor in apply() }
            }
        }
    }

    static func text(for match: Match) -> String? {
        switch match.kind {
        case .app:
            let quit = isRunning(match.path) ? " · ⌃↩ QUIT" : ""
            return "⌘↩ REVEAL · ⌥↩ PATH" + quit
        case .port:
            // Reveal only when `ps` could name the executable; the core leaves the path
            // empty when it could not, and the row then has nothing to show in Finder.
            let reveal = match.path.isEmpty ? "" : " · ⌘↩ REVEAL"
            return "↩ COPY PID" + reveal + " · ⌃↩ STOP"
        case .file:
            // Quick Look is advertised on files and not on apps: previewing a bundle shows
            // you its icon, which you can already see in the row.
            return "⌘Y LOOK · ⌘↩ REVEAL · ⌥↩ PATH"
        case .agentPrompt:
            return "↩ ASK"
        case .agentStep:
            return "↩ RUN ALL · ⌘↩ COPY"
        case .agentOk, .agentFailed:
            return "↩ COPY OUTPUT · ⌘↩ COPY COMMAND"
        case .agentRunning:
            // Said in its own column now rather than in the subtitle, which is counting
            // the seconds this has been going.
            return "↩ LEAVE IT · ⎋ STOP"
        case .agentPast:
            return "↩ ASK AGAIN · ⌘↩ COPY"
        case .command: return "↩ COMPLETE"
        case .setting: return "↩ OPEN SETTING"
        case .shortcut: return "↩ SAVE"
        case .quickLink: return "↩ OPEN"
        case .snippet: return "↩ PASTE"
        case .system: return "↩ RUN"
        case .prompt: return "↩ USE"
        case .event: return "↩ JOIN OR OPEN"
        case .calc, .tool, .clipText, .clipImage:
            return "↩ COPY"
        // An answer draws its own key line, and a refusal and a model both carry a badge.
        case .agentAnswer, .agentModel, .header, .agentBlocked:
            return nil
        }
    }

    private static func isRunning(_ path: String) -> Bool {
        running.contains(path) || running.contains(resolved(path))
    }

    /// The index and a running app can spell one bundle's path differently — measured, a
    /// bundle indexed under `/private/tmp` reports its URL as `/tmp` — and
    /// `/Applications/Safari.app` is itself a symlink into a cryptex. So a lookup compares
    /// the resolved form as well as the literal one.
    static func resolved(_ path: String) -> String {
        URL(fileURLWithPath: path).resolvingSymlinksInPath().path
    }
}

/// "2 min ago" for clip rows. Main-actor state because `RelativeDateTimeFormatter` is
/// not `Sendable`, and building one per row per keystroke would be wasteful.
@MainActor
private enum ClipTime {
    private static let formatter: RelativeDateTimeFormatter = {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .short
        return formatter
    }()

    static func describe(_ timestamp: UInt64) -> String {
        formatter.localizedString(
            for: Date(timeIntervalSince1970: TimeInterval(timestamp)), relativeTo: Date())
    }
}

/// What to call an image clip in its row.
///
/// "Screenshot" when its pixel size matches an attached display exactly — a reliable tell
/// for a full-screen capture — and "Image" otherwise. Decided at render time because only
/// the shell knows the displays, and they can change after the copy.
@MainActor
private enum ImageLabel {
    /// The placeholder name Rust gives an image in which no text was recognised.
    static let placeholder = "Image"

    static func of(width: Int, height: Int) -> String {
        let matchesDisplay = NSScreen.screens.contains { screen in
            let points = screen.frame.size
            let scale = screen.backingScaleFactor
            return [points, CGSize(width: points.width * scale, height: points.height * scale)]
                .contains { Int($0.width.rounded()) == width && Int($0.height.rounded()) == height }
        }
        return matchesDisplay ? "Screenshot" : "Image"
    }
}

/// How a row's leading edge is spent.
///
/// Three shapes, because three kinds of row want different things there: a name wants
/// the icon of the thing it names, a converted value wants the form it is in, and a
/// command wants its place in the plan or the mark saying how it ended.
private enum Lead {
    /// The application's or file's own icon.
    case icon
    /// A fixed rail naming the form — "RAW", "BINARY" — so the figures beside it line up
    /// as a column you can read down.
    case rail(String)
    /// A narrow rail: the step's number, or ✓ / ✗ / ■ once it has run.
    case mark(String, NSColor)
    /// Nothing at all; the text starts at the gutter.
    case none
}

/// One fixed-height row of the plate: a lead, a name, what it is, and — at the right —
/// either the keys that act on it or what became of it.
@MainActor
private final class ResultRow: NSView {
    private let separator = PlateView(fill: Theme.hairline)
    private let iconView = NSImageView()
    private let rail = NSTextField(labelWithString: "")
    private let title = NSTextField(labelWithString: "")
    private let subtitle = NSTextField(labelWithString: "")
    /// The keys that act on this row, or the outcome it ended in.
    private let note = NSTextField(labelWithString: "")
    /// A refusal, and only a refusal: the one place in the design where a label becomes a
    /// filled field.
    private let badge = PlateView(fill: Theme.danger)
    private let badgeLabel = NSTextField(labelWithString: "")

    private let text = NSStackView()

    /// The width of whatever `Lead` put at the leading edge, and where the text starts
    /// after it. Both change per row, so both are held rather than rebuilt. Lazy because
    /// they anchor against `self`, which does not exist until `super.init` has run.
    private lazy var leadWidth = iconView.widthAnchor.constraint(
        equalToConstant: Theme.iconSize)
    private lazy var textLeading = text.leadingAnchor.constraint(
        equalTo: leadingAnchor, constant: Theme.gutter + Theme.iconSize + Theme.gap)

    private var shown: Match?
    /// What the title actually says, which is not always `match.name` — a screenshot with
    /// no text in it is titled "Screenshot". The matched characters are offsets into
    /// `match.name`, so they are only drawn when the two are the same string.
    private var shownTitle = ""

    /// Names of things are prose; commands and computed values are not. A monospaced title
    /// is the cheapest way to say "this is something you would type", and it lines digits up.
    private static let nameFont = Theme.text(14.5, .medium)
    private static let commandFont = Theme.mono(12.5)
    /// Converted values are the row's whole point, so they are set at reading size.
    private static let figureFont = Theme.mono(17)

    private static func font(for kind: MatchKind) -> NSFont {
        switch kind {
        case .calc, .tool: figureFont
        case .agentStep, .agentRunning, .agentOk, .agentFailed, .agentBlocked: commandFont
        case .clipText: commandFont
        default: nameFont
        }
    }

    override init(frame: NSRect) {
        super.init(frame: frame)

        title.lineBreakMode = .byTruncatingTail
        // A command may carry newlines — a heredoc writing a file — and a row is one line.
        title.maximumNumberOfLines = 1
        title.usesSingleLineMode = true
        subtitle.font = Theme.text(11)
        subtitle.maximumNumberOfLines = 1
        subtitle.usesSingleLineMode = true
        subtitle.lineBreakMode = .byTruncatingMiddle
        // Low horizontal compression resistance on both labels, so long text truncates
        // instead of widening the window. A label's intrinsic width is its whole string,
        // at the default priority of 750 — and the panel's width is not a required
        // constraint — so a long clip made Auto Layout grow the panel to 1812pt, off the
        // right edge of the screen. Truncation needs something willing to give way.
        title.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        subtitle.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        rail.lineBreakMode = .byTruncatingTail
        // One line: the label's attributed string carries a wrapping paragraph style, which broke
        // "centimetres" in the middle of the word on a second line.
        rail.maximumNumberOfLines = 1
        rail.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        iconView.imageScaling = .scaleProportionallyUpOrDown

        badgeLabel.translatesAutoresizingMaskIntoConstraints = false
        badge.addSubview(badgeLabel)

        text.setViews([title, subtitle], in: .leading)
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 1

        for view in [separator, iconView, rail, text, note, badge] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }

        NSLayoutConstraint.activate([
            // The hairline between two rows belongs to the lower one and runs the whole
            // width of the plate, not just the width of the text.
            separator.topAnchor.constraint(equalTo: topAnchor),
            separator.leadingAnchor.constraint(equalTo: leadingAnchor),
            separator.trailingAnchor.constraint(equalTo: trailingAnchor),
            separator.heightAnchor.constraint(equalToConstant: 1),

            iconView.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Theme.gutter),
            iconView.centerYAnchor.constraint(equalTo: centerYAnchor),
            leadWidth,
            iconView.heightAnchor.constraint(equalToConstant: Theme.iconSize),

            rail.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Theme.gutter),
            rail.centerYAnchor.constraint(equalTo: centerYAnchor),
            // Bounded, so a label longer than the rail truncates instead of running under
            // the value beside it. Measured the ugly way: "formatted · 8 lines" drew
            // straight through the JSON it was labelling.
            rail.widthAnchor.constraint(lessThanOrEqualToConstant: Theme.railWidth - 8),

            textLeading,
            text.centerYAnchor.constraint(equalTo: centerYAnchor),
            text.trailingAnchor.constraint(lessThanOrEqualTo: note.leadingAnchor, constant: -12),
            text.trailingAnchor.constraint(lessThanOrEqualTo: badge.leadingAnchor, constant: -12),

            note.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -Theme.gutter),
            note.centerYAnchor.constraint(equalTo: centerYAnchor),

            // The badge's own padding puts its type where a plain note's type sits, so the
            // two line up on the right margin whichever a row happens to have.
            badge.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -(Theme.gutter - 6)),
            badge.centerYAnchor.constraint(equalTo: centerYAnchor),
            badgeLabel.leadingAnchor.constraint(equalTo: badge.leadingAnchor, constant: 6),
            badgeLabel.trailingAnchor.constraint(equalTo: badge.trailingAnchor, constant: -6),
            badgeLabel.topAnchor.constraint(equalTo: badge.topAnchor, constant: 3),
            badgeLabel.bottomAnchor.constraint(equalTo: badge.bottomAnchor, constant: -3),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    func show(_ match: Match, selected: Bool, separated: Bool) {
        shown = match
        separator.isHidden = !separated

        let (name, detail) = Self.contents(of: match)
        shownTitle = name
        subtitle.stringValue = detail
        subtitle.isHidden = detail.isEmpty

        apply(Self.lead(of: match))
        select(selected)
    }

    /// Re-applies only what depends on the row being the selected one, so moving the
    /// highlight does not rebuild a row that has not otherwise changed.
    func select(_ on: Bool) {
        guard let match = shown else { return }
        renderTitle(match, selected: on)

        if !iconView.isHidden {
            iconView.image = Self.image(for: match, colour: on)
        }
        // The rail comes forward with the row it names.
        if !rail.isHidden, case let .rail(text) = Self.lead(of: match) {
            rail.attributedStringValue = Theme.label(
                text.uppercased(), size: 9.5, tracking: 0.14,
                color: on ? Theme.muted : Theme.faint)
        }

        subtitle.textColor = on ? Theme.muted : Theme.faint

        switch Self.rightHand(of: match, selected: on) {
        case let .filled(text, colour):
            badge.isHidden = false
            note.isHidden = true
            badge.fill(colour)
            badgeLabel.attributedStringValue = Theme.label(
                text, size: 9.5, tracking: 0.12, color: Theme.surface, weight: .semibold)
        case let .word(text, colour):
            badge.isHidden = true
            note.isHidden = false
            note.attributedStringValue = Theme.label(
                text, size: 9.5, tracking: 0.12, color: colour)
        case let .keys(text):
            badge.isHidden = true
            note.isHidden = false
            note.attributedStringValue = Theme.label(
                text, size: 10, tracking: 0.06, color: Theme.muted)
        case .nothing:
            badge.isHidden = true
            note.isHidden = true
        }
    }

    /// What sits at the right of a row.
    private enum RightHand {
        /// A filled field. Two rows in the whole design earn one: a command that will not
        /// run, and the model that answers.
        case filled(String, NSColor)
        /// What became of a past request, coloured by how it ended.
        case word(String, NSColor)
        /// The keys that act on this row — the selected row only. On every row it would
        /// be the same line fifty times over.
        case keys(String)
        case nothing
    }

    private static func rightHand(of match: Match, selected: Bool) -> RightHand {
        switch match.kind {
        case .agentBlocked:
            return .filled("REFUSED", Theme.danger)
        case .agentModel where match.subtitle.hasPrefix(currentPrefix):
            return .filled("CURRENT", Theme.accent)
        case .agentPast:
            let word = match.subtitle.components(separatedBy: " · ").first ?? ""
            if !word.isEmpty { return .word(word.uppercased(), outcomeColour(word)) }
        default:
            break
        }
        guard selected, let keys = ActionHint.text(for: match) else { return .nothing }
        return .keys(keys)
    }

    /// Ran, answered, left running, failed — each its own colour, so a list of past
    /// requests reads as a log you can scan down rather than a column of identical words.
    private static func outcomeColour(_ outcome: String) -> NSColor {
        if outcome.hasPrefix("left running") { return Theme.warn }
        switch outcome {
        case "ran": return Theme.ok
        case "failed", "blocked", "stopped": return Theme.danger
        default: return Theme.faint
        }
    }

    /// How the core marks the model in use, in a row it otherwise fills with the size.
    private static let currentPrefix = "current · "

    private func renderTitle(_ match: Match, selected: Bool) {
        var base: [NSAttributedString.Key: Any] = [
            .font: Self.font(for: match.kind),
            .foregroundColor: selected ? Theme.ink : Theme.inkSoft,
            .paragraphStyle: Theme.truncating,
        ]
        // A refusal is struck through rather than removed: you see what the model wanted,
        // and that it will not happen.
        if match.kind == .agentBlocked {
            base[.foregroundColor] = Theme.muted
            base[.strikethroughStyle] = NSUnderlineStyle.single.rawValue
            base[.strikethroughColor] = Theme.danger
        }
        let highlights = shownTitle == match.name ? match.highlights : []
        title.attributedStringValue =
            highlights.isEmpty
            ? NSAttributedString(string: shownTitle, attributes: base)
            : Self.highlighted(shownTitle, highlights, base: base)
    }

    /// The characters nucleo matched, set in the accent — the only thing colour does in a
    /// results list besides mark the selection.
    ///
    /// The core counts in Unicode scalars and `NSAttributedString` counts in UTF-16 units.
    /// The two agree for anything outside the astral planes, which every application name
    /// on this machine is, so the general conversion only runs when it has to.
    private static func highlighted(
        _ name: String, _ offsets: [Int], base: [NSAttributedString.Key: Any]
    ) -> NSAttributedString {
        let text = NSMutableAttributedString(string: name, attributes: base)
        let scalars = name.unicodeScalars
        let units = name.utf16.count
        let aligned = scalars.count == units
        for offset in offsets {
            let range: NSRange
            if aligned {
                guard offset < units else { continue }
                range = NSRange(location: offset, length: 1)
            } else {
                guard
                    let start = scalars.index(
                        scalars.startIndex, offsetBy: offset, limitedBy: scalars.endIndex),
                    start < scalars.endIndex
                else { continue }
                range = NSRange(start..<scalars.index(after: start), in: name)
            }
            text.addAttribute(.foregroundColor, value: Theme.accent, range: range)
        }
        return text
    }

    /// Fills the leading edge and moves the text to start after it.
    private func apply(_ lead: Lead) {
        iconView.isHidden = true
        rail.isHidden = true
        switch lead {
        case .icon:
            iconView.isHidden = false
            // A symbol standing in for something sits back the way a file icon does; an
            // application's own icon, and a clip's own thumbnail, are left as they are —
            // and `select` decides whether those keep their colour.
            iconView.contentTintColor =
                switch shown?.kind {
                case .app, .file, .clipImage, .port: nil
                default: Theme.faint
                }
            leadWidth.constant = Theme.iconSize
            textLeading.constant = Theme.gutter + Theme.iconSize + Theme.gap
        case let .rail(text):
            rail.isHidden = false
            rail.attributedStringValue = Theme.label(
                text.uppercased(), size: 9.5, tracking: 0.14, color: Theme.faint)
            textLeading.constant = Theme.gutter + Theme.railWidth
        case let .mark(text, colour):
            rail.isHidden = false
            rail.attributedStringValue = NSAttributedString(
                string: text,
                attributes: [.font: Theme.mono(10, .medium), .foregroundColor: colour])
            textLeading.constant = Theme.gutter + Theme.stepRail
        case .none:
            textLeading.constant = Theme.gutter
        }
    }

    private static func lead(of match: Match) -> Lead {
        switch match.kind {
        case .app, .file, .clipText, .clipImage, .agentPrompt, .agentPast, .port, .command, .setting, .shortcut, .quickLink, .snippet, .system, .prompt, .event:
            .icon
        case .calc:
            .rail("Result")
        case .tool:
            .rail(match.detail.isEmpty ? "Result" : match.detail)
        // A plan reads as a numbered list. `id` is the row's place in what the core
        // handed over, and the header sits at zero, so the steps number from one.
        case .agentStep:
            .mark(String(match.id), Theme.faint)
        case .agentOk:
            .mark("✓", Theme.ok)
        case .agentFailed:
            .mark("✗", Theme.danger)
        case .agentRunning:
            .mark("■", Theme.accent)
        case .agentBlocked:
            .mark("✗", Theme.danger)
        case .agentModel, .agentAnswer, .header:
            .none
        }
    }

    /// The row's two lines, with everything the shell alone knows folded in — a clip's
    /// age, an epoch in the local zone, the outcome lifted out of the agent's subtitle.
    private static func contents(of match: Match) -> (String, String) {
        switch match.kind {
        case .clipImage:
            // Titled by the text found in it. With none, the label takes the title, so the
            // row never shows a placeholder or a pair of pixel dimensions.
            let label = ImageLabel.of(width: match.width, height: match.height)
            let time = ClipTime.describe(match.timestamp)
            return match.name == ImageLabel.placeholder
                ? (label, time) : (match.name, "\(label) · \(time)")
        case .clipText:
            return (match.name, ClipTime.describe(match.timestamp))
        case .tool where match.timestamp > 0:
            // The form is on the rail now, so the second line is only the instant.
            return (match.name, LocalTime.describe(match.timestamp))
        case .tool, .calc:
            return (oneLine(match.name), "")
        case .agentPast, .agentRunning:
            // The core writes these as one sentence — "ran · 2 commands · ~/dev · model",
            // "running… 6s · ↩ leave it running · ⎋ stop". The outcome moves to the right
            // of the row and the keys to the note, so the rest is what is left. A string
            // that does not split the way this expects simply shows whole.
            let parts = match.subtitle.components(separatedBy: " · ")
            guard parts.count > 1 else { return (match.name, match.subtitle) }
            let rest =
                match.kind == .agentPast
                ? parts.dropFirst() : parts.prefix { !$0.hasPrefix("↩") && !$0.hasPrefix("⎋") }
            return (match.name, rest.joined(separator: " · "))
        case .agentModel:
            // "current · 2.4 GB" marks the model in use; the badge says so instead.
            return (match.name, match.subtitle.replacingOccurrences(of: currentPrefix, with: ""))
        case .file where match.page > 0 || match.line > 0:
            let paged = URL(fileURLWithPath: match.path).pathExtension.lowercased() == "pptx" ? "slide" : "p."
            let place = match.page > 0 ? "\(paged) \(match.page)" : "line \(match.line)"
            return (match.name, match.subtitle.isEmpty ? place : "\(place) · \(match.subtitle)")
        case .app, .file, .port, .command, .setting, .shortcut, .quickLink, .snippet, .system, .prompt, .event, .header, .agentPrompt, .agentStep, .agentBlocked,
            .agentOk, .agentFailed, .agentAnswer:
            return (match.name, match.subtitle)
        }
    }

    /// A value with newlines in it, on one line.
    ///
    /// Display only — Enter copies `match.name`, so formatted JSON still reaches the
    /// clipboard formatted. Without this, the row for a pretty-printed blob would be
    /// titled `{`, which says nothing about which row you are standing on.
    private static func oneLine(_ text: String) -> String {
        guard text.contains(where: \.isNewline) else { return text }
        return text.split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .joined(separator: " ")
    }

    /// `colour` is false for every row but the selected one: a list of grey icons with a
    /// single coloured one in it says which row Enter acts on without a second mark.
    private static func image(for match: Match, colour: Bool) -> NSImage? {
        switch match.kind {
        case .command: NSImage(systemSymbolName: "terminal", accessibilityDescription: "Command")
        case .setting: NSImage(systemSymbolName: "gearshape", accessibilityDescription: "Setting")
        case .shortcut: NSImage(systemSymbolName: "plus.circle", accessibilityDescription: "Save shortcut")
        case .quickLink: NSImage(systemSymbolName: "link", accessibilityDescription: "Quick link")
        case .snippet: NSImage(systemSymbolName: "text.quote", accessibilityDescription: "Snippet")
        case .system: NSImage(systemSymbolName: "power", accessibilityDescription: "System command")
        case .prompt: NSImage(systemSymbolName: "sparkles", accessibilityDescription: "AI command")
        case .event: NSImage(systemSymbolName: match.path.hasPrefix("https") ? "video" : "calendar", accessibilityDescription: "Calendar event")
        case .agentPrompt: IconCache.agentPrompt
        case .agentPast: IconCache.agentPast
        case .clipText: IconCache.textClip
        case .clipImage: IconCache.thumbnail(forClip: match.id, colour: colour)
        case .app, .file: IconCache.icon(for: match.path, colour: colour)
        // A listener whose executable `ps` could not name has nothing to draw.
        case .port where !match.path.isEmpty:
            IconCache.icon(for: match.path, colour: colour)
        default: nil
        }
    }
}

/// The model's prose answer, wrapped over as many lines as it needs.
///
/// Its own row class because everything else here is one fixed-height line: an answer is the
/// only result whose height depends on what it says.
@MainActor
final class AnswerRow: NSView {
    private let label = NSTextField(wrappingLabelWithString: "")
    private let footer = NSTextField(labelWithString: "")
    private let bar = PlateView(fill: Theme.accent)
    private static let font = Theme.text(14)
    private static let top: CGFloat = 16
    private static let bottom: CGFloat = 16
    /// The key line under the answer, and the air above it.
    private static let footerHeight: CGFloat = 13
    private static let footerGap: CGFloat = 12
    /// Past this it scrolls rather than filling the screen with one answer.
    private static let maximum: CGFloat = 420

    /// What the table should give this text, measured with the font it will be drawn in.
    static func height(for text: String, width: CGFloat) -> CGFloat {
        let box = NSSize(width: max(width - Theme.gutter * 2, 1), height: .greatestFiniteMagnitude)
        let measured = (text as NSString).boundingRect(
            with: box,
            options: [.usesLineFragmentOrigin, .usesFontLeading],
            attributes: [.font: font]
        )
        let chrome = top + bottom + footerGap + footerHeight
        return min(max(ceil(measured.height) + chrome, ResultsView.rowHeight), maximum)
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        // Its own wash, rather than the sliding selection behind it: an answer is not a
        // row in a list you step through, it is the whole reply.
        layer?.backgroundColor = Theme.accent.withAlphaComponent(0.05).cgColor

        label.font = Self.font
        label.textColor = Theme.ink
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        footer.attributedStringValue = Theme.label(
            "↩ COPY THE ANSWER", size: 10, tracking: 0.12, color: Theme.faint)

        for view in [bar, label, footer] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }

        NSLayoutConstraint.activate([
            bar.leadingAnchor.constraint(equalTo: leadingAnchor),
            bar.topAnchor.constraint(equalTo: topAnchor),
            bar.bottomAnchor.constraint(equalTo: bottomAnchor),
            bar.widthAnchor.constraint(equalToConstant: Theme.selectionBar),

            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Theme.gutter),
            label.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -Theme.gutter),
            label.topAnchor.constraint(equalTo: topAnchor, constant: Self.top),

            footer.leadingAnchor.constraint(equalTo: label.leadingAnchor),
            footer.topAnchor.constraint(
                equalTo: label.bottomAnchor, constant: Self.footerGap),
            footer.bottomAnchor.constraint(
                lessThanOrEqualTo: bottomAnchor, constant: -Self.bottom),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    func show(_ text: String) {
        label.stringValue = text
        label.preferredMaxLayoutWidth = max(bounds.width - Theme.gutter * 2, 1)
    }
}

/// A section title: small tracked capitals with a hairline running from them to the edge
/// of the plate. The rule is what makes a label read as the head of a section rather than
/// a very quiet result.
@MainActor
private final class HeaderRow: NSView {
    private let label = NSTextField(labelWithString: "")
    /// The agent's working folder, beside the title — the one thing a header has to say
    /// about itself.
    private let detail = NSTextField(labelWithString: "")
    private let line = PlateView(fill: Theme.hairline)

    override init(frame: NSRect) {
        super.init(frame: frame)

        let text = NSStackView(views: [label, detail])
        text.spacing = 10
        text.translatesAutoresizingMaskIntoConstraints = false
        addSubview(text)
        addSubview(line)

        NSLayoutConstraint.activate([
            // Level with the gutter every row's icon starts at.
            text.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Theme.gutter),
            text.centerYAnchor.constraint(equalTo: centerYAnchor),
            line.leadingAnchor.constraint(equalTo: text.trailingAnchor, constant: 12),
            line.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -Theme.gutter),
            line.centerYAnchor.constraint(equalTo: centerYAnchor),
            line.heightAnchor.constraint(equalToConstant: 1),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    /// `detail` is the agent's working folder; the welcome screen's headers have none.
    func show(_ title: String, detail path: String) {
        label.attributedStringValue = Theme.label(
            title.uppercased(), size: 9.5, tracking: 0.17, color: Theme.faint)
        detail.isHidden = path.isEmpty
        detail.attributedStringValue = Theme.label(
            (path as NSString).abbreviatingWithTildeInPath, size: 9.5, tracking: 0.10,
            color: Theme.faint)
    }
}

/// The selected row: a wash the width of the plate, and the accent as a bar at its left
/// edge. The accent appears exactly once in a results list, and this is it.
@MainActor
private final class Selection: NSView {
    private let bar = PlateView(fill: Theme.accent)

    init() {
        super.init(frame: .zero)
        wantsLayer = true
        layer?.backgroundColor = Theme.selection.cgColor
        bar.translatesAutoresizingMaskIntoConstraints = true
        addSubview(bar)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    /// Not `layout()`: the wash's frame is set directly, and during the glide between two
    /// rows AppKit resizes it without ever marking it as needing layout.
    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        bar.frame = NSRect(x: 0, y: 0, width: Theme.selectionBar, height: newSize.height)
    }
}

/// The results list: a scrolling table of fixed-height rows.
///
/// An `NSTableView` since the list learned to scroll, reversing M1's hand-drawn stack —
/// CLAUDE.md's open question, answered by measurement rather than taste. For eight rows the
/// stack was right, and a table's reuse machinery would have been pure overhead. For a
/// scrolling list of up to fifty it is the other way round: refilling fifty stacked rows on
/// every keystroke measured 1.56ms median and peaked at 21ms, over a frame, where a table
/// builds and fills only the rows actually on screen. It also brings the native scrolling a
/// hand-rolled list would have to fake — momentum, rubber-banding, the overlay scroller.
@MainActor
final class ResultsView: NSScrollView, NSTableViewDataSource, NSTableViewDelegate {
    static let rowHeight: CGFloat = Theme.rowHeight
    /// Exactly half a row: two headers stand in one row's height, which is the arithmetic the
    /// core's welcome screen relies on to fit `max_results` rows unscrolled. Keep it derived.
    ///
    /// The drawing says 26. Twenty-two is what keeps that arithmetic true, and the
    /// arithmetic is load-bearing where the four extra points are not.
    static let headerHeight: CGFloat = (rowHeight - spacing) / 2
    static let iconSize: CGFloat = Theme.iconSize
    /// No gap between rows. A row is separated from the one above it by a hairline it
    /// draws itself, so a gap here would put a band of plate through every rule.
    private static let spacing: CGFloat = 0
    /// Nothing at the top: the first section title sits directly under the heavy rule,
    /// the way it does in the drawing. Six at the bottom so the last row is not flush
    /// against the plate's edge.
    private static let padding = NSEdgeInsets(top: 0, left: 0, bottom: 6, right: 0)
    private static let rowID = NSUserInterfaceItemIdentifier("result")

    private let table = NSTableView()

    /// One wash that slides between rows, rather than a fill each row paints for itself.
    ///
    /// Full bleed now, and square: the drawing's selection is a band across the plate with
    /// the accent as a bar at its left edge, not an inset pill. It still sits *behind* the
    /// rows, so no text is dimmed by it, and it is still one view that moves rather than
    /// fifty that repaint.
    private let highlight = Selection()
    /// Long enough to read as movement, short enough that holding ↓ does not lag behind.
    private static let glide: CFTimeInterval = 0.12
    /// The rows' own entrance. Deliberately not run while typing — see `update(_:entering:)`.
    private static let entrance: CFTimeInterval = 0.09

    /// Row views the table has let go of, handed straight back out.
    ///
    /// Kept by hand because `makeView(withIdentifier:)` was not recycling: measured, 400
    /// updates vended 2,950 distinct row views — a fresh icon, two labels, a stack view and
    /// their constraints for every visible row on every re-tile. That construction was the
    /// entire cost of a keystroke that changed the list's height: 1.85ms median, 30ms peak.
    private var pool: [ResultRow] = []
    private var headerPool: [HeaderRow] = []
    private var answerPool: [AnswerRow] = []

    /// Rows shown before the list scrolls — `max_results`. A `var` because the settings
    /// window changes it without a restart; it was read once when the panel was built.
    private var visibleRows: Int

    private(set) var matches: [Match] = []

    /// Which row Enter will launch. Lives here rather than in Rust: it is keystroke-driven
    /// UI state, and the shell owns keystrokes.
    private(set) var selection = 0

    /// Called when a row is clicked. A list you can scroll with a trackpad has to let you
    /// act on what you scrolled to; clicking launches, as it does in Spotlight.
    var onActivate: (() -> Void)?

    init(visibleRows: Int) {
        self.visibleRows = max(visibleRows, 1)
        super.init(frame: .zero)

        let column = NSTableColumn(identifier: Self.rowID)
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.style = .plain
        table.rowHeight = Self.rowHeight
        table.intercellSpacing = NSSize(width: 0, height: Self.spacing)
        table.backgroundColor = .clear
        table.columnAutoresizingStyle = .firstColumnOnlyAutoresizingStyle
        // Selection is drawn by the row itself, in the same style as before the table.
        table.selectionHighlightStyle = .none
        // The search field keeps focus throughout. A table that took it on click would
        // silently stop typing from reaching the query.
        table.refusesFirstResponder = true
        table.dataSource = self
        table.delegate = self
        // The entrance animates the table's own layer, so it needs one.
        table.wantsLayer = true
        table.target = self
        table.action = #selector(clicked)

        highlight.isHidden = true
        // Behind the rows, and inside the table so it scrolls with them.
        table.addSubview(highlight, positioned: .below, relativeTo: nil)

        documentView = table
        drawsBackground = false
        borderType = .noBorder
        hasVerticalScroller = true
        autohidesScrollers = true
        scrollerStyle = .overlay
        automaticallyAdjustsContentInsets = false
        contentInsets = Self.padding
        translatesAutoresizingMaskIntoConstraints = false
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    /// Changes how many rows show before the list scrolls. The caller re-lays out.
    func show(atMost rows: Int) {
        visibleRows = max(rows, 1)
    }

    var selectedMatch: Match? {
        isSelectable(selection) ? matches[selection] : nil
    }

    private func isSelectable(_ index: Int) -> Bool {
        matches.indices.contains(index) && matches[index].kind != .header
    }

    /// Replaces the list. `entering` asks for the rows to fade and rise into place.
    ///
    /// Only where rows genuinely arrive — the panel opening, a change of mode, the agent's
    /// steps landing as the model writes them. Never on ordinary typing: the keystroke path is
    /// measured in single-figure milliseconds, and animating fifty rows per keystroke is the
    /// cost the row pool exists to avoid.
    func update(_ matches: [Match], entering: Bool = false) {
        self.matches = matches
        // A new query is a new list; keeping the old index would land Enter on whatever
        // happened to slide into that position. The first real row, past any header.
        selection = matches.firstIndex { $0.kind != .header } ?? 0
        table.reloadData()
        // And it starts at the top: staying scrolled down would hide the best match. Skipped
        // when everything fits, because scrolling a list that cannot scroll still flashes the
        // overlay scroller.
        if contentView.bounds.origin.y != -Self.padding.top {
            contentView.scroll(to: NSPoint(x: 0, y: -Self.padding.top))
            reflectScrolledClipView(contentView)
        }
        // Laid out before the wash is placed: `rect(ofRow:)` answers with the old geometry
        // until the table has re-tiled.
        table.layoutSubtreeIfNeeded()
        moveHighlight(animated: false)
        if entering {
            playEntrance()
        }
    }

    /// Moves the highlight by `offset`, clamped rather than wrapping — from the bottom of
    /// a fifty-row list, jumping back to the top reads as the list having reset.
    func moveSelection(by offset: Int) {
        // Headers are stepped over, and a header at either end stops the move rather than
        // taking the highlight.
        var target = selection + offset
        while matches.indices.contains(target), !isSelectable(target) {
            target += offset.signum()
        }
        guard isSelectable(target), target != selection else { return }
        let previous = selection
        selection = target
        highlight(from: previous)
        // What makes the arrow keys scroll: moving past the last visible row brings the
        // next one into view, one row at a time. Moving up onto a section's first row
        // brings its title along, so the highlight never sits under a hidden header.
        let above = selection - 1
        if offset < 0, above >= 0, !isSelectable(above) {
            table.scrollRowToVisible(above)
        }
        table.scrollRowToVisible(selection)
    }

    /// The height of the rows showing, capped at `visibleRows` rows' worth — past that the
    /// list scrolls instead of the panel growing down the screen. Summed rather than
    /// multiplied, since a header is shorter than a row.
    var fittingHeight: CGFloat {
        guard !matches.isEmpty else { return 0 }
        let content =
            matches.enumerated().reduce(0) { total, row in
                total + tableView(table, heightOfRow: row.offset)
            } + CGFloat(matches.count - 1) * Self.spacing
        let budget =
            CGFloat(visibleRows) * Self.rowHeight + CGFloat(visibleRows - 1) * Self.spacing
        return min(content, budget) + Self.padding.top + Self.padding.bottom
    }

    /// Fades the list in and lifts it the last couple of points into place.
    ///
    /// One animation on the table's layer rather than one per row: fifty row animations cost
    /// fifty times as much and look no different at this speed.
    private func playEntrance() {
        guard let layer = table.layer else { return }
        layer.removeAnimation(forKey: "entrance")
        let fade = CABasicAnimation(keyPath: "opacity")
        fade.fromValue = 0
        fade.toValue = 1
        let rise = CABasicAnimation(keyPath: "transform.translation.y")
        // Up in a flipped view is negative, and the table is flipped.
        rise.fromValue = 2
        rise.toValue = 0
        let group = CAAnimationGroup()
        group.animations = [fade, rise]
        group.duration = Self.entrance
        group.timingFunction = CAMediaTimingFunction(name: .easeOut)
        layer.add(group, forKey: "entrance")
    }

    private func highlightFrame(for row: Int) -> NSRect {
        table.rect(ofRow: row)
    }

    /// Puts the wash on the selected row. `animated` only when the move came from the arrow
    /// keys or a click: a new list has nothing to glide from, and animating there would drag
    /// the wash across a list that is no longer the same list.
    private func moveHighlight(animated: Bool) {
        guard isSelectable(selection) else {
            highlight.isHidden = true
            return
        }
        let frame = highlightFrame(for: selection)
        // Nothing to glide from when it was hidden, or when the list just changed.
        let appearing = highlight.isHidden
        highlight.isHidden = false
        guard animated, !appearing else {
            highlight.frame = frame
            return
        }
        NSAnimationContext.runAnimationGroup { context in
            context.duration = Self.glide
            context.timingFunction = CAMediaTimingFunction(name: .easeOut)
            context.allowsImplicitAnimation = true
            highlight.animator().frame = frame
        }
    }

    private static func height(of match: Match) -> CGFloat {
        match.kind == .header ? headerHeight : rowHeight
    }

    /// The live row view, if it is on screen. Off-screen rows get their selection state
    /// when the table next asks for them, in `tableView(_:viewFor:row:)`.
    private func row(at index: Int) -> NSView? {
        guard matches.indices.contains(index) else { return nil }
        return table.view(atColumn: 0, row: index, makeIfNecessary: false)
    }

    @objc private func clicked() {
        let clicked = table.clickedRow
        guard isSelectable(clicked) else { return }
        let previous = selection
        selection = clicked
        highlight(from: previous)
        onActivate?()
    }

    /// Moves the selection's type — the brighter title, the keys at the right — from
    /// `previous` to `selection`, on whichever of the two rows are live; one scrolled off
    /// picks its state up in `viewFor`.
    private func highlight(from previous: Int) {
        (row(at: previous) as? ResultRow)?.select(false)
        (row(at: selection) as? ResultRow)?.select(true)
        moveHighlight(animated: true)
    }

    func numberOfRows(in tableView: NSTableView) -> Int {
        matches.count
    }

    func tableView(_ tableView: NSTableView, didRemove rowView: NSTableRowView, forRow row: Int) {
        // Bounded, though it never gets near it: only the rows on screen at once are ever
        // out of the pool.
        switch rowView.view(atColumn: 0) {
        case let cell as ResultRow where pool.count < 32: pool.append(cell)
        case let header as HeaderRow where headerPool.count < 4: headerPool.append(header)
        case let answer as AnswerRow where answerPool.count < 4: answerPool.append(answer)
        default: break
        }
    }

    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
        guard matches.indices.contains(row) else { return Self.rowHeight }
        if matches[row].kind == .agentAnswer {
            return AnswerRow.height(for: matches[row].name, width: contentView.bounds.width)
        }
        return Self.height(of: matches[row])
    }

    func tableView(
        _ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int
    ) -> NSView? {
        if matches.indices.contains(row), matches[row].kind == .agentAnswer {
            let answer = answerPool.popLast() ?? AnswerRow(frame: .zero)
            answer.show(matches[row].name)
            return answer
        }
        if matches.indices.contains(row), matches[row].kind == .header {
            let header = headerPool.popLast() ?? HeaderRow(frame: .zero)
            header.show(matches[row].name, detail: matches[row].subtitle)
            return header
        }
        // Reused as rows leave the table, so only the visible handful ever exist.
        let view = pool.popLast() ?? ResultRow(frame: .zero)
        if matches.indices.contains(row) {
            // The hairline belongs to the lower of two rows, and a section title already
            // draws one — so the first row under a header goes without.
            let separated = row > 0 && matches[row - 1].kind != .header
            view.show(matches[row], selected: row == selection, separated: separated)
        }
        return view
    }
}
