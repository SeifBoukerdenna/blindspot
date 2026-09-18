import AppKit
import SwiftUI
import Charts
import UniformTypeIdentifiers

@MainActor
private final class ContainerFrame: NSWindow {
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
            .subtracting([.capsLock, .numericPad, .function])
        if flags == .command, event.charactersIgnoringModifiers == "w", attachedSheet == nil {
            performClose(nil)
            return true
        }
        return super.performKeyEquivalent(with: event)
    }
}

enum ContainerSection: String, CaseIterable { case containers = "Containers", images = "Images" }

@MainActor
final class ContainerWindow: NSObject, ObservableObject, NSWindowDelegate {
    static let shared = ContainerWindow()
    @Published var endpoints: [ContainerEndpoint] = []
    @Published var endpointID = ""
    @Published var rows: [LocalContainer] = []
    @Published var selectedID: String?
    @Published var filter = ""
    @Published var notice = "Open Docker or start your Podman machine, then refresh."
    @Published var error: String?
    @Published var busy = false
    @Published var logs: String?
    @Published var status = "Local containers"
    @Published var section = ContainerSection.containers
    @Published var images: [LocalContainerImage] = []
    @Published var selectedImageID: String?
    @Published var inspection: ContainerDetails?
    @Published var resourceUsage: String?
    @Published var preferences = ContainerPreferences.read()
    @Published var creation: ContainerCreation?
    @Published var detailTab = "Overview"
    @Published var samples: [ContainerSample] = []
    @Published var events: [String] = []
    @Published var monitoring = false
    @Published var monitoringStatus = "Monitoring paused"
    @Published var eventStatus = "Events paused"
    @Published var followingLogs = false
    @Published var logStatus = "Snapshot · not live"
    @Published var logQuery = ""
    @Published var logTruncated = false
    private var monitorTask: Task<Void, Never>?
    private var eventTask: Task<Void, Never>?
    private var logTask: Task<Void, Never>?
    private var monitorToken = UUID()
    private var logToken = UUID()
    var filteredLogText: String {
        guard let logs else { return "" }
        return logQuery.isEmpty ? logs : logs.components(separatedBy: "\n").filter { $0.localizedCaseInsensitiveContains(logQuery) }.joined(separator: "\n")
    }
    var selectedImage: LocalContainerImage? { images.first { $0.id == selectedImageID } }
    var filteredImages: [LocalContainerImage] {
        let words = filter.lowercased().split(whereSeparator: \.isWhitespace)
        return images.filter { row in words.allSatisfy { (row.references.joined(separator: " ") + " " + row.id).lowercased().contains($0) } }
    }
    private(set) var window: NSWindow?
    private var operation: Task<Void, Never>?
    private var generation = UUID()
    private var changingContainer = false
    private var paletteName = ""

    var endpoint: ContainerEndpoint? { endpoints.first { $0.id == endpointID } }
    var selected: LocalContainer? { rows.first { $0.id == selectedID } }
    var filtered: [LocalContainer] {
        let words = filter.lowercased().split(whereSeparator: \.isWhitespace)
        return rows.filter { row in
            let text = "\(row.name) \(row.image) \(row.state) \(row.ports)".lowercased()
            return words.allSatisfy { text.contains($0) }
        }
    }

    isolated deinit { operation?.cancel(); monitorTask?.cancel(); eventTask?.cancel(); logTask?.cancel() }

