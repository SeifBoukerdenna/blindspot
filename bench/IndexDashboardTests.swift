import AppKit

@main
@MainActor
enum IndexDashboardTests {
    static func fixture(_ state: String = "ready", enabled: Bool = true, sampled: Bool = true,
                        empty: Bool = false, failed: UInt64 = 0, stage: String? = nil, written: UInt64 = 420) throws -> IndexOverview {
        let object: [String: Any] = [
            "state": state, "stage": stage ?? (state == "indexing" ? "Embedding passages" : ""),
            "message": state == "failed" ? "Semantic unavailable; text search ready"
                : state == "paused" ? "Paused by resource policy · Low Power Mode · Semantic indexing queued"
                : state == "indexing" ? "Embedding · \(written) written · 18,004 current"
                : "Semantic ready · embeddinggemma:300m",
            "stageSeconds": 38, "lastPassAgo": 42, "lastPassSeconds": 12.4,
            "currentFolder": "~/Documents/Research", "reclaimable": 32 * 1_048_576,
            "pass": ["checked": 1250, "updated": 84, "sourceBytes": 8_400_000,
                     "unchanged": 1128, "skipped": 36, "unreadable": 2, "removed": 3],
            "roots": ["~/Documents", "~/Projects"],
            "semantic": ["enabled": enabled, "embedded": empty ? 0 : 18424,
                         "eligible": empty ? 0 : 24180, "passEmbedded": written, "passFailed": failed],
            "documents": empty ? 0 : 14522, "pdfText": 216,
            "partialDocuments": state == "partial" ? 3 : 0, "budgetBytes": 3_221_225_472,
            "pdfIssues": [0, 0, 0, 0], "disk": ["database": 780_000_000, "cache": 43_000_000],
            "processes": [["name": "Blindspot", "memory": 64_000_000, "cpu": 2.4],
                          ["name": "vectors", "memory": 18_000_000, "cpu": 0.2]],
            "pacing": ["lowImpact": true, "battery": false, "documents": true, "documentLimitMB": 32],
            "busy": [], "sampled": sampled, "sampleAgo": 2,
            "kinds": empty ? [] : [["label": "Notes & text", "count": 9210, "bytes": 120_000_000],
                                    ["label": "Code", "count": 5096, "bytes": 240_000_000],
                                    ["label": "PDF & Office documents", "count": 216, "bytes": 90_000_000]],
            "folders": empty ? [] : [["folder": "~/Documents", "path": "/fixture/Documents", "count": 9426, "bytes": 210_000_000],
                                      ["folder": "~/Projects", "path": "/fixture/Projects", "count": 5096, "bytes": 240_000_000]],
            "attention": [], "recent": empty ? [] : [["name": "Planning notes.md", "folder": "~/Documents", "path": "/fixture/notes.md", "modified": Int(Date().timeIntervalSince1970) - 120]],
        ]
        return try JSONDecoder().decode(IndexOverview.self, from: JSONSerialization.data(withJSONObject: object))
    }

    static func descendants(_ view: NSView) -> [NSView] {
        [view] + view.subviews.flatMap(descendants)
    }

    static func text(_ view: NSView) -> String {
        descendants(view).compactMap { ($0 as? NSTextField)?.stringValue }.joined(separator: "\n")
    }

    static func visibleText(_ view: NSView) -> String {
        descendants(view).filter { !$0.isHiddenOrHasHiddenAncestor }
            .compactMap { ($0 as? NSTextField)?.stringValue }.joined(separator: "\n")
    }

    static func controls(manual: Bool = false, policy: Bool = false, running: Bool = false,
                         checking: Bool = false, recovery: Bool = false, total: UInt64? = nil,
                         health: IndexControls.Health? = nil) -> IndexControls {
        IndexControls(manualPause: manual, policyPause: policy, canPause: true,
            canRun: !manual && !policy && !running, checking: checking, recoveryNeeded: recovery,
            health: health, retrySeconds: recovery ? 60 : nil, embeddingTotal: total,
            embeddingRemaining: total.map { $0 - 40 }, embeddingDone: total == nil ? 0 : 40,
            roots: ["/fixture/Documents", "/fixture/Projects"])
    }

