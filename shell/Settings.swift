import AppKit
import Carbon.HIToolbox

@MainActor
private final class SettingsFrame: NSWindow {
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
            .subtracting([.capsLock, .numericPad, .function])
        if flags == .command, event.charactersIgnoringModifiers == "w" {
            guard attachedSheet == nil else { return false }
            performClose(nil)
            return true
        }
        return super.performKeyEquivalent(with: event)
    }
}

/// A stack that lays out from the top.
///
/// An `NSScrollView`'s document view has a bottom-left origin unless it says otherwise, so
/// an unflipped stack draws its first row at the bottom and opens showing the end of the
/// list. Measured the obvious way: the Agent page opened on "Roots".
@MainActor
private final class TopStack: NSStackView {
    override var isFlipped: Bool { true }
}

@MainActor
private final class SettingsSidebar: NSView {
    private var buttons: [NSButton] = []
    private(set) var selected = 0
    var onSelect: ((Int) -> Void)?

    static func title(for section: String) -> String {
        switch section {
        case "Launcher": "General"
        case "Ranking": "Search"
        case "Agent": "AI"
        case "Status": "About"
        default: section
        }
    }

    init(titles: [String]) {
        super.init(frame: .zero)
        translatesAutoresizingMaskIntoConstraints = false
        let brand = NSTextField(labelWithString: "Blindspot")
        brand.font = Theme.text(20)
        brand.textColor = Theme.inkSoft
        let stack = NSStackView(views: [brand])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 5
        stack.setCustomSpacing(22, after: brand)
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        for (index, section) in titles.enumerated() {
            let button = NSButton(title: Self.title(for: section), target: self, action: #selector(changed(_:)))
            button.tag = index
            button.identifier = NSUserInterfaceItemIdentifier("settings.section.\(section)")
            button.font = Theme.text(13)
            button.alignment = .left
            button.isBordered = false
            let symbol: String = switch section {
            case "General": "gearshape"
            case "Ranking": "magnifyingglass"
            case "Content": "doc.text"
            case "Index": "externaldrive"
            case "Clipboard": "doc.on.clipboard"
            case "Agent": "brain"
            case "Appearance": "paintbrush"
            default: "info.circle"
            }
            button.image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil)?
                .withSymbolConfiguration(.init(pointSize: 14, weight: .regular))
            button.imagePosition = .imageLeading
            button.wantsLayer = true
            button.layer?.cornerRadius = 7
            button.setAccessibilityLabel(Self.title(for: section))
            button.widthAnchor.constraint(equalToConstant: 148).isActive = true
            button.heightAnchor.constraint(equalToConstant: 34).isActive = true
            buttons.append(button)
            stack.addArrangedSubview(button)
        }
        let footer = NSTextField(labelWithString: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "")
        footer.font = Theme.text(11)
        footer.textColor = Theme.muted
        footer.translatesAutoresizingMaskIntoConstraints = false
        addSubview(footer)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: topAnchor),
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 16),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -16),
            stack.bottomAnchor.constraint(lessThanOrEqualTo: footer.topAnchor, constant: -20),
            footer.leadingAnchor.constraint(equalTo: stack.leadingAnchor),
            footer.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
        select(0)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    func select(_ index: Int) {
        selected = index
        for button in buttons {
            let current = button.tag == index
            button.layer?.backgroundColor = current ? Theme.selection.cgColor : NSColor.clear.cgColor
            button.contentTintColor = current ? Theme.ink : Theme.muted
            button.setAccessibilityValue(current ? "Selected" : "")
        }
    }

    func show(status: String) {}
    @objc private func changed(_ sender: NSButton) { onSelect?(sender.tag) }
}

/// A switch, in the plate's own language.
///
/// Not `NSSwitch`: its on state is the system accent, and this design spends exactly one
/// colour on exactly one thing per screen.
@MainActor
private final class Toggle: NSSwitch {
    var onChange: ((Bool) -> Bool)?

    init(on: Bool) {
        super.init(frame: .zero)
        state = on ? .on : .off
        target = self
        action = #selector(changed)
        translatesAutoresizingMaskIntoConstraints = false
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    @objc private func changed() {
        if onChange?(state == .on) == false { state = state == .on ? .off : .on }
    }
}

/// The hotkey recorder.
///
/// Click it and it swallows the next chord. What it sends back is a Carbon key code and
/// mask — the only part of this that is genuinely shell-side, since Carbon and AppKit
/// modifier bits share no positions — and the core spells it. The window therefore never
/// knows how a chord is written, and cannot drift from the parser config.toml is read with.
@MainActor
private final class ChordWell: NSButton {
    private let label = NSTextField(labelWithString: "")
    private let plate = PlateView(fill: Theme.ink.withAlphaComponent(0.04), radius: 6, border: Theme.hairline)
    private var monitor: Any?
    private var resting: String

    /// A captured chord, as Carbon reckons it.
    var onChord: ((UInt32, UInt32) -> Void)?
    /// ⌫ while armed: no hotkey at all, which is a legitimate answer for the agent's.
    var onClear: (() -> Void)?

    init(_ value: String) {
        resting = value
        super.init(frame: .zero)
        title = ""
        isBordered = false
        target = self
        action = #selector(arm)
        setAccessibilityLabel("Record keyboard shortcut")
        setAccessibilityValue(value.isEmpty ? "None" : value)
        translatesAutoresizingMaskIntoConstraints = false
        label.alignment = .right
        label.translatesAutoresizingMaskIntoConstraints = false
        plate.addSubview(label)
        addSubview(plate)
        NSLayoutConstraint.activate([
            plate.leadingAnchor.constraint(equalTo: leadingAnchor),
            plate.trailingAnchor.constraint(equalTo: trailingAnchor),
            plate.topAnchor.constraint(equalTo: topAnchor),
            plate.bottomAnchor.constraint(equalTo: bottomAnchor),
            widthAnchor.constraint(equalToConstant: 180),
            label.leadingAnchor.constraint(equalTo: plate.leadingAnchor, constant: 8),
            label.trailingAnchor.constraint(equalTo: plate.trailingAnchor, constant: -8),
            label.topAnchor.constraint(equalTo: plate.topAnchor, constant: 5),
            label.bottomAnchor.constraint(equalTo: plate.bottomAnchor, constant: -5),
        ])
        rest()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    /// `isolated` so this runs on the main actor and may read `monitor`, which is `Any?`
    /// and therefore not `Sendable` — the same reason `HotKey`'s deinit is isolated.
    /// Removing the monitor matters: one left armed would keep swallowing every keystroke
    /// in the app.
    isolated deinit {
        if let monitor { NSEvent.removeMonitor(monitor) }
    }

    private func rest() {
        let empty = resting.trimmingCharacters(in: .whitespaces).isEmpty
        label.attributedStringValue = NSAttributedString(
            string: empty ? "Not set" : resting.split(separator: "+").map {
                switch $0.lowercased() {
                case "cmd", "command": "⌘"
                case "shift": "⇧"
                case "alt", "option": "⌥"
                case "ctrl", "control": "⌃"
                case "space": "Space"
                default: String($0).uppercased()
                }
            }.joined(separator: " "),
            attributes: [
                .font: Theme.text(13),
                .foregroundColor: empty ? Theme.faint : Theme.ink,
            ])
        plate.fill(Theme.ink.withAlphaComponent(0.04))
    }

    @objc private func arm() {
        guard monitor == nil else { return disarm() }
        label.attributedStringValue = Theme.label(
            "PRESS A CHORD · ⎋ CANCEL", size: 9.5, tracking: 0.1, color: Theme.accent)
        plate.fill(Theme.accent.withAlphaComponent(0.12))
        // Local, not global: this needs no Accessibility grant, which is the same reason
        // the hotkey itself is Carbon rather than a `CGEventTap`. Returning nil swallows
        // the key so the chord being recorded cannot also act on the window.
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown]) { [weak self] event in
            self?.capture(event)
            return nil
        }
    }

