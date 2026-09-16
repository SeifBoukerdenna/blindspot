import AppKit

@main
@MainActor
enum PanelSmoke {
    static func main() async {
        NSApplication.shared.setActivationPolicy(.accessory)
        precondition(ProcessInfo.processInfo.environment["HOME"] == nil,
                     "Run with env -u HOME so this fixture cannot open live Blindspot stores")
        Theme.current = .ember
        guard let core = Core() else { fatalError("Core initialization failed") }
        let pasteboard = NSPasteboard.withUniqueName()
        let watcher = ClipboardWatcher(sink: core.clipSink, pasteboard: pasteboard)
        let panel = Panel(core: core, watcher: watcher)
        panel.show()
        precondition(panel.isVisible)
        guard let field = panel.contentView?.subviews.compactMap({ $0 as? NSTextField }).first(where: { $0.isEditable }),
              let results = panel.contentView?.subviews.compactMap({ $0 as? ResultsView }).first,
              let editor = field.currentEditor() as? NSTextView else { fatalError("Panel not keyboard ready") }
        precondition(panel.contentView is SurfaceView)
        guard let scope = panel.contentView?.subviews.compactMap({ $0 as? NSPopUpButton }).first else {
            fatalError("Search scope missing")
        }
        for (index, prefix) in [(0, ""), (1, "?"), (2, ";"), (3, ">"), (4, ":content ")] {
            scope.selectItem(at: index)
            _ = scope.sendAction(scope.action, to: scope.target)
            precondition(panel.query == prefix, "Scope must retain the original query prefixes")
            precondition(field.currentEditor() != nil, "Scope must return focus to search")
        }
        for query in ["sa", "documents", "?documents", "?kind:pdf size:>5MB", "?\"quarterly report\" modified:week", "?size:invalid", ";", ":3000", ":ports", ":processes", ":node", ":localhost", ">explain", ""] {
            field.stringValue = query
            panel.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
            precondition(panel.query == query)
            precondition(panel.control(field, textView: editor, doCommandBy: #selector(NSResponder.moveDown(_:))))
            precondition(panel.control(field, textView: editor, doCommandBy: #selector(NSResponder.moveUp(_:))))
            precondition(results.matches.count <= 50)
        }
        field.stringValue = ":po"
        panel.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
        precondition(results.selectedMatch?.kind == .command)
        precondition(panel.control(field, textView: editor, doCommandBy: #selector(NSResponder.insertTab(_:))))
        for _ in 0..<20 { await Task.yield() }
        precondition(panel.query == ":ports", "Tab completes a registered command")
        let settings = SettingsWindow(core: core)
        var opened: String?
        panel.onOpenSetting = { key in opened = key; settings.show(settingKey: key) }
        field.stringValue = ":settings agent.model"
        panel.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
        precondition(results.selectedMatch?.kind == .setting)
        precondition(panel.control(field, textView: editor, doCommandBy: #selector(NSResponder.insertNewline(_:))))
        for _ in 0..<20 { await Task.yield() }
        precondition(opened == "agent.model", "Return opens the selected schema key")
        guard let window = NSApplication.shared.windows.first(where: { $0.title == "blindspot" }),
              let content = window.contentView,
              let row = findRow(in: content, key: "agent.model") else { fatalError("Setting row not presented") }
        precondition(window.isVisible && !row.visibleRect.isEmpty)
        precondition(window.styleMask.contains(.resizable) && content is SurfaceView)
        for section in ["General", "Ranking", "Content", "Index", "Clipboard", "Agent", "Appearance", "Status"] {
            guard let button = findRow(in: content, key: "settings.section.\(section)") as? NSButton else {
                fatalError("Sidebar section missing: \(section)")
            }
            button.performClick(nil)
            content.layoutSubtreeIfNeeded()
            precondition(!content.hasAmbiguousLayout)
            snapshot(content, name: "settings-\(section.lowercased())")
        }
        let indexSection = findRow(in: content, key: "settings.section.Index") as! NSButton
        indexSection.performClick(nil)
        let navigation = findRow(in: content, key: "index.navigation") as! NSSegmentedControl
        navigation.selectedSegment = 1
        _ = navigation.sendAction(navigation.action, to: navigation.target)
        content.layoutSubtreeIfNeeded()
        snapshot(content, name: "settings-index-folders")
        (findRow(in: content, key: "index.manageFolders") as! NSButton).performClick(nil)
        content.layoutSubtreeIfNeeded()
        precondition(findRow(in: content, key: "content.roots")?.visibleRect.isEmpty == false,
                     "Manage folders must reveal the folder controls")
        indexSection.performClick(nil)
        (findRow(in: content, key: "index.settings") as! NSButton).performClick(nil)
        content.layoutSubtreeIfNeeded()
        precondition(findRow(in: content, key: "content.enabled")?.visibleRect.isEmpty == false,
                     "Indexing settings must reveal the indexing controls")
        settings.show(settingKey: "content.enabled")
        guard let eraseRow = findRow(in: content, key: "content.erase"),
              let erase = findButton(in: eraseRow, title: "Erase index") else { fatalError("Content erasure control missing") }
        let wasEnabled = core.contentState?.enabled
        precondition(core.contentState?.erasing == false)
        erase.performClick(nil)
        for _ in 0..<20 {
            if window.attachedSheet != nil { break }
            try? await Task.sleep(for: .milliseconds(25))
        }
        guard let sheet = window.attachedSheet,
              let sheetContent = sheet.contentView,
              let cancel = findButton(in: sheetContent, title: "Cancel") else { fatalError("Erasure confirmation missing") }
        cancel.performClick(nil)
        for _ in 0..<40 {
            if window.attachedSheet == nil { break }
            try? await Task.sleep(for: .milliseconds(25))
        }
        precondition(window.attachedSheet == nil)
        precondition(core.contentState?.enabled == wasEnabled && core.contentState?.erasing == false,
                     "Cancelling confirmation must not change content settings or erase data")
        settings.show(settingKey: "status.version")
        guard let updates = findRow(in: content, key: "updates"),
              findButton(in: updates, title: "Check for updates") != nil else { fatalError("Updates row missing from Status") }
        window.close()
        panel.standDown()
        panel.show()
        precondition(panel.isVisible && panel.query.isEmpty)
        field.stringValue = ":content weekend in Kyoto"
        panel.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
        scope.selectItem(at: 4)
        let documents: [(String, String)] = [
            ("Kyoto weekend itinerary.md", "A quiet morning in Higashiyama, followed by the Philosopher’s Path."),
            ("Places to eat.md", "Small cafés, late-night ramen, and the Nishiki market."),
            ("Train reservations.pdf", "Tokyo to Kyoto — arrival Friday evening."),
            ("Packing list.md", "Walking shoes, a light rain jacket, and a pocket notebook."),
        ]
        results.update(documents.enumerated().map { index, item in
            Match(id: UInt64(index), name: item.0, kind: .file, path: "/Travel/Japan/" + item.0,
                  score: 1, timestamp: 0, width: 0, height: 0, detail: item.1, highlights: [], page: index == 2 ? 2 : 0)
        })
        precondition(results.fittingHeight == 4 * (Theme.rowHeight + 20) + 12)
        let resultHeight = results.constraints.first { $0.firstAttribute == .height && $0.secondItem == nil }!
        resultHeight.constant = results.fittingHeight
        panel.setContentSize(NSSize(width: Theme.panelWidth, height: results.fittingHeight + Theme.fieldHeight + 39))
        panel.contentView?.layoutSubtreeIfNeeded()
        snapshot(panel.contentView!, name: "launcher-passages")
        results.update([Match(id: 1, name: "7", kind: .calc, path: "", score: 1,
                              timestamp: 0, width: 0, height: 0, detail: "", highlights: [])])
        panel.contentView?.layoutSubtreeIfNeeded()
        precondition(!visibleText(results).contains("Higashiyama"), "Pooled rows must clear old excerpts")
        panel.standDown()
        print("Panel show/reopen, 14 queries, command Tab completion, setting Return navigation, content erasure confirmation cancellation, Status updates row, bounded rows and arrow routing: passed")
        print("Five search scopes with keyboard focus, eight sidebar sections, Index folder/settings navigation, resizable native glass surfaces: passed; persistent stores disabled")
    }

    private static func snapshot(_ view: NSView, name: String) {
        let directory = URL(fileURLWithPath: "build/ui-03", isDirectory: true)
        try! FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        guard let bitmap = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { fatalError("No bitmap") }
        view.cacheDisplay(in: view.bounds, to: bitmap)
        try! bitmap.representation(using: .png, properties: [:])!.write(to: directory.appendingPathComponent(name + ".png"))
    }

    private static func visibleText(_ view: NSView) -> String {
        guard !view.isHiddenOrHasHiddenAncestor else { return "" }
        return ((view as? NSTextField)?.stringValue ?? "") + view.subviews.map(visibleText).joined()
    }

    private static func findRow(in view: NSView, key: String) -> NSView? {
        if view.identifier?.rawValue == key { return view }
        return view.subviews.lazy.compactMap { findRow(in: $0, key: key) }.first
    }

    private static func findButton(in view: NSView, title: String) -> NSButton? {
        if let button = view as? NSButton, button.title.caseInsensitiveCompare(title) == .orderedSame { return button }
        return view.subviews.lazy.compactMap { findButton(in: $0, title: title) }.first
    }
}
