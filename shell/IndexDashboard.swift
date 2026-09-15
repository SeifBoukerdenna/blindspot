import AppKit

/// The Index page: what the content index holds, what it is doing and what it costs, drawn
/// from one structured snapshot per refresh. Numbers, sizes and times are formatted here; what
/// each count means is decided in the core, so the page cannot drift from the indexer.
///
/// Sections backed by slowly changing data (inventory, busy folders) are rebuilt only when that
/// data changes, so a click on a row or button is not lost to the once-a-second refresh.
@MainActor
final class IndexDashboardView: NSView {
    var onRefresh: (() -> Void)?
    var onExclude: ((String) -> Void)?
    var onCompact: (() -> Void)?

    private let column = NSStackView()
    private let dot = PlateView(fill: Theme.faint, radius: 4)
    private let title = IndexUI.label("Index", IndexUI.font(20, .semibold), Theme.ink)
    private let subtitle = IndexUI.label("", IndexUI.font(12), Theme.muted, breaking: .byTruncatingMiddle)
    private let meter = MeterView(height: 5)
    private let meterCaption = IndexUI.label("", IndexUI.font(11), Theme.muted)
    private let tiles = (0..<4).map { _ in Tile() }
    private let busy = NSStackView()
    private let types = Card()
    private let folders = Card()
    private let attention = Card()
    private let recent = Card()
    private let activity = Card()
    private let resources = Card()
    private var last: IndexOverview?

