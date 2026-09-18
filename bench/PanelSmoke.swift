import AppKit

@main
@MainActor
enum PanelSmoke {
    private static let appDelegate = SmokeAppDelegate()
    private static let manualFolder = ProcessInfo.processInfo.environment["BLINDSPOT_TEST_FOLDER"]

    static func main() {
        Task { @MainActor in
            await run()
            exit(0)
        }
        if manualFolder == nil {
            DispatchQueue.main.asyncAfter(deadline: .now() + 45) { fatalError("Panel smoke timed out") }
        }
        NSApplication.shared.run()
        fatalError("Application loop ended before the fixture completed")
    }

    static func run() async {
        NSApplication.shared.setActivationPolicy(.accessory)
        NSApplication.shared.delegate = appDelegate
        NSApplication.shared.mainMenu = MainMenu.make()
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
        if let start = manualFolder {
            precondition(start.hasPrefix("/") && !start.contains("\0"), "BLINDSPOT_TEST_FOLDER must be an absolute folder path")
            guard let folder = findRow(in: panel.contentView!, key: "documents.folder") as? DocumentScope else {
                fatalError("Document folder control missing")
            }
            field.stringValue = ":content "
            panel.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
            folder.roots = { [start] }
            folder.selectFolder(start)
            print("Manual folder chooser ready. Choose a folder or Cancel; no live index is loaded.")
            folder.onChoose?()
            print("Manual chooser closed. Inspect the folder in the launcher footer; Escape ends this fixture.")
            while panel.isVisible { try? await Task.sleep(for: .milliseconds(100)) }
            print("Manual folder fixture ended; automated assertions were not run.")
            return
        }
        for (index, prefix) in [(0, ""), (1, "?"), (2, ";"), (3, ">"), (4, ":content ")] {
            scope.selectItem(at: index)
            _ = scope.sendAction(scope.action, to: scope.target)
            precondition(panel.query == prefix, "Scope must retain the original query prefixes")
            precondition(field.currentEditor() != nil, "Scope must return focus to search")
        }
        guard let folder = findRow(in: panel.contentView!, key: "documents.folder") as? DocumentScope else {
            fatalError("Document folder control missing")
        }
        field.stringValue = ":content kind:md modified:week bicycle"
        panel.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
        folder.roots = { ["/fixture/Café's project", "/fixture/Second project"] }
        folder.selectFolder("/fixture/Café's project/Notes")
        precondition(!folder.isHidden && folder.folder == "/fixture/Café's project/Notes")
        precondition(panel.query == ":content kind:md modified:week bicycle" && field.currentEditor() != nil)
        field.stringValue += " maintenance"
        panel.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
        precondition(folder.folder == "/fixture/Café's project/Notes")
        scope.selectItem(at: 1)
        _ = scope.sendAction(scope.action, to: scope.target)
        precondition(folder.isHidden)
        scope.selectItem(at: 4)
        _ = scope.sendAction(scope.action, to: scope.target)
        precondition(!folder.isHidden && folder.folder == "/fixture/Café's project/Notes")
        cancelFolderChooser(attempts: 50)
        folder.selectItem(at: folder.numberOfItems - 1)
        precondition(folder.selectedItem?.tag == -1, "Choose subfolder command must be selected")
        _ = folder.sendAction(folder.action, to: folder.target)
        precondition(panel.isVisible && folder.folder == "/fixture/Café's project/Notes", "Cancelling the chooser must retain the scope and launcher")
        folder.selectItem(at: 0)
        _ = folder.sendAction(folder.action, to: folder.target)
        precondition(folder.folder == nil && folder.title == "All indexed folders")
        precondition(panel.query == ":content kind:md modified:week bicycle maintenance")
        let invalid = core.queryDocuments("used:week bicycle", folder: "/fixture", limit: 50)
        precondition(!invalid.pending && invalid.matches.first?.name.contains("used:") == true)
        precondition(core.queryDocuments("bicycle", folder: "/fixture", limit: 0).matches.isEmpty)
        for query in ["sa", "documents", "?documents", "?kind:pdf size:>5MB", "?\"quarterly report\" modified:week", "?size:invalid", ";", ":3000", ":ports", ":processes", ":node", ":localhost", ">explain", ""] {
            field.stringValue = query
            panel.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
            precondition(panel.query == query)
            precondition(panel.control(field, textView: editor, doCommandBy: #selector(NSResponder.moveDown(_:))))
            precondition(panel.control(field, textView: editor, doCommandBy: #selector(NSResponder.moveUp(_:))))
            precondition(results.matches.count <= 50)
        }
        for command in [":containers", ":docker", ":podman", "docker containers"] {
            field.stringValue = command
            panel.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
            precondition(results.selectedMatch?.kind == .command && results.selectedMatch?.path == ContainerRequest.command(command))
        }
        let browser = ContainerWindow.shared
        browser.show(discover: false)
        let browserWindow = browser.window!
        browserWindow.miniaturize(nil)
        for command in [":containers", ":docker", ":podman", "docker containers"] {
            browser.busy = true
            NSApp.deactivate()
            for _ in 0..<20 {
                if !NSApp.isActive { break }
                try? await Task.sleep(for: .milliseconds(25))
            }
            if NSApp.isActive { print("Inactive-app setup was not granted by macOS; continuing Return/visibility checks with the app active") }
            panel.show()
            field.stringValue = command
            panel.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
            sendReturn(to: panel)
            try? await Task.sleep(for: .milliseconds(150))
            precondition(!panel.isVisible && browserWindow.isVisible && !browserWindow.isMiniaturized,
                         "Return must present the container window, including after minimization")
            precondition(browserWindow.occlusionState.contains(.visible) && browserWindow.isOnActiveSpace,
                         "The container window must actually be displayed on the active Space")
            precondition(browserWindow.collectionBehavior.contains(.moveToActiveSpace))
            browserWindow.performClose(nil)
        }
        let commands = core.query(":help", limit: 50).matches.filter { $0.kind == .command }
        precondition(commands.count == 23)
        let historyRoot = FileManager.default.temporaryDirectory.appendingPathComponent("blindspot-history-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: historyRoot) }
        let history = CommandHistory(home: historyRoot.path)
        let invocations = commands.map(\.path)
        history.record(":docker", registered: invocations)
        history.record(":ports", registered: invocations)
        history.record(":docker", registered: invocations)
        history.record(":snippet secret private text", registered: invocations)
        let reloaded = CommandHistory(home: historyRoot.path)
        precondition(reloaded.recent == [":docker", ":ports"], "Persist only unique registered commands, never arguments")
        precondition(reloaded.ordered(commands).prefix(2).map(\.path) == [":docker", ":ports"])
        CommandHistory.shared.record(":docker", registered: invocations)
        panel.show()
        field.stringValue = ":"
        panel.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
        precondition(results.selectedMatch?.path == ":docker", "Colon selects the most recently used command")
        for command in commands where ContainerRequest.command(command.path) == nil {
            panel.show()
            results.update([command])
            sendReturn(to: panel)
            for _ in 0..<20 { await Task.yield() }
            precondition(panel.isVisible && panel.query == command.path,
                         "Return must navigate the registered command: \(command.path)")
        }
        panel.show()
        field.stringValue = ":por"
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
        sendReturn(to: panel)
        for _ in 0..<20 { await Task.yield() }
        precondition(opened == "agent.model", "Return opens the selected schema key")
        guard let window = NSApplication.shared.windows.first(where: { $0.title == "blindspot" }),
              let content = window.contentView,
              let row = findRow(in: content, key: "agent.model") else { fatalError("Setting row not presented") }
        precondition(window.isVisible && !row.visibleRect.isEmpty)
        precondition(window.collectionBehavior.contains(.moveToActiveSpace))
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
        navigation.selectedSegment = 2
        _ = navigation.sendAction(navigation.action, to: navigation.target)
        guard let inspector = findRow(in: content, key: "index.checkFile")?.superview?.superview as? FileInspectionView else {
            fatalError("File diagnostic control missing")
        }
        inspector.check(URL(fileURLWithPath: "/fixture/no-file.md"))
        for _ in 0..<40 {
            if visibleText(inspector).contains("Outside your indexed folders") { break }
            try? await Task.sleep(for: .milliseconds(25))
        }
        precondition(visibleText(inspector).contains("Outside your indexed folders"))
        snapshot(content, name: "settings-file-diagnostic")
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
        let closeEvent = NSEvent.keyEvent(
            with: .keyDown, location: .zero, modifierFlags: .command,
            timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber,
            context: nil, characters: "w", charactersIgnoringModifiers: "w", isARepeat: false, keyCode: 13)!
        precondition(window.performKeyEquivalent(with: closeEvent))
        for _ in 0..<40 {
            if !window.isVisible { break }
            try? await Task.sleep(for: .milliseconds(25))
        }
        precondition(!window.isVisible, "Command-W must close Settings")
        settings.show(settingKey: "agent.host")
        content.layoutSubtreeIfNeeded()
        guard let hostRow = findRow(in: content, key: "agent.host"),
              let hostField = editableField(in: hostRow) else { fatalError("Host text field missing") }
        precondition(window.isVisible && window.makeFirstResponder(hostField))
        precondition(hostField.currentEditor() != nil)
        precondition(window.performKeyEquivalent(with: closeEvent))
        for _ in 0..<40 {
            if !window.isVisible { break }
            try? await Task.sleep(for: .milliseconds(25))
        }
        precondition(!window.isVisible, "Command-W must also close Settings while editing text")
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
        folder.selectFolder("/fixture/Café's project/Research and planning notes")
        results.update(documents.enumerated().map { index, item in
            Match(id: UInt64(index), name: item.0, kind: .file,
                  path: "/fixture/Café's project/Research and planning notes/" + item.0,
                  score: 1, timestamp: 0, width: 0, height: 0, detail: item.1, highlights: [])
        })
        resultHeight.constant = results.fittingHeight
        panel.setContentSize(NSSize(width: Theme.panelWidth, height: results.fittingHeight + Theme.fieldHeight + 39))
        panel.contentView?.layoutSubtreeIfNeeded()
        precondition(!panel.contentView!.hasAmbiguousLayout)
        snapshot(panel.contentView!, name: "launcher-folder-scope")
        folder.selectFolder(nil)
        results.update([Match(id: 1, name: "7", kind: .calc, path: "", score: 1,
                              timestamp: 0, width: 0, height: 0, detail: "", highlights: [])])
        panel.contentView?.layoutSubtreeIfNeeded()
        precondition(!visibleText(results).contains("Higashiyama"), "Pooled rows must clear old excerpts")
        panel.standDown()
        await checkPassagePreview()
        await captureContrasts(core: core, watcher: watcher)
        print("Panel show/reopen, 14 queries, command Tab completion, setting Return navigation, content erasure confirmation cancellation, Status updates row, bounded rows and arrow routing: passed")
        print("Return key events: four container aliases, all 23 registered command routes and Settings; visible active-Space container window and minimized-window recovery: passed")
        print("Five search scopes with keyboard focus, eight sidebar sections, Index folder/settings navigation, resizable native glass surfaces: passed; persistent stores disabled")
        print("Settings Command-W close, reopen and close with text-field focus: passed")
        print("File diagnostic bridge and Settings presentation: passed")
        print("Passage preview navigation, Unicode highlighting, stale-result rejection and close: passed")
        print("Document folder selection, chooser cancellation, query/filter retention, mode isolation, clearing, focus, FFI validation and layout: passed")
    }

    private static func sendReturn(to window: NSWindow) {
        let event = NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: [],
            timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber,
            context: nil, characters: "\r", charactersIgnoringModifiers: "\r", isARepeat: false, keyCode: 36)!
        NSApp.sendEvent(event)
    }

    private static func captureContrasts(core: Core, watcher: ClipboardWatcher) async {
        guard ProcessInfo.processInfo.environment["BLINDSPOT_CAPTURE_CONTAINERS"] == "1" else { return }
        let original = Theme.current
        defer { Theme.current = original }
        let directory = URL(fileURLWithPath: "build/container-screens")
        try! FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        for palette in Palette.all {
            Theme.current = palette
            for (name, color) in [("bright", NSColor.white), ("dark", NSColor.black)] {
                let backdrop = NSWindow(contentRect: NSRect(x: 100, y: 100, width: 1000, height: 800),
                                        styleMask: [.borderless], backing: .buffered, defer: false)
                backdrop.isReleasedWhenClosed = false
                backdrop.backgroundColor = color
                let panel = Panel(core: core, watcher: watcher)
                panel.show()
                let field = panel.contentView!.subviews.compactMap { $0 as? NSTextField }.first { $0.isEditable }!
                field.stringValue = "harbor"
                panel.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
                let results = panel.contentView!.subviews.compactMap { $0 as? ResultsView }.first!
                results.update([
                    Match(id: 1, name: "Harbor project notes.md", kind: .file, path: "/Fixture/Projects/Harbor project notes.md", score: 1, timestamp: 0, width: 0, height: 0, detail: "", highlights: []),
                    Match(id: 2, name: "Harbor architecture.pdf", kind: .file, path: "/Fixture/Projects/Harbor architecture.pdf", score: 1, timestamp: 0, width: 0, height: 0, detail: "", highlights: []),
                    Match(id: 3, name: "Open local containers", kind: .command, path: ":containers", score: 1, timestamp: 0, width: 0, height: 0, detail: "Status, ports, recent logs and controls", highlights: []),
                ])
                results.constraints.first { $0.firstAttribute == .height && $0.secondItem == nil }!.constant = results.fittingHeight
                panel.setContentSize(NSSize(width: Theme.panelWidth, height: results.fittingHeight + Theme.fieldHeight + 39))
                backdrop.setFrame(panel.frame.insetBy(dx: -80, dy: -80), display: true)
                backdrop.orderFrontRegardless()
                panel.orderFrontRegardless()
                panel.contentView!.layoutSubtreeIfNeeded()
                try? await Task.sleep(for: .milliseconds(250))
                let capture = Process()
                capture.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
                capture.arguments = ["-x", "-o", "-l", String(panel.windowNumber), directory.appendingPathComponent("launcher-\(palette.name)-\(name).png").path]
                try! capture.run(); capture.waitUntilExit()
                precondition(capture.terminationStatus == 0)
                panel.standDown()
                backdrop.close()
            }
        }
    }

    private static func checkPassagePreview() async {
        let first = PassagePreview.Item(id: 1, path: "/fixture/cycling.md", title: "Cycling route", page: 0, line: 12)
        let second = PassagePreview.Item(id: 2, path: "/fixture/café.pdf", title: "Café stops", page: 3, line: 0)
        let preview = PassagePreview()
        var closed = 0
        preview.onClose = { closed += 1 }
        let text = "🚲 A café beside the canal. Repair your bicycle brakes before leaving."
        let ranges = PassagePreview.highlights(in: text, query: ":content kind:pdf CAFÉ bicycle")
        precondition(ranges.map { (text as NSString).substring(with: $0) } == ["café", "bicycle"])
        precondition(PassagePreview.highlights(in: "plain text", query: "a kind:pdf").isEmpty)
        precondition(preview.toggle(items: [first, second], selected: first, query: "bicycle café") { item in
            if item.id == 1 { Thread.sleep(forTimeInterval: 0.15); return "Old bicycle passage" }
            return text
        })
        preview.move(by: 1)
        guard let window = NSApp.windows.first(where: { $0.title == "Passage preview" && $0.isVisible }),
              let content = window.contentView,
              let body = findRow(in: content, key: "passage.text") as? NSTextView else { fatalError("Reading window missing") }
        for _ in 0..<40 {
            if body.string == text { break }
            try? await Task.sleep(for: .milliseconds(25))
        }
        try? await Task.sleep(for: .milliseconds(200))
        precondition(body.string == text, "Previous loader must not replace a newer result")
        precondition((findRow(in: content, key: "passage.next") as? NSButton)?.isEnabled == false)
        precondition(visibleText(content).contains("Page 3"))
        content.layoutSubtreeIfNeeded()
        precondition(!content.hasAmbiguousLayout)
        snapshot(content, name: "passage-preview")
        preview.close()
        precondition(!preview.isShowing && closed == 1 && body.string.isEmpty)
        precondition(preview.toggle(items: [first], selected: first, query: "route") { _ in nil })
        for _ in 0..<40 {
            if body.string.contains("no longer available") { break }
            try? await Task.sleep(for: .milliseconds(25))
        }
        precondition(body.string.contains("no longer available"))
        preview.close()
        precondition(closed == 2)
    }

    private static func cancelFolderChooser(attempts: Int) {
        let deadline = Date().addingTimeInterval(Double(attempts) * 0.1)
        let timer = Timer(timeInterval: 0.1, repeats: true) { timer in
            let finished = MainActor.assumeIsolated {
                if let picker = NSApp.windows.compactMap({ $0 as? NSOpenPanel }).first(where: \.isVisible) {
                    picker.cancel(nil)
                    return true
                } else if Date() >= deadline {
                    fatalError("Folder chooser did not open")
                }
                return false
            }
            if finished { timer.invalidate() }
        }
        RunLoop.main.add(timer, forMode: .modalPanel)
        RunLoop.main.add(timer, forMode: .default)
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

    private static func editableField(in view: NSView) -> NSTextField? {
        if let field = view as? NSTextField, field.isEditable { return field }
        return view.subviews.lazy.compactMap { editableField(in: $0) }.first
    }
}

@MainActor
private final class SmokeAppDelegate: NSObject, NSApplicationDelegate {
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        fatalError("Closing Settings must not terminate the app")
    }
}
