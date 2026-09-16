import AppKit

@main
@MainActor
enum IndexDashboardTests {
    static func fixture(_ state: String = "ready", enabled: Bool = true, sampled: Bool = true,
                        empty: Bool = false, failed: UInt64 = 0) throws -> IndexOverview {
        let object: [String: Any] = [
            "state": state, "stage": state == "indexing" ? "Embedding passages" : "",
            "message": state == "failed" ? "Semantic unavailable; text search ready"
                : state == "indexing" ? "Embedding · 420 written · 18,004 current"
                : "Semantic ready · embeddinggemma:300m",
            "stageSeconds": 38, "lastPassAgo": 42, "lastPassSeconds": 12.4,
            "currentFolder": "~/Documents/Research", "reclaimable": 32 * 1_048_576,
            "pass": ["checked": 1250, "updated": 84, "sourceBytes": 8_400_000,
                     "unchanged": 1128, "skipped": 36, "unreadable": 2, "removed": 3],
            "roots": ["~/Documents", "~/Projects"],
            "semantic": ["enabled": enabled, "embedded": empty ? 0 : 18424,
                         "eligible": empty ? 0 : 24180, "passEmbedded": 420, "passFailed": failed],
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
        let window = NSWindow(contentRect: plate.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.appearance = NSAppearance(named: Theme.isDark ? .darkAqua : .aqua)
        window.contentView = plate
        plate.layoutSubtreeIfNeeded()
        guard let bitmap = plate.bitmapImageRepForCachingDisplay(in: plate.bounds) else { fatalError("No bitmap") }
        plate.cacheDisplay(in: plate.bounds, to: bitmap)
        guard let png = bitmap.representation(using: .png, properties: [:]) else { fatalError("No PNG") }
        try png.write(to: directory.appendingPathComponent(name + ".png"))
        precondition(!dashboard.hasAmbiguousLayout, "Dashboard layout is ambiguous")
        for field in descendants(dashboard).compactMap({ $0 as? NSTextField }) {
            let rect = field.convert(field.bounds, to: dashboard)
            precondition(rect.minX >= -1 && rect.maxX <= 641, "Text outside dashboard: \(field.stringValue)")
        }
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
            let dashboard = IndexDashboardView()
            dashboard.widthAnchor.constraint(equalToConstant: 640).isActive = true
            var refreshes = 0
            var compactions = 0
            dashboard.onRefresh = { refreshes += 1 }
            dashboard.onCompact = { compactions += 1 }
            let refresh = descendants(dashboard).compactMap { $0 as? NSButton }.first { $0.identifier?.rawValue == "index.refresh" }!
            let compact = descendants(dashboard).compactMap { $0 as? NSButton }.first { $0.identifier?.rawValue == "index.compact" }!
            precondition(!refresh.isEnabled && !compact.isEnabled)
            for state in ["ready", "indexing", "paused", "partial", "failed", "erasing", "compacting", "erased", "needsRoots", "off"] {
                dashboard.update(try fixture(state, failed: state == "partial" ? 7 : 0))
                let labels = text(dashboard)
                precondition(labels.contains("Coverage of stored passages, not a work queue"))
                precondition(labels.contains("Database") && labels.contains("Vector cache"))
                if state == "partial" { precondition(labels.contains("7 passages could not") && labels.contains("3 documents are partly indexed")) }
                if state == "failed" { precondition(labels.contains("Semantic unavailable; text search ready")) }
                if ["indexing", "erasing", "compacting", "erased"].contains(state) { precondition(!compact.isEnabled) }
                if ["ready", "indexing", "partial"].contains(state) { try render(dashboard, name: name + "-" + state, directory: directory) }
                cases += 1
            }
            dashboard.update(try fixture())
            refresh.performClick(nil)
            compact.performClick(nil)
            precondition(refreshes == 1 && compactions == 1)
            dashboard.update(try fixture(enabled: false))
            precondition(text(dashboard).contains("Search by meaning is off"))
            dashboard.update(try fixture(empty: true))
            precondition(text(dashboard).contains("No stored passages yet"))
            dashboard.update(try fixture(sampled: false))
            precondition(text(dashboard).contains("Measuring passage coverage"))
            dashboard.update(nil)
            precondition(!text(dashboard).contains("18,424") && !text(dashboard).contains("embeddinggemma"))
            precondition(!refresh.isEnabled && !compact.isEnabled)
            dashboard.update(try fixture())
            precondition(text(dashboard).contains("embeddinggemma"))
            cases += 5
        }
        print("PASS: \(cases) dashboard states across dark/light palettes; action callbacks, stale-state clearing, layout bounds; 6 PNG renders")
    }
}