    private func capture(_ event: NSEvent) {
        defer { disarm() }
        if event.keyCode == UInt16(kVK_Escape) { return }
        if event.keyCode == UInt16(kVK_Delete) {
            onClear?()
            return
        }
        let flags = event.modifierFlags
        var carbon: UInt32 = 0
        // Carbon masks, not `NSEvent.ModifierFlags`: the two share no bit positions, and a
        // raw AppKit value handed to Carbon is silently a different chord.
        if flags.contains(.command) { carbon |= UInt32(cmdKey) }
        if flags.contains(.shift) { carbon |= UInt32(shiftKey) }
        if flags.contains(.option) { carbon |= UInt32(optionKey) }
        if flags.contains(.control) { carbon |= UInt32(controlKey) }
        onChord?(UInt32(event.keyCode), carbon)
    }

    private func disarm() {
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
        rest()
    }
}

/// One row of the settings window: what the knob is, what it holds, and where that came
/// from.
///
/// Two columns. The left names the knob and explains it — and, when a write is refused,
/// says why in place of the explanation. The right carries the control and, under it, the
/// one thing a layered config has to answer: whether this value is the built-in, your
/// config.toml, or something this window set.
@MainActor
private final class PathListControl: NSView, NSTableViewDataSource, NSTableViewDelegate {
    private var items: [String]
    private let commit: ([String]) -> Void
    private let table = NSTableView()
    private let remove = NSButton(title: "Remove selected", target: nil, action: nil)

    init(items: [String], commit: @escaping ([String]) -> Void) {
        self.items = items
        self.commit = commit
        super.init(frame: .zero)
        translatesAutoresizingMaskIntoConstraints = false
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("folder"))
        column.width = 240
        table.addTableColumn(column)
        table.columnAutoresizingStyle = .lastColumnOnlyAutoresizingStyle
        table.autoresizingMask = [.width]
        table.headerView = nil
        table.rowHeight = 22
        table.intercellSpacing = NSSize(width: 0, height: 1)
        table.backgroundColor = .clear
        table.selectionHighlightStyle = .regular
        table.dataSource = self
        table.delegate = self
        table.translatesAutoresizingMaskIntoConstraints = false
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = false
        scroll.borderType = .bezelBorder
        scroll.documentView = table
        scroll.translatesAutoresizingMaskIntoConstraints = false

        let plate = PlateView(fill: Theme.ground, radius: 3, border: Theme.hairline)
        plate.translatesAutoresizingMaskIntoConstraints = false
        plate.addSubview(scroll)
        let add = NSButton(title: "Add folder…", target: self, action: #selector(addFolder))
        add.bezelStyle = .rounded
        add.controlSize = .small
        add.translatesAutoresizingMaskIntoConstraints = false
        remove.bezelStyle = .rounded
        remove.controlSize = .small
        remove.target = self
        remove.action = #selector(removeFolder)
        remove.isEnabled = false
        remove.translatesAutoresizingMaskIntoConstraints = false

        addSubview(plate)
        addSubview(add)
        addSubview(remove)
        NSLayoutConstraint.activate([
            plate.leadingAnchor.constraint(equalTo: leadingAnchor),
            plate.trailingAnchor.constraint(equalTo: trailingAnchor),
            plate.topAnchor.constraint(equalTo: topAnchor),
            scroll.leadingAnchor.constraint(equalTo: plate.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: plate.trailingAnchor),
            scroll.topAnchor.constraint(equalTo: plate.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: plate.bottomAnchor),
            plate.heightAnchor.constraint(equalToConstant: CGFloat(min(max(items.count, 1), 5)) * 23 + 2),
            remove.leadingAnchor.constraint(equalTo: leadingAnchor),
            remove.topAnchor.constraint(equalTo: plate.bottomAnchor, constant: 6),
            add.trailingAnchor.constraint(equalTo: trailingAnchor),
            add.topAnchor.constraint(equalTo: plate.bottomAnchor, constant: 6),
            remove.bottomAnchor.constraint(equalTo: bottomAnchor),
            add.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    @objc private func addFolder() {
        let panel = NSOpenPanel()
        panel.title = "Choose folders to index"
        panel.message = "Select one or more local folders"
        panel.prompt = "Add folders"
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = true
        panel.directoryURL = URL(fileURLWithPath: NSHomeDirectory())
        guard panel.runModal() == .OK else { return }
        let additions = panel.urls.map { $0.standardizedFileURL.path }
        var merged = items
        for path in additions where !merged.contains(path) && merged.count < 32 { merged.append(path) }
        guard merged != items else { return }
        items = merged
        table.reloadData()
        commit(merged)
    }

    @objc private func removeFolder() {
        let row = table.selectedRow
        guard items.indices.contains(row) else { return }
        items.remove(at: row)
        table.reloadData()
        remove.isEnabled = false
        commit(items)
    }

    func numberOfRows(in tableView: NSTableView) -> Int { items.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let cell = NSTableCellView()
        let label = NSTextField(labelWithString: (items[row] as NSString).abbreviatingWithTildeInPath)
        label.font = Theme.mono(11)
        label.textColor = Theme.inkSoft
        label.lineBreakMode = .byTruncatingMiddle
        label.toolTip = items[row]
        label.translatesAutoresizingMaskIntoConstraints = false
        cell.addSubview(label)
        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 6),
            label.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -6),
            label.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])
        return cell
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        remove.isEnabled = items.indices.contains(table.selectedRow)
    }
}

@MainActor
private final class ModelPickerControl: NSView, URLSessionDataDelegate, URLSessionTaskDelegate {
    private final class RefreshMarker: NSObject {}
    private final class AutomaticMarker: NSObject {}

    private static let maximumResponseBytes = 1 << 20
    private static let maximumModels = 256
    private let popup = NSPopUpButton()
    private let configured: String
    private let host: String
    private let allowsAutomatic: Bool
    private let commit: (String) -> Void
    private var loading = false
    private var availableModels = Set<String>()
    private var session: URLSession?
    private var task: URLSessionDataTask?
    private var response = Data()
    private var responseError: String?