    func show(engine: ContainerEngine? = nil, discover: Bool = true) {
        if window == nil {
            let frame = ContainerFrame(contentRect: NSRect(x: 0, y: 0, width: 960, height: 640),
                                 styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView], backing: .buffered, defer: false)
            frame.title = "Containers"
            frame.titleVisibility = .hidden
            frame.minSize = NSSize(width: 820, height: 520)
            frame.isReleasedWhenClosed = false
            frame.hidesOnDeactivate = false
            frame.isOpaque = false
            frame.backgroundColor = .clear
            frame.titlebarAppearsTransparent = true
            frame.delegate = self
            frame.setFrameAutosaveName("BlindspotContainers")
            frame.center()
            window = frame
        }
        if let window {
            if paletteName != Theme.current.name {
                paletteName = Theme.current.name
                window.appearance = NSAppearance(named: Theme.isDark ? .darkAqua : .aqua)
                let surface = SurfaceView(radius: 12)
                let content = NSHostingView(rootView: ContainerBrowser(model: self, palette: Theme.current))
                content.translatesAutoresizingMaskIntoConstraints = false
                surface.addSubview(content)
                NSLayoutConstraint.activate([
                    content.leadingAnchor.constraint(equalTo: surface.leadingAnchor),
                    content.trailingAnchor.constraint(equalTo: surface.trailingAnchor),
                    content.topAnchor.constraint(equalTo: surface.topAnchor, constant: 28),
                    content.bottomAnchor.constraint(equalTo: surface.bottomAnchor),
                ])
                window.contentView = surface
            }
            window.level = preferences.keepOnTop ? .floating : .normal
            WindowPresentation.show(window)
        }
        if discover && !busy { discoverEngines(preferred: engine) }
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        cancel()
        rows = []
        selectedID = nil
        logs = nil
        return true
    }

    func discoverEngines(preferred: ContainerEngine? = nil) {
        guard !busy else { return }
        let previous = endpointID
        clearInventory(); endpoints = []; endpointID = ""
        run("Finding local engines…") { token in
            let discovery = await ContainerService.discover()
            guard self.generation == token, !Task.isCancelled else { return }
            self.endpoints = discovery.endpoints
            self.notice = discovery.notices.joined(separator: " ")
            self.endpointID = discovery.endpoints.first(where: { $0.engine == preferred })?.id
                ?? discovery.endpoints.first(where: { $0.id == previous })?.id
                ?? discovery.endpoints.first?.id ?? ""
            if let endpoint = self.endpoint {
                try await self.loadInventory(endpoint, token: token)
            } else { self.status = "No local engines found" }
        }
    }

    func selectEndpoint(_ id: String) {
        guard !busy else { return }
        endpointID = id
        clearInventory()
        refresh()
    }

    func selectContainer(_ id: String?) { stopMonitoring(); pauseLogs(); samples = []; events = []; logQuery = ""; selectedID = id; logs = nil; inspection = nil; resourceUsage = nil }

    func clearInventory() {
        stopMonitoring(); pauseLogs(); samples = []; events = []; logQuery = ""
        rows = []; images = []; selectedID = nil; selectedImageID = nil
        logs = nil; inspection = nil; resourceUsage = nil
    }

    func selectSection(_ value: ContainerSection) {
        guard !busy else { return }
        stopMonitoring(); pauseLogs(); samples = []; events = []
        section = value; filter = ""; logs = nil; inspection = nil; resourceUsage = nil
        refresh()
    }

    func setKeepOnTop(_ value: Bool) {
        preferences.keepOnTop = value
        preferences.save()
        window?.level = value ? .floating : .normal
    }

    func setLogLines(_ value: Int) {
        guard ContainerPreferences.lineChoices.contains(value) else { return }
        pauseLogs()
        preferences.logLines = value; preferences.save(); logs = nil
    }

    private func loadInventory(_ endpoint: ContainerEndpoint, token: UUID) async throws {
        if section == .images {
            let values = try await ContainerService.images(endpoint)
            guard generation == token, !Task.isCancelled else { return }
            images = values
            selectedImageID = values.contains(where: { $0.id == selectedImageID }) ? selectedImageID : values.first?.id
            status = "\(values.count) local images · including untagged images"
        } else {
            let values = try await ContainerService.list(endpoint)
            guard generation == token, !Task.isCancelled else { return }
            rows = values
            selectedID = values.contains(where: { $0.id == selectedID }) ? selectedID : values.first?.id
            status = "\(values.count) containers · \(values.filter(\.running).count) running"
        }
    }

    func refresh() {
        guard !busy, let endpoint else { return }
        stopMonitoring(); pauseLogs()
        logs = nil; inspection = nil; resourceUsage = nil
        run("Refreshing \(section.rawValue.lowercased())…") { token in
            try await self.loadInventory(endpoint, token: token)
        }
    }

    func inspectSelected() {
        guard !busy, let endpoint, let selected else { return }
        run("Inspecting container…") { token in
            let details = try await ContainerService.inspect(selected, endpoint: endpoint)
            guard self.generation == token, self.selectedID == selected.id, !Task.isCancelled else { return }
            self.inspection = details
            self.status = "Container details loaded"
        }
    }

    func readStats() {
        guard !busy, let endpoint, let selected else { return }
        run("Reading resource snapshot…") { token in
            let value = try await ContainerService.stats(selected, endpoint: endpoint)
            guard self.generation == token, self.selectedID == selected.id, !Task.isCancelled else { return }
            self.resourceUsage = value
            self.status = "Resource snapshot loaded · refresh explicitly to sample again"
        }
    }

    func createContainer() {
        guard !busy, let window, let endpoint, let image = selectedImage else { return }
        let form = ContainerCreation(image: image, endpoint: endpoint)
        creation = form
        form.show(on: window) { [weak self] draft in
            guard let self else { return }
            self.creation = nil
            guard let draft, !self.busy, self.endpoint == endpoint, self.selectedImageID == image.id else { return }
            self.changingContainer = true
            self.run("Creating container…") { token in
                try await ContainerService.create(draft, image: image, endpoint: endpoint)
                guard self.generation == token, !Task.isCancelled else { return }
                self.section = .containers; self.filter = ""
                try await self.loadInventory(endpoint, token: token)
            }
        }
    }

    func readLogs() {
        guard !busy, let endpoint, let selected else { return }
        pauseLogs(); logTruncated = false; logStatus = "Snapshot · not live"
        logs = nil
        run("Reading recent logs…") { token in
            let logs = try await ContainerService.logs(selected, endpoint: endpoint, lines: self.preferences.logLines)
            guard self.generation == token, !Task.isCancelled, self.selectedID == selected.id else { return }
            self.logs = logs.isEmpty ? "No recent log output." : logs
            self.status = "Recent logs loaded · refresh explicitly to read again"
        }
    }

    func stopMonitoring() {
        monitorToken = UUID(); monitorTask?.cancel(); eventTask?.cancel()
        monitorTask = nil; eventTask = nil; monitoring = false
        monitoringStatus = "Monitoring paused · retained samples may be stale"
        eventStatus = "Events paused · gaps are possible"
    }

    func startMonitoring() {
        guard window?.isVisible == true, let endpoint, let selected, !busy else { return }
        stopMonitoring(); samples = []; events = []
        monitoring = true; monitoringStatus = "Connecting…"; eventStatus = "Connecting to events…"
        let token = monitorToken
        monitorTask = Task { [weak self] in
            while !Task.isCancelled {
                let started = Date()
                do {
                    let details = try await ContainerService.inspect(selected, endpoint: endpoint)
                    let rows = try await ContainerService.list(endpoint, id: selected.id)
                    guard let latest = rows.first(where: { $0.id == selected.id }) else { throw ContainerFailure("Container is no longer available.") }
                    let sample = latest.running ? try await ContainerService.sample(latest, endpoint: endpoint) : nil
                    guard let self, self.monitorToken == token, !Task.isCancelled else { return }
                    self.inspection = details
                    if let index = self.rows.firstIndex(where: { $0.id == latest.id }) { self.rows[index] = latest }
                    if let sample { self.samples.append(sample); self.samples = Array(self.samples.suffix(120)); self.resourceUsage = sample.summary }
                    self.monitoringStatus = latest.running ? "Live · updated " + Date().formatted(date: .omitted, time: .standard) : "Container stopped · waiting for start"
                } catch {
                    guard let self, self.monitorToken == token, !Task.isCancelled else { return }
                    self.monitoring = false
                    self.monitoringStatus = "Disconnected · retained samples may be stale. Reconnect to try again."
                    self.eventTask?.cancel(); self.eventStatus = "Events disconnected · history may be incomplete"
                    return
                }
                do { try await Task.sleep(for: .seconds(max(0.1, 2 - Date().timeIntervalSince(started)))) } catch { return }
            }
        }
        eventTask = Task { [weak self] in
            let args = ["events", "--since", Date().ISO8601Format(), "--filter", "container=" + selected.id, "--filter", "type=container", "--format", "{{json .}}"]
            do {
                self?.eventStatus = "Live events · since this connection"
                for try await chunk in ContainerCommand.stream(endpoint, arguments: args) {
                    guard let self, self.monitorToken == token, !Task.isCancelled else { return }
                    if !chunk.stderr, let value = ContainerService.event(chunk.text, id: selected.id) {
                        self.events.append(value); self.events = Array(self.events.suffix(200))
                    }
                }
                guard let self, self.monitorToken == token, !Task.isCancelled else { return }
                self.eventStatus = "Event connection ended · reconnect monitoring to resume"
            } catch {
                guard let self, self.monitorToken == token, !Task.isCancelled else { return }
                self.eventStatus = "Events disconnected · history may be incomplete"
            }
        }
    }

    func pauseLogs() {
        logToken = UUID(); logTask?.cancel(); logTask = nil; followingLogs = false
        logStatus = "Paused · history may be incomplete"
    }

    func followLogs() {
        guard window?.isVisible == true, !busy, let selected, let endpoint else { return }
        pauseLogs(); logs = ""; logTruncated = false; followingLogs = true
        logStatus = "Live · timestamps supplied by runtime"
        let token = logToken, lines = preferences.logLines
        logTask = Task { [weak self] in
            do {
                for try await chunk in ContainerCommand.stream(endpoint, arguments: ["logs", "--follow", "--tail", String(lines), "--timestamps", selected.id]) {
                    guard let self, self.logToken == token, !Task.isCancelled else { return }
                    self.appendLog(ContainerService.cleanLog(chunk.text))
                }
                guard let self, self.logToken == token, !Task.isCancelled else { return }
                self.followingLogs = false; self.logStatus = "Stream ended · reconnect for new output"
            } catch {
                guard let self, self.logToken == token, !Task.isCancelled else { return }
                self.followingLogs = false; self.logStatus = "Disconnected · history incomplete. Reconnect to reload recent logs."
            }
        }
    }

    func appendLog(_ text: String) {
        var lines = ((logs ?? "") + text).components(separatedBy: "\n")
        let limit = preferences.logLines * 2 + 1
        if lines.count > limit { lines.removeFirst(lines.count - limit); logTruncated = true }
        var bytes = lines.reduce(0) { $0 + $1.utf8.count + 1 }
        while bytes > 1_048_576, !lines.isEmpty { bytes -= lines.removeFirst().utf8.count + 1; logTruncated = true }
        logs = lines.joined(separator: "\n")
    }

    func logExportText(filtered: Bool) -> String {
        "Blindspot container logs · " + Date().ISO8601Format() + "\n"
            + (endpoint?.title ?? "Local engine") + " · " + (selected?.name ?? "Container") + "\n"
            + (selectedID ?? "") + "\n" + logStatus
            + (logTruncated ? " · earlier loaded output discarded" : "")
            + (filtered ? " · filtered view" : " · all loaded output") + "\n\n" + (filtered ? filteredLogText : logs ?? "")
    }

    func exportLogs(filtered: Bool) {
        guard let window, logs != nil else { return }
        let snapshot = logExportText(filtered: filtered)
        let panel = NSSavePanel(); panel.allowedContentTypes = [.plainText]; panel.nameFieldStringValue = "container-logs.txt"
        panel.beginSheetModal(for: window) { [weak self] response in
            guard response == .OK, let url = panel.url else { return }
            Task {
                do {
                    try await Task.detached { try Data(snapshot.utf8).write(to: url, options: .atomic) }.value
                    self?.status = "Logs exported"
                } catch { self?.error = "The log file could not be saved. Choose another location and try again." }
            }
        }
    }

    func change(_ operation: ContainerOperation) {
        guard !busy, let window, let endpoint, let selected, selected.allows(operation) else { return }
        let alert = NSAlert()
        alert.messageText = "\(operation.title) \(selected.name)?"
        alert.informativeText = "\(endpoint.title)\nContainer \(selected.id.prefix(12))\n\n"
            + (operation == .start ? "This starts the container's existing configured workload." : "This interrupts the container's workload. Containers configured for automatic removal may be removed by the engine when they stop.")
        alert.addButton(withTitle: "Cancel")
        alert.addButton(withTitle: operation.title)
        alert.beginSheetModal(for: window) { [weak self] response in
            guard response == .alertSecondButtonReturn, let self, !self.busy, self.endpoint == endpoint,
                  self.selectedID == selected.id else { return }
            self.stopMonitoring(); self.pauseLogs()
            self.changingContainer = true
            self.run("\(operation.title) requested…") { token in
                try await ContainerService.mutate(operation, container: selected, endpoint: endpoint)
                let rows = try await ContainerService.list(endpoint)
                guard self.generation == token, !Task.isCancelled else { return }
                self.rows = rows
                self.selectedID = rows.contains(where: { $0.id == selected.id }) ? selected.id : nil
                self.logs = nil
                self.inspection = nil; self.resourceUsage = nil
                self.status = "\(operation.title) finished · current state refreshed"
            }
        }
    }

    private func run(_ status: String, action: @escaping @MainActor (UUID) async throws -> Void) {
        guard !busy else { return }
        busy = true
        error = nil
        self.status = status
        generation = UUID()
        let token = generation
        operation = Task { [weak self] in
            guard let self else { return }
            defer {
                if self.generation == token { self.busy = false; self.changingContainer = false; self.operation = nil }
            }
            do { try await action(token) }
            catch {
                guard self.generation == token, !Task.isCancelled else { return }
                self.error = (error as? ContainerFailure)?.message ?? "The local runtime returned an unsupported response. Check its version and refresh."
                self.status = "Needs attention"
                self.logs = nil
            }
        }
    }

    func cancel() {
        creation?.finish(false); creation = nil
        let mutation = changingContainer
        generation = UUID()
        operation?.cancel(); operation = nil
        busy = false; changingContainer = false
        clearInventory()
        status = mutation ? "Stopped waiting. The engine may have applied the change; refresh before retrying." : "Request cancelled"
    }
}

