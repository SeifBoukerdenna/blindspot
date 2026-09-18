import AppKit
import SwiftUI
import ServiceManagement
@preconcurrency import ApplicationServices

@MainActor
final class Onboarding: NSObject, ObservableObject, NSWindowDelegate {
    @Published var page: SetupPage
    @Published var record: SetupRecord
    @Published var error: String?
    @Published var values: [String: String] = [:]
    @Published var folders: [String] = []
    @Published var shortcutWorked = false
    @Published var accessibility = false
    @Published var accessTest = false
    @Published var indexSummary = "Choose folders to begin."
    @Published var indexDetails = ""
    @Published var paused = false
    @Published var busy = false
    @Published var connected = false
    @Published var installed: [String] = []
    @Published var selectedModel: String
    @Published var aiStatus = "Connect Ollama to see your installed models."
    @Published var progress: SetupDownloadProgress?
    @Published var testedModel: String?
    @Published var loginStatus = ""
    @Published var diskStatus = ""
    let hardware: SetupHardware
    let store: SetupStore
    let core: Core?
    var onChange: (() -> Void)?
    var onOpen: (() -> Void)?
    var onSettings: ((String) -> Void)?
    private(set) var window: NSWindow?
    weak var accessField: NSTextView?
    private var operation: Task<Void, Never>?
    private var poller: Task<Void, Never>?
    private var operationID = UUID()

    init(core: Core?, store: SetupStore, record: SetupRecord, hardware: SetupHardware = .current()) {
        self.core = core
        self.store = store
        self.record = record
        self.page = record.page
        self.hardware = hardware
        self.selectedModel = SetupModel.recommended(memory: hardware.memory).id
        super.init()
        reload()
        if let current = values["agent.model"], !current.isEmpty { selectedModel = current }
        folders = (values["content.roots"] ?? "").components(separatedBy: "\0").filter { !$0.isEmpty }
    }

    isolated deinit { operation?.cancel(); poller?.cancel() }