    init(configured: String, host: String, allowsAutomatic: Bool, commit: @escaping (String) -> Void) {
        self.configured = configured
        self.host = host
        self.allowsAutomatic = allowsAutomatic
        self.commit = commit
        super.init(frame: .zero)
        translatesAutoresizingMaskIntoConstraints = false
        popup.controlSize = .small
        popup.alignment = .right
        popup.addItem(withTitle: "Loading local Ollama models…")
        popup.lastItem?.isEnabled = false
        popup.target = self
        popup.action = #selector(changed)
        popup.translatesAutoresizingMaskIntoConstraints = false
        addSubview(popup)
        NSLayoutConstraint.activate([
            popup.leadingAnchor.constraint(equalTo: leadingAnchor),
            popup.trailingAnchor.constraint(equalTo: trailingAnchor),
            popup.topAnchor.constraint(equalTo: topAnchor),
            popup.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
        // Start loading when the row appears. Waiting for the user to select the one visible
        // item made a configured model look like a non-functional picker.
        Task { @MainActor [weak self] in self?.reloadModels() }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    isolated deinit {
        task?.cancel()
        session?.invalidateAndCancel()
    }

    /// The core validates this setting too. The UI still uses a literal loopback address so
    /// URLSession never consults DNS (including a modified `localhost` hosts entry).
    private func tagsURL() -> URL? {
        let raw = host.trimmingCharacters(in: .whitespacesAndNewlines)
        let endpoint: String
        if let port = Self.suffix(after: "localhost:", in: raw) ?? Self.suffix(after: "127.0.0.1:", in: raw),
           Self.port(port) != nil {
            endpoint = "127.0.0.1:" + port
        } else if let port = Self.suffix(after: "[::1]:", in: raw), Self.port(port) != nil {
            endpoint = "[::1]:" + port
        } else {
            return nil
        }
        return URL(string: "http://" + endpoint + "/api/tags")
    }

    private static func port(_ value: String) -> UInt16? {
        guard !value.isEmpty, value.allSatisfy({ $0.isNumber }), let port = UInt16(value), port > 0 else {
            return nil
        }
        return port
    }

    private static func suffix(after prefix: String, in value: String) -> String? {
        guard value.hasPrefix(prefix) else { return nil }
        return String(value.dropFirst(prefix.count))
    }

    private func reloadModels() {
        guard !loading else { return }
        loading = true
        task?.cancel()
        session?.invalidateAndCancel()
        response.removeAll(keepingCapacity: true)
        responseError = nil
        popup.removeAllItems()
        popup.addItem(withTitle: "Loading Ollama models…")
        popup.lastItem?.isEnabled = false
        popup.isEnabled = false
        guard let url = tagsURL() else {
            finish([], error: "Configured Ollama host is not a loopback address")
            return
        }
        var request = URLRequest(url: url)
        request.timeoutInterval = 2
        let configuration = URLSessionConfiguration.ephemeral
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        configuration.urlCache = nil
        configuration.timeoutIntervalForRequest = 2
        configuration.timeoutIntervalForResource = 3
        let session = URLSession(configuration: configuration, delegate: self, delegateQueue: .main)
        self.session = session
        let task = session.dataTask(with: request)
        self.task = task
        task.resume()
    }

    private func finish(_ names: [String], error: String? = nil) {
        loading = false
        task = nil
        session?.finishTasksAndInvalidate()
        session = nil
        availableModels = Set(names)
        popup.removeAllItems()
        if allowsAutomatic {
            popup.addItem(withTitle: "Automatic")
            popup.lastItem?.representedObject = AutomaticMarker()
        }
        if !configured.isEmpty && !names.contains(configured) {
            popup.addItem(withTitle: configured + " · unavailable")
            popup.lastItem?.isEnabled = false
        } else if !configured.isEmpty {
            addModel(configured)
        }
        for name in names where name != configured { addModel(name) }
        if names.isEmpty {
            popup.addItem(withTitle: error ?? "No local Ollama models found")
            popup.lastItem?.isEnabled = false
        }
        popup.menu?.addItem(.separator())
        popup.addItem(withTitle: "Refresh local models")
        popup.lastItem?.representedObject = RefreshMarker()
        if configured.isEmpty, allowsAutomatic {
            popup.selectItem(at: 0)
        } else if let index = popup.itemArray.firstIndex(where: { ($0.representedObject as? String) == configured }) {
            popup.selectItem(at: index)
        } else {
            popup.selectItem(at: 0)
        }
        popup.isEnabled = true
    }

    private func addModel(_ name: String) {
        popup.addItem(withTitle: name)
        popup.lastItem?.representedObject = name
    }

    @objc private func changed() {
        guard let item = popup.selectedItem else { return }
        if item.representedObject is RefreshMarker {
            reloadModels()
        } else if item.representedObject is AutomaticMarker {
            commit("")
        } else if let model = item.representedObject as? String, availableModels.contains(model) {
            commit(model)
        }
    }

    nonisolated func urlSession(
        _ session: URLSession, task: URLSessionTask,
        willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
        completionHandler: @escaping @Sendable (URLRequest?) -> Void
    ) {
        // A local tag query never needs a redirect. Refusing it prevents a local service from
        // turning this picker into an outbound request.
        completionHandler(nil)
    }

    nonisolated func urlSession(
        _ session: URLSession, dataTask: URLSessionDataTask,
        didReceive response: URLResponse,
        completionHandler: @escaping @Sendable (URLSession.ResponseDisposition) -> Void
    ) {
        let allowed = MainActor.assumeIsolated {
            guard session === self.session,
                  let http = response as? HTTPURLResponse,
                  http.statusCode == 200,
                  http.url?.scheme == "http",
                  http.expectedContentLength <= Int64(Self.maximumResponseBytes) || http.expectedContentLength == NSURLSessionTransferSizeUnknown else {
                self.responseError = "Ollama did not return a usable model list"
                return false
            }
            return true
        }
        completionHandler(allowed ? .allow : .cancel)
    }

    nonisolated func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        MainActor.assumeIsolated {
            guard session === self.session else { return }
            guard self.response.count <= Self.maximumResponseBytes - data.count else {
                self.responseError = "Ollama model list is too large"
                dataTask.cancel()
                return
            }
            self.response.append(data)
        }
    }

    nonisolated func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        MainActor.assumeIsolated {
            guard session === self.session else { return }
            defer { self.response.removeAll(keepingCapacity: true) }
            guard error == nil, self.responseError == nil,
                  let object = try? JSONSerialization.jsonObject(with: self.response) as? [String: Any],
                  let models = object["models"] as? [[String: Any]] else {
                self.finish([], error: self.responseError ?? "Ollama is unavailable — refresh to retry")
                return
            }
            let validNames: [String] = models.compactMap { model in model["name"] as? String }
                .filter { name in
                    !name.isEmpty && name.utf8.count <= 256
                        && !name.unicodeScalars.contains(where: { scalar in CharacterSet.controlCharacters.contains(scalar) })
                }
            self.finish(Array(Set(validNames).sorted().prefix(Self.maximumModels)))
        }
    }
}

@MainActor
private final class SettingRow: NSView, NSTextFieldDelegate, NSTextViewDelegate {
    private static let controlWidth: CGFloat = 240

    private let setting: Setting
    /// Returns the reason a value was refused, or nil if it took.
    private let apply: (String) -> String?
    private let forget: () -> Void
    private let spell: (UInt32, UInt32) -> String?
    private let modelHost: String

    private let separator = PlateView(fill: Theme.hairline)
    private let label = NSTextField(labelWithString: "")
    private let help = NSTextField(labelWithString: "")
    private let origin = NSButton()

    init(
        _ setting: Setting,
        separated: Bool,
        apply: @escaping (String) -> String?,
        forget: @escaping () -> Void,
        spell: @escaping (UInt32, UInt32) -> String?,
        modelHost: String
    ) {
        self.setting = setting
        self.apply = apply
        self.forget = forget
        self.spell = spell
        self.modelHost = modelHost
        super.init(frame: .zero)
        translatesAutoresizingMaskIntoConstraints = false
        separator.isHidden = !separated

        label.attributedStringValue = NSAttributedString(
            string: setting.key == "hotkey" ? "Open Blindspot" : setting.key == "agent_hotkey" ? "Open AI" : setting.label,
            attributes: [.font: Theme.text(13, .medium), .foregroundColor: Theme.ink])
        help.lineBreakMode = .byWordWrapping
        help.maximumNumberOfLines = 0
        help.preferredMaxLayoutWidth = 320
        let shortHelp: String = switch setting.key {
        case "hotkey", "agent_hotkey", "max_results": ""
        case "launch_at_login": "Ready whenever you need it."
        case "fallback_search": "Offered when local results are limited."
        default: setting.help
        }
        say(shortHelp, wrong: false)
        help.isHidden = shortHelp.isEmpty

        // A button, not a label: on a row this window set, it is how you put it back.
        origin.isBordered = false
        origin.target = self
        origin.action = #selector(reset)
        origin.attributedTitle = Self.provenance(of: setting)
        origin.isEnabled = setting.source == .window
        origin.toolTip = setting.source == .window ? "Back to config.toml" : nil
        // Nothing set a diagnostic, so "where did this come from" has no answer to give.
        origin.isHidden = setting.kind == .readonly || (setting.source != .window && setting.live)
        label.toolTip = setting.help + "\n" + (setting.source == .configFile ? "From config.toml" : "Local changes override config.toml")

        let text = NSStackView(views: [label, help])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 3

        let side = NSStackView(views: [control(), origin])
        side.orientation = .vertical
        side.alignment = .trailing
        side.spacing = 6

        for view in [separator, text, side] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }

        NSLayoutConstraint.activate([
            separator.topAnchor.constraint(equalTo: topAnchor),
            separator.leadingAnchor.constraint(equalTo: leadingAnchor),
            separator.trailingAnchor.constraint(equalTo: trailingAnchor),
            separator.heightAnchor.constraint(equalToConstant: 1),

            text.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Theme.gutter),
            text.topAnchor.constraint(equalTo: topAnchor, constant: 14),
            text.bottomAnchor.constraint(lessThanOrEqualTo: bottomAnchor, constant: -14),
            text.trailingAnchor.constraint(lessThanOrEqualTo: side.leadingAnchor, constant: -16),

            side.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -Theme.gutter),
            side.topAnchor.constraint(equalTo: topAnchor, constant: 14),
            side.bottomAnchor.constraint(lessThanOrEqualTo: bottomAnchor, constant: -14),
            side.widthAnchor.constraint(equalToConstant: Self.controlWidth),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    /// The second line: what this knob is for, or why the last write was refused.
    private func say(_ message: String, wrong: Bool) {
        help.isHidden = message.isEmpty
        help.attributedStringValue = NSAttributedString(
            string: message,
            attributes: [
                .font: Theme.text(12),
                .foregroundColor: wrong ? Theme.danger : Theme.muted,
            ])
    }

    /// Sends a value, and either lets the window reload or says why it was refused.
    @discardableResult
    private func commit(_ value: String) -> Bool {
        if let why = apply(value) {
            say(why, wrong: true)
            NSSound.beep()
            return false
        }
        return true
    }

    @objc private func reset() {
        forget()
    }

    private static func provenance(of setting: Setting) -> NSAttributedString {
        let (word, colour): (String, NSColor) =
            switch setting.source {
            case .builtIn: ("Default", Theme.muted)
            case .configFile: ("config.toml", Theme.muted)
            case .window: ("Reset", Theme.muted)
            }
        let line = setting.live ? word : "\(word) · Restart required"
        return Theme.label(line, size: 10, tracking: 0, color: colour)
    }

    // MARK: - Controls

    private func control() -> NSView {
        switch setting.kind {
        case .flag:
            let toggle = Toggle(on: setting.value == "true")
            toggle.setAccessibilityLabel(setting.label)
            toggle.setAccessibilityHelp(setting.help)
            toggle.onChange = { [weak self] on in self?.commit(on ? "true" : "false") ?? false }
            return trailing(toggle)

        case .count, .number:
            return trailing(well(width: 90, mono: true))

        case .chord:
            let well = ChordWell(setting.value)
            well.onChord = { [weak self] code, mask in
                guard let self else { return }
                guard let spelled = spell(code, mask) else {
                    say("that key has no name blindspot can write down", wrong: true)
                    NSSound.beep()
                    return
                }
                commit(spelled)
            }
            well.onClear = { [weak self] in self?.commit("") }
            return trailing(well)

        case .paths:
            return PathListControl(items: setting.items) { [weak self] items in
                _ = self?.commit(Setting.joined(items))
            }

        case .text:
            if setting.key == "agent.model" || setting.key == "agent.question_model" {
                return ModelPickerControl(
                    configured: setting.value,
                    host: modelHost,
                    allowsAutomatic: setting.key == "agent.question_model"
                ) { [weak self] value in
                    _ = self?.commit(value)
                }
            }
            return trailing(well(width: 200, mono: false))

        case .readonly:
            let value = NSTextField(labelWithString: setting.value)
            value.font = Theme.text(12)
            value.textColor = Theme.inkSoft
            value.alignment = .right
            value.usesSingleLineMode = false
            value.lineBreakMode = .byWordWrapping
            value.maximumNumberOfLines = 0
            value.preferredMaxLayoutWidth = Self.controlWidth
            return value
        }
    }

    /// A one-line editable well. The plate draws the border, not `NSTextField`'s bezel —
    /// the system bezel is the loudest thing on a dark ground and this design is hairlines.
    private func well(width: CGFloat, mono: Bool) -> NSView {
        let field = NSTextField(string: setting.value)
        field.setAccessibilityLabel(setting.label)
        field.font = mono ? Theme.mono(12) : Theme.text(12)
        field.textColor = Theme.ink
        field.alignment = .right
        field.isBordered = false
        field.drawsBackground = false
        field.focusRingType = .none
        field.lineBreakMode = .byTruncatingHead
        field.delegate = self
        field.translatesAutoresizingMaskIntoConstraints = false

        let plate = PlateView(fill: Theme.ink.withAlphaComponent(0.04), radius: 6, border: Theme.hairline)
        plate.addSubview(field)
        NSLayoutConstraint.activate([
            plate.widthAnchor.constraint(equalToConstant: width),
            field.leadingAnchor.constraint(equalTo: plate.leadingAnchor, constant: 8),
            field.trailingAnchor.constraint(equalTo: plate.trailingAnchor, constant: -8),
            field.topAnchor.constraint(equalTo: plate.topAnchor, constant: 5),
            field.bottomAnchor.constraint(equalTo: plate.bottomAnchor, constant: -5),
        ])
        return plate
    }

    private func trailing(_ view: NSView) -> NSView {
        let box = NSView()
        view.translatesAutoresizingMaskIntoConstraints = false
        box.addSubview(view)
        NSLayoutConstraint.activate([
            view.trailingAnchor.constraint(equalTo: box.trailingAnchor),
            view.topAnchor.constraint(equalTo: box.topAnchor),
            view.bottomAnchor.constraint(equalTo: box.bottomAnchor),
            view.leadingAnchor.constraint(greaterThanOrEqualTo: box.leadingAnchor),
        ])
        return box
    }

    // MARK: - Commit

    /// Enter, and clicking away. Both, because a settings field you typed in and then left
    /// should not quietly discard what you typed.
    func controlTextDidEndEditing(_ obj: Notification) {
        guard let field = obj.object as? NSTextField, field.stringValue != setting.value else {
            return
        }
        commit(field.stringValue)
    }

    func textDidEndEditing(_ notification: Notification) {
        guard let view = notification.object as? NSTextView else { return }
        let items = view.string
            .components(separatedBy: .newlines)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        guard items != setting.items else { return }
        commit(Setting.joined(items))
    }
}

/// A row that does something rather than holding something.
///
/// Deliberately not a `Kind` in the schema: an action has no value, no default and nowhere
/// for a provenance to come from, and every consumer of a settings row would have to branch
/// around the one case where `value` means nothing.
@MainActor
private final class ActionRow: NSView {
    private let act: () -> Void
    private let note = NSTextField(labelWithString: "")
    private var press: NSButton?