    init() {
        super.init(frame: .zero)
        translatesAutoresizingMaskIntoConstraints = false
        column.orientation = .vertical
        column.alignment = .leading
        column.spacing = 0
        column.translatesAutoresizingMaskIntoConstraints = false
        addSubview(column)
        NSLayoutConstraint.activate([
            column.topAnchor.constraint(equalTo: topAnchor, constant: 18),
            column.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Theme.gutter),
            column.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -Theme.gutter),
            column.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -24),
        ])
        busy.orientation = .vertical
        busy.alignment = .leading
        busy.spacing = 8
        busy.isHidden = true

        let pair = NSStackView(views: [types, folders])
        pair.orientation = .horizontal
        pair.distribution = .fillEqually
        pair.alignment = .top
        pair.spacing = 12

        add(header(), after: 0)
        add(meter, after: 14)
        add(meterCaption, after: 6)
        add(tileRow(), after: 16)
        add(busy, after: 14)
        add(IndexUI.caption("What's indexed"), after: 24)
        add(pair, after: 8)
        add(IndexUI.caption("Needs attention"), after: 22)
        add(attention, after: 8)
        add(IndexUI.caption("Recently changed"), after: 22)
        add(recent, after: 8)
        add(IndexUI.caption("Activity"), after: 22)
        add(activity, after: 8)
        add(IndexUI.caption("Resources"), after: 22)
        add(resources, after: 8)
        update(nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    private func add(_ view: NSView, after spacing: CGFloat) {
        if let previous = column.arrangedSubviews.last { column.setCustomSpacing(spacing, after: previous) }
        column.addArrangedSubview(view)
        view.widthAnchor.constraint(equalTo: column.widthAnchor).isActive = true
    }

    private func header() -> NSView {
        dot.widthAnchor.constraint(equalToConstant: 9).isActive = true
        dot.heightAnchor.constraint(equalToConstant: 9).isActive = true
        let heading = NSStackView(views: [dot, title])
        heading.orientation = .horizontal
        heading.alignment = .centerY
        heading.spacing = 9
        let text = NSStackView(views: [heading, subtitle])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 3
        subtitle.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        text.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let refresh = ActionButton("Refresh", colour: Theme.accent) { [weak self] in self?.onRefresh?() }
        let compact = ActionButton("Compact", colour: Theme.muted) { [weak self] in self?.onCompact?() }
        let row = NSStackView(views: [text, IndexUI.spacer(), compact, refresh])
        row.orientation = .horizontal
        row.alignment = .centerY
        row.spacing = 12
        return row
    }

    private func tileRow() -> NSView {
        let row = NSStackView(views: tiles.map(\.view))
        row.orientation = .horizontal
        row.distribution = .fillEqually
        row.spacing = 10
        return row
    }

    func update(_ overview: IndexOverview?) {
        guard let overview else {
            title.stringValue = "Index status unavailable"
            dot.fill(Theme.faint)
            subtitle.stringValue = "The content index has not reported yet."
            return
        }
        guard overview != last else { return }
        let previous = last
        last = overview
        updateHeader(overview)
        updateTiles(overview)
        if previous?.busy != overview.busy { rebuildBusy(overview.busy) }
        if previous?.kinds != overview.kinds { types.set(typeRows(overview)) }
        if previous?.folders != overview.folders { folders.set(folderRows(overview)) }
        if previous?.attention != overview.attention || previous?.pdfIssues != overview.pdfIssues || previous?.sampled != overview.sampled {
            attention.set(attentionRows(overview))
        }
        if previous?.recent != overview.recent { recent.set(recentRows(overview)) }
        activity.set(activityRows(overview))
        resources.set(resourceRows(overview))
    }

    private func updateHeader(_ overview: IndexOverview) {
        let (text, colour): (String, NSColor) = switch overview.state {
        case "indexing": ("Indexing", Theme.accent)
        case "ready": ("Up to date", Theme.ok)
        case "partial": ("Partly indexed", Theme.warn)
        case "paused": ("Paused", Theme.warn)
        case "needsRoots": ("Choose folders to index", Theme.warn)
        case "off": ("Indexing is off", Theme.faint)
        case "erasing": ("Erasing index", Theme.warn)
        case "compacting": ("Compacting index", Theme.accent)
        case "erased": ("Index erased", Theme.faint)
        default: ("Index unavailable", Theme.danger)
        }
        title.stringValue = text
        dot.fill(colour)
        var parts: [String] = []
        switch overview.state {
        case "indexing":
            parts.append(overview.stage.isEmpty ? "Working" : overview.stage)
            if let seconds = overview.stageSeconds { parts.append(IndexUI.duration(Double(seconds))) }
            if let folder = overview.currentFolder { parts.append(folder) }
        case "compacting":
            parts.append(overview.stage.isEmpty ? "Starting" : overview.stage)
        case "ready", "partial":
            if let ago = overview.lastPassAgo {
                parts.append("Last pass \(IndexUI.ago(ago))")
                if let took = overview.lastPassSeconds { parts.append("took \(IndexUI.duration(took))") }
            }
        default:
            parts.append(overview.message)
        }
        if overview.state != "indexing", !overview.roots.isEmpty { parts.append("Watching \(overview.roots.joined(separator: ", "))") }
        subtitle.stringValue = parts.joined(separator: "  ·  ")

        let semantic = overview.semantic
        if semantic.enabled, let eligible = semantic.eligible, let embedded = semantic.embedded, eligible > 0 {
            meter.fraction = Double(embedded) / Double(eligible)
            meter.tint(embedded >= eligible ? Theme.ok : Theme.accent)
            let waiting = eligible > embedded ? "  ·  \(IndexUI.number(eligible - embedded)) waiting" : ""
            meterCaption.stringValue = "\(IndexUI.number(embedded)) of \(IndexUI.number(eligible)) notes and PDFs searchable by meaning\(waiting)"
        } else {
            meter.fraction = 0
            meterCaption.stringValue = semantic.enabled
                ? "Search by meaning: counting eligible notes and PDFs…"
                : "Search by meaning is off — exact-word search only"
        }
    }

    private func updateTiles(_ overview: IndexOverview) {
        let issues = overview.pdfIssues?.reduce(0, +) ?? 0
        tiles[0].set("Documents", IndexUI.number(overview.documents),
                     "\(overview.kinds.count) kinds · \(overview.folders.count) top folders")
        tiles[1].set("Docs with text", IndexUI.number(overview.pdfText),
                     issues > 0 ? "\(IndexUI.number(issues)) need attention" : "none need attention")
        tiles[2].set("By meaning", IndexUI.number(overview.semantic.enabled ? overview.semantic.embedded : nil),
                     overview.semantic.enabled ? "notes and PDFs" : "off")
        tiles[3].set("On disk", IndexUI.bytes(overview.disk.database + (overview.disk.cache ?? 0)),
                     (overview.reclaimable ?? 0) > 16 << 20
                         ? "\(IndexUI.bytes(overview.reclaimable ?? 0)) reclaimable"
                         : "cache \(IndexUI.bytes(overview.disk.cache ?? 0))")
    }

    private func rebuildBusy(_ items: [IndexOverview.Busy]) {
        for view in busy.arrangedSubviews { view.removeFromSuperview() }
        for item in items {
            let plate = PlateView(fill: Theme.warn.withAlphaComponent(0.10), radius: 10, border: Theme.warn.withAlphaComponent(0.35))
            let icon = IndexUI.symbol("bolt.fill", Theme.warn)
            let text = IndexUI.wrapping(
                "\(item.folder) changed \(IndexUI.number(item.changes)) times in the last 5 minutes. Every change starts indexing; exclude it if it holds generated data.",
                IndexUI.font(12), Theme.ink, width: 400)
            let path = item.path
            let button = ActionButton("Exclude folder", colour: Theme.warn) { [weak self] in self?.onExclude?(path) }
            let row = NSStackView(views: [icon, text, IndexUI.spacer(), button])
            row.orientation = .horizontal
            row.alignment = .centerY
            row.spacing = 10
            row.translatesAutoresizingMaskIntoConstraints = false
            plate.addSubview(row)
            NSLayoutConstraint.activate([
                row.topAnchor.constraint(equalTo: plate.topAnchor, constant: 10),
                row.bottomAnchor.constraint(equalTo: plate.bottomAnchor, constant: -10),
                row.leadingAnchor.constraint(equalTo: plate.leadingAnchor, constant: 12),
                row.trailingAnchor.constraint(equalTo: plate.trailingAnchor, constant: -12),
            ])
            busy.addArrangedSubview(plate)
            plate.widthAnchor.constraint(equalTo: busy.widthAnchor).isActive = true
        }
        busy.isHidden = items.isEmpty
    }

    private func typeRows(_ overview: IndexOverview) -> [NSView] {
        guard !overview.kinds.isEmpty else { return [IndexUI.note(overview.sampled ? "Nothing indexed yet" : "Counting…")] }
        let largest = Double(overview.kinds.map(\.count).max() ?? 1)
        return overview.kinds.map { kind in
            IndexUI.row(symbol: IndexUI.symbolName(forKind: kind.label), tint: Theme.muted, title: kind.label,
                        detail: IndexUI.bytes(kind.bytes), trailing: IndexUI.number(kind.count),
                        fraction: Double(kind.count) / largest)
        }
    }

    private func folderRows(_ overview: IndexOverview) -> [NSView] {
        guard !overview.folders.isEmpty else { return [IndexUI.note(overview.sampled ? "No folders indexed yet" : "Counting…")] }
        let largest = Double(overview.folders.map(\.count).max() ?? 1)
        return overview.folders.map { folder in
            let path = folder.path
            return IndexUI.row(symbol: "folder", tint: Theme.muted, title: folder.folder,
                               detail: IndexUI.bytes(folder.bytes), trailing: IndexUI.number(folder.count),
                               fraction: Double(folder.count) / largest, breaking: .byTruncatingHead) {
                IndexUI.reveal(path)
            }
        }
    }

    private func attentionRows(_ overview: IndexOverview) -> [NSView] {
        guard overview.sampled else { return [IndexUI.note("Counting…")] }
        guard !overview.attention.isEmpty else {
            let text = overview.pacing.documents ? "Every indexed PDF and Word document produced text" : "PDF and Word extraction is off"
            return [IndexUI.row(symbol: "checkmark.circle.fill", tint: Theme.ok, title: text)]
        }
        var rows = overview.attention.prefix(8).map { item in
            let path = item.path
            return IndexUI.row(symbol: "exclamationmark.triangle.fill", tint: Theme.warn, title: item.name,
                               detail: "\(item.reason) · \(item.folder)") { IndexUI.reveal(path) }
        }
        let total = Int(overview.pdfIssues?.reduce(0, +) ?? UInt64(overview.attention.count))
        if total > rows.count { rows.append(IndexUI.note("and \(IndexUI.number(UInt64(total - rows.count))) more")) }
        return rows
    }

    private func recentRows(_ overview: IndexOverview) -> [NSView] {
        guard !overview.recent.isEmpty else { return [IndexUI.note(overview.sampled ? "No documents yet" : "Counting…")] }
        let now = Int64(Date().timeIntervalSince1970)
        return overview.recent.map { item in
            let path = item.path
            return IndexUI.row(symbol: IndexUI.symbolName(forFile: item.name), tint: Theme.muted, title: item.name,
                               detail: item.folder, trailing: IndexUI.ago(UInt64(max(0, now - item.modified)))) {
                IndexUI.reveal(path)
            }
        }
    }

    private func activityRows(_ overview: IndexOverview) -> [NSView] {
        let pass = overview.pass
        var rows = [
            IndexUI.pair("This pass", [
                "\(IndexUI.number(pass.checked)) checked", "\(IndexUI.number(pass.updated)) updated",
                "\(IndexUI.number(pass.unchanged)) unchanged", "\(IndexUI.number(pass.skipped)) skipped",
                "\(IndexUI.number(pass.unreadable)) unreadable",
            ].joined(separator: " · ") + (pass.removed > 0 ? " · \(IndexUI.number(pass.removed)) removed" : "")),
        ]
        if pass.sourceBytes > 0 { rows.append(IndexUI.pair("Read this pass", IndexUI.bytes(pass.sourceBytes))) }
        if let embedded = overview.semantic.passEmbedded {
            rows.append(IndexUI.pair("Embedded this pass", "\(IndexUI.number(embedded)) documents · \(IndexUI.number(overview.semantic.passFailed ?? 0)) failed"))
        }
        rows.append(IndexUI.pair("Watching", overview.roots.isEmpty ? "No folders" : overview.roots.joined(separator: ", ")))
        if let compact = overview.compact {
            rows.append(IndexUI.pair("Last compact", "\(IndexUI.bytes(compact.before)) → \(IndexUI.bytes(compact.after)) · \(IndexUI.number(compact.removed)) unused vectors removed"))
        }
        if let error = overview.compactError { rows.append(IndexUI.pair("Compact", error)) }
        rows.append(IndexUI.pair("Counts", overview.sampleAgo.map { "Sampled \(IndexUI.ago($0))" } ?? "Sampling…"))
        return rows
    }

    private func resourceRows(_ overview: IndexOverview) -> [NSView] {
        var rows: [NSView] = overview.processes.map { process in
            let name = switch process.name {
            case "Blindspot": "Blindspot"
            case "semantic": "Semantic model"
            case "vectors": "Vector search"
            case "extract": "PDF extraction"
            default: process.name
            }
            let cpu = process.cpu.map { String(format: "%.1f%% CPU", $0) } ?? "—"
            return IndexUI.row(symbol: process.name == "Blindspot" ? "app.badge" : "cpu", tint: Theme.muted, title: name,
                               detail: IndexUI.bytes(process.memory) + " memory", trailing: cpu,
                               fraction: process.cpu.map { $0 / 100 }, meterColour: (process.cpu ?? 0) > 50 ? Theme.warn : Theme.accent)
        }
        if rows.isEmpty { rows.append(IndexUI.note("Process usage unavailable")) }
        let pacing = overview.pacing
        rows.append(IndexUI.pair("Pace", [
            pacing.lowImpact ? "Low-impact" : "Normal",
            pacing.battery ? "indexes on battery" : "pauses on battery",
            pacing.documents ? "documents up to \(pacing.documentLimitMB) MB" : "document extraction off",
        ].joined(separator: " · ")))
        rows.append(IndexUI.note("Ollama models run as a separate process and are not counted here. macOS has no per-app CPU or memory quota; Low-impact indexing spreads the work out."))
        return rows
    }
}

