import AppKit

@MainActor
final class PassagePreview: NSObject, NSWindowDelegate {
    struct Item: Sendable, Equatable {
        let id: UInt64
        let path, title: String
        let page, line: Int

        var location: String {
            if page > 0 { return "\((path as NSString).pathExtension.lowercased() == "pptx" ? "Slide" : "Page") \(page)" }
            return line > 0 ? "Line \(line)" : "Indexed passage"
        }
    }

    var onClose: (() -> Void)?
    var onDocument: ((Item) -> Void)?
    private var panel: ReadingPanel?
    private var task: Task<Void, Never>?
    private var generation = UUID()
    private var items: [Item] = []
    private var selected = 0
    private var query = ""
    private var load: (@Sendable (Item) -> String?)?
    private let heading = NSTextField(labelWithString: "")
    private let location = NSTextField(labelWithString: "")
    private let position = NSTextField(labelWithString: "")
    private let note = NSTextField(wrappingLabelWithString: "Stored passage · highlights show literal query words, not semantic similarity.")
    private let text = NSTextView()
    private let previous = NSButton(title: "←", target: nil, action: nil)
    private let next = NSButton(title: "→", target: nil, action: nil)
    private let document = NSButton(title: "View document", target: nil, action: nil)

    var isShowing: Bool { panel?.isVisible == true }