    init(label: String, help: String, button: String, lines: Int = 2, width: CGFloat = 320, act: @escaping () -> Void) {
        self.act = act
        super.init(frame: .zero)
        translatesAutoresizingMaskIntoConstraints = false

        let separator = PlateView(fill: Theme.hairline)
        let title = NSTextField(labelWithString: "")
        title.attributedStringValue = NSAttributedString(
            string: label,
            attributes: [.font: Theme.text(13, .medium), .foregroundColor: Theme.ink])
        showHelp(help)
        note.lineBreakMode = .byWordWrapping
        note.maximumNumberOfLines = lines
        note.preferredMaxLayoutWidth = width

        let press = NSButton(title: button, target: self, action: #selector(fire))
        press.isBordered = false
        self.press = press
        showButton(button)

        let text = NSStackView(views: [title, note])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 3

        for view in [separator, text, press] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }
        NSLayoutConstraint.activate([
            separator.topAnchor.constraint(equalTo: topAnchor),
            separator.leadingAnchor.constraint(equalTo: leadingAnchor),
            separator.trailingAnchor.constraint(equalTo: trailingAnchor),
            separator.heightAnchor.constraint(equalToConstant: 1),
            text.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Theme.gutter),
            text.topAnchor.constraint(equalTo: topAnchor, constant: 14),
            text.bottomAnchor.constraint(lessThanOrEqualTo: bottomAnchor, constant: -14),
            press.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -Theme.gutter),
            press.centerYAnchor.constraint(equalTo: text.centerYAnchor),
            press.leadingAnchor.constraint(greaterThanOrEqualTo: text.trailingAnchor, constant: 16),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    func showHelp(_ message: String) {
        note.attributedStringValue = NSAttributedString(
            string: message,
            attributes: [.font: Theme.text(11), .foregroundColor: Theme.faint])
    }

    func showButton(_ title: String) {
        press?.attributedTitle = Theme.label(
            title, size: 12, tracking: 0,
            color: title.hasPrefix("Erase") || title.hasPrefix("Forget") ? Theme.danger : Theme.accent)
    }

    @objc private func fire() { act() }
}

/// One colourway, with its own colours as the sample.
///
/// Its own page rather than a settings row: the core cannot enumerate what palettes exist,
/// because which ones exist is a fact about `Theme.swift`. Asking Rust to describe an enum
/// it cannot see would be backwards.
@MainActor
private final class PaletteRow: NSButton {
    private let choose: () -> Void

    init(_ palette: Palette, current: Bool, separated: Bool, choose: @escaping () -> Void) {
        self.choose = choose
        super.init(frame: .zero)
        title = ""
        isBordered = false
        target = self
        action = #selector(selectPalette)
        setAccessibilityLabel(palette.name)
        setAccessibilityValue(current ? "Selected" : "")
        translatesAutoresizingMaskIntoConstraints = false

        let separator = PlateView(fill: Theme.hairline)
        separator.isHidden = !separated

        let title = NSTextField(labelWithString: "")
        title.attributedStringValue = NSAttributedString(
            string: palette.name,
            attributes: [.font: Theme.text(13, .medium), .foregroundColor: Theme.ink])

        let note = NSTextField(labelWithString: "")
        note.attributedStringValue = NSAttributedString(
            string: palette.note,
            attributes: [.font: Theme.text(11), .foregroundColor: Theme.faint])

        // The sample is the palette's own values, so the row is the thing it describes.
        let swatches = NSStackView(views: [palette.surface, palette.ground, palette.ink,
                                           palette.accent].map(Self.swatch))
        swatches.spacing = 4

        let mark = NSTextField(labelWithString: "")
        mark.attributedStringValue = Theme.label(
            current ? "✓" : "", size: 13, tracking: 0, color: Theme.accent)

        let text = NSStackView(views: [title, note])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 3

        let side = NSStackView(views: [swatches, mark])
        side.orientation = .vertical
        side.alignment = .trailing
        side.spacing = 6

        for view in [separator, text, side] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }
        NSLayoutConstraint.activate([
            separator.topAnchor.constraint(equalTo: topAnchor),
            separator.leadingAnchor.constraint(equalTo: leadingAnchor),
            separator.trailingAnchor.constraint(equalTo: trailingAnchor),
            separator.heightAnchor.constraint(equalToConstant: 1),
            text.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Theme.gutter),
            text.topAnchor.constraint(equalTo: topAnchor, constant: 14),
            text.bottomAnchor.constraint(lessThanOrEqualTo: bottomAnchor, constant: -14),
            side.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -Theme.gutter),
            side.topAnchor.constraint(equalTo: topAnchor, constant: 14),
            side.bottomAnchor.constraint(lessThanOrEqualTo: bottomAnchor, constant: -14),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    private static func swatch(_ colour: NSColor) -> NSView {
        let chip = PlateView(fill: colour, radius: 3, border: Theme.hairline)
        chip.widthAnchor.constraint(equalToConstant: 26).isActive = true
        chip.heightAnchor.constraint(equalToConstant: 18).isActive = true
        return chip
    }

    @objc private func selectPalette() { choose() }
}

/// The preferences window.
///
/// A separate window rather than a fifth panel mode: recording a chord, editing a folder
/// list and nudging a number are controls, not rows you arrow through. It is the same plate
/// as the panel — same palette, same tab bar, same gutter — so the two read as one
/// application rather than an app and its preferences.
///
/// Every row it draws comes from `Core.settings()`, and every value it writes is validated
/// by the core. The shell holds no schema and no parser of its own, which is what keeps
/// this window from drifting from config.toml.
@MainActor
final class SettingsWindow: NSObject, NSWindowDelegate {
    private static let width: CGFloat = 860
    private static let height: CGFloat = 650

    private let core: Core
    private let window: NSWindow
    /// All three are rebuilt when the colourway changes: a `CALayer` bakes its colour in
    /// when it is built, so repainting in place would leave every hairline and the tab
    /// bar's band in the old palette.
    private var tabs: SettingsSidebar
    private var pageTitle = NSTextField(labelWithString: "")
    private var rows = TopStack()
    private var scroll = NSScrollView()
    private var sections: [String] = []
    private var settings: [Setting] = []
    private var eraseRow: ActionRow?
    private var statusRow: ActionRow?
    private var updatesRow: ActionRow?
    private let updater = Updater.shared
    private var dashboard: IndexDashboardView?
    private var statusTask: Task<Void, Never>?
    private var erasureTask: Task<Void, Never>?
    private var clearingClips = false
    var beforeClearClips: (() async -> Void)?
    var afterClearClips: (() -> Void)?

    /// Called after every accepted write, so the shell can take up the half of a setting
    /// it holds rather than reads: the two Carbon hotkeys, the panel's row count, the
    /// login item.
    var onChange: (() -> Void)?
    /// Called when the colourway changes, so the panel can be rebuilt in it.
    var onPalette: (() -> Void)?