    static func render(_ dashboard: IndexDashboardView, name: String, directory: URL) throws {
        dashboard.layoutSubtreeIfNeeded()
        let height = dashboard.fittingSize.height
        precondition(height > 400 && height < 4000, "Unexpected dashboard height: \(height)")
        dashboard.setFrameSize(NSSize(width: 640, height: height))
        dashboard.layoutSubtreeIfNeeded()
        let plate = PlateView(fill: Theme.surface)
        plate.translatesAutoresizingMaskIntoConstraints = true
        plate.frame = dashboard.frame
        plate.addSubview(dashboard)
        let placement = [
            dashboard.leadingAnchor.constraint(equalTo: plate.leadingAnchor),
            dashboard.trailingAnchor.constraint(equalTo: plate.trailingAnchor),
            dashboard.topAnchor.constraint(equalTo: plate.topAnchor),
            dashboard.bottomAnchor.constraint(equalTo: plate.bottomAnchor),
        ]
        NSLayoutConstraint.activate(placement)
        let window = NSWindow(contentRect: plate.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.appearance = NSAppearance(named: Theme.isDark ? .darkAqua : .aqua)
        window.contentView = plate
        plate.layoutSubtreeIfNeeded()
        let visibleFields = descendants(dashboard).compactMap { $0 as? NSTextField }
            .filter { !$0.isHiddenOrHasHiddenAncestor }
        let firstTextTop = visibleFields.map { $0.convert($0.bounds, to: dashboard).maxY }.max() ?? 0
        precondition(firstTextTop > dashboard.bounds.height - 110, "Selected page must start below navigation, without hidden-page space")
        guard let bitmap = plate.bitmapImageRepForCachingDisplay(in: plate.bounds) else { fatalError("No bitmap") }
        plate.cacheDisplay(in: plate.bounds, to: bitmap)
        guard let png = bitmap.representation(using: .png, properties: [:]) else { fatalError("No PNG") }
        try png.write(to: directory.appendingPathComponent(name + ".png"))
        precondition(!dashboard.hasAmbiguousLayout, "Dashboard layout is ambiguous")
        for field in descendants(dashboard).compactMap({ $0 as? NSTextField }).filter({ !$0.isHiddenOrHasHiddenAncestor }) {
            let rect = field.convert(field.bounds, to: dashboard)
            precondition(rect.minX >= -1 && rect.maxX <= 641, "Text outside dashboard: \(field.stringValue)")
        }
        NSLayoutConstraint.deactivate(placement)
        dashboard.removeFromSuperview()
        window.close()
    }

    static func main() throws {
        NSApplication.shared.setActivationPolicy(.accessory)
        let directory = URL(fileURLWithPath: CommandLine.arguments.dropFirst().first ?? "build/index-dashboard-ui", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        var cases = 0
        for (palette, name) in [(Palette.ember, "dark"), (.parchment, "light")] {
            Theme.current = palette
            let glass = SurfaceView(radius: 18)
            glass.applyAccessibility(reduceTransparency: true, increaseContrast: false)
            precondition(glass.usesOpaqueFallback)
            glass.applyAccessibility(reduceTransparency: false, increaseContrast: true)
            precondition(glass.usesOpaqueFallback)
            glass.applyAccessibility(reduceTransparency: false, increaseContrast: false)
            precondition(!glass.usesOpaqueFallback)
            let dashboard = IndexDashboardView()
            dashboard.widthAnchor.constraint(equalToConstant: 640).isActive = true
            var refreshes = 0
            var compactions = 0
            var opened: [String] = []
            dashboard.onRefresh = { refreshes += 1 }
            dashboard.onCompact = { compactions += 1 }
            dashboard.onOpenSetting = { opened.append($0) }
            let navigation = descendants(dashboard).compactMap { $0 as? NSSegmentedControl }.first!
            precondition(navigation.superview === dashboard, "Navigation must stay outside the scroller")
            var allViews: [NSView] = []
            for page in [1, 2, 0] {
                navigation.selectedSegment = page
                _ = navigation.sendAction(navigation.action, to: navigation.target)
                allViews += descendants(dashboard)
            }
            var seen = Set<ObjectIdentifier>()
            allViews = allViews.filter { seen.insert(ObjectIdentifier($0)).inserted }
            func allText() -> String {
                allViews.compactMap { $0 as? NSStackView }.flatMap { descendants($0) }
                    .compactMap { ($0 as? NSTextField)?.stringValue }.joined(separator: "\n")
            }
            let refresh = allViews.compactMap { $0 as? NSButton }.first { $0.identifier?.rawValue == "index.refresh" }!
            let compact = allViews.compactMap { $0 as? NSButton }.first { $0.identifier?.rawValue == "index.compact" }!
            precondition(!refresh.isEnabled && !compact.isEnabled)
            dashboard.update(try fixture())
            try render(dashboard, name: name + "-summary", directory: directory)
            let disclosures = allViews.compactMap { $0 as? NSButton }
                .filter { $0.identifier?.rawValue.hasPrefix("index.section.") == true }
            precondition(disclosures.count == 5)
            for button in disclosures {
                precondition(button.title.hasPrefix("▸"))
                button.performClick(nil)
                precondition(button.title.hasPrefix("▾"))
            }
            for state in ["ready", "indexing", "paused", "partial", "failed", "erasing", "compacting", "erased", "needsRoots", "off", "eraseFailed"] {
                dashboard.update(try fixture(state, failed: state == "partial" ? 7 : 0))
                let labels = allText()
                precondition(disclosures.allSatisfy { $0.title.hasPrefix("▾") }, "Refresh must preserve disclosure state")
                precondition(labels.contains("Library coverage, not progress toward completion"))
                precondition(!visibleText(dashboard).contains("%"), "Overview must not present coverage as progress")
                precondition(refresh.isEnabled == ["ready", "partial", "failed"].contains(state))
                precondition(labels.contains("Database") && labels.contains("Vector cache"))
                if state == "partial" { precondition(labels.contains("7 passages could not") && labels.contains("3 documents are partly indexed")) }
                if state == "failed" { precondition(labels.contains("Semantic unavailable; text search ready")) }
                if state == "paused" {
                    precondition(labels.contains("Low Power Mode") && labels.contains("Resumes automatically"))
                    precondition(!visibleText(dashboard).contains("Working in"))
                    try render(dashboard, name: name + "-paused", directory: directory)
                }
                if ["indexing", "erasing", "compacting", "erased"].contains(state) { precondition(!compact.isEnabled) }
                if ["ready", "indexing", "partial"].contains(state) { try render(dashboard, name: name + "-" + state, directory: directory) }
                cases += 1
            }
            for stage in ["Scanning folders", "Scanning folders and extracting PDFs", "Embedding passages", "Building vector cache"] {
                dashboard.update(try fixture("indexing", stage: stage))
                let labels = visibleText(dashboard)
                precondition(labels.contains(stage) && labels.contains("This stage: 38s"))
                precondition(labels.contains("1,250 entries checked") && labels.contains("84 documents updated"))
                precondition(labels.contains("No reliable total or time estimate"))
                let indicator = descendants(dashboard).compactMap { $0 as? NSProgressIndicator }.first!
                precondition(indicator.isIndeterminate && !indicator.isHidden)
                cases += 1
            }
            for page in 1...2 {
                navigation.selectedSegment = page
                _ = navigation.sendAction(navigation.action, to: navigation.target)
                dashboard.update(try fixture("partial", failed: 7))
                precondition(dashboard.selectedPage == page, "Polling must not reset navigation")
                if page == 1 {
                    precondition(visibleText(dashboard).contains("Search locations"))
                    precondition(!visibleText(dashboard).contains("7 passages could not"))
                } else {
                    precondition(visibleText(dashboard).contains("7 passages could not"))
                }
                try render(dashboard, name: name + (page == 1 ? "-folders" : "-diagnostics"), directory: directory)
                cases += 1
            }
            let buttons = allViews.compactMap { $0 as? NSButton }
            for id in ["index.manageFolders", "index.settings"] {
                buttons.first { $0.identifier?.rawValue == id }!.performClick(nil)
            }
            precondition(opened == ["content.roots", "content.enabled"])
            navigation.selectedSegment = 0
            _ = navigation.sendAction(navigation.action, to: navigation.target)
            buttons.first { $0.identifier?.rawValue == "index.reviewIssues" }!.performClick(nil)
            precondition(dashboard.selectedPage == 2)
            dashboard.update(try fixture())
            refresh.performClick(nil)
            compact.performClick(nil)
            precondition(refreshes == 1 && compactions == 1)
            dashboard.update(try fixture(enabled: false))
            precondition(allText().contains("Search by meaning is off"))
            dashboard.update(try fixture(empty: true))
            precondition(allText().contains("No stored passages yet"))
            dashboard.update(try fixture(sampled: false))
            precondition(allText().contains("Measuring passage coverage"))
            dashboard.update(nil)
            precondition(!allText().contains("18,424") && !allText().contains("embeddinggemma"))
            precondition(!refresh.isEnabled && !compact.isEnabled)
            dashboard.update(try fixture())
            precondition(allText().contains("embeddinggemma"))
            cases += 5
            navigation.selectedSegment = 0
            _ = navigation.sendAction(navigation.action, to: navigation.target)
            var actions: [(String, String)] = []
            dashboard.onAction = { action, folder in actions.append((action, folder)); return nil }
            dashboard.update(try fixture("indexing", written: 40), controls: controls(running: true, total: 100))
            precondition(visibleText(dashboard).contains("40 of 100 passages attempted · 60 remaining"))
            let indicator = descendants(dashboard).compactMap { $0 as? NSProgressIndicator }.first!
            let meter = descendants(dashboard).first { $0.identifier?.rawValue == "index.embeddingProgress" }!
            precondition(indicator.isHidden && !meter.isHidden)
            try render(dashboard, name: name + "-embedding-progress", directory: directory)
            precondition(abs(meter.subviews.last!.frame.width - meter.bounds.width * 0.4) <= 1)
            let pause = descendants(dashboard).compactMap { $0 as? NSButton }.first { $0.identifier?.rawValue == "index.pause" }!
            pause.performClick(nil)
            precondition(actions.last?.0 == "pause")
            dashboard.update(try fixture("paused"), controls: controls(manual: true, policy: true, recovery: true))
            precondition(pause.title == "Resume indexing" && visibleText(dashboard).contains("Paused by you for this app session"))
            pause.performClick(nil)
            precondition(actions.last?.0 == "resume")
            dashboard.update(try fixture(), controls: controls(checking: true))
            let check = descendants(dashboard).compactMap { $0 as? NSButton }.first { $0.identifier?.rawValue == "index.checkModel" }!
            precondition(!check.isEnabled && check.title == "Checking…")
            let health = IndexControls.Health(available: true, model: "fixture-model", dimensions: 768, milliseconds: 12, message: "Available · matches active model name, revision and dimensions")
            dashboard.update(try fixture(), controls: controls(health: health))
            precondition(visibleText(dashboard).contains("768 dimensions · 12 ms"))
            check.performClick(nil)
            precondition(actions.last?.0 == "check")
            try render(dashboard, name: name + "-model-health", directory: directory)
            navigation.selectedSegment = 1
            _ = navigation.sendAction(navigation.action, to: navigation.target)
            let rescan = descendants(dashboard).compactMap { $0 as? NSButton }.first { $0.identifier?.rawValue == "index.rescanFolder" }!
            rescan.performClick(nil)
            precondition(actions.last?.0 == "folder" && actions.last?.1 == "/fixture/Documents")
            navigation.selectedSegment = 2
            _ = navigation.sendAction(navigation.action, to: navigation.target)
            let retry = descendants(dashboard).compactMap { $0 as? NSButton }.first { $0.identifier?.rawValue == "index.retry" }!
            retry.performClick(nil)
            precondition(actions.last?.0 == "retry")
            dashboard.update(try fixture("paused"), controls: controls(policy: true))
            precondition(!retry.isEnabled)
            dashboard.update(nil)
            precondition(!check.isEnabled && !pause.isEnabled)
            precondition(!visibleText(dashboard).contains("fixture-model"))
            cases += 7
        }
        print("PASS: \(cases) dashboard states across dark/light palettes; pinned navigation, settings links, measurable embedding progress, pause/resume/retry/folder/check actions, stale-state clearing, accessibility fallbacks, layout bounds; 18 PNG renders")
    }
}
