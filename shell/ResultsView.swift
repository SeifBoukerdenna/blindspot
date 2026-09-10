import AppKit

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
    private static var cache: [String: NSImage] = [:]

    static func icon(for path: String) -> NSImage {
        if let hit = cache[path] { return hit }
        let flattened = flatten(NSWorkspace.shared.icon(forFile: path))
        cache[path] = flattened
        return flattened
    }

    private static var pending: [String] = []
    private static var draining = false

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
        guard !draining, !pending.isEmpty else { return }
        draining = true
        drain()
    }

    private static func drain() {
        // Anything warmed by a real query since being queued is already done.
        while let next = pending.first, cache[next] != nil {
            pending.removeFirst()
        }
        guard !pending.isEmpty else {
            draining = false
            return
        }
        let path = pending.removeFirst()
        cache[path] = flatten(NSWorkspace.shared.icon(forFile: path))
        Task { @MainActor in drain() }
    }

    /// Warms everything synchronously.
    ///
    /// Only for the latency harness, which needs a known-warm cache before it can measure
    /// steady state. The app deliberately uses the incremental path above instead.
    static func warmNow(_ paths: [String]) {
        for path in paths where cache[path] == nil {
            cache[path] = flatten(NSWorkspace.shared.icon(forFile: path))
        }
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

/// One fixed-height row: icon, name, and the folder it lives in.
@MainActor
private final class ResultRow: NSView {
    private let iconView = NSImageView()
    private let title = NSTextField(labelWithString: "")
    private let subtitle = NSTextField(labelWithString: "")

    var isSelected = false {
        didSet {
            guard isSelected != oldValue else { return }
            // No emphasized/unemphasized split. The usual reason to draw a grey
            // selection is a window that is visible but not key — which this panel is
            // never in, because `windowDidResignKey` dismisses it. Handling that state
            // would be unreachable code.
            layer?.backgroundColor =
                isSelected ? NSColor.selectedContentBackgroundColor.cgColor : nil
            title.textColor = isSelected ? .alternateSelectedControlTextColor : .labelColor
            subtitle.textColor =
                isSelected
                ? NSColor.alternateSelectedControlTextColor.withAlphaComponent(0.7)
                : .secondaryLabelColor
        }
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = 10

        title.font = .systemFont(ofSize: 15)
        title.lineBreakMode = .byTruncatingTail
        // Semantic colours, not literal black/grey, so the labels pick up vibrancy
        // against the glass instead of sitting flat on top of it.
        title.textColor = .labelColor
        subtitle.font = .systemFont(ofSize: 12)
        subtitle.lineBreakMode = .byTruncatingMiddle
        subtitle.textColor = .secondaryLabelColor
        iconView.imageScaling = .scaleProportionallyUpOrDown

        let text = NSStackView(views: [title, subtitle])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 1

        for view in [iconView, text] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }

        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: ResultsView.rowHeight),
            iconView.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            iconView.centerYAnchor.constraint(equalTo: centerYAnchor),
            iconView.widthAnchor.constraint(equalToConstant: ResultsView.iconSize),
            iconView.heightAnchor.constraint(equalToConstant: ResultsView.iconSize),
            text.leadingAnchor.constraint(equalTo: iconView.trailingAnchor, constant: 12),
            text.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            text.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    func show(_ match: Match) {
        title.stringValue = match.name
        subtitle.stringValue = match.subtitle
        iconView.image = IconCache.icon(for: match.path)
    }
}

/// The results list: a stack of fixed-height rows.
///
/// Hand-drawn rather than an `NSTableView`, per the decision in CLAUDE.md's open
/// questions. A table earns its complexity through reuse across thousands of rows;
/// here there are at most `max_results` of them, all the same height, and the row views
/// are allocated once at launch and refilled in place — so a table's cell-reuse
/// machinery would be pure overhead on the keystroke path.
@MainActor
final class ResultsView: NSStackView {
    static let rowHeight: CGFloat = 54
    static let iconSize: CGFloat = 36

    private var rows: [ResultRow] = []
    private(set) var matches: [Match] = []

    /// Which row Enter will launch. Lives here rather than in Rust: it is keystroke-
    /// driven UI state, and the shell owns keystrokes.
    private(set) var selection = 0

    init(capacity: Int) {
        super.init(frame: .zero)
        orientation = .vertical
        alignment = .leading
        distribution = .fill
        spacing = 1
        edgeInsets = NSEdgeInsets(top: 8, left: 8, bottom: 10, right: 8)
        translatesAutoresizingMaskIntoConstraints = false

        // Built once, up front. Allocating views on the keystroke path is exactly the
        // kind of thing that turns an instant launcher into a sluggish one.
        for _ in 0..<max(capacity, 1) {
            let row = ResultRow(frame: .zero)
            row.isHidden = true
            rows.append(row)
            addArrangedSubview(row)
            row.widthAnchor.constraint(equalTo: widthAnchor, constant: -16).isActive = true
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    var selectedMatch: Match? {
        matches.indices.contains(selection) ? matches[selection] : nil
    }

    func update(_ matches: [Match]) {
        self.matches = Array(matches.prefix(rows.count))
        // Collapse the padding when there is nothing to pad, so an empty result list
        // takes zero height instead of the insets' worth and `fittingHeight` agrees
        // with what the stack actually occupies.
        edgeInsets =
            self.matches.isEmpty
            ? NSEdgeInsets()
            : NSEdgeInsets(top: 8, left: 8, bottom: 10, right: 8)
        // A new query is a new list; keeping the old index would land Enter on whatever
        // happened to slide into that position.
        selection = 0
        for (index, row) in rows.enumerated() {
            if index < self.matches.count {
                row.show(self.matches[index])
                row.isHidden = false
            } else {
                row.isHidden = true
            }
        }
        applySelection()
    }

    /// Moves the highlight by `offset`, clamped. Deliberately not wrapping: at eight
    /// rows, wrapping from the bottom to the top reads as the list having jumped.
    func moveSelection(by offset: Int) {
        guard !matches.isEmpty else { return }
        selection = min(max(selection + offset, 0), matches.count - 1)
        applySelection()
    }

    private func applySelection() {
        for (index, row) in rows.enumerated() {
            row.isSelected = index == selection && index < matches.count
        }
    }

    /// The height this view wants for the number of rows currently showing, so the
    /// panel can shrink to fit instead of leaving empty space under short result lists.
    var fittingHeight: CGFloat {
        let visible = CGFloat(matches.count)
        guard visible > 0 else { return 0 }
        return visible * Self.rowHeight + (visible - 1) * spacing + edgeInsets.top
            + edgeInsets.bottom
    }
}