    init(core: Core) {
        self.core = core
        settings = core.settings()
        sections = Self.sections(of: settings)
        tabs = SettingsSidebar(titles: sections)

        window = SettingsFrame(
            contentRect: NSRect(x: 0, y: 0, width: Self.width, height: Self.height),
            styleMask: [.titled, .closable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false)
        super.init()

        window.title = "blindspot"
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.minSize = NSSize(width: 820, height: 540)
        window.setFrameAutosaveName("BlindspotSettings03")
        window.isReleasedWhenClosed = false
        window.delegate = self
        window.center()

        buildContent()
        choose(0)
    }

    isolated deinit {
        statusTask?.cancel()
        erasureTask?.cancel()
    }

    func windowWillClose(_ notification: Notification) {
        // A closed settings window should not keep a status-refresh task alive. `show()` calls
        // `reload()`, which selects the Content page again and starts a fresh task when needed.
        statusTask?.cancel()
        statusTask = nil
    }

    /// Builds the plate, the tab bar and the scroller in whatever palette is current.
    ///
    /// Called again when the colourway changes, keeping the `NSWindow` itself so it does
    /// not flash or move out from under the pointer.
    private func buildContent() {
        // Pinned to match the palette, for the same reason the panel is: the title bar,
        // the scroller and every other system-drawn part resolve against the window's
        // appearance, and a light title bar over a dark plate reads as two applications.
        window.appearance = NSAppearance(named: Theme.isDark ? .darkAqua : .aqua)
        window.backgroundColor = .clear
        window.isOpaque = false

        tabs = SettingsSidebar(titles: sections)
        tabs.onSelect = { [weak self] index in self?.choose(index) }

        rows = TopStack()
        rows.orientation = .vertical
        rows.alignment = .leading
        rows.spacing = 0
        rows.translatesAutoresizingMaskIntoConstraints = false

        scroll = NSScrollView()
        scroll.documentView = rows
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.scrollerStyle = .overlay
        scroll.translatesAutoresizingMaskIntoConstraints = false

        let plate = SurfaceView()
        plate.translatesAutoresizingMaskIntoConstraints = true
        pageTitle = NSTextField(labelWithString: "")
        pageTitle.font = Theme.text(24, .semibold)
        pageTitle.textColor = Theme.ink
        pageTitle.translatesAutoresizingMaskIntoConstraints = false
        let contentTint = PlateView(fill: Theme.surface.withAlphaComponent(0.18))
        for view in [contentTint, tabs, pageTitle, scroll] as [NSView] { plate.addSubview(view) }

        NSLayoutConstraint.activate([
            tabs.topAnchor.constraint(equalTo: plate.topAnchor, constant: 48),
            tabs.leadingAnchor.constraint(equalTo: plate.leadingAnchor),
            tabs.widthAnchor.constraint(equalToConstant: 180),
            tabs.bottomAnchor.constraint(equalTo: plate.bottomAnchor, constant: -20),
            contentTint.leadingAnchor.constraint(equalTo: tabs.trailingAnchor),
            contentTint.trailingAnchor.constraint(equalTo: plate.trailingAnchor),
            contentTint.topAnchor.constraint(equalTo: plate.topAnchor),
            contentTint.bottomAnchor.constraint(equalTo: plate.bottomAnchor),
            pageTitle.topAnchor.constraint(equalTo: plate.topAnchor, constant: 48),
            pageTitle.leadingAnchor.constraint(equalTo: tabs.trailingAnchor, constant: Theme.gutter),
            scroll.topAnchor.constraint(equalTo: pageTitle.bottomAnchor, constant: 18),
            scroll.leadingAnchor.constraint(equalTo: tabs.trailingAnchor),
            scroll.trailingAnchor.constraint(equalTo: plate.trailingAnchor),
            scroll.bottomAnchor.constraint(equalTo: plate.bottomAnchor),
            // The document view is as wide as the clip, so a row's trailing gutter lands
            // where the tab bar's does rather than at the end of its own text.
            rows.widthAnchor.constraint(equalTo: scroll.widthAnchor),
        ])
        window.contentView = plate
    }

    /// The sections that actually have rows, in the order the schema lists them.
    private static func sections(of settings: [Setting]) -> [String] {
        var seen: Set<String> = []
        var found = settings.map(\.section).filter { seen.insert($0).inserted }
        // The index dashboard is the window's own page, placed right after the settings it reports on.
        if let content = found.firstIndex(of: "Content") { found.insert(Self.index, at: content + 1) }
        // The window's own page, not the schema's — see `PaletteRow`. Before Status, so
        // the two read-mostly pages sit together at the end.
        if let status = found.firstIndex(of: Self.status) {
            found.insert(Self.appearance, at: status)
        } else {
            found.append(Self.appearance)
        }
        return found
    }

    /// Re-reads everything and shows the window.
    ///
    /// Re-read on every open rather than watched: config.toml is hand-edited, and the
    /// answer to "did my edit take" should be "open the window", not "restart".
    func show(settingKey: String? = nil) {
        reload()
        var target: NSView?
        if let settingKey, let setting = settings.first(where: { $0.key == settingKey }),
           let section = sections.firstIndex(of: setting.section) {
            select(section)
            window.contentView?.layoutSubtreeIfNeeded()
            if let row = rows.arrangedSubviews.first(where: { $0.identifier?.rawValue == settingKey }) {
                row.scrollToVisible(row.bounds)
                target = row
            }
        }
        if sections.indices.contains(tabs.selected), sections[tabs.selected] == Self.status {
            probe()
        }
        // An accessory app's window will not take key focus unless the app activates.
        NSApp.activate()
        window.makeKeyAndOrderFront(nil)
        if let target, let control = Self.firstControl(in: target) {
            window.makeFirstResponder(control)
        }
    }

    private static func firstControl(in view: NSView) -> NSControl? {
        if let control = view as? NSControl, control.isEnabled, control.acceptsFirstResponder { return control }
        return view.subviews.lazy.compactMap { firstControl(in: $0) }.first
    }

    private func reload() {
        settings = core.settings()
        let refreshed = Self.sections(of: settings)
        if refreshed == sections {
            select(tabs.selected)
        } else {
            sections = refreshed
            select(0)
        }
    }

    private static let status = "Status"
    private static let clipboard = "Clipboard"
    private static let appearance = "Appearance"
    private static let index = "Index"

    /// A tab was clicked. Separate from `select` so that arriving at the Status page asks
    /// the core to probe, while the redraw that lands the answer does not ask again — which
    /// is what keeps the two from chasing each other forever.
    private func choose(_ index: Int) {
        select(index)
        guard sections.indices.contains(index), sections[index] == Self.status else { return }
        probe()
    }

    /// Asks whether the model's host answers, then redraws once the answer has had time to
    /// arrive. The core does the asking on a thread; a synchronous check here would hang
    /// the window for a connect timeout whenever Ollama is not running.
    private func probe() {
        core.refreshDiagnostics()
        Task { @MainActor [weak self] in
            try? await Task.sleep(for: .milliseconds(600))
            guard let self, window.isVisible,
                sections.indices.contains(tabs.selected),
                sections[tabs.selected] == Self.status
            else { return }
            settings = core.settings()
            select(tabs.selected)
        }
    }

    /// Switches colourway.
    ///
    /// Both windows are rebuilt rather than repainted: row views are pooled and a layer
    /// bakes its colour when it is built, so nothing already on screen would change. The
    /// panel is rebuilt by the delegate, which owns it — and that is safe only because the
    /// hotkey closures resolve the panel through it rather than capturing one.
    private func repaint(as name: String) {
        guard name != Theme.current.name else { return }
        Theme.use(name)
        onPalette?()
        rebuild()
    }

    /// Rebuilds this window's own content in the new palette, keeping the window itself so
    /// it does not flash or move out from under the pointer.
    func rebuild() {
        let page = tabs.selected
        buildContent()
        select(sections.indices.contains(page) ? page : 0)
    }

    /// Forgets every clip, after asking. Modelled on the Spotlight warning in
    /// `AppDelegate` rather than `die`, which always terminates.
    private func clearClips() {
        guard !clearingClips else { return }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "Forget every clip?"
        alert.informativeText =
            "Clipboard history is its own file, so what you have launched is untouched. "
            + "This cannot be undone."
        alert.addButton(withTitle: "Forget")
        alert.addButton(withTitle: "Cancel")
        NSApp.activate()
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        clearingClips = true
        reload()
        Task { @MainActor [weak self] in
            guard let self else { return }
            await self.beforeClearClips?()
            let cleared = await self.core.clearClips()
            self.afterClearClips?()
            self.clearingClips = false
            self.reload()
            if !cleared {
                let failure = NSAlert()
                failure.messageText = "Clipboard history could not be cleared"
                failure.informativeText = "Your previous history is retained. Try again."
                failure.runModal()
            }
        }
    }

    private func eraseContent() {
        if core.contentState?.erasing == true {
            core.cancelContentErasure()
            erasureTask?.cancel(); erasureTask = nil
            reload()
            return
        }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "Erase the content index?"
        alert.informativeText = "Disables content indexing and removes stored excerpts and embeddings. Your source files stay in place. Stopping erasure leaves any data not yet removed; you can retry to finish."
        alert.addButton(withTitle: "Erase index")
        alert.addButton(withTitle: "Cancel")
        alert.beginSheetModal(for: window) { [weak self] response in
            guard response == .alertFirstButtonReturn else { return }
            self?.beginContentErasure()
        }
    }

    private func beginContentErasure() {
        if let why = core.set("content.enabled", to: "false") { showProblem(why); return }
        onChange?()
        if let why = core.eraseContent(confirmed: true) { reload(); showProblem(why); return }
        reload()
        erasureTask?.cancel()
        erasureTask = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                do { try await Task.sleep(for: .milliseconds(250)) } catch { return }
                guard let self else { return }
                guard let state = self.core.contentState else {
                    self.erasureTask = nil
                    self.showProblem("Content-index status unavailable. Reopen settings to check whether erasure finished.")
                    return
                }
                if state.erasing { self.eraseRow?.showHelp(state.status) }
                else {
                    self.erasureTask = nil
                    self.reload()
                    if state.eraseFailed { self.showProblem(state.status) }
                    return
                }
            }
        }
    }