    func show() {
        if window == nil {
            let frame = SetupWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 650),
                                    styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                                    backing: .buffered, defer: false)
            frame.title = "Welcome to Blindspot"
            frame.titleVisibility = .hidden
            frame.titlebarAppearsTransparent = true
            frame.isReleasedWhenClosed = false
            frame.minSize = NSSize(width: 820, height: 600)
            frame.contentView = NSHostingView(rootView: SetupView(model: self))
            frame.delegate = self
            frame.setFrameAutosaveName("BlindspotSetup")
            frame.center()
            window = frame
        }
        record.presented = true
        persist()
        reload()
        NSApp.activate()
        window?.makeKeyAndOrderFront(nil)
        poller?.cancel()
        poller = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(1))
                guard let self, !Task.isCancelled, self.window?.isVisible == true else { return }
                self.refreshStatus()
            }
        }
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        if busy {
            let alert = NSAlert()
            alert.messageText = "Keep setup working in the background?"
            alert.informativeText = "You can return from the Blindspot menu → Set up Blindspot. Stopping disconnects this request; another Ollama client may still be downloading the same model."
            alert.addButton(withTitle: "Keep Working")
            alert.addButton(withTitle: "Stop Request")
            alert.addButton(withTitle: "Stay Here")
            let response = alert.runModal()
            if response == .alertThirdButtonReturn { return false }
            if response == .alertSecondButtonReturn { cancel() }
        }
        return true
    }

    func windowWillClose(_ notification: Notification) { poller?.cancel(); persist() }
    func windowDidBecomeKey(_ notification: Notification) {
        reload()
        if core != nil, page == .ai, !busy, !connected, values["agent.enabled"] != "false" { connect() }
    }

    func navigate(_ next: SetupPage) {
        record.visited.insert(page.rawValue)
        page = next
        record.page = next
        error = nil
        persist()
        reload()
    }

    func next() {
        if page == .ready { openLauncher(); return }
        navigate(SetupPage(rawValue: page.rawValue + 1) ?? .ready)
    }

    func openLauncher() {
        record.presented = true
        persist()
        window?.orderOut(nil)
        poller?.cancel()
        onOpen?()
    }

    func receivedShortcut() { shortcutWorked = true }

    private func persist() {
        do { try store.save(record) }
        catch { self.error = "Your setup progress could not be saved. Your enabled features are unchanged. Try again before quitting." }
    }

    func reload() {
        if let core { values = Dictionary(uniqueKeysWithValues: core.settings().map { ($0.key, $0.value) }) }
        if let testedModel, values["agent.model"] != testedModel { self.testedModel = nil }
        refreshStatus()
    }

    func refreshStatus() {
        accessibility = AXIsProcessTrusted()
        switch SMAppService.mainApp.status {
        case .enabled: loginStatus = "Enabled in macOS"
        case .requiresApproval: loginStatus = "Approve Blindspot in System Settings → General → Login Items."
        case .notFound: loginStatus = "Move Blindspot into Applications and reopen it to enable login startup."
        default: loginStatus = values["launch_at_login"] == "true" ? "Not registered yet. Check Login Items in System Settings." : "Off · open Blindspot whenever you need it"
        }
        guard let state = core?.contentState else { return }
        paused = core?.indexControls?.manualPause == true
        if !state.enabled { indexSummary = "Not enabled · your folders are waiting for you"; indexDetails = ""; return }
        indexSummary = state.overview?.message ?? state.status
        let documents = state.overview?.documents ?? state.documents
        var details: [String] = []
        if let documents { details.append("\(documents.formatted()) documents searchable") }
        if let overview = state.overview {
            details.append("\(overview.pass.checked.formatted()) files checked this pass")
            if overview.semantic.enabled, let embedded = overview.semantic.embedded {
                details.append("\(embedded.formatted()) passages searchable by meaning")
            }
            if overview.pass.unreadable > 0 { details.append("Some files need access. Open Index for details.") }
        }
        indexDetails = details.joined(separator: " · ")
    }

    @discardableResult
    func set(_ key: String, _ value: String) -> Bool {
        guard let core else { return false }
        if let reason = core.set(key, to: value) { error = reason; return false }
        values[key] = value
        error = nil
        onChange?()
        refreshStatus()
        return true
    }

    func chooseFolders() {
        guard let window else { return }
        let picker = NSOpenPanel()
        picker.title = "Choose folders for document search"
        picker.message = "Start with a small folder of notes or documents. Only confirmed folders will be indexed."
        picker.prompt = "Choose Folders"
        picker.canChooseFiles = false
        picker.canChooseDirectories = true
        picker.allowsMultipleSelection = true
        picker.beginSheetModal(for: window) { [weak self] response in
            guard response == .OK, let self else { return }
            for url in picker.urls where !self.folders.contains(url.standardizedFileURL.path) && self.folders.count < 32 {
                self.folders.append(url.standardizedFileURL.path)
            }
        }
    }

    func startIndexing() {
        guard !folders.isEmpty else { error = "Choose at least one folder, or continue without document search."; return }
        guard set("content.roots", Setting.joined(folders)), set("content.enabled", "true") else { return }
        core?.refreshContent()
        refreshStatus()
    }

    func pauseIndexing() {
        if let reason = core?.indexAction(paused ? "resume" : "pause") { error = reason }
        refreshStatus()
    }

    func enableAccessibility() {
        _ = AXIsProcessTrustedWithOptions([kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary)
        openURL("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
    }

    func testAccessibility() {
        accessTest = false
        guard accessibility else { error = "Enable Blindspot in Accessibility, then return here and try again."; return }
        guard let field = accessField, let window, field.window === window else { return }
        let sample = "A small idea, ready to grow."
        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(field)
        field.string = sample
        field.setSelectedRange(NSRange(location: 0, length: (sample as NSString).length))
        let application = AXUIElementCreateApplication(getpid())
        var focused: CFTypeRef?
        guard AXUIElementCopyAttributeValue(application, kAXFocusedUIElementAttribute as CFString, &focused) == .success,
              let focused, CFGetTypeID(focused) == AXUIElementGetTypeID() else {
            error = "macOS has not made the test field available. Return from System Settings and retry."
            return
        }
        let element = focused as! AXUIElement
        var selected: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, kAXSelectedTextAttribute as CFString, &selected) == .success,
              selected as? String == sample else {
            error = "Access is enabled, but selection reading did not succeed. Reopen Blindspot and retry."
            return
        }
        let replacement = "Your next idea starts here."
        let result = AXUIElementSetAttributeValue(element, kAXSelectedTextAttribute as CFString, replacement as CFString)
        accessTest = result == .success && field.string == replacement
        error = accessTest ? nil : "Selection reading worked. Text insertion was not available; manual copy and paste still works."
    }

    func openURL(_ value: String) { if let url = URL(string: value) { NSWorkspace.shared.open(url) } }

    func openOllama() {
        guard let app = NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.electron.ollama")
                ?? (["/Applications/Ollama.app", NSHomeDirectory() + "/Applications/Ollama.app"].first { FileManager.default.fileExists(atPath: $0) }.map { URL(fileURLWithPath: $0) }) else {
            openURL("https://ollama.com/download/mac"); return
        }
        NSWorkspace.shared.openApplication(at: app, configuration: .init()) { _, _ in }
    }

    private func client() throws -> SetupOllama {
        try SetupOllama(host: values["agent.host"] ?? "127.0.0.1:11434")
    }

    func connect() {
        run(status: "Connecting to Ollama…") { client in
            let names = try await client.models()
            try Task.checkCancellation()
            self.installed = names
            let home = FileManager.default.homeDirectoryForCurrentUser
            let capacity = try? home.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey])
            if let bytes = capacity?.volumeAvailableCapacityForImportantUsage {
                self.diskStatus = "Home volume: " + ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file) + " available. Ollama may use another volume if you changed its model location."
            } else { self.diskStatus = "Storage space could not be checked. Leave room for the download and working files on Ollama’s model volume." }
            self.connected = true
            self.aiStatus = names.isEmpty ? "Ollama is ready. Choose your first model below." : "Ollama is ready · \(names.count) installed models"
        }
    }

    func download(_ model: SetupModel) {
        guard let window, !busy else { return }
        let alert = NSAlert()
        alert.messageText = "Download \(model.title)?"
        alert.informativeText = "Approximately \(model.size), plus working disk space. Ollama downloads the model from its registry. Your documents are not part of the download.\n\nOllama manages the storage location; check free space on that volume if you changed it. Existing models will be kept."
        alert.addButton(withTitle: "Download Model")
        alert.addButton(withTitle: "Cancel")
        alert.beginSheetModal(for: window) { [weak self] response in
            guard response == .alertFirstButtonReturn, let self else { return }
            self.beginDownload(model)
        }
    }

    private func beginDownload(_ model: SetupModel) {
        run(status: "Preparing \(model.title)…") { client in
            let token = self.operationID
            try await client.pull(model) { [weak self] update in
                await MainActor.run {
                    guard let self, self.operationID == token else { return }
                    self.progress = update
                    self.aiStatus = update.status
                }
            }
            try Task.checkCancellation()
            self.installed = try await client.models()
            self.progress = nil
            self.aiStatus = "Downloaded. Testing the model on this Mac…"
            try await self.testAndSave(client, name: model.id, embedding: model == .embedding)
        }
    }

    func testSelected() {
        let name = selectedModel
        run(status: "Loading and testing \(name)… First load can take a moment.") { client in
            try await self.testAndSave(client, name: name, embedding: false)
        }
    }

    private func testAndSave(_ client: SetupOllama, name: String, embedding: Bool) async throws {
        let started = Date()
        try await client.test(name, embedding: embedding)
        try Task.checkCancellation()
        reload()
        guard client.endpoint == SetupOllama.endpoint(values["agent.host"] ?? "127.0.0.1:11434") else {
            throw SetupFailure("The Ollama address changed during the test. Check the connection and test again before using this model.")
        }
        if embedding {
            guard set("content.embedding_model", name), set("content.semantic", "true") else { return }
            _ = core?.indexAction("check")
            aiStatus = "Search model tested. Meaning search will become available as passages are prepared."
        } else {
            guard set("agent.model", name), set("agent.question_model", "") else { return }
            if values["agent.enabled"] == "false" {
                guard set("agent.enabled", "true") else { return }
                aiStatus = "Model tested and saved. Reopen Blindspot to enable AI."
            } else {
                aiStatus = "Ready for writing and questions · test finished in \(Int(Date().timeIntervalSince(started))) seconds"
            }
            selectedModel = name
            testedModel = name
            record.testedModel = name
            persist()
        }
    }

    func enableMeaningSearch() {
        let model = SetupModel.embedding
        if installed.contains(model.id) {
            run(status: "Checking the search model…") { client in
                try await self.testAndSave(client, name: model.id, embedding: true)
            }
        } else { download(model) }
    }

    private func run(status: String, action: @escaping @MainActor (SetupOllama) async throws -> Void) {
        guard !busy else { return }
        error = nil
        progress = nil
        busy = true
        aiStatus = status
        operationID = UUID()
        let token = operationID
        operation = Task { [weak self] in
            guard let self else { return }
            defer { if self.operationID == token { self.busy = false; self.progress = nil; self.operation = nil } }
            do { try await action(self.client()) }
            catch {
                guard self.operationID == token, !Task.isCancelled else { return }
                if let network = error as? URLError, [.cannotConnectToHost, .cannotFindHost, .networkConnectionLost].contains(network.code) {
                    self.connected = false
                    self.error = "Ollama is not responding. Open Ollama, wait for it to start, then check the connection again."
                } else if let network = error as? URLError, network.code == .timedOut {
                    self.error = "Ollama took too long. If the model was loading, try a smaller model or close other memory-heavy apps, then retry."
                } else {
                    self.error = (error as? SetupFailure)?.message ?? "Ollama could not finish. Check that it is running, then retry. If the model could not load, try a smaller one."
                }
                self.aiStatus = "Needs attention · your other features still work"
            }
        }
    }

    func cancel() {
        operationID = UUID()
        operation?.cancel()
        operation = nil
        busy = false
        progress = nil
        aiStatus = "Request stopped. Retry when you’re ready. Ollama may retain downloaded layers."
    }
}