/// Formatting and small view builders shared by the Index page.
@MainActor
enum IndexUI {
    static func font(_ size: CGFloat, _ weight: NSFont.Weight = .regular) -> NSFont { Theme.text(size, weight) }

    static func label(_ text: String, _ font: NSFont, _ colour: NSColor, breaking: NSLineBreakMode = .byTruncatingTail) -> NSTextField {
        let field = NSTextField(labelWithString: text)
        field.font = font
        field.textColor = colour
        field.lineBreakMode = breaking
        field.maximumNumberOfLines = 1
        field.cell?.truncatesLastVisibleLine = true
        field.translatesAutoresizingMaskIntoConstraints = false
        return field
    }

    static func wrapping(_ text: String, _ font: NSFont, _ colour: NSColor, width: CGFloat) -> NSTextField {
        let field = NSTextField(wrappingLabelWithString: text)
        field.font = font
        field.textColor = colour
        field.preferredMaxLayoutWidth = width
        field.translatesAutoresizingMaskIntoConstraints = false
        field.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        return field
    }

    static func caption(_ text: String) -> NSTextField {
        let field = NSTextField(labelWithString: "")
        field.attributedStringValue = Theme.label(text.uppercased(), size: 9.5, tracking: 0.12, color: Theme.faint, weight: .medium)
        field.translatesAutoresizingMaskIntoConstraints = false
        return field
    }