    private func showProblem(_ message: String) {
        guard window.isVisible else { return }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "The change could not be completed"
        alert.informativeText = message
        alert.addButton(withTitle: "OK")
        alert.beginSheetModal(for: window)
    }

    private func select(_ index: Int) {
        guard sections.indices.contains(index) else { return }
        statusTask?.cancel()
        statusTask = nil
        tabs.select(index)
        let section = sections[index]
        pageTitle.stringValue = SettingsSidebar.title(for: section)
        eraseRow = nil
        dashboard = nil
        for view in rows.arrangedSubviews {
            rows.removeArrangedSubview(view)
            view.removeFromSuperview()
        }
        if section == Self.index {
            let view = IndexDashboardView()
            let reader = core.passageReader
            view.inspectFile = { path in reader.inspect(path: path) }
            view.onRefresh = { [weak self] in
                self?.core.refreshContent()
                self?.core.refreshDiagnostics()
            }
            view.onExclude = { [weak self] path in self?.exclude(path) }
            view.onCompact = { [weak self] in self?.confirmCompact() }
            view.onOpenSetting = { [weak self] key in self?.show(settingKey: key) }
            view.onAction = { [weak self, weak view] action, folder in
                guard let self else { return "Settings is unavailable" }
                let error = self.core.indexAction(action, folder: folder)
                let controls = self.core.indexControls
                view?.update(self.core.contentState?.overview, controls: controls)
                return error
            }
            rows.addArrangedSubview(view)
            view.widthAnchor.constraint(equalTo: rows.widthAnchor).isActive = true
            view.heightAnchor.constraint(equalTo: scroll.contentView.heightAnchor).isActive = true
            dashboard = view
            core.refreshDiagnostics()
            let controls = core.indexControls
            view.update(core.contentState?.overview, controls: controls)
            startIndexPolling()
            rows.layoutSubtreeIfNeeded()
            scroll.contentView.scroll(to: .zero)
            scroll.reflectScrolledClipView(scroll.contentView)
            tabs.show(status: "")
            return
        }
        if section == Self.appearance {
            for (at, palette) in Palette.all.enumerated() {
                let name = palette.name
                let row = PaletteRow(
                    palette,
                    current: palette.name == Theme.current.name,
                    separated: at > 0
                ) { [weak self] in self?.repaint(as: name) }
                rows.addArrangedSubview(row)
                row.widthAnchor.constraint(equalTo: rows.widthAnchor).isActive = true
            }
            tabs.show(status: "")
            return
        }

        for (at, setting) in settings.filter({ $0.section == section }).enumerated() {
            let key = setting.key
            let group: String? = switch key {
            case "hotkey": "Shortcuts"
            case "launch_at_login": "Startup"
            case "fallback_search": "Search"
            case "app_paths": "Application folders"
            default: nil
            }
            if let group {
                let heading = IndexUI.caption(group)
                let inset = NSStackView(views: [heading])
                inset.edgeInsets = NSEdgeInsets(top: at == 0 ? 0 : 20, left: Theme.gutter, bottom: 6, right: Theme.gutter)
                rows.addArrangedSubview(inset)
                inset.widthAnchor.constraint(equalTo: rows.widthAnchor).isActive = true
            }
            let row = SettingRow(
                setting,
                separated: section != "General" && at > 0,
                apply: { [weak self] value in self?.write(key, value) },
                forget: { [weak self] in self?.forget(key) },
                spell: { [weak self] code, mask in
                    self?.core.formatHotkey(keyCode: code, modifiers: mask)
                },
                modelHost: settings.first(where: { $0.key == "agent.host" })?.value ?? "127.0.0.1:11434")
            row.identifier = NSUserInterfaceItemIdentifier(key)
            rows.addArrangedSubview(row)
            row.widthAnchor.constraint(equalTo: rows.widthAnchor).isActive = true
        }
        // Not a setting, so not in the schema — it is hardcoded onto the page it belongs
        // to, which is what CLAUDE.md's non-goals ask for in a single-user app.
        if section == Self.status {
            addUpdatesRow()
        }
        if section == Self.clipboard {
            let clear = ActionRow(
                label: "Clear history now",
                help: "Forgets every clip. Launch history is a separate file and is untouched.",
                button: clearingClips ? "Clearing…" : "Forget everything"
            ) { [weak self] in self?.clearClips() }
            rows.addArrangedSubview(clear)
            clear.widthAnchor.constraint(equalTo: rows.widthAnchor).isActive = true
        }
        if section == "Content" {
            let state = core.contentState
            let busy = state?.erasing == true
            let help = (busy || state?.erased == true || state?.eraseFailed == true)
                ? (state?.status ?? "Status unavailable") : "Removes stored excerpts and embeddings, and turns content indexing off."
            let status = ActionRow(label: "Index", help: Self.contentStatus(state), button: "Open index") { [weak self] in
                self?.openIndex()
            }
            statusRow = status
            rows.addArrangedSubview(status)
            status.widthAnchor.constraint(equalTo: rows.widthAnchor).isActive = true
            let row = ActionRow(label: "Erase content index", help: help, button: busy ? "Stop erasing" : "Erase index") { [weak self] in self?.eraseContent() }
            row.identifier = NSUserInterfaceItemIdentifier("content.erase")
            eraseRow = row
            rows.addArrangedSubview(row)
            row.widthAnchor.constraint(equalTo: rows.widthAnchor).isActive = true
            startContentStatusPolling()
        }

        // A page you switch to starts at its top, not wherever the last one was scrolled.
        rows.layoutSubtreeIfNeeded()
        scroll.contentView.scroll(to: .zero)
        scroll.reflectScrolledClipView(scroll.contentView)

        // What the tab bar's right-hand slot is for: how much of this page is yours.
        let set = settings.filter { $0.section == section && $0.source == .window }.count
        tabs.show(status: set == 0 ? "" : "\(set) SET HERE")
    }