    @discardableResult
    func toggle(items: [Item], selected: Item, query: String, load: @escaping @Sendable (Item) -> String?) -> Bool {
        if isShowing { panel?.close(); return false }
        self.items = Array(items.prefix(50))
        guard let index = self.items.firstIndex(of: selected) else { return false }
        self.selected = index
        self.query = String(query.prefix(4096))
        self.load = load
        let panel = ReadingPanel(contentRect: NSRect(x: 0, y: 0, width: 720, height: 590),
                                 styleMask: [.titled, .closable, .resizable, .fullSizeContentView], backing: .buffered, defer: false)
        panel.title = "Passage preview"
        panel.titleVisibility = .hidden
        panel.titlebarAppearsTransparent = true
        panel.isReleasedWhenClosed = false
        panel.minSize = NSSize(width: 560, height: 360)
        panel.appearance = NSAppearance(named: Theme.isDark ? .darkAqua : .aqua)
        panel.backgroundColor = .clear
        panel.isOpaque = false
        panel.delegate = self
        panel.move = { [weak self] offset in self?.move(by: offset) }
        self.panel = panel
        let surface = SurfaceView()
        panel.contentView = surface
        heading.font = Theme.text(20, .semibold)
        heading.textColor = Theme.ink
        heading.lineBreakMode = .byTruncatingMiddle
        location.font = Theme.text(12)
        location.textColor = Theme.muted
        location.lineBreakMode = .byTruncatingMiddle
        position.font = Theme.text(12)
        position.textColor = Theme.muted
        for (button, action, label) in [(previous, #selector(back), "Previous result, Command left bracket"),
                                         (next, #selector(forward), "Next result, Command right bracket"),
                                         (document, #selector(viewDocument), "View document")] {
            button.target = self
            button.action = action
            button.bezelStyle = .rounded
            button.setAccessibilityLabel(label)
        }
        previous.identifier = NSUserInterfaceItemIdentifier("passage.previous")
        next.identifier = NSUserInterfaceItemIdentifier("passage.next")
        document.identifier = NSUserInterfaceItemIdentifier("passage.document")
        let bar = NSStackView(views: [previous, position, next, NSView(), document])
        bar.spacing = 10
        bar.distribution = .fill
        text.isEditable = false
        text.isSelectable = true
        text.isRichText = false
        text.drawsBackground = false
        text.isHorizontallyResizable = false
        text.isVerticallyResizable = true
        text.autoresizingMask = [.width]
        text.textContainer?.widthTracksTextView = true
        text.textContainerInset = NSSize(width: 4, height: 12)
        text.identifier = NSUserInterfaceItemIdentifier("passage.text")
        text.setAccessibilityLabel("Indexed passage text")
        let scroll = NSScrollView()
        scroll.documentView = text
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.scrollerStyle = .overlay
        note.font = Theme.text(11)
        note.textColor = Theme.muted
        note.preferredMaxLayoutWidth = 650
        for view in [heading, location, bar, scroll, note] {
            view.translatesAutoresizingMaskIntoConstraints = false
            surface.addSubview(view)
            view.leadingAnchor.constraint(equalTo: surface.leadingAnchor, constant: 28).isActive = true
            view.trailingAnchor.constraint(equalTo: surface.trailingAnchor, constant: -28).isActive = true
        }
        NSLayoutConstraint.activate([
            heading.topAnchor.constraint(equalTo: surface.topAnchor, constant: 44),
            location.topAnchor.constraint(equalTo: heading.bottomAnchor, constant: 6),
            bar.topAnchor.constraint(equalTo: location.bottomAnchor, constant: 18),
            scroll.topAnchor.constraint(equalTo: bar.bottomAnchor, constant: 14),
            scroll.bottomAnchor.constraint(equalTo: note.topAnchor, constant: -12),
            note.bottomAnchor.constraint(equalTo: surface.bottomAnchor, constant: -20),
        ])
        panel.center()
        panel.makeKeyAndOrderFront(nil)
        showSelected()
        return true
    }

    private func showSelected() {
        guard items.indices.contains(selected), let load else { return }
        task?.cancel()
        generation = UUID()
        let expected = generation, item = items[selected]
        heading.stringValue = item.title
        heading.toolTip = item.title
        location.stringValue = item.location + " · " + (item.path as NSString).abbreviatingWithTildeInPath
        location.toolTip = item.path
        position.stringValue = "\(selected + 1) of \(items.count) results"
        previous.isEnabled = selected > 0
        next.isEnabled = selected + 1 < items.count
        text.string = "Loading indexed passage…"
        document.isEnabled = false
        task = Task { [weak self] in
            let worker = Task.detached(priority: .userInitiated) { load(item) }
            let body = await withTaskCancellationHandler { await worker.value } onCancel: { worker.cancel() }
            guard !Task.isCancelled, let self, self.generation == expected, self.isShowing else { return }
            self.document.isEnabled = true
            guard let body, body.utf8.count <= 8192 else {
                self.text.string = "This passage changed or is no longer available in the current index. Search again, or view the original document."
                return
            }
            let paragraph = NSMutableParagraphStyle()
            paragraph.lineSpacing = 6
            paragraph.paragraphSpacing = 12
            let styled = NSMutableAttributedString(string: body, attributes: [
                .font: Theme.text(15), .foregroundColor: Theme.ink, .paragraphStyle: paragraph,
            ])
            let ranges = Self.highlights(in: body, query: self.query)
            for range in ranges { styled.addAttribute(.backgroundColor, value: Theme.accent.withAlphaComponent(0.22), range: range) }
            self.text.textStorage?.setAttributedString(styled)
            self.text.setSelectedRange(NSRange(location: 0, length: 0))
            self.text.scrollRangeToVisible(ranges.first ?? NSRange(location: 0, length: 0))
        }
    }

    static func highlights(in text: String, query: String) -> [NSRange] {
        let ignored: Set<String> = ["documents", "about", "content", "docs", "find", "files", "the", "and", "for"]
        let components = query.split(whereSeparator: { $0.isWhitespace }).filter { !$0.contains(":") }
        var words: [String] = []
        for component in components {
            for part in component.split(whereSeparator: { !$0.isLetter && !$0.isNumber }) {
                let word = part.lowercased()
                if word.count >= 2 && word.count <= 64 && !ignored.contains(word) { words.append(word) }
            }
        }
        let source = text as NSString
        var ranges: [NSRange] = []
        for word in Set(words.prefix(32)).sorted() {
            var remaining = NSRange(location: 0, length: source.length)
            while remaining.length > 0 && ranges.count < 128 {
                let range = source.range(of: word, options: [.caseInsensitive, .diacriticInsensitive], range: remaining)
                if range.location == NSNotFound { break }
                ranges.append(range)
                let end = NSMaxRange(range)
                remaining = NSRange(location: end, length: source.length - end)
            }
        }
        return ranges.sorted { $0.location < $1.location }
    }

    func move(by offset: Int) {
        let index = selected + offset
        guard items.indices.contains(index), index != selected else { return }
        selected = index
        showSelected()
    }

    @objc private func back() { move(by: -1) }
    @objc private func forward() { move(by: 1) }
    @objc private func viewDocument() {
        guard items.indices.contains(selected) else { return }
        let item = items[selected]
        panel?.close()
        onDocument?(item)
    }

    func close() { panel?.close() }

    func windowWillClose(_ notification: Notification) {
        guard notification.object as? NSWindow === panel else { return }
        generation = UUID()
        task?.cancel()
        task = nil
        panel = nil
        items = []
        load = nil
        text.string = ""
        onClose?()
    }

    isolated deinit { task?.cancel() }
}

@MainActor
private final class ReadingPanel: NSPanel {
    var move: ((Int) -> Void)?
    override func cancelOperation(_ sender: Any?) { close() }
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask).subtracting([.capsLock, .numericPad, .function])
        if flags == .command {
            switch event.charactersIgnoringModifiers {
            case "y", "w": close(); return true
            case "[": move?(-1); return true
            case "]": move?(1); return true
            default: break
            }
        }
        return super.performKeyEquivalent(with: event)
    }
}