    static func spacer() -> NSView {
        let view = NSView()
        view.translatesAutoresizingMaskIntoConstraints = false
        view.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        view.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        return view
    }

    static func symbol(_ name: String, _ tint: NSColor) -> NSImageView {
        let image = NSImage(systemSymbolName: name, accessibilityDescription: nil)?
            .withSymbolConfiguration(NSImage.SymbolConfiguration(pointSize: 12, weight: .regular))
        let view = NSImageView(image: image ?? NSImage())
        view.contentTintColor = tint
        view.translatesAutoresizingMaskIntoConstraints = false
        view.widthAnchor.constraint(equalToConstant: 16).isActive = true
        view.setContentHuggingPriority(.required, for: .horizontal)
        return view
    }

    static func symbolName(forKind label: String) -> String {
        switch label {
        case "Notes & text": "doc.text"
        case "PDF & Word documents": "doc.richtext"
        case "Code": "chevron.left.forwardslash.chevron.right"
        case "Data & config": "tablecells"
        case "Web pages & styles": "globe"
        default: "doc"
        }
    }

    static func symbolName(forFile name: String) -> String {
        switch (name as NSString).pathExtension.lowercased() {
        case "pdf", "docx", "doc", "rtf", "odt": "doc.richtext"
        case "md", "markdown", "txt", "rst": "doc.text"
        case "json", "csv", "tsv", "toml", "yaml", "yml", "xml": "tablecells"
        case "html", "css": "globe"
        default: "chevron.left.forwardslash.chevron.right"
        }
    }