    // MARK: - Updates

    /// Beside the version it replaces. Built from the updater's state, so the Status page's own
    /// redraws after a probe show the same step.
    private func addUpdatesRow() {
        let row = ActionRow(label: "Updates", help: updater.summary, button: updater.buttonTitle, lines: 4, width: 380) { [weak self] in
            self?.updateAction()
        }
        row.identifier = NSUserInterfaceItemIdentifier("updates")
        updatesRow = row
        updater.onChange = { [weak self] in self?.showUpdateState() }
        rows.addArrangedSubview(row)
        row.widthAnchor.constraint(equalTo: rows.widthAnchor).isActive = true
    }

    private func updateAction() {
        switch updater.state {
        case .checking, .installing: break
        case .downloading: updater.cancel()
        case .available(let release): updater.download(release)
        case .ready(let prepared): confirmInstall(prepared)
        case .idle, .current, .failed: updater.check()
        }
    }

    private func showUpdateState() {
        updatesRow?.showHelp(updater.summary)
        updatesRow?.showButton(updater.buttonTitle)
        if case .ready(let prepared) = updater.state, window.isVisible {
            confirmInstall(prepared)
        }
    }

    /// The last step before anything on disk changes, so it says what will happen and, when the
    /// download is not signed by the installed copy's developer, what that costs.
    private func confirmInstall(_ prepared: Updater.Prepared) {
        guard window.attachedSheet == nil else { return }
        let alert = NSAlert()
        alert.messageText = "Install Blindspot \(prepared.release.version)?"
        var text = "The download matches its published checksum and its code signature is valid. Blindspot will quit, replace itself and reopen."
        switch prepared.trust {
        case .sameDeveloper:
            alert.addButton(withTitle: "Install and Relaunch")
        case .unverified(let installedTeam):
            alert.alertStyle = .warning
            if let installedTeam {
                text += "\n\nThis download is not signed with the Developer ID of the copy you have (team \(installedTeam)). macOS will treat it as a different app, so permissions such as Accessibility must be granted again, and its origin rests on GitHub and the release checksum alone."
            } else {
                text += "\n\nThis copy and the download are not signed with a Developer ID, so macOS may ask again for permissions such as Accessibility."
            }
            alert.addButton(withTitle: "Install Anyway")
        }
        let notes = prepared.release.notesExcerpt
        if !notes.isEmpty { text += "\n\n" + notes }
        alert.informativeText = text
        alert.addButton(withTitle: "Cancel")
        alert.beginSheetModal(for: window) { [weak self] response in
            MainActor.assumeIsolated {
                guard let self else { return }
                if response == .alertFirstButtonReturn {
                    self.updater.install(prepared)
                } else {
                    self.updater.discard(prepared)
                }
            }
        }
    }

    private static func contentStatus(_ state: ContentRuntime?) -> String {
        guard let state else { return "Status unavailable" }
        if let overview = state.overview {
            let phase = overview.state == "ready" ? "Up to date" : overview.state == "indexing" ? "Indexing" + (overview.stage.isEmpty ? "" : " · \(overview.stage)") : overview.message
            return ([phase] + [overview.documents.map { "\(IndexUI.number($0)) documents" }, overview.semantic.enabled ? overview.semantic.embedded.map { "\(IndexUI.number($0)) passages searchable by meaning" } : nil].compactMap { $0 }).joined(separator: " · ")
        }
        var line = state.status
        if let documents = state.documents, let embeddings = state.embeddings {
            line += " · sampled: \(documents) documents · \(embeddings) embeddings"
        }
        if let counts = state.extractionCounts, counts.count == 4 {
            let labels = zip(["no text", "locked", "oversized", "unreadable"], counts)
                .compactMap { $1 > 0 ? "\($1) \($0)" : nil }
            if !labels.isEmpty { line += " · PDF: " + labels.joined(separator: ", ") }
        }
        if let database = state.databaseBytes {
            line += String(format: " · DB %.1f MiB", Double(database) / 1_048_576.0)
        }
        if let catalog = state.vectorCatalogBytes, catalog > 0 {
            line += String(format: " · vector catalog %.1f MiB", Double(catalog) / 1_048_576.0)
        }
        if let path = state.currentPath, !path.isEmpty { line += " · " + path }
        return line
    }

    private func refreshContentStatus() {
        statusRow?.showHelp(Self.contentStatus(core.contentState))
        if let state = core.contentState, state.erasing || state.erased || state.eraseFailed {
            eraseRow?.showHelp(state.status)
        }
    }

    private func startIndexPolling() {
        statusTask = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(1))
                guard let self, !Task.isCancelled, self.window.isVisible, let dashboard = self.dashboard else { return }
                let controls = self.core.indexControls
                dashboard.update(self.core.contentState?.overview, controls: controls)
            }
        }
    }

    private func confirmCompact() {
        let reclaimable = core.contentState?.overview?.reclaimable ?? 0
        let alert = NSAlert()
        alert.messageText = "Compact the content index?"
        alert.informativeText = "Removes vectors left by the retired search model and free database pages (about \(IndexUI.bytes(reclaimable))), merges the word index and rewrites the database file. Indexing and content search pause until it finishes, which can take a few minutes. Your documents and current search-by-meaning vectors are kept."
        alert.addButton(withTitle: "Compact")
        alert.addButton(withTitle: "Cancel")
        alert.beginSheetModal(for: window) { [weak self] response in
            guard response == .alertFirstButtonReturn, let self else { return }
            if let why = self.core.compactContent() { self.showProblem(why) }
        }
    }

    private func openIndex() {
        guard let index = sections.firstIndex(of: Self.index) else { return }
        choose(index)
    }

    /// Adds a busy folder to the content exclusions after confirmation. It is the same list the
    /// Content page edits, so the exclusion stays visible and removable there.
    private func exclude(_ path: String) {
        let alert = NSAlert()
        alert.messageText = "Exclude this folder from the content index?"
        alert.informativeText = "\(path)\n\nIts files stop being indexed and their stored excerpts are removed on the next pass. The files themselves are not touched. You can remove the exclusion in Content settings."
        alert.addButton(withTitle: "Exclude folder")
        alert.addButton(withTitle: "Cancel")
        alert.beginSheetModal(for: window) { [weak self] response in
            guard response == .alertFirstButtonReturn, let self else { return }
            var items = self.settings.first(where: { $0.key == "content.excluded_paths" })?.items ?? []
            guard !items.contains(path) else { return }
            items.append(path)
            if let why = self.write("content.excluded_paths", Setting.joined(items)) { self.showProblem(why) }
        }
    }

    private func startContentStatusPolling() {
        statusTask = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(750))
                guard let self, !Task.isCancelled, self.window.isVisible else { return }
                self.refreshContentStatus()
            }
        }
    }

    /// Returns the reason a write was refused, and reloads when it was not.
    private func write(_ key: String, _ value: String) -> String? {
        if let why = core.set(key, to: value) { return why }
        settled()
        return nil
    }

    private func forget(_ key: String) {
        // A reset cannot be refused for a key that came out of the schema, and a refusal
        // here would have nowhere to be shown — the row is about to be rebuilt.
        if let why = core.reset(key) { showProblem(why); return }
        settled()
    }

    /// After a write: the shell takes up what it holds a copy of, and the page redraws so
    /// every row's provenance is current.
    private func settled() {
        onChange?()
        reload()
    }
}
