import AppKit

@main
@MainActor
enum PanelSmoke {
    static func main() async {
        NSApplication.shared.setActivationPolicy(.accessory)
        guard let core = Core() else { fatalError("Core initialization failed") }
        let pasteboard = NSPasteboard.withUniqueName()
        let watcher = ClipboardWatcher(sink: core.clipSink, pasteboard: pasteboard)
        let panel = Panel(core: core, watcher: watcher)
        panel.show()
        precondition(panel.isVisible)
        guard let field = panel.contentView?.subviews.compactMap({ $0 as? NSTextField }).first(where: { $0.isEditable }),
              let results = panel.contentView?.subviews.compactMap({ $0 as? ResultsView }).first,
              let editor = field.currentEditor() as? NSTextView else { fatalError("Panel not keyboard ready") }
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
        window.close()
        panel.standDown()
        panel.show()
        precondition(panel.isVisible && panel.query.isEmpty)
        panel.standDown()
        print("Panel show/reopen, 14 queries, command Tab completion, setting Return navigation, content erasure confirmation cancellation, bounded rows and arrow routing: passed")
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