    /// One list row: icon, title with optional detail underneath, trailing figure, and an optional
    /// proportion meter. With an action the whole row highlights and is clickable.
    static func row(symbol name: String, tint: NSColor, title: String, detail: String? = nil, trailing: String? = nil,
                    fraction: Double? = nil, meterColour: NSColor = Theme.accent, breaking: NSLineBreakMode = .byTruncatingMiddle,
                    action: (() -> Void)? = nil) -> NSView {
        let heading = label(title, font(12.5, .medium), Theme.ink, breaking: breaking)
        heading.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let text = NSStackView(views: [heading])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 1
        if let detail {
            let sub = label(detail, font(11), Theme.muted, breaking: .byTruncatingMiddle)
            sub.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
            text.addArrangedSubview(sub)
        }
        text.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        var line: [NSView] = [symbol(name, tint), text, spacer()]
        if let trailing {
            let figure = label(trailing, Theme.mono(11.5), Theme.muted)
            figure.setContentCompressionResistancePriority(.required, for: .horizontal)
            figure.setContentHuggingPriority(.required, for: .horizontal)
            line.append(figure)
        }
        let top = NSStackView(views: line)
        top.orientation = .horizontal
        top.alignment = .centerY
        top.spacing = 9
        let body = NSStackView(views: [top])
        body.orientation = .vertical
        body.alignment = .leading
        body.spacing = 6
        top.widthAnchor.constraint(equalTo: body.widthAnchor).isActive = true
        if let fraction {
            let meter = MeterView(height: 3)
            meter.fraction = fraction
            meter.tint(meterColour)
            body.addArrangedSubview(meter)
            meter.leadingAnchor.constraint(equalTo: body.leadingAnchor, constant: 25).isActive = true
            meter.trailingAnchor.constraint(equalTo: body.trailingAnchor).isActive = true
        }
        let container: NSView = action.map { LinkRow(action: $0) } ?? NSView()
        container.translatesAutoresizingMaskIntoConstraints = false
        body.translatesAutoresizingMaskIntoConstraints = false
        container.addSubview(body)
        NSLayoutConstraint.activate([
            body.topAnchor.constraint(equalTo: container.topAnchor, constant: 7),
            body.bottomAnchor.constraint(equalTo: container.bottomAnchor, constant: -7),
            body.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: 6),
            body.trailingAnchor.constraint(equalTo: container.trailingAnchor, constant: -6),
        ])
        return container
    }

    static func pair(_ key: String, _ value: String) -> NSView {
        let name = label(key, font(11), Theme.muted)
        name.widthAnchor.constraint(equalToConstant: 118).isActive = true
        let content = wrapping(value, font(12), Theme.ink, width: 430)
        let row = NSStackView(views: [name, content])
        row.orientation = .horizontal
        row.alignment = .firstBaseline
        row.spacing = 8
        row.edgeInsets = NSEdgeInsets(top: 5, left: 6, bottom: 5, right: 6)
        row.translatesAutoresizingMaskIntoConstraints = false
        return row
    }

    static func note(_ text: String) -> NSView {
        let field = wrapping(text, font(11), Theme.muted, width: 520)
        let row = NSStackView(views: [field])
        row.edgeInsets = NSEdgeInsets(top: 6, left: 6, bottom: 6, right: 6)
        row.translatesAutoresizingMaskIntoConstraints = false
        return row
    }

    private static let numbers: NumberFormatter = {
        let formatter = NumberFormatter()
        formatter.numberStyle = .decimal
        return formatter
    }()

    static func number(_ value: UInt64?) -> String {
        guard let value else { return "—" }
        return numbers.string(from: NSNumber(value: value)) ?? String(value)
    }

    static func bytes(_ value: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: value), countStyle: .file)
    }

    static func duration(_ seconds: Double) -> String {
        let whole = Int(seconds.rounded())
        if seconds < 1 { return "under 1s" }
        if whole < 60 { return "\(whole)s" }
        if whole < 3600 { return "\(whole / 60)m \(whole % 60)s" }
        if whole < 86_400 { return "\(whole / 3600)h \(whole % 3600 / 60)m" }
        return "\(whole / 86_400)d \(whole % 86_400 / 3600)h"
    }

    static func ago(_ seconds: UInt64) -> String {
        switch seconds {
        case ..<5: "just now"
        case ..<60: "\(seconds)s ago"
        case ..<3600: "\(seconds / 60)m ago"
        case ..<86_400: "\(seconds / 3600)h ago"
        default: "\(seconds / 86_400)d ago"
        }
    }

    static func reveal(_ path: String) {
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
    }
}