@MainActor
private final class SetupWindow: NSWindow {
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        if event.modifierFlags.intersection(.deviceIndependentFlagsMask).subtracting([.capsLock, .numericPad, .function]) == .command,
           event.charactersIgnoringModifiers == "w" { performClose(nil); return true }
        return super.performKeyEquivalent(with: event)
    }
}

private struct SetupMaterial: NSViewRepresentable {
    @Environment(\.accessibilityReduceTransparency) var reduceTransparency
    func makeNSView(context: Context) -> NSVisualEffectView { NSVisualEffectView() }
    func updateNSView(_ view: NSVisualEffectView, context: Context) {
        view.material = .sidebar
        view.blendingMode = .behindWindow
        view.state = .active
        view.wantsLayer = true
        view.layer?.backgroundColor = reduceTransparency ? NSColor.windowBackgroundColor.cgColor : nil
    }
}

struct SetupView: View {
    @ObservedObject var model: Onboarding
    @Environment(\.colorSchemeContrast) private var contrast
    private let accent = Color(nsColor: .controlAccentColor)

    var body: some View {
        HStack(spacing: 0) {
            sidebar
            VStack(spacing: 0) {
                ScrollView {
                    VStack(alignment: .leading, spacing: 24) {
                        heading
                        pageContent
                        if let error = model.error {
                            Label(error, systemImage: "exclamationmark.circle")
                                .foregroundStyle(.red).font(.callout).textSelection(.enabled)
                                .padding(14).frame(maxWidth: .infinity, alignment: .leading)
                                .background(.red.opacity(0.06), in: RoundedRectangle(cornerRadius: 12))
                                .accessibilityIdentifier("setup.error")
                        }
                    }
                    .frame(maxWidth: 570, alignment: .leading)
                    .padding(.horizontal, 38).padding(.top, 48).padding(.bottom, 28)
                    .frame(maxWidth: .infinity, alignment: .topLeading)
                }
                footer
            }
            .background(Color(nsColor: .windowBackgroundColor))
        }
        .frame(minWidth: 820, minHeight: 570)
        .tint(accent)
    }

