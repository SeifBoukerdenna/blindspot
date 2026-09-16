import AppKit

@MainActor
private final class IndexPageStack: NSStackView {
    override var isFlipped: Bool { true }
}

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
    var onOpenSetting: ((String) -> Void)?
    var onAction: ((String, String) -> String?)?
    var inspectFile: (@Sendable (String) -> FileInspection?)? {
        didSet { fileCheck.inspect = inspectFile }
    }
    private let fileCheck = FileInspectionView()

    private let column = IndexPageStack()
    private let pages = (0..<3).map { _ in IndexPageStack() }
    private let navigation = NSSegmentedControl()
    private let scroll = NSScrollView()
    private(set) var selectedPage = 0
    private let dot = PlateView(fill: Theme.faint, radius: 4)
    private let title = IndexUI.label("Index", IndexUI.font(20, .semibold), Theme.ink)
    private let subtitle = IndexUI.wrapping("", IndexUI.font(12), Theme.muted, width: 560)
    private let currentWork = IndexUI.wrapping("", IndexUI.font(13), Theme.ink, width: 560)
    private let passSummary = IndexUI.wrapping("", IndexUI.font(12), Theme.inkSoft, width: 560)
    private let progressNote = IndexUI.wrapping("", IndexUI.font(12), Theme.muted, width: 560)
    private let progress = NSProgressIndicator()
    private let embeddingMeter = MeterView(height: 4)
    private let embeddingWork = IndexUI.wrapping("", IndexUI.font(12), Theme.inkSoft, width: 560)
    private let modelHealth = IndexUI.wrapping("", IndexUI.font(12), Theme.muted, width: 560)
    private let actionError = IndexUI.wrapping("", IndexUI.font(12), Theme.danger, width: 560)
    private var controls: IndexControls?
    private var pauseButton: ActionButton?
    private var retryButton: ActionButton?
    private var checkButton: ActionButton?
    private let issueSummary = IndexUI.wrapping("", IndexUI.font(12), Theme.muted, width: 560)
    private let inventory = IndexUI.wrapping("Waiting for index", IndexUI.font(12), Theme.muted, width: 600)
    private let roots = Card()
    private let semantic = Card()
    private let storage = Card()
    private let busy = NSStackView()
    private let types = Card()
    private let folders = Card()
    private let attention = Card()
    private let recent = Card()
    private let activity = Card()
    private let resources = Card()
    private var last: IndexOverview?
    private var refreshButton: ActionButton?
    private var compactButton: ActionButton?

    init() {
        super.init(frame: .zero)
        translatesAutoresizingMaskIntoConstraints = false
        column.orientation = .vertical
        column.alignment = .leading
        column.spacing = 0
        column.detachesHiddenViews = true
        column.translatesAutoresizingMaskIntoConstraints = false
        scroll.documentView = column
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.scrollerStyle = .overlay
        scroll.translatesAutoresizingMaskIntoConstraints = false
        navigation.segmentCount = 3
        for (index, name) in ["Overview", "Folders", "Diagnostics"].enumerated() {
            navigation.setLabel(name, forSegment: index)
            navigation.setWidth(112, forSegment: index)
        }
        navigation.trackingMode = .selectOne
        navigation.target = self
        navigation.action = #selector(navigate)
        navigation.identifier = NSUserInterfaceItemIdentifier("index.navigation")
        navigation.setAccessibilityLabel("Index sections")
        navigation.translatesAutoresizingMaskIntoConstraints = false
        addSubview(navigation)
        addSubview(scroll)
        let preferredHeight = heightAnchor.constraint(equalToConstant: 520)
        preferredHeight.priority = .defaultLow
        NSLayoutConstraint.activate([
            preferredHeight,
            navigation.topAnchor.constraint(equalTo: topAnchor, constant: 4),
            navigation.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Theme.gutter),
            navigation.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -Theme.gutter),
            scroll.topAnchor.constraint(equalTo: navigation.bottomAnchor, constant: 18),
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor),
            column.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor),
        ])
        for page in pages {
            page.orientation = .vertical
            page.alignment = .leading
            page.spacing = 0
            page.edgeInsets = NSEdgeInsets(top: 4, left: Theme.gutter, bottom: 24, right: Theme.gutter)
            column.addArrangedSubview(page)
            page.widthAnchor.constraint(equalTo: column.widthAnchor).isActive = true
        }
        busy.orientation = .vertical
        busy.alignment = .leading
        busy.spacing = 8
        busy.isHidden = true

        add(header(), after: 0)
        add(subtitle, after: 8)
        progress.style = .bar
        progress.isIndeterminate = true
        progress.isDisplayedWhenStopped = false
        progress.setAccessibilityLabel("Indexing activity; total work is not known")
        add(progress, after: 16)
        embeddingMeter.identifier = NSUserInterfaceItemIdentifier("index.embeddingProgress")
        embeddingMeter.setAccessibilityElement(true)
        embeddingMeter.setAccessibilityRole(.progressIndicator)
        embeddingMeter.setAccessibilityLabel("Embedding pass progress")
        add(embeddingMeter, after: 0)
        add(currentWork, after: 12)
        add(passSummary, after: 8)
        add(embeddingWork, after: 8)
        add(progressNote, after: 8)
        let pause = ActionButton("Pause indexing", colour: Theme.accent) { [weak self] in
            guard let self else { return }
            self.perform(self.controls?.manualPause == true ? "resume" : "pause")
        }
        pause.identifier = NSUserInterfaceItemIdentifier("index.pause")
        pauseButton = pause
        add(NSStackView(views: [pause, IndexUI.spacer()]), after: 8)
        actionError.isHidden = true
        add(actionError, after: 8)
        add(IndexUI.caption("Your library"), after: 24)
        add(inventory, after: 8)
        add(semantic, after: 8)
        add(issueSummary, after: 16)
        let issues = ActionButton("Review diagnostics →", colour: Theme.accent) { [weak self] in self?.showPage(2) }
        issues.identifier = NSUserInterfaceItemIdentifier("index.reviewIssues")
        let settings = ActionButton("Indexing settings…", colour: Theme.muted) { [weak self] in self?.onOpenSetting?("content.enabled") }
        settings.identifier = NSUserInterfaceItemIdentifier("index.settings")
        add(NSStackView(views: [issues, IndexUI.spacer(), settings]), after: 12)
        let check = ActionButton("Check model", colour: Theme.accent) { [weak self] in self?.perform("check") }
        check.identifier = NSUserInterfaceItemIdentifier("index.checkModel")
        checkButton = check
        add(NSStackView(views: [IndexUI.caption("Embedding model"), IndexUI.spacer(), check]), after: 20)
        add(modelHealth, after: 8)

        let manage = ActionButton("Manage folders…", colour: Theme.accent) { [weak self] in self?.onOpenSetting?("content.roots") }
        manage.identifier = NSUserInterfaceItemIdentifier("index.manageFolders")
        add(NSStackView(views: [IndexUI.caption("Search locations"), IndexUI.spacer(), manage]), page: 1, after: 0)
        add(IndexUI.note("These are the folders being watched. Manage folders also opens exclusions and code-folder controls."), page: 1, after: 8)
        add(roots, page: 1, after: 8)
        add(busy, page: 1, after: 14)
        add(IndexUI.caption("Indexed folders"), page: 1, after: 20)
        add(folders, page: 1, after: 8)
        add(IndexDisclosure("File types", content: types), page: 1, after: 12)
        add(IndexDisclosure("Recently changed", content: recent), page: 1, after: 12)

        let compact = ActionButton("Compact…", colour: Theme.muted) { [weak self] in self?.onCompact?() }
        compact.identifier = NSUserInterfaceItemIdentifier("index.compact")
        compact.toolTip = "Reclaim unused index space. A confirmation explains what will be removed."
        compactButton = compact
        let retry = ActionButton("Retry failed items", colour: Theme.accent) { [weak self] in self?.perform("retry") }
        retry.identifier = NSUserInterfaceItemIdentifier("index.retry")
        retry.toolTip = "Retry recorded extraction problems and missing embeddings. Successful files and vectors are reused."
        retryButton = retry
        fileCheck.onOpenSetting = { [weak self] key in self?.onOpenSetting?(key) }
        add(fileCheck, page: 2, after: 0)
        add(NSStackView(views: [IndexUI.caption("Needs attention"), IndexUI.spacer(), retry, compact]), page: 2, after: 24)
        add(attention, page: 2, after: 8)
        add(IndexDisclosure("Last reported pass", content: activity), page: 2, after: 16)
        add(IndexDisclosure("Storage", content: storage), page: 2, after: 12)
        add(IndexDisclosure("Resources", content: resources), page: 2, after: 12)
        showPage(0)
        update(nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    private func add(_ view: NSView, page: Int = 0, after spacing: CGFloat) {
        let stack = pages[page]
        if let previous = stack.arrangedSubviews.last { stack.setCustomSpacing(spacing, after: previous) }
        stack.addArrangedSubview(view)
        view.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -Theme.gutter * 2).isActive = true
    }

    @objc private func navigate() { showPage(navigation.selectedSegment) }

    private func showPage(_ index: Int) {
        guard pages.indices.contains(index) else { return }
        selectedPage = index
        navigation.selectedSegment = index
        for (at, page) in pages.enumerated() { page.isHidden = at != index }
        column.layoutSubtreeIfNeeded()
        scroll.contentView.scroll(to: .zero)
        scroll.reflectScrolledClipView(scroll.contentView)
    }

    private func header() -> NSView {
        dot.widthAnchor.constraint(equalToConstant: 9).isActive = true
        dot.heightAnchor.constraint(equalToConstant: 9).isActive = true
        let heading = NSStackView(views: [dot, title])
        heading.orientation = .horizontal
        heading.alignment = .centerY
        heading.spacing = 9
        let text = NSStackView(views: [heading])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 3
        subtitle.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        text.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let refresh = ActionButton("Rescan folders", colour: Theme.accent) { [weak self] in self?.onRefresh?() }
        refresh.identifier = NSUserInterfaceItemIdentifier("index.refresh")
        refresh.toolTip = "Rescan your selected folders and retry semantic indexing. No model downloads."
        refreshButton = refresh
        let row = NSStackView(views: [text, IndexUI.spacer(), refresh])
        row.orientation = .horizontal
        row.alignment = .centerY
        row.spacing = 12
        return row
    }

    private func perform(_ action: String, folder: String = "") {
        actionError.stringValue = onAction?(action, folder) ?? ""
        actionError.isHidden = actionError.stringValue.isEmpty
        if !actionError.isHidden { showPage(0) }
    }

    func update(_ overview: IndexOverview?, controls: IndexControls? = nil) {
        let oldControls = self.controls
        self.controls = controls
        guard let overview else {
            last = nil
            title.stringValue = "Index status unavailable"
            dot.fill(Theme.faint)
            subtitle.stringValue = "Waiting for an index snapshot. Previous counts are not shown."
            refreshButton?.isEnabled = false
            compactButton?.isEnabled = false
            pauseButton?.isEnabled = false
            retryButton?.isEnabled = false
            checkButton?.isEnabled = false
            modelHealth.stringValue = "Waiting for model status"
            embeddingWork.isHidden = true
            embeddingMeter.isHidden = true
            inventory.stringValue = "Waiting for index"
            currentWork.stringValue = ""
            passSummary.stringValue = ""
            progressNote.stringValue = ""
            issueSummary.stringValue = ""
            currentWork.isHidden = true
            passSummary.isHidden = true
            progressNote.isHidden = true
            progress.stopAnimation(nil)
            progress.isHidden = true
            for card in [semantic, storage, types, roots, folders, attention, recent, activity, resources] {
                card.set([IndexUI.note("Waiting for index status…")])
            }
            rebuildBusy([])
            return
        }
        guard overview != last || controls != oldControls else { return }
        let previous = last
        last = overview
        updateHeader(overview)
        inventory.stringValue = "\(IndexUI.number(overview.documents)) documents  ·  \(IndexUI.number(overview.semantic.eligible)) passages  ·  \(IndexUI.bytes(overview.disk.database + (overview.disk.cache ?? 0))) on disk"
        refreshButton?.isEnabled = ["ready", "partial", "failed"].contains(overview.state)
            && (controls?.canRun ?? true)
        pauseButton?.showTitle(controls?.manualPause == true ? "Resume indexing" : "Pause indexing", colour: Theme.accent)
        pauseButton?.isEnabled = controls?.canPause == true
        retryButton?.isEnabled = controls?.canRun == true
        checkButton?.showTitle(controls?.checking == true ? "Checking…" : "Check model", colour: Theme.accent)
        checkButton?.isEnabled = controls != nil && controls?.checking == false && !["erasing", "compacting"].contains(overview.state)
        var healthText = controls?.health.map { health in
            "Last check: \(health.model)\(health.dimensions.map { " · \($0) dimensions" } ?? "") · \(health.milliseconds) ms\n\(health.message)"
        } ?? "Checks your selected embedding model locally with a short test phrase. No documents are sent and no model is downloaded."
        if controls?.checking == true { healthText = "Checking the selected model locally…" }
        if controls?.recoveryNeeded == true {
            healthText += controls?.manualPause == true || controls?.policyPause == true
                ? "\nAutomatic recovery waits until indexing is resumed and power policy allows it."
                : overview.state == "indexing" ? "\nAutomatic recovery waits for the current pass to finish."
                : "\nAutomatic recovery checks again in about \(IndexUI.duration(Double(controls?.retrySeconds ?? 0)))."
        }
        modelHealth.stringValue = healthText
        compactButton?.isEnabled = overview.sampled && !["indexing", "erasing", "compacting", "erased"].contains(overview.state)
        if previous?.semantic != overview.semantic || previous?.message != overview.message || previous?.sampled != overview.sampled || previous?.state != overview.state {
            semantic.set(semanticRows(overview))
        }
        if previous?.busy != overview.busy { rebuildBusy(overview.busy) }
        if previous?.roots != overview.roots || oldControls?.roots != controls?.roots || oldControls?.canRun != controls?.canRun {
            roots.set(overview.roots.isEmpty ? [IndexUI.note("No folders selected. Choose Manage folders to get started.")]
                : (controls?.roots ?? overview.roots).map { path in
                    let label = IndexUI.label((path as NSString).abbreviatingWithTildeInPath, IndexUI.font(13), Theme.ink, breaking: .byTruncatingMiddle)
                    label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
                    let rescan = ActionButton("Rescan", colour: Theme.muted) { [weak self] in self?.perform("folder", folder: path) }
                    rescan.identifier = NSUserInterfaceItemIdentifier("index.rescanFolder")
                    rescan.setAccessibilityLabel("Rescan \(path)")
                    rescan.isEnabled = controls?.canRun == true
                    return NSStackView(views: [IndexUI.symbol("folder", Theme.muted), label, IndexUI.spacer(), rescan])
                })
        }
        if previous?.kinds != overview.kinds { types.set(typeRows(overview)) }
        if previous?.folders != overview.folders { folders.set(folderRows(overview)) }
        if previous?.attention != overview.attention || previous?.pdfIssues != overview.pdfIssues || previous?.sampled != overview.sampled || previous?.partialDocuments != overview.partialDocuments || previous?.semantic.passFailed != overview.semantic.passFailed || previous?.state != overview.state || previous?.message != overview.message || previous?.compactError != overview.compactError || previous?.pacing != overview.pacing {
            attention.set(attentionRows(overview))
        }
        if previous?.recent != overview.recent { recent.set(recentRows(overview)) }
        activity.set(activityRows(overview))
        storage.set(storageRows(overview))
        resources.set(resourceRows(overview))
        var issues: [String] = []
        let extraction = overview.pdfIssues?.reduce(0, +) ?? UInt64(overview.attention.count)
        if extraction > 0 { issues.append("\(IndexUI.number(extraction)) extraction issues") }
        if let partial = overview.partialDocuments, partial > 0 { issues.append("\(IndexUI.number(partial)) partly indexed documents") }
        if let failed = overview.semantic.passFailed, failed > 0 { issues.append("\(IndexUI.number(failed)) embedding failures this pass") }
        issueSummary.stringValue = !overview.sampled ? "Checking for issues…" : issues.isEmpty
            ? "No file issues reported. Diagnostics has detailed activity, storage and resource usage."
            : issues.joined(separator: " · ") + ". Review diagnostics for details."
    }

    private func updateHeader(_ overview: IndexOverview) {
        let (text, colour): (String, NSColor) = switch overview.state {
        case "indexing": (overview.stage.isEmpty ? "Preparing index" : overview.stage, Theme.accent)
        case "ready": ("Index pass complete", Theme.ok)
        case "partial": ("Partly indexed", Theme.warn)
        case "paused": ("Paused", Theme.warn)
        case "needsRoots": ("Choose folders to index", Theme.warn)
        case "off": ("Indexing is off", Theme.faint)
        case "erasing": ("Erasing index", Theme.warn)
        case "compacting": ("Compacting index", Theme.accent)
        case "erased": ("Index erased", Theme.faint)
        case "eraseFailed": ("Erasure incomplete", Theme.danger)
        default: ("Index unavailable", Theme.danger)
        }
        title.stringValue = text
        dot.fill(colour)
        let working = ["indexing", "compacting", "erasing"].contains(overview.state)
        let embeddingTotal = overview.stage == "Embedding passages" ? controls?.embeddingTotal : nil
        embeddingMeter.isHidden = !working || embeddingTotal == nil
        embeddingMeter.fraction = Double(controls?.embeddingDone ?? 0) / Double(max(1, embeddingTotal ?? 1))
        embeddingMeter.setAccessibilityValue("\(controls?.embeddingDone ?? 0) of \(embeddingTotal ?? 0) passages attempted")
        progress.isHidden = !working || embeddingTotal != nil
        if working && embeddingTotal == nil && !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion { progress.startAnimation(nil) }
        else { progress.stopAnimation(nil) }
        currentWork.stringValue = overview.state == "indexing"
            ? overview.currentFolder.map { "Working in \($0)" } ?? "Preparing the next batch…" : ""
        currentWork.isHidden = currentWork.stringValue.isEmpty
        passSummary.stringValue = ""
        embeddingWork.stringValue = ""
        progressNote.stringValue = ""
        var parts: [String] = []
        switch overview.state {
        case "indexing":
            parts.append(overview.message)
            if let seconds = overview.stageSeconds { parts.append("This stage: \(IndexUI.duration(Double(seconds)))") }
            passSummary.stringValue = "This pass: \(IndexUI.number(overview.pass.checked)) entries checked · \(IndexUI.number(overview.pass.updated)) documents updated"
            if overview.stage.lowercased().contains("embed"), let written = overview.semantic.passEmbedded {
                passSummary.stringValue += " · \(IndexUI.number(written)) passages embedded"
            }
            progressNote.stringValue = "Work is still being counted. No reliable total or time estimate yet. This view updates automatically."
            if let total = embeddingTotal {
                embeddingWork.stringValue = "Embedding: \(IndexUI.number(controls?.embeddingDone)) of \(IndexUI.number(total)) passages attempted · \(IndexUI.number(controls?.embeddingRemaining)) remaining"
                progressNote.stringValue = "This embedding pass only; failed passages are counted as attempted, not successful. Cache preparation follows."
            }
        case "compacting":
            parts.append(overview.stage.isEmpty ? "Starting" : overview.stage)
            progressNote.stringValue = "Indexing and content search resume when compaction finishes. Your source files are not changed."
        case "erasing":
            parts.append(overview.message)
            progressNote.stringValue = "Stored search data is being removed. Source files are not deleted."
        case "paused":
            parts.append(overview.message)
            progressNote.stringValue = controls?.manualPause == true
                ? "Paused by you for this app session. Resume indexing keeps completed work; power and thermal protection still apply."
                : "Resumes automatically when the power or thermal condition clears. No need to rescan."
        case "off", "erased", "needsRoots":
            parts.append(overview.message)
            progressNote.stringValue = "Use Folders to choose what to search, then Indexing settings to enable indexing."
        case "ready", "partial":
            if let ago = overview.lastPassAgo {
                parts.append("Last pass \(IndexUI.ago(ago))")
                if let took = overview.lastPassSeconds { parts.append("took \(IndexUI.duration(took))") }
            }
            progressNote.stringValue = overview.state == "partial"
                ? "Some work did not finish. Review diagnostics before retrying."
                : "The last pass has finished. Folder changes are picked up automatically."
        default:
            parts.append(overview.message)
        }
        passSummary.isHidden = passSummary.stringValue.isEmpty
        embeddingWork.isHidden = embeddingWork.stringValue.isEmpty
        progressNote.isHidden = progressNote.stringValue.isEmpty
        subtitle.stringValue = parts.joined(separator: "  ·  ")

        subtitle.toolTip = subtitle.stringValue
    }

    private func semanticRows(_ overview: IndexOverview) -> [NSView] {
        let state = overview.semantic
        var rows: [NSView] = []
        if !state.enabled {
            rows.append(IndexUI.note("Search by meaning is off. Stored vectors are retained; enable it in Content settings to use them."))
        } else if let total = state.eligible, let embedded = state.embedded, overview.sampled {
            rows.append(IndexUI.row(symbol: "square.stack.3d.up", tint: Theme.muted,
                title: "\(IndexUI.number(embedded)) passages have active-model embeddings"))
            rows.append(IndexUI.note(total == 0
                ? "No stored passages yet. Choose folders under Folders, then use Rescan folders to start indexing."
                : "Library coverage, not progress toward completion. Data and spreadsheets stay word-only; code needs selected code folders."))
        } else {
            rows.append(IndexUI.note("Measuring passage coverage… Word search and meaning search have separate readiness."))
        }
        if ["ready", "partial"].contains(overview.state) {
            rows.append(IndexUI.pair("Search worker", overview.message.isEmpty ? "Waiting for worker status" : overview.message))
        }
        return rows
    }

    private func storageRows(_ overview: IndexOverview) -> [NSView] {
        let used = overview.disk.database + (overview.disk.cache ?? 0)
        var rows: [NSView] = []
        if let budget = overview.budgetBytes, budget > 0 {
            rows.append(IndexUI.row(symbol: "internaldrive", tint: Theme.muted,
                title: "\(IndexUI.bytes(used)) of \(IndexUI.bytes(budget)) target",
                trailing: used >= budget ? "AT TARGET" : "\(Int(Double(used) / Double(budget) * 100))%",
                fraction: Double(used) / Double(budget), meterColour: used >= budget ? Theme.warn : Theme.accent))
        }
        rows.append(IndexUI.pair("Database", "\(IndexUI.bytes(overview.disk.database)) · includes write-ahead log and shared memory"))
        rows.append(IndexUI.pair("Vector cache", overview.disk.cache.map(IndexUI.bytes) ?? "Measuring…"))
        if let reclaimable = overview.reclaimable {
            rows.append(IndexUI.pair("Reclaimable", "\(IndexUI.bytes(reclaimable)) · use Compact to free unused index space"))
        }
        rows.append(IndexUI.note("The storage target stops new batches, not an in-flight write. It is not a hard quota. Source files are never removed by Compact."))
        return rows
    }

    private func rebuildBusy(_ items: [IndexOverview.Busy]) {
        for view in busy.arrangedSubviews { view.removeFromSuperview() }
        for item in items {
            let plate = PlateView(fill: .clear)
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
        return overview.kinds.map { kind in
            IndexUI.row(symbol: IndexUI.symbolName(forKind: kind.label), tint: Theme.muted, title: kind.label,
                        detail: IndexUI.bytes(kind.bytes), trailing: IndexUI.number(kind.count))
        }
    }

    private func folderRows(_ overview: IndexOverview) -> [NSView] {
        guard !overview.folders.isEmpty else { return [IndexUI.note(overview.sampled ? "No folders indexed yet" : "Counting…")] }
        return overview.folders.map { folder in
            let path = folder.path
            return IndexUI.row(symbol: "folder", tint: Theme.muted, title: folder.folder,
                               detail: IndexUI.bytes(folder.bytes), trailing: IndexUI.number(folder.count),
                               breaking: .byTruncatingHead) {
                IndexUI.reveal(path)
            }
        }
    }

    private func attentionRows(_ overview: IndexOverview) -> [NSView] {
        guard overview.sampled else { return [IndexUI.note("Counting…")] }
        var rows: [NSView] = []
        if ["failed", "partial", "eraseFailed"].contains(overview.state) {
            rows.append(IndexUI.row(symbol: "exclamationmark.triangle.fill", tint: Theme.warn,
                title: "Indexing needs attention", detail: overview.message))
        }
        if let failed = overview.semantic.passFailed, failed > 0 {
            rows.append(IndexUI.row(symbol: "sparkles", tint: Theme.warn,
                title: "\(IndexUI.number(failed)) passages could not be embedded this pass",
                detail: "Their indexed text remains searchable by words."))
        }
        if let partial = overview.partialDocuments, partial > 0 {
            rows.append(IndexUI.row(symbol: "doc.badge.ellipsis", tint: Theme.warn,
                title: "\(IndexUI.number(partial)) documents are partly indexed",
                detail: "A byte, passage, page or OCR limit was reached."))
        }
        if let error = overview.compactError { rows.append(IndexUI.pair("Compact failed", error)) }
        rows += overview.attention.prefix(8).map { item in
            let path = item.path
            return IndexUI.row(symbol: "exclamationmark.triangle.fill", tint: Theme.warn, title: item.name,
                               detail: "\(item.reason) · \(item.folder)") { IndexUI.reveal(path) }
        }
        let total = Int(overview.pdfIssues?.reduce(0, +) ?? UInt64(overview.attention.count))
        if total > min(8, overview.attention.count) { rows.append(IndexUI.note("and \(IndexUI.number(UInt64(total - min(8, overview.attention.count)))) more extraction issues")) }
        if rows.isEmpty {
            rows.append(IndexUI.row(symbol: overview.pacing.documents ? "checkmark.circle" : "info.circle", tint: Theme.muted,
                title: overview.pacing.documents ? "No reported extraction issues" : "PDF and Office extraction is off",
                detail: "This is worker status, not a database integrity check."))
        }
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
            rows.append(IndexUI.pair("Embedded this pass", "\(IndexUI.number(embedded)) passages · \(IndexUI.number(overview.semantic.passFailed ?? 0)) failed"))
        }
        rows.append(IndexUI.pair("Watching", overview.roots.isEmpty ? "No folders" : overview.roots.joined(separator: ", ")))
        if let text = overview.pdfText { rows.append(IndexUI.pair("Document text", "\(IndexUI.number(text)) PDF / Office documents extracted")) }
        if let compact = overview.compact {
            rows.append(IndexUI.pair("Last compact", "\(IndexUI.bytes(compact.before)) → \(IndexUI.bytes(compact.after)) · \(IndexUI.number(compact.removed)) unused vectors removed"))
        }
        rows.append(IndexUI.pair("Counts", overview.sampleAgo.map { "Sampled \(IndexUI.ago($0))" } ?? "Sampling…"))
        return rows
    }

    private func resourceRows(_ overview: IndexOverview) -> [NSView] {
        var rows: [NSView] = overview.processes.map { process in
            let name = switch process.name {
            case "Blindspot": "Blindspot"
            case "semantic": "Semantic model"
            case "vectors": "Vector search"
            case "extract": "Document extraction"
            default: process.name
            }
            let cpu = process.cpu.map { String(format: "%.1f%% CPU", $0) } ?? "—"
            return IndexUI.row(symbol: process.name == "Blindspot" ? "app.badge" : "cpu", tint: Theme.muted, title: name,
                               detail: IndexUI.bytes(process.memory) + " memory", trailing: cpu)
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

@MainActor
final class FileInspectionView: NSStackView {
    var inspect: (@Sendable (String) -> FileInspection?)?
    var onOpenSetting: ((String) -> Void)?
    private var task: Task<Void, Never>?
    private var generation = UUID()
    private var setting = ""
    private let result = IndexUI.wrapping("Choose a file to check its scope, extraction and embedding coverage. This does not change the index.", IndexUI.font(12), Theme.muted, width: 550)
    private let filename = IndexUI.label("", IndexUI.font(12, .medium), Theme.ink, breaking: .byTruncatingMiddle)
    private let settings = NSButton(title: "Open relevant settings…", target: nil, action: nil)

    init() {
        super.init(frame: .zero)
        orientation = .vertical
        alignment = .leading
        spacing = 10
        translatesAutoresizingMaskIntoConstraints = false
        let choose = NSButton(title: "Check a file…", target: self, action: #selector(chooseFile))
        choose.identifier = NSUserInterfaceItemIdentifier("index.checkFile")
        choose.bezelStyle = .rounded
        let heading = NSStackView(views: [IndexUI.caption("Why can’t I find this file?"), IndexUI.spacer(), choose])
        for view in [heading, filename, result] {
            addArrangedSubview(view)
            view.widthAnchor.constraint(equalTo: widthAnchor).isActive = true
        }
        result.identifier = NSUserInterfaceItemIdentifier("index.fileResult")
        filename.isHidden = true
        settings.isBordered = false
        settings.contentTintColor = Theme.accent
        settings.target = self
        settings.action = #selector(openSettings)
        settings.identifier = NSUserInterfaceItemIdentifier("index.fileSettings")
        settings.isHidden = true
        addArrangedSubview(settings)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    isolated deinit { task?.cancel() }

    override func viewWillMove(toWindow newWindow: NSWindow?) {
        if newWindow == nil { task?.cancel(); generation = UUID() }
        super.viewWillMove(toWindow: newWindow)
    }

    @objc private func chooseFile() {
        guard let window else { return }
        let picker = NSOpenPanel()
        picker.title = "Check a file in Blindspot"
        picker.prompt = "Check file"
        picker.canChooseDirectories = false
        picker.allowsMultipleSelection = false
        picker.resolvesAliases = false
        picker.beginSheetModal(for: window) { [weak self] response in
            guard response == .OK, let url = picker.url else { return }
            self?.check(url)
        }
    }

    func check(_ url: URL) {
        guard url.isFileURL, let inspect else { return }
        task?.cancel()
        generation = UUID()
        let expected = generation
        filename.stringValue = url.lastPathComponent
        filename.toolTip = url.path
        filename.isHidden = false
        settings.isHidden = true
        result.stringValue = "Checking this file locally…"
        let path = url.path
        task = Task { [weak self] in
            let worker = Task.detached(priority: .userInitiated) { inspect(path) }
            let report = await withTaskCancellationHandler { await worker.value } onCancel: { worker.cancel() }
            guard !Task.isCancelled, let self, self.generation == expected else { return }
            guard let report else {
                self.result.stringValue = "The check is unavailable. Try again after current indexing work finishes."
                return
            }
            self.result.stringValue = "\(report.title)\n\n\(report.detail)\n\n\(report.next)"
            if let passages = report.passages, let embedded = report.embedded {
                self.result.stringValue += "\n\n\(passages) stored passages · \(embedded) with active-generation embeddings"
            }
            self.setting = report.setting
            self.settings.isHidden = report.setting.isEmpty
        }
    }

    @objc private func openSettings() { if !setting.isEmpty { onOpenSetting?(setting) } }
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
        field.toolTip = text
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
        field.attributedStringValue = Theme.label(text, size: 13, tracking: 0, color: Theme.ink, weight: .medium)
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
        case "PDF & Word documents", "PDF & Office documents": "doc.richtext"
        case "Code": "chevron.left.forwardslash.chevron.right"
        case "Data & config": "tablecells"
        case "Web pages & styles": "globe"
        default: "doc"
        }
    }

    static func symbolName(forFile name: String) -> String {
        switch (name as NSString).pathExtension.lowercased() {
        case "pdf", "docx", "doc", "rtf", "odt", "pptx": "doc.richtext"
        case "xlsx": "tablecells"
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
        if action != nil {
            container.setAccessibilityElement(true)
            container.setAccessibilityRole(.button)
            container.setAccessibilityLabel([title, detail].compactMap { $0 }.joined(separator: ", "))
            container.setAccessibilityHelp("Reveal in Finder")
        }
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
private final class IndexDisclosure: NSStackView {
    private let heading: String
    private let content: NSView
    private let button = NSButton()

    init(_ heading: String, content: NSView) {
        self.heading = heading
        self.content = content
        super.init(frame: .zero)
        orientation = .vertical
        alignment = .leading
        spacing = 6
        translatesAutoresizingMaskIntoConstraints = false
        button.isBordered = false
        button.alignment = .left
        button.font = Theme.text(13, .medium)
        button.contentTintColor = Theme.ink
        button.target = self
        button.action = #selector(toggle)
        button.identifier = NSUserInterfaceItemIdentifier("index.section.\(heading)")
        button.setAccessibilityLabel(heading)
        for view in [button, content] {
            addArrangedSubview(view)
            view.widthAnchor.constraint(equalTo: widthAnchor).isActive = true
        }
        button.heightAnchor.constraint(equalToConstant: 30).isActive = true
        content.isHidden = true
        updateLabel()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    @objc private func toggle() {
        content.isHidden.toggle()
        updateLabel()
    }

    private func updateLabel() {
        button.title = (content.isHidden ? "▸  " : "▾  ") + heading
        button.setAccessibilityValue(content.isHidden ? "Collapsed" : "Expanded")
    }
}

@MainActor
private final class Card: NSView {
    private let rows = NSStackView()

    init() {
        super.init(frame: .zero)
        translatesAutoresizingMaskIntoConstraints = false
        let plate = PlateView(fill: .clear)
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
    override var acceptsFirstResponder: Bool { true }
    override func accessibilityPerformPress() -> Bool { action(); return true }
    override func keyDown(with event: NSEvent) {
        if event.charactersIgnoringModifiers == " " || event.keyCode == 36 { action() }
        else { super.keyDown(with: event) }
    }
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
        attributedTitle = Theme.label(title, size: 12, tracking: 0, color: colour, weight: .medium)
        target = self
        action = #selector(fire)
        wantsLayer = true
        layer?.cornerRadius = 7
        layer?.cornerCurve = .continuous
        setAccessibilityLabel(title)
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

    func showTitle(_ title: String, colour: NSColor) {
        attributedTitle = Theme.label(title, size: 12, tracking: 0, color: colour, weight: .medium)
        setAccessibilityLabel(title)
    }
}