@MainActor
private struct Tile {
    let view = PlateView(fill: Theme.ground, radius: 10, border: Theme.hairline)
    private let caption = IndexUI.caption("")
    private let value = IndexUI.label("—", IndexUI.font(21, .semibold), Theme.ink)
    private let detail = IndexUI.label("", IndexUI.font(10.5), Theme.muted)

    init() {
        let stack = NSStackView(views: [caption, value, detail])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 3
        stack.translatesAutoresizingMaskIntoConstraints = false
        value.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        detail.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: view.topAnchor, constant: 11),
            stack.bottomAnchor.constraint(equalTo: view.bottomAnchor, constant: -11),
            stack.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 12),
            stack.trailingAnchor.constraint(lessThanOrEqualTo: view.trailingAnchor, constant: -10),
        ])
    }

    func set(_ title: String, _ figure: String, _ note: String) {
        caption.attributedStringValue = Theme.label(title.uppercased(), size: 9.5, tracking: 0.12, color: Theme.faint, weight: .medium)
        value.stringValue = figure
        detail.stringValue = note
    }
}

@MainActor
private final class Card: NSView {
    private let rows = NSStackView()

    init() {
        super.init(frame: .zero)
        translatesAutoresizingMaskIntoConstraints = false
        let plate = PlateView(fill: Theme.ground, radius: 10, border: Theme.hairline)
        addSubview(plate)
        rows.orientation = .vertical
        rows.alignment = .leading
        rows.spacing = 0
        rows.translatesAutoresizingMaskIntoConstraints = false
        plate.addSubview(rows)
        NSLayoutConstraint.activate([
            plate.topAnchor.constraint(equalTo: topAnchor),
            plate.bottomAnchor.constraint(equalTo: bottomAnchor),
            plate.leadingAnchor.constraint(equalTo: leadingAnchor),
            plate.trailingAnchor.constraint(equalTo: trailingAnchor),
            rows.topAnchor.constraint(equalTo: plate.topAnchor, constant: 5),
            rows.bottomAnchor.constraint(equalTo: plate.bottomAnchor, constant: -5),
            rows.leadingAnchor.constraint(equalTo: plate.leadingAnchor, constant: 6),
            rows.trailingAnchor.constraint(equalTo: plate.trailingAnchor, constant: -6),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    func set(_ views: [NSView]) {
        for view in rows.arrangedSubviews { view.removeFromSuperview() }
        for view in views {
            rows.addArrangedSubview(view)
            view.widthAnchor.constraint(equalTo: rows.widthAnchor).isActive = true
        }
    }
}

@MainActor
private final class MeterView: NSView {
    private let track = PlateView(fill: Theme.hairline, radius: 1.5)
    private let bar = PlateView(fill: Theme.accent, radius: 1.5)
    var fraction: Double = 0 { didSet { needsLayout = true } }

    init(height: CGFloat) {
        super.init(frame: .zero)
        translatesAutoresizingMaskIntoConstraints = false
        for layer in [track, bar] {
            layer.translatesAutoresizingMaskIntoConstraints = true
            addSubview(layer)
        }
        heightAnchor.constraint(equalToConstant: height).isActive = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    func tint(_ colour: NSColor) { bar.fill(colour) }

    override func layout() {
        super.layout()
        track.frame = bounds
        let clamped = CGFloat(min(max(fraction.isFinite ? fraction : 0, 0), 1))
        bar.frame = NSRect(x: 0, y: 0, width: (bounds.width * clamped).rounded(), height: bounds.height)
    }
}

@MainActor
private final class LinkRow: NSView {
    private let action: () -> Void
    private var tracking: NSTrackingArea?

    init(action: @escaping () -> Void) {
        self.action = action
        super.init(frame: .zero)
        wantsLayer = true
        layer?.cornerRadius = 6
        layer?.cornerCurve = .continuous
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let tracking { removeTrackingArea(tracking) }
        let area = NSTrackingArea(rect: bounds, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self)
        addTrackingArea(area)
        tracking = area
    }

    override func mouseEntered(with event: NSEvent) { layer?.backgroundColor = Theme.selection.cgColor }
    override func mouseExited(with event: NSEvent) { layer?.backgroundColor = nil }
    override func mouseDown(with event: NSEvent) {}
    override func mouseUp(with event: NSEvent) {
        if bounds.contains(convert(event.locationInWindow, from: nil)) { action() }
    }
    override func resetCursorRects() { addCursorRect(bounds, cursor: .pointingHand) }
}

@MainActor
private final class ActionButton: NSButton {
    private let handler: () -> Void

    init(_ title: String, colour: NSColor, handler: @escaping () -> Void) {
        self.handler = handler
        super.init(frame: .zero)
        isBordered = false
        attributedTitle = Theme.label(title.uppercased(), size: 10, tracking: 0.1, color: colour, weight: .medium)
        target = self
        action = #selector(fire)
        wantsLayer = true
        layer?.cornerRadius = 7
        layer?.cornerCurve = .continuous
        layer?.borderWidth = 1
        layer?.borderColor = colour.withAlphaComponent(0.45).cgColor
        translatesAutoresizingMaskIntoConstraints = false
        heightAnchor.constraint(equalToConstant: 26).isActive = true
        setContentHuggingPriority(.required, for: .horizontal)
        setContentCompressionResistancePriority(.required, for: .horizontal)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    override var intrinsicContentSize: NSSize {
        var size = super.intrinsicContentSize
        size.width += 22
        return size
    }

    @objc private func fire() { handler() }
}