    private var sidebar: some View {
        VStack(alignment: .leading, spacing: 28) {
            HStack(spacing: 10) {
                Image(systemName: "command").font(.system(size: 23, weight: .medium))
                Text("Blindspot").font(.system(size: 20, weight: .semibold))
            }.padding(.horizontal, 12)
            VStack(spacing: 5) {
                ForEach(SetupPage.allCases, id: \.rawValue) { page in
                    Button { model.navigate(page) } label: {
                        HStack(spacing: 10) {
                            Image(systemName: page.symbol).frame(width: 19)
                            Text(page.title).font(.system(size: 12, weight: model.page == page ? .semibold : .regular))
                            Spacer(minLength: 0)
                        }
                        .foregroundStyle(model.page == page ? .primary : .secondary)
                        .padding(.horizontal, 12).padding(.vertical, 11)
                        .background(model.page == page ? accent.opacity(contrast == .increased ? 0.25 : 0.12) : .clear,
                                    in: RoundedRectangle(cornerRadius: 9))
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityAddTraits(model.page == page ? .isSelected : [])
                    .accessibilityIdentifier("setup.page.\(page.rawValue)")
                }
            }
            Spacer()
            if model.busy {
                Button { model.navigate(.ai) } label: {
                    Label("Model setup in progress", systemImage: "arrow.down.circle").font(.caption)
                }.buttonStyle(.plain).padding(.horizontal, 12)
            }
            VStack(alignment: .leading, spacing: 6) {
                Label("Made for your Mac", systemImage: "desktopcomputer").font(.caption.weight(.medium))
                Text("No account. No telemetry.\nYour pace, your choices.").font(.caption).foregroundStyle(.secondary).lineSpacing(3)
            }.padding(.horizontal, 12)
        }
        .padding(.horizontal, 14).padding(.top, 54).padding(.bottom, 26)
        .frame(width: 205).background(SetupMaterial())
    }

    private var heading: some View {
        VStack(alignment: .leading, spacing: 14) {
            Image(systemName: model.page.symbol)
                .font(.system(size: 32, weight: .light)).foregroundStyle(accent)
                .frame(width: 62, height: 62)
                .background(accent.opacity(0.08), in: RoundedRectangle(cornerRadius: 18))
                .accessibilityHidden(true)
            Text(headline).font(.system(size: 31, weight: .semibold)).tracking(-0.7)
                .accessibilityAddTraits(.isHeader)
            Text(subtitle).font(.system(size: 14)).foregroundStyle(.secondary).lineSpacing(4)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var headline: String {
        switch model.page {
        case .welcome: "A little less searching.\nA lot more doing."
        case .shortcut: "One shortcut.\nYour Mac, within reach."
        case .documents: "Find the words\nyou remember."
        case .clipboard: "A second chance\nfor your clipboard."
        case .accessibility: "Work with the text\nin front of you."
        case .ai: model.connected ? "Meet your local assistant." : "A capable assistant.\nRight here on your Mac."
        case .ready: "Your next idea\nis a shortcut away."
        }
    }
    private var subtitle: String {
        switch model.page {
        case .welcome: "Open apps, find a thought in your documents, and pick up where you left off. Let’s make Blindspot feel like yours."
        case .shortcut: "Try your shortcut now. Then type Safari and press Return. You can come back to this guide from the menu bar."
        case .documents: "Choose the folders you want to search. Blindspot builds a private index on this Mac while you carry on working."
        case .clipboard: "Keep a local history of what you copy, so a useful line is never just one copy away from disappearing."
        case .accessibility: "Accessibility lets Blindspot read selected text and paste for you. You choose when to use these actions."
        case .ai: "We’ll help you install Ollama, choose a model for this Mac, and try your first local response. Every download is your choice."
        case .ready: "Start with what you need. Everything here can be changed or finished later in Settings or Set up Blindspot."
        }
    }

    @ViewBuilder private var pageContent: some View {
        switch model.page {
        case .welcome: welcome
        case .shortcut: shortcut
        case .documents: documents
        case .clipboard: clipboard
        case .accessibility: accessibility
        case .ai: ai
        case .ready: ready
        }
    }

    private var welcome: some View {
        VStack(alignment: .leading, spacing: 20) {
            feature("command", "Start in seconds", "Open an app with a few keystrokes. No permissions or model needed.")
            feature("doc.text", "Add your world", "Choose document search, clipboard history, and local AI when you’re ready.")
            feature("lock", "You stay in control", "Nothing is enabled just because you visit a setup page.")
            Divider()
            Text("Blindspot lives in the menu bar, with no Dock icon. Reopen the app in Finder if you ever lose the shortcut.")
                .font(.callout).foregroundStyle(.secondary)
            if !Bundle.main.bundleURL.path.contains("/Applications/") {
                Label("Move Blindspot.app into Applications before enabling Open at login.", systemImage: "folder")
                    .font(.callout).foregroundStyle(.secondary)
            }
        }
    }

    private var shortcut: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text(shortcutLabel).font(.system(size: 30, weight: .medium, design: .rounded))
                .padding(.horizontal, 24).padding(.vertical, 18)
                .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 14))
            Label(model.shortcutWorked ? "Shortcut received. You’re connected." : "Press the shortcut to test it, or use Open Blindspot below.",
                  systemImage: model.shortcutWorked ? "checkmark.circle.fill" : "keyboard")
                .foregroundStyle(model.shortcutWorked ? .green : .secondary).font(.callout)
            Button("Change shortcut…") { model.onSettings?("hotkey") }
            Divider()
            Toggle("Open Blindspot when I log in", isOn: flag("launch_at_login"))
                .disabled(!Bundle.main.bundleURL.path.contains("/Applications/"))
            Text(model.loginStatus).font(.caption).foregroundStyle(.secondary)
            if model.loginStatus.contains("Settings") {
                Button("Open Login Items") { model.openURL("x-apple.systempreferences:com.apple.LoginItems-Settings.extension") }
            }
            Button("Open Blindspot") { model.openLauncher() }.buttonStyle(.bordered)
        }
    }

    private var documents: some View {
        VStack(alignment: .leading, spacing: 16) {
            if model.folders.isEmpty {
                Text("A small folder of notes is a great place to start.").foregroundStyle(.secondary)
            }
            ForEach(model.folders, id: \.self) { folder in
                HStack {
                    Image(systemName: "folder").foregroundStyle(accent)
                    Text((folder as NSString).abbreviatingWithTildeInPath).lineLimit(1).truncationMode(.middle)
                        .help(folder)
                    Spacer()
                    Button { model.folders.removeAll { $0 == folder } } label: { Image(systemName: "minus.circle") }
                        .buttonStyle(.plain).accessibilityLabel("Remove \((folder as NSString).lastPathComponent)")
                }.font(.callout)
            }
            HStack {
                Button("Choose folders…") { model.chooseFolders() }
                Button(model.values["content.enabled"] == "true" ? "Save folders & index" : "Start indexing") { model.startIndexing() }
                    .disabled(model.folders.isEmpty)
            }
            Text("Confirm the folders above to store searchable excerpts locally. macOS may ask for folder access. Files stored only in iCloud need to be downloaded in Finder first.")
                .font(.caption).foregroundStyle(.secondary)
            Divider()
            Label(model.indexSummary, systemImage: "text.magnifyingglass").font(.callout)
            if !model.indexDetails.isEmpty { Text(model.indexDetails).font(.caption).foregroundStyle(.secondary) }
            HStack {
                Button(model.paused ? "Resume indexing" : "Pause indexing") { model.pauseIndexing() }
                    .disabled(model.values["content.enabled"] != "true")
                Button("Details & access help…") { model.onSettings?("content.enabled") }
            }
            Text("Try it: open Blindspot, choose Documents, and search a phrase from one of your files. Select a result and press ⌘Y to read the matching passage.")
                .font(.callout).foregroundStyle(.secondary)
        }
    }

    private var clipboard: some View {
        VStack(alignment: .leading, spacing: 18) {
            Toggle("Keep clipboard history on this Mac", isOn: flag("clips.enabled"))
            Text("New copies are saved while history is on. Turning it off stops capture; you can erase saved history separately in Settings → Clipboard.")
                .font(.callout).foregroundStyle(.secondary)
            Toggle("Include copied images", isOn: flag("clips.images"))
            Toggle("Make text inside images searchable", isOn: flag("clips.ocr"))
                .disabled(model.values["clips.images"] != "true")
            Divider()
            Text("A small thing worth remembering.").font(.system(.body, design: .serif)).textSelection(.enabled)
            Text("Select and copy the line above. Open Blindspot and type ; to find it. Return copies a result; Accessibility enables automated paste where offered.")
                .font(.callout).foregroundStyle(.secondary)
            Button("Clipboard settings…") { model.onSettings?("clips.enabled") }
        }
    }

    private var accessibility: some View {
        VStack(alignment: .leading, spacing: 18) {
            feature("1.circle", "Open Accessibility", "System Settings → Privacy & Security → Accessibility.")
            feature("2.circle", "Allow Blindspot", "Turn on Blindspot. If it isn’t listed, use + to add your installed Blindspot.app.")
            feature("3.circle", "Come back here", "We check access automatically when you return. Other Blindspot features work without this permission.")
            HStack {
                Button(model.accessibility ? "Open Accessibility Settings" : "Enable Accessibility") { model.enableAccessibility() }
                Label(model.accessibility ? "Access enabled" : "Not enabled", systemImage: model.accessibility ? "checkmark.circle.fill" : "circle")
                    .font(.callout).foregroundStyle(model.accessibility ? .green : .secondary)
            }
            if model.accessibility {
                SetupAccessField(model: model).frame(height: 60)
                Button("Test selected text & insertion") { model.testAccessibility() }
                Text("The test only changes the sample above. Your clipboard stays untouched.").font(.caption).foregroundStyle(.secondary)
                if model.accessTest { Text("Selection reading and insertion worked in the sample. To try another app, select a line in TextEdit, open Blindspot, and type “make shorter” after setting up AI.")
                    .font(.callout).foregroundStyle(.secondary) }
            }
        }
    }

    private var ai: some View {
        VStack(alignment: .leading, spacing: 20) {
            Label(model.hardware.description, systemImage: "desktopcomputer").font(.callout.weight(.medium))
            if !model.connected {
                feature("1.circle", "Install and open Ollama", "Download Ollama for Mac, open the DMG, drag Ollama into Applications, then open it. No Terminal setup is required.")
                HStack {
                    Button("Download Ollama ↗") { model.openURL("https://ollama.com/download/mac") }
                    Button("Open Ollama") { model.openOllama() }
                    Button("Check connection") { model.connect() }.disabled(model.busy)
                }
                Text("Ollama may offer to install its command-line tool. Blindspot doesn’t need that option. Already running Ollama? Check connection to reuse it.")
                    .font(.caption).foregroundStyle(.secondary)
            } else if !model.busy {
                modelSelection
            }
            Divider()
            VStack(alignment: .leading, spacing: 10) {
                HStack(alignment: .top) {
                    if model.busy { ProgressView().controlSize(.small) }
                    Text(model.aiStatus).font(.callout).textSelection(.enabled)
                }
                if let progress = model.progress {
                    if let fraction = progress.fraction {
                        ProgressView(value: fraction)
                        Text("\(ByteCountFormatter.string(fromByteCount: progress.completed, countStyle: .file)) of \(ByteCountFormatter.string(fromByteCount: progress.total, countStyle: .file)) · current layer")
                            .font(.caption).foregroundStyle(.secondary).monospacedDigit()
                    } else { ProgressView().progressViewStyle(.linear) }
                }
                if model.busy {
                    Text("You can keep using Blindspot. Return here from the menu bar to check progress.").font(.caption).foregroundStyle(.secondary)
                    Button("Stop request") { model.cancel() }
                }
            }
            if model.testedModel != nil {
                    Text("Try it: open Blindspot, type > and ask a question. If you previously picked a model with ⌘M, choose Automatic there to use your setup choice. To ask your indexed files, start with >docs and open a citation to check the source.")
                    .font(.callout).foregroundStyle(.secondary)
                DisclosureGroup("Add search by meaning") {
                    VStack(alignment: .leading, spacing: 12) {
                        Text("A separate search model finds related passages. Word search already works without it. Download approximately \(SetupModel.embedding.size) only if you want this feature.")
                            .font(.caption).foregroundStyle(.secondary)
                        Button(model.installed.contains(SetupModel.embedding.id) ? "Test & enable meaning search" : "Download search model…") { model.enableMeaningSearch() }.disabled(model.busy)
                    }.padding(.top, 8)
                }
            }
        }
    }

    private var modelSelection: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("A good fit for this Mac").font(.headline)
            Text("\(SetupModel.recommended(memory: model.hardware.memory).title) is our starting suggestion based on your memory. A smaller model leaves more room for other apps; your chip and workload affect speed.")
                .font(.callout).foregroundStyle(.secondary)
            Picker("Answer model", selection: $model.selectedModel) {
                ForEach(SetupModel.choices) { choice in
                    Text("\(choice.title) · ~\(choice.size)\(model.installed.contains(choice.id) ? " · installed" : "")").tag(choice.id)
                }
                ForEach(model.installed.filter { name in !SetupModel.choices.contains { $0.id == name } }, id: \.self) { name in
                    Text(name + " · installed").tag(name)
                }
                if !model.installed.contains(model.selectedModel), !SetupModel.choices.contains(where: { $0.id == model.selectedModel }) {
                    Text(model.selectedModel + " · unavailable").tag(model.selectedModel)
                }
            }.disabled(model.busy)
            HStack {
                if model.installed.contains(model.selectedModel) {
                    Button("Test & use this model") { model.testSelected() }.buttonStyle(.bordered).disabled(model.busy)
                } else if let choice = SetupModel.choices.first(where: { $0.id == model.selectedModel }) {
                    Button("Download model…") { model.download(choice) }.buttonStyle(.bordered).disabled(model.busy)
                }
                Button("Refresh models") { model.connect() }.disabled(model.busy)
            }
            if !model.diskStatus.isEmpty { Text(model.diskStatus).font(.caption).foregroundStyle(.secondary) }
            Text("Downloads use the internet. Verified local models run on your Mac. The test uses a sample greeting, never your documents. Download size is smaller than working memory usage.")
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    private var ready: some View {
        VStack(alignment: .leading, spacing: 20) {
            readiness("command", "Launcher", "Ready · \(shortcutLabel)")
            readiness("doc.text", "Documents", model.values["content.enabled"] == "true" ? model.indexSummary : "Not enabled · set up anytime")
            readiness("clipboard", "Clipboard", model.values["clips.enabled"] == "true" ? "Capturing new copies" : "Not enabled · set up anytime")
            readiness("hand.point.up.left", "Selected text & paste", model.accessibility ? "Access enabled" : "Optional · enable when you need it")
            readiness("sparkles", "Local AI", model.testedModel != nil ? model.aiStatus : model.record.testedModel != nil ? "Previously tested · recheck in Local AI" : "Not tested in this setup")
            Divider()
            Text("⌘K for actions. ⌘Y for previews. ⌘, for Settings.\nType : to discover what else Blindspot can do.")
                .font(.callout).foregroundStyle(.secondary).lineSpacing(5)
        }
    }

    private var footer: some View {
        HStack {
            if model.page != .welcome {
                Button("Back") { model.navigate(SetupPage(rawValue: model.page.rawValue - 1) ?? .welcome) }
            }
            Button("Finish later") { model.window?.performClose(nil) }.buttonStyle(.plain).foregroundStyle(.secondary)
            Spacer()
            if model.page == .welcome {
                Button("Open Blindspot") { model.openLauncher() }
            }
            Button(model.page == .welcome ? "Make it yours" : model.page == .ready ? "Open Blindspot" : "Continue") { model.next() }
                .buttonStyle(.bordered).controlSize(.large).keyboardShortcut(.defaultAction)
                .accessibilityIdentifier("setup.continue")
        }
        .padding(.horizontal, 30).padding(.vertical, 18)
        .background(.bar)
    }

    private var shortcutLabel: String {
        (model.values["hotkey"] ?? "cmd+shift+space").split(separator: "+").map {
            switch $0.lowercased() {
            case "cmd", "command": "⌘"
            case "shift": "⇧"
            case "ctrl", "control": "⌃"
            case "alt", "option": "⌥"
            case "space": "Space"
            default: String($0).uppercased()
            }
        }.joined(separator: " ")
    }

    private func flag(_ key: String) -> Binding<Bool> {
        Binding(get: { model.values[key] == "true" }, set: { _ = model.set(key, $0 ? "true" : "false") })
    }

    private func feature(_ symbol: String, _ title: String, _ detail: String) -> some View {
        HStack(alignment: .top, spacing: 14) {
            Image(systemName: symbol).font(.system(size: 20, weight: .light)).foregroundStyle(accent).frame(width: 26)
            VStack(alignment: .leading, spacing: 5) {
                Text(title).font(.system(size: 14, weight: .semibold))
                Text(detail).font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
        }
    }
    private func readiness(_ symbol: String, _ title: String, _ detail: String) -> some View {
        feature(symbol, title, detail)
    }
}

private struct SetupAccessField: NSViewRepresentable {
    let model: Onboarding
    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        let text = NSTextView(frame: NSRect(x: 0, y: 0, width: 400, height: 60))
        text.string = "A small idea, ready to grow."
        text.font = .systemFont(ofSize: 14)
        text.isRichText = false
        text.textContainerInset = NSSize(width: 10, height: 10)
        text.autoresizingMask = [.width]
        text.setAccessibilityLabel("Accessibility test sample")
        scroll.documentView = text
        scroll.borderType = .bezelBorder
        model.accessField = text
        return scroll
    }
    func updateNSView(_ view: NSScrollView, context: Context) {}
}
