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

    /// Every tool row's icon — one symbol for all of them, since the subtitle already says
    /// which form each row is.
    static let tool: NSImage? = {
        NSImage(
            systemSymbolName: "wrench.and.screwdriver",
            accessibilityDescription: "Converted value")
    }()

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
            let quit = isRunning(match.path) ? " · ⌃↩ Quit" : ""
            return "⌘↩ Reveal in Finder · ⌥↩ Copy path" + quit
        case .file:
            return "⌘↩ Reveal in Finder · ⌥↩ Copy path"
        case .calc, .tool, .clipText, .clipImage, .header:
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

/// One fixed-height row: icon, name, and the folder it lives in.
@MainActor
private final class ResultRow: NSView {
    private let iconView = NSImageView()
    private let title = NSTextField(labelWithString: "")
    private let subtitle = NSTextField(labelWithString: "")
    private var plainSubtitle = ""

    /// Stands in for the second line while set — the selected row's action keys.
    var hint: String? {
        didSet { subtitle.stringValue = hint ?? plainSubtitle }
    }

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
                plainSubtitle = time
            } else {
                title.stringValue = match.name
                plainSubtitle = "\(label) · \(time)"
            }
        case .clipText:
            title.stringValue = match.name
            plainSubtitle = ClipTime.describe(match.timestamp)
        case .tool where match.timestamp > 0:
            title.stringValue = match.name
            plainSubtitle = [LocalTime.describe(match.timestamp), match.detail]
                .filter { !$0.isEmpty }.joined(separator: " · ")
        case .app, .file, .calc, .tool, .header:
            title.stringValue = match.name
            plainSubtitle = match.subtitle
        }
        hint = nil
        iconView.image =
            switch match.kind {
            case .calc: IconCache.calculator
            case .tool: IconCache.tool
            case .clipText: IconCache.textClip
            case .clipImage: IconCache.thumbnail(forClip: match.id)
            case .app, .file: IconCache.icon(for: match.path)
            case .header: nil
            }
    }
}

/// A welcome-screen section title: small, bold and secondary, in title case — the sidebar
/// section style macOS has used since Big Sur — so it reads as a label rather than a result.
@MainActor
private final class HeaderRow: NSView {
    private let label = NSTextField(labelWithString: "")

    override init(frame: NSRect) {
        super.init(frame: frame)
        label.font = .systemFont(ofSize: 11, weight: .semibold)
        label.textColor = .secondaryLabelColor
        label.translatesAutoresizingMaskIntoConstraints = false
        addSubview(label)
        NSLayoutConstraint.activate([
            // Level with the row icons below it.
            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            label.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -12),
            label.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -3),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    func show(_ title: String) {
        label.stringValue = title
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
    /// Half a row less half the spacing, so two headers stand in exactly one row's height.
    /// The core relies on that to fit a welcome screen into `max_results` rows unscrolled.
    static let headerHeight: CGFloat = 26
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
    private var headerPool: [HeaderRow] = []

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
        isSelectable(selection) ? matches[selection] : nil
    }

    private func isSelectable(_ index: Int) -> Bool {
        matches.indices.contains(index) && matches[index].kind != .header
    }

    func update(_ matches: [Match]) {
        self.matches = matches
        // A new query is a new list; keeping the old index would land Enter on whatever
        // happened to slide into that position. The first real row, past any header.
        selection = matches.firstIndex { $0.kind != .header } ?? 0
        table.reloadData()
        // And it starts at the top: staying scrolled down would hide the best match.
        contentView.scroll(to: NSPoint(x: 0, y: -Self.padding.top))
        reflectScrolledClipView(contentView)
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
            matches.reduce(0) { $0 + Self.height(of: $1) }
            + CGFloat(matches.count - 1) * Self.spacing
        let budget =
            CGFloat(visibleRows) * Self.rowHeight + CGFloat(visibleRows - 1) * Self.spacing
        return min(content, budget) + Self.padding.top + Self.padding.bottom
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

    /// Moves the highlight and the action hint from `previous` to `selection`, on whichever
    /// of the two rows are live; one scrolled off picks its state up in `viewFor`.
    private func highlight(from previous: Int) {
        if let old = row(at: previous) as? ResultRow {
            old.isSelected = false
            old.hint = nil
        }
        if let new = row(at: selection) as? ResultRow {
            new.isSelected = true
            new.hint = ActionHint.text(for: matches[selection])
        }
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
        default: break
        }
    }

    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
        matches.indices.contains(row) ? Self.height(of: matches[row]) : Self.rowHeight
    }

    func tableView(
        _ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int
    ) -> NSView? {
        if matches.indices.contains(row), matches[row].kind == .header {
            let header = headerPool.popLast() ?? HeaderRow(frame: .zero)
            header.show(matches[row].name)
            return header
        }
        // Reused as rows leave the table, so only the visible handful ever exist.
        let view = pool.popLast() ?? ResultRow(frame: .zero)
        if matches.indices.contains(row) {
            view.show(matches[row])
            if row == selection {
                view.hint = ActionHint.text(for: matches[row])
            }
        }
        view.isSelected = row == selection
        return view
    }
}