private struct ContainerBrowser: View {
    @ObservedObject var model: ContainerWindow
    let palette: Palette

    private var rule: some View { Color(nsColor: palette.hairline).frame(height: 0.5) }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 14) {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Containers").font(.system(size: 20, weight: .medium))
                    Text("Blindspot · On this Mac").font(.system(size: 11))
                        .foregroundStyle(Color(nsColor: palette.muted))
                }
                Spacer(minLength: 20)
                Picker("Engine", selection: Binding(get: { model.endpointID }, set: { model.selectEndpoint($0) })) {
                    if model.endpoints.isEmpty { Text("No local engine").tag("") }
                    ForEach(model.endpoints) { Text($0.title).tag($0.id) }
                }.frame(maxWidth: 350).disabled(model.busy)
                Spacer()
                Button("Find engines") { model.discoverEngines() }.disabled(model.busy)
                Button { model.refresh() } label: { Label("Refresh", systemImage: "arrow.clockwise") }
                    .keyboardShortcut("r").disabled(model.busy || model.endpoint == nil)
            }.padding(20)
            HStack(spacing: 16) {
                Picker("Browse", selection: Binding(get: { model.section }, set: { model.selectSection($0) })) {
                    ForEach(ContainerSection.allCases, id: \.self) { Text($0.rawValue).tag($0) }
                }.pickerStyle(.segmented).frame(width: 220).disabled(model.busy)
                Spacer()
                Toggle("Keep on top", isOn: Binding(get: { model.preferences.keepOnTop }, set: { model.setKeepOnTop($0) }))
                    .toggleStyle(.checkbox)
            }.padding(.horizontal, 20).padding(.bottom, 14)
            rule
            if let error = model.error {
                Label(error, systemImage: "exclamationmark.circle").font(.callout).foregroundStyle(Color(nsColor: palette.danger))
                    .frame(maxWidth: .infinity, alignment: .leading).padding(16)
                rule
            }
            HSplitView {
                if model.section == .images {
                    imageList.frame(minWidth: 280, idealWidth: 330, maxWidth: 440)
                } else {
                VStack(spacing: 0) {
                    TextField("Filter name, image, state or port", text: $model.filter)
                        .textFieldStyle(.plain).font(.system(size: 13))
                        .padding(12)
                        .background(Color(nsColor: palette.ground).opacity(0.35), in: RoundedRectangle(cornerRadius: 7))
                        .padding(12)
                    if model.rows.isEmpty {
                        VStack(spacing: 12) {
                            Image(systemName: "shippingbox").font(.system(size: 32)).foregroundStyle(Color(nsColor: palette.faint))
                            Text(model.busy ? "Checking your engine…" : "No containers to show").font(.headline)
                            Text(model.busy ? "You can keep using the launcher." : model.endpoint != nil && model.error == nil ? "This engine has no containers yet. Create them with your runtime tools, then refresh." : "Open Docker or start your Podman machine, then refresh. Remote engines are not connected.")
                                .font(.callout).foregroundStyle(Color(nsColor: palette.muted)).multilineTextAlignment(.center)
                        }.padding(24).frame(maxWidth: .infinity, maxHeight: .infinity)
                    } else {
                        List(selection: Binding(get: { model.selectedID }, set: { model.selectContainer($0) })) {
                            ForEach(model.filtered) { row in
                                HStack(alignment: .top, spacing: 10) {
                                    Image(systemName: row.running ? "circle.fill" : "circle")
                                        .foregroundStyle(Color(nsColor: row.running ? palette.ok : palette.muted)).font(.system(size: 8)).padding(.top, 5)
                                    VStack(alignment: .leading, spacing: 5) {
                                        Text(row.name).font(.body.weight(.medium)).lineLimit(1)
                                        Text(row.image).font(.caption).foregroundStyle(Color(nsColor: palette.muted)).lineLimit(1)
                                        Text(row.state.capitalized).font(.caption).foregroundStyle(Color(nsColor: palette.muted))
                                    }
                                }.padding(.vertical, 8).tag(row.id)
                                    .listRowBackground(Color(nsColor: model.selectedID == row.id ? palette.selection : .clear))
                                    .listRowSeparator(.hidden)
                            }
                        }.listStyle(.plain).scrollContentBackground(.hidden).disabled(model.busy)
                    }
                }.frame(minWidth: 280, idealWidth: 330, maxWidth: 440)
                }
                Group {
                    if model.section == .images { imageDetail } else { detail }
                }.frame(minWidth: 390, maxWidth: .infinity, maxHeight: .infinity)
            }
            rule
            HStack(spacing: 12) {
                if model.busy { ProgressView().controlSize(.small) }
                Text(model.status).font(.caption).foregroundStyle(Color(nsColor: palette.muted)).lineLimit(2)
                Spacer()
                if model.busy { Button("Stop waiting") { model.cancel() } }
                Text("On this Mac").font(.caption).foregroundStyle(Color(nsColor: palette.muted))
            }.padding(14)
        }
        .font(.system(size: 13))
        .foregroundStyle(Color(nsColor: palette.ink))
        .tint(Color(nsColor: palette.accent))
        .buttonStyle(.borderless)
        .controlSize(.small)
        .frame(minWidth: 820, minHeight: 490)
    }

    @ViewBuilder private var detail: some View {
        if let row = model.selected {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    Text(row.name).font(.title2.weight(.semibold)).textSelection(.enabled)
                    Text(row.status).foregroundStyle(Color(nsColor: palette.muted))
                    HStack(spacing: 18) {
                        ForEach(ContainerOperation.allCases, id: \.rawValue) { operation in
                            Button(operation.title + "…") { model.change(operation) }
                                .disabled(model.busy || !row.allows(operation))
                                .opacity(model.busy || !row.allows(operation) ? 0.35 : 1)
                        }
                    }
                    HStack(spacing: 18) {
                        Button("Inspect details") { model.inspectSelected() }.disabled(model.busy)
                        Button(model.monitoring ? "Pause monitoring" : "Start monitoring") { if model.monitoring { model.stopMonitoring() } else { model.startMonitoring() } }.disabled(model.busy)
                        Button("Copy ID") { copy(row.id) }
                    }
                    Picker("Details", selection: $model.detailTab) {
                        ForEach(["Overview", "Resources", "Logs", "Events"], id: \.self) { Text($0).tag($0) }
                    }.pickerStyle(.segmented)
                    if model.detailTab == "Overview" {
                    Grid(alignment: .leading, horizontalSpacing: 20, verticalSpacing: 12) {
                        info("Image", row.image)
                        info("Container", row.id)
                        info("Ports", row.ports.isEmpty ? "No published or exposed ports" : row.ports)
                        info("Engine", model.endpoint?.title ?? "")
                    }.font(.callout)
                    if let inspection = model.inspection {
                        VStack(alignment: .leading, spacing: 8) {
                            ForEach(inspection.fields, id: \.self) { Text($0).textSelection(.enabled) }
                            ForEach(inspection.localPorts, id: \.self) { port in
                                Link("Open localhost:\(port)", destination: URL(string: "http://127.0.0.1:\(port)")!)
                            }
                        }.font(.caption).foregroundStyle(Color(nsColor: palette.muted))
                    }
                    }
                    if model.detailTab == "Resources" {
                    if let usage = model.resourceUsage {
                        Text(usage).font(.system(size: 11, design: .monospaced)).textSelection(.enabled)
                    }
                    Text(model.monitoringStatus).font(.caption).foregroundStyle(Color(nsColor: palette.muted))
                    if !model.samples.isEmpty {
                        VStack(alignment: .leading) {
                            Text("CPU (%)").font(.caption)
                            Chart(model.samples) { sample in LineMark(x: .value("Time", sample.date), y: .value("CPU", sample.cpu)).symbol(.circle) }.frame(height: 90)
                            Text("Memory (MiB)").font(.caption)
                            Chart(model.samples) { sample in LineMark(x: .value("Time", sample.date), y: .value("Memory", sample.memory)).symbol(.circle) }.frame(height: 90)
                            Text("Last 120 samples · targets a 2-second interval · I/O values are cumulative").font(.caption)
                        }
                    }
                    }
                    if model.detailTab == "Events" {
                        VStack(alignment: .leading, spacing: 6) {
                            Text(model.eventStatus).font(.caption)
                            ForEach(Array(model.events.enumerated()), id: \.offset) { _, event in Text(event).font(.caption).textSelection(.enabled) }
                        }.frame(maxWidth: .infinity, alignment: .leading)
                    }
                    if model.detailTab == "Logs" {
                    HStack {
                        Text("Logs").font(.headline)
                        Picker("Lines", selection: Binding(get: { model.preferences.logLines }, set: { model.setLogLines($0) })) {
                            ForEach(ContainerPreferences.lineChoices, id: \.self) { Text(String($0)).tag($0) }
                        }.frame(width: 130).disabled(model.busy)
                        Spacer()
                        Button(model.logs == nil ? "Read logs" : "Reload logs") { model.readLogs() }.disabled(model.busy)
                    }
                    HStack {
                        TextField("Search loaded logs", text: $model.logQuery).textFieldStyle(.roundedBorder)
                        Button(model.followingLogs ? "Pause" : "Follow / Reconnect") { if model.followingLogs { model.pauseLogs() } else { model.followLogs() } }.disabled(model.busy)
                        Menu("Copy / Export") {
                            Button("Copy all loaded") { copy(model.logExportText(filtered: false)) }
                            Button("Copy filtered") { copy(model.logExportText(filtered: true)) }
                            Button("Export all loaded…") { model.exportLogs(filtered: false) }
                            Button("Export filtered…") { model.exportLogs(filtered: true) }
                        }.disabled(model.logs == nil)
                    }
                    Text(model.logStatus + (model.logTruncated ? " · earlier output discarded" : "")).font(.caption)
                    Text("Up to \(model.preferences.logLines) lines per stream, capped at 1 MB. Output clears when you switch containers or close this window.")
                        .font(.caption).foregroundStyle(Color(nsColor: palette.muted))
                    if model.logs != nil {
                        Text(model.filteredLogText.isEmpty ? "No matching output." : model.filteredLogText).font(.system(size: 11, design: .monospaced)).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading).padding(12)
                            .background(Color(nsColor: palette.ground).opacity(0.45), in: RoundedRectangle(cornerRadius: 8))
                    }
                    }
                }.padding(24).frame(maxWidth: .infinity, alignment: .leading)
            }
        } else {
            VStack(spacing: 14) {
                Image(systemName: "cube.transparent").font(.system(size: 42, weight: .light)).foregroundStyle(Color(nsColor: palette.muted))
                Text("Your local workloads, within reach.").font(.headline)
                Text("Select a container to see its status, ports, controls and recent logs.")
                    .font(.callout).foregroundStyle(Color(nsColor: palette.muted)).multilineTextAlignment(.center)
                if !model.notice.isEmpty { Text(model.notice).font(.caption).foregroundStyle(Color(nsColor: palette.muted)).multilineTextAlignment(.center) }
                Text("Install or start runtimes in their own apps. Blindspot does not pull images or start machines automatically.")
                    .font(.caption).foregroundStyle(Color(nsColor: palette.muted)).multilineTextAlignment(.center)
            }.padding(30)
        }
    }
    private var imageList: some View {
        VStack(spacing: 0) {
            TextField("Filter image name, tag or ID", text: $model.filter)
                .textFieldStyle(.plain).padding(12)
                .background(Color(nsColor: palette.ground).opacity(0.35), in: RoundedRectangle(cornerRadius: 7)).padding(12)
            if model.images.isEmpty {
                VStack(spacing: 12) {
                    Image(systemName: "square.stack.3d.up").font(.largeTitle)
                    Text(model.busy ? "Reading local images…" : "No local images to show")
                    Text("Images are templates. Containers are instances created from them.")
                        .font(.caption).foregroundStyle(Color(nsColor: palette.muted))
                }.padding(24).frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List(selection: $model.selectedImageID) {
                    ForEach(model.filteredImages) { image in
                        VStack(alignment: .leading, spacing: 5) {
                            Text(image.name).fontWeight(.medium).lineLimit(2)
                            Text(image.size).font(.caption).foregroundStyle(Color(nsColor: palette.muted))
                        }.padding(.vertical, 8).tag(image.id)
                            .listRowBackground(Color(nsColor: model.selectedImageID == image.id ? palette.selection : .clear))
                            .listRowSeparator(.hidden)
                    }
                }.listStyle(.plain).scrollContentBackground(.hidden).disabled(model.busy)
            }
        }
    }

    @ViewBuilder private var imageDetail: some View {
        if let image = model.selectedImage {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    Text(image.name).font(.title2.weight(.semibold)).textSelection(.enabled)
                    Text("Local image").foregroundStyle(Color(nsColor: palette.muted))
                    Grid(alignment: .leading, horizontalSpacing: 20, verticalSpacing: 12) {
                        info("Tags", image.references.isEmpty ? "Untagged" : image.references.joined(separator: "\n"))
                        info("Image ID", image.id)
                        info("Size", image.size)
                        info("Created", image.created)
                        info("Engine", model.endpoint?.title ?? "")
                    }.font(.callout)
                    HStack(spacing: 18) {
                        Button("Create container…") { model.createContainer() }.disabled(model.busy)
                        Button("Copy image ID") { copy(image.id) }
                    }
                    Text("Create a container from this installed image. Nothing is downloaded; its default startup command is used.")
                        .font(.caption).foregroundStyle(Color(nsColor: palette.muted))
                }.padding(24).frame(maxWidth: .infinity, alignment: .leading)
            }
        } else {
            Text("Select a local image to see its tags, size and creation options.")
                .foregroundStyle(Color(nsColor: palette.muted)).padding(30)
        }
    }

    private func copy(_ value: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(value, forType: .string)
    }

    private func info(_ label: String, _ value: String) -> some View {
        GridRow(alignment: .top) {
            Text(label).foregroundStyle(Color(nsColor: palette.muted))
            Text(value).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}
