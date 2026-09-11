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

    /// The calculator row's icon. A symbol, not a file icon, because a calculated result
    /// has no path to look one up from.
    static let calculator: NSImage? = {
        let image = NSImage(
            systemSymbolName: "equal.square", accessibilityDescription: "Calculated result")
        image?.size = NSSize(width: ResultsView.iconSize, height: ResultsView.iconSize)
        return image
    }()

    /// Where clip thumbnails come from. Set once at launch; left `nil` by the latency
    /// harness, which never shows a clip.
    static var clipThumbnails: ((UInt64) -> Data?)?
    private static var clipCache: [UInt64: NSImage] = [:]

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
    static func thumbnail(forClip id: UInt64) -> NSImage? {
        if let hit = clipCache[id] { return hit }
        guard let data = clipThumbnails?(id), let image = NSImage(data: data) else {
            return textClip
        }
        // Bounded, because clips come and go while this process lives for weeks and the
        // cache would otherwise hold every thumbnail ever shown. History itself caps at
        // 200 clips, so dropping everything past that costs a re-decode at worst.
        if clipCache.count >= 256 { clipCache.removeAll(keepingCapacity: true) }
        let flattened = flatten(image)
        clipCache[id] = flattened
        return flattened
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
        // Low horizontal compression resistance on both labels, so long text truncates
        // instead of widening the window. A label's intrinsic width is its whole string,
        // at the default priority of 750 — and the panel's width is not a required
        // constraint — so a long clip made Auto Layout grow the panel to 1812pt, off the
        // right edge of the screen. Truncation needs something willing to give way.
        title.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        // Semantic colours, not literal black/grey, so the labels pick up vibrancy
        // against the glass instead of sitting flat on top of it.
        title.textColor = .labelColor
        subtitle.font = .systemFont(ofSize: 12)
        subtitle.lineBreakMode = .byTruncatingMiddle
        subtitle.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
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
        switch match.kind {
        case .clipImage:
            // Titled by the text found in it. With none, the label takes the title, so the
            // row never shows a placeholder or a pair of pixel dimensions.
            let label = ImageLabel.of(width: match.width, height: match.height)
            let time = ClipTime.describe(match.timestamp)
            if match.name == ImageLabel.placeholder {
                title.stringValue = label
                subtitle.stringValue = time
            } else {
                title.stringValue = match.name
                subtitle.stringValue = "\(label) · \(time)"
            }
        case .clipText:
            title.stringValue = match.name
            subtitle.stringValue = ClipTime.describe(match.timestamp)
        case .app, .file, .calc:
            title.stringValue = match.name
            subtitle.stringValue = match.subtitle
        }
        iconView.image =
            switch match.kind {
            case .calc: IconCache.calculator
            case .clipText: IconCache.textClip
            case .clipImage: IconCache.thumbnail(forClip: match.id)
            case .app, .file: IconCache.icon(for: match.path)
            }
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
    static let rowHeight: CGFloat = 54
    static let iconSize: CGFloat = 36
    private static let spacing: CGFloat = 1
    private static let padding = NSEdgeInsets(top: 8, left: 0, bottom: 10, right: 0)
    private static let rowID = NSUserInterfaceItemIdentifier("result")

    private let table = NSTableView()

    /// Row views the table has let go of, handed straight back out.
    ///
    /// Kept by hand because `makeView(withIdentifier:)` was not recycling: measured, 400
    /// updates vended 2,950 distinct row views — a fresh icon, two labels, a stack view and
    /// their constraints for every visible row on every re-tile. That construction was the
    /// entire cost of a keystroke that changed the list's height: 1.85ms median, 30ms peak.
    private var pool: [ResultRow] = []

    /// Rows shown before the list scrolls — `max_results` in config.toml.
    private let visibleRows: Int

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
        table.target = self
        table.action = #selector(clicked)

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

    var selectedMatch: Match? {
        matches.indices.contains(selection) ? matches[selection] : nil
    }

    func update(_ matches: [Match]) {
        self.matches = matches
        // A new query is a new list; keeping the old index would land Enter on whatever
        // happened to slide into that position.
        selection = 0
        table.reloadData()
        // And it starts at the top: staying scrolled down would hide the best match.
        contentView.scroll(to: NSPoint(x: 0, y: -Self.padding.top))
        reflectScrolledClipView(contentView)
    }

    /// Moves the highlight by `offset`, clamped rather than wrapping — from the bottom of
    /// a fifty-row list, jumping back to the top reads as the list having reset.
    func moveSelection(by offset: Int) {
        guard !matches.isEmpty else { return }
        let previous = selection
        selection = min(max(selection + offset, 0), matches.count - 1)
        guard selection != previous else { return }
        row(at: previous)?.isSelected = false
        row(at: selection)?.isSelected = true
        // What makes the arrow keys scroll: moving past the last visible row brings the
        // next one into view, one row at a time.
        table.scrollRowToVisible(selection)
    }

    /// The height for the rows showing, capped at `visibleRows` — past that the list
    /// scrolls instead of the panel growing down the screen.
    var fittingHeight: CGFloat {
        let shown = CGFloat(min(matches.count, visibleRows))
        guard shown > 0 else { return 0 }
        return shown * Self.rowHeight + (shown - 1) * Self.spacing + Self.padding.top
            + Self.padding.bottom
    }

    /// The live row view, if it is on screen. Off-screen rows get their selection state
    /// when the table next asks for them, in `tableView(_:viewFor:row:)`.
    private func row(at index: Int) -> ResultRow? {
        guard matches.indices.contains(index) else { return nil }
        return table.view(atColumn: 0, row: index, makeIfNecessary: false) as? ResultRow
    }

    @objc private func clicked() {
        let clicked = table.clickedRow
        guard matches.indices.contains(clicked) else { return }
        row(at: selection)?.isSelected = false
        selection = clicked
        row(at: selection)?.isSelected = true
        onActivate?()
    }

    func numberOfRows(in tableView: NSTableView) -> Int {
        matches.count
    }

    func tableView(_ tableView: NSTableView, didRemove rowView: NSTableRowView, forRow row: Int) {
        // Bounded, though it never gets near it: only the rows on screen at once are ever
        // out of the pool.
        if let cell = rowView.view(atColumn: 0) as? ResultRow, pool.count < 32 {
            pool.append(cell)
        }
    }

    func tableView(
        _ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int
    ) -> NSView? {
        // Reused as rows leave the table, so only the visible handful ever exist.
        let view = pool.popLast() ?? ResultRow(frame: .zero)
        if matches.indices.contains(row) {
            view.show(matches[row])
        }
        view.isSelected = row == selection
        return view
    }
}
