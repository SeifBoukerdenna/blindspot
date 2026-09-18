import AppKit

/// README screenshots and GIFs of the shipping panel: `make media`.
///
/// scripts/capture-media.sh prepares a throwaway `$HOME` (/Users/Shared/Demo) holding invented
/// documents and a demo config, then runs this inside a sandbox that denies Spotlight.
///
/// Everything shown comes from that home: the clipboard is a private pasteboard, clips and
/// snippets are seeded here, and a full-screen backdrop window hides the desktop and every other
/// app. Recent files and plain-word file search query Spotlight across the whole disk, not
/// `$HOME`, so the harness first proves Spotlight is unreachable. It then checks every row it
/// captures and deletes the capture if a path from outside the demo home ever appears.
@main
@MainActor
enum MediaCapture {
    private static var director: Director?

    static func main() {
        let home = ProcessInfo.processInfo.environment["HOME"] ?? ""
        guard home == "/Users/Shared/Demo" else {
            FileHandle.standardError.write(Data("error: run through scripts/capture-media.sh; HOME must be a throwaway demo home\n".utf8))
            exit(2)
        }
        let application = NSApplication.shared
        application.setActivationPolicy(.accessory)
        let director = Director()
        Self.director = director
        application.delegate = director
        application.run()
    }
}

private struct Failure: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}

@MainActor
private final class Director: NSObject, NSApplicationDelegate {
    private let environment = ProcessInfo.processInfo.environment
    private lazy var output = URL(fileURLWithPath: environment["MEDIA_OUT"] ?? "media")
    private lazy var work = URL(fileURLWithPath: environment["MEDIA_WORK"] ?? "build/media")
    private lazy var useModel = environment["MEDIA_AI"] == "1" && wanted("ask-your-documents")
    /// MEDIA_ONLY=time-zones,units re-takes part of the set without disturbing the rest.
    private lazy var only = Set((environment["MEDIA_ONLY"] ?? "").split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) })

    private func wanted(_ name: String) -> Bool { only.isEmpty || only.contains(name) }

    private var core: Core!
    private var watcher: ClipboardWatcher!
    private var panel: Panel!
    private var field: NSTextField!
    private var editor: NSTextView!
    private var results: ResultsView!
    private var backdrop: NSWindow!
    private var failures: [String] = []

    func applicationDidFinishLaunching(_ notification: Notification) {
        Task { @MainActor in
            do {
                try await self.run()
            } catch {
                print("error: \(error)")
                exit(1)
            }
            exit(self.failures.isEmpty ? 0 : 1)
        }
    }

    private func run() async throws {
        // Phase one runs outside the Spotlight sandbox: the extraction helper applies its own
        // no-network sandbox, which macOS refuses to nest, and it reports every PDF and Word file
        // as unreadable when nested. This phase only builds the demo index. It shows no window,
        // captures nothing and makes no queries that reach Spotlight.
        if environment["MEDIA_PHASE"] == "index" {
            guard let core = Core() else { throw Failure("core did not start") }
            self.core = core
            try await waitForIndex()
            print("demo index ready")
            return
        }
        try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: work, withIntermediateDirectories: true)
        NSApp.activate()
        showBackdrop()
        guard let screen = NSScreen.main else { throw Failure("no screen") }
        CGWarpMouseCursorPosition(CGPoint(x: screen.frame.width - 4, y: screen.frame.height - 4))

        guard let core = Core() else { throw Failure("core did not start") }
        self.core = core
        seed()
        watcher = ClipboardWatcher(sink: core.clipSink, pasteboard: NSPasteboard.withUniqueName())
        panel = Panel(core: core, watcher: watcher)

        try await proveSpotlightIsBlocked()
        print("waiting for the demo index…")
        try await waitForIndex()
        var answerTime: Duration = .seconds(12)
        if useModel {
            print("warming the local model…")
            answerTime = try await warmModel()
            print("model answered in \(answerTime)")
        }

        panel.show()
        try bindPanel()
        try await Task.sleep(for: .milliseconds(600))

        await scene("search-inside-documents") {
            try await self.setQuery("documents about northwind")
            try await self.settle(minimum: 3)
            try await self.still("search-inside-documents")
        }
        await scene("clipboard-history") {
            try await self.setQuery(";")
            try await self.settle(minimum: 4)
            try await self.still("clipboard-history")
        }
        await scene("calculator") {
            try await self.setQuery("1500MB")
            try await self.settle(minimum: 1)
            try await self.still("calculator")
        }
        await scene("time-zones") {
            try await self.setQuery("3pm montreal in tokyo")
            try await self.settle(minimum: 3)
            try await self.still("time-zones")
        }
        await scene("units") {
            try await self.setQuery("5 ft 11 in")
            try await self.settle(minimum: 2)
            try await self.still("units")
        }
        await scene("system-commands") {
            try await self.setQuery(":system")
            try await self.settle(minimum: 2)
            try await self.still("system-commands")
        }
        await scene("snippets") {
            try await self.setQuery("!")
            try await self.settle(minimum: 3)
            try await self.still("snippets")
        }
        await scene("commands") {
            try await self.setQuery(":")
            try await self.settle(minimum: 5)
            try await self.still("commands")
        }
        await scene("launcher") {
            try await self.setQuery("")
            try await Task.sleep(for: .milliseconds(400))
            try await self.record("launcher", seconds: 17, height: 640) {
                try await self.type("cal")
                try await self.hold(1.4)
                try await self.erase()
                try await self.type("documents about northwind")
                try await self.hold(1.8)
                try await self.down(2)
                try await self.hold(0.8)
                try await self.erase()
                try await self.type("1500MB")
                try await self.hold(1.6)
                try await self.erase()
                try await self.type(";")
                try await self.hold(1.2)
                try await self.down(2)
                try await self.hold(1.2)
            }
        }
        if useModel {
            await scene("ask-your-documents") {
                try await self.setQuery("")
                try await Task.sleep(for: .milliseconds(400))
                let question = ">docs When is the Northwind renewal due?"
                let seconds = Int((Double(question.count) * 0.05 + answerTime.seconds * 1.6 + 6).rounded(.up))
                try await self.record("ask-your-documents", seconds: seconds, height: 600) {
                    try await self.type(question, every: .milliseconds(45))
                    try await self.hold(0.7)
                    try await self.returnKey()
                    try await self.waitForAnswer(question)
                    try await self.hold(2.5)
                }
                try await self.still("ask-your-documents")
            }
        }
        await scene("index-dashboard") {
            try await self.dashboard()
        }
        panel.standDown()
        await scene("onboarding-ai") { try await self.onboardingScene() }
        await scene("containers") { try await self.containerScenes() }
    }

    private func onboardingScene() async throws {
        backdrop.level = .normal; backdrop.orderFrontRegardless()
        let setup = Onboarding(core: nil, store: SetupStore(directory: nil), record: SetupRecord(),
            hardware: SetupHardware(chip: "Apple M3", memory: 16 * 1_073_741_824))
        setup.values = ["hotkey": "cmd+shift+space", "content.enabled": "false"]
        setup.show(); setup.navigate(.ai)
        setup.connected = true; setup.installed = ["qwen3.5:4b"]
        setup.aiStatus = "Ollama is ready · 1 installed model"
        guard let window = setup.window else { throw Failure("onboarding window did not open") }
        window.center(); window.level = .floating
        window.makeKeyAndOrderFront(nil); window.orderFrontRegardless()
        try await Task.sleep(for: .milliseconds(500))
        guard window.isVisible else { throw Failure("onboarding window is not visible") }
        try await still("onboarding-ai", window: window)
        window.performClose(nil)
    }

    private func containerScenes() async throws {
        backdrop.level = .normal; backdrop.orderFrontRegardless()
        let model = ContainerWindow()
        let endpoint = ContainerEndpoint(engine: .docker, name: "Demo engine", socket: "unix:///tmp/blindspot-media-demo.sock", executable: URL(fileURLWithPath: "/nonexistent/demo-docker"))
        let row = LocalContainer(id: String(repeating: "a", count: 64), name: "harbor-api", image: "harbor/api:dev", state: "running", status: "Up 12 minutes", ports: "127.0.0.1:8080 → 80/tcp")
        let image = LocalContainerImage(id: String(repeating: "b", count: 64), references: ["harbor/api:dev"], size: "42 MB", created: "2026-09-18")
        model.endpoints = [endpoint]; model.endpointID = endpoint.id
        model.rows = [row]; model.selectedID = row.id
        model.inspection = ContainerDetails(fields: ["Health: healthy", "Restart policy: unless-stopped", "Networks: bridge", "Mount: demo_data → /data"], localPorts: [8080])
        model.status = "Demo fixture · 1 container · 1 running"
        model.show(discover: false)
        guard let window = model.window else { throw Failure("container window did not open") }
        window.center()
        try await Task.sleep(for: .milliseconds(500))
        try await still("containers", window: window)
        model.detailTab = "Logs"
        model.logs = "2026-09-18T10:00:00Z Server listening on port 80\n2026-09-18T10:00:01Z Health check passed\n2026-09-18T10:00:04Z GET /api/projects 200 (12 ms)\n2026-09-18T10:00:07Z GET /api/projects/harbor 200 (8 ms)"
        model.logStatus = "Paused · demo log fixture"
        try await Task.sleep(for: .milliseconds(300))
        try await still("container-logs", window: window)
        model.section = .images; model.images = [image]; model.selectedImageID = image.id
        model.createContainer()
        guard let creation = model.creation, let sheet = creation.sheet else { throw Failure("creation sheet did not open") }
        creation.name = "harbor-api-dev"; creation.hostPort = "8081"; creation.containerPort = "80"
        creation.cpu = "2"; creation.memory = "512"; creation.restart = "unless-stopped"
        creation.fileValues = ["MODE":"development", "API_TOKEN":"demo-only"]
        creation.variables = [ContainerVariable(name: "MODE", value: "local")]
        creation.mounts = [ContainerMount(source: "demo_data", destination: "/data", volume: true)]
        creation.reviewConfiguration()
        guard creation.review else { throw Failure("demo creation did not validate") }
        try await Task.sleep(for: .milliseconds(500))
        try await still("container-create", window: sheet)
        creation.finish(false)
        window.performClose(nil)
    }

    // MARK: - Privacy

    /// Where a captured row's path may point: the demo home and Apple's own apps.
    private lazy var allowedRoots = [environment["HOME"] ?? "/nonexistent", "/System/Applications"]

    /// Rows whose path is outside `allowedRoots`, described without the path itself so a failure
    /// never prints private file names either.
    private func leaks(_ matches: [Match]) -> [String] {
        matches.compactMap { match in
            guard match.path.hasPrefix("/") || match.path.hasPrefix("~") else { return nil }
            let path = (match.path as NSString).expandingTildeInPath
            return allowedRoots.contains { path == $0 || path.hasPrefix($0 + "/") } ? nil : "a \(match.kind) row"
        }
    }

    /// The empty launcher (recent files) and a plain word (file search) are exactly the queries
    /// that showed real files before the sandbox existed. If either returns a path outside the
    /// demo home, stop before capturing anything.
    private func proveSpotlightIsBlocked() async throws {
        for query in ["", "sleep", "?a"] {
            _ = core.query(query, limit: 50)
            try await Task.sleep(for: .seconds(2))
            let found = leaks(core.query(query, limit: 50).0)
            guard found.isEmpty else {
                throw Failure("\(found.count) result(s) outside the demo home for a Spotlight query; run through scripts/capture-media.sh so the sandbox applies. Nothing was captured.")
            }
        }
        print("Spotlight is unreachable: only demo files can appear")
    }

    // MARK: - Demo state

    private func seed() {
        let clips = [
            "brew install ollama",
            "SELECT name, renewal_date FROM accounts WHERE status = 'active';",
            "Alex Martin · alex.martin@northwind.example",
            "https://github.com/SeifBoukerdenna/blindspot/releases/latest",
            "git log --oneline --graph --decorate",
            "Tracking: 1Z 999 AA1 01 2345 6784",
            "Northwind renewal call · Thursday 10:30 · meet.example.com/northwind",
        ]
        for clip in clips {
            core.clipSink.add(image: false, content: Data(clip.utf8))
        }
        _ = core.saveSnippet(keyword: "sig", text: "Best regards,\nAlex Martin")
        _ = core.saveSnippet(keyword: "addr", text: "1250 Rue University, Montréal, QC")
        _ = core.saveSnippet(keyword: "ty", text: "Thanks so much, talk soon!")
        _ = core.applyShortcut(":link gh https://github.com/search?q={query}")
    }

    private func showBackdrop() {
        guard let screen = NSScreen.main else { return }
        let window = NSWindow(contentRect: screen.frame, styleMask: .borderless, backing: .buffered, defer: false)
        window.level = NSWindow.Level(rawValue: NSWindow.Level.floating.rawValue - 1)
        window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        window.ignoresMouseEvents = true
        window.isOpaque = true
        window.contentView = Backdrop(frame: NSRect(origin: .zero, size: screen.frame.size))
        window.setFrame(screen.frame, display: true)
        window.orderFrontRegardless()
        backdrop = window
    }

    private func bindPanel() throws {
        guard let field = panel.contentView?.subviews.compactMap({ $0 as? NSTextField }).first(where: { $0.isEditable }),
              let results = panel.contentView?.subviews.compactMap({ $0 as? ResultsView }).first,
              let editor = field.currentEditor() as? NSTextView else { throw Failure("panel is not keyboard ready") }
        self.field = field
        self.results = results
        self.editor = editor
    }

    /// One word that appears only in the body of each demo PDF or Word file, never in a file name
    /// or plain-text note. All three being searchable proves extraction worked, so an answer or
    /// dashboard is never captured over documents that silently failed to index.
    private static let extractedWords = ["licensing", "escalations", "payable"]

    /// The app's ContentWatcher reports power state (indexing starts paused until it does) and
    /// asks for passes. The harness has no watcher, and a handful of demo files needs no power
    /// policy, so it lifts the pause and requests passes itself.
    private func waitForIndex() async throws {
        core.pauseContent(false, reason: .none)
        core.refreshContent()
        for attempt in 0..<600 {
            var missing = 0
            for word in Self.extractedWords {
                if try await !searchable(word) { missing += 1 }
            }
            let state = core.contentState
            if missing == 0, state?.indexing == false { return }
            if attempt % 10 == 9 {
                print("  index: \(missing) PDF/Word document(s) not searchable yet; \(state?.status ?? "no state")")
                if state?.indexing == false { core.refreshContent() }
            }
            try await Task.sleep(for: .milliseconds(300))
        }
        throw Failure("the demo PDF and Word text never became searchable, so the captures would be misleading")
    }

    /// Whether a content search for `word` finds a file. The core runs one content search at a
    /// time and a different query cancels the one in flight, so each word is polled until its
    /// own search finishes before the next is asked.
    private func searchable(_ word: String) async throws -> Bool {
        for _ in 0..<50 {
            let (matches, pending) = core.query(":content \(word)", limit: 5)
            if !pending { return matches.contains { $0.kind == .file } }
            try await Task.sleep(for: .milliseconds(100))
        }
        return false
    }

    /// A first question loads the model, so the recorded answer shows normal speed. It asks
    /// something different from the recorded question so no answer is already on screen.
    private func warmModel() async throws -> Duration {
        let question = "docs What is the total due on invoice INV-2041?"
        let started = ContinuousClock.now
        core.agentSubmit(question)
        try await Task.sleep(for: .milliseconds(500))
        for _ in 0..<720 {
            if !core.query(">" + question, limit: 50).1 { return ContinuousClock.now - started }
            try await Task.sleep(for: .milliseconds(250))
        }
        throw Failure("the local model did not answer within three minutes")
    }

    // MARK: - Driving the panel

    private func setQuery(_ text: String) async throws {
        field.stringValue = text
        panel.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
        editor.setSelectedRange(NSRange(location: (text as NSString).length, length: 0))
        try await Task.sleep(for: .milliseconds(150))
    }

    private func type(_ text: String, every interval: Duration = .milliseconds(70)) async throws {
        for character in text {
            editor.insertText(String(character), replacementRange: editor.selectedRange())
            try await Task.sleep(for: interval)
        }
    }

    private func erase() async throws {
        editor.selectAll(nil)
        try await Task.sleep(for: .milliseconds(180))
        editor.deleteBackward(nil)
        try await Task.sleep(for: .milliseconds(350))
    }

    private func down(_ count: Int) async throws {
        for _ in 0..<count {
            _ = panel.control(field, textView: editor, doCommandBy: #selector(NSResponder.moveDown(_:)))
            try await Task.sleep(for: .milliseconds(380))
        }
    }

    private func returnKey() async throws {
        _ = panel.control(field, textView: editor, doCommandBy: #selector(NSResponder.insertNewline(_:)))
        try await Task.sleep(for: .milliseconds(300))
    }

    private func hold(_ seconds: Double) async throws {
        try await Task.sleep(for: .milliseconds(Int(seconds * 1000)))
    }

    private func settle(minimum: Int, quiet: Duration = .milliseconds(900), timeout: Duration = .seconds(25)) async throws {
        let clock = ContinuousClock()
        let deadline = clock.now + timeout
        var last = results.matches
        var since = clock.now
        while clock.now < deadline {
            try await Task.sleep(for: .milliseconds(100))
            let current = results.matches
            if current != last {
                last = current
                since = clock.now
            } else if current.count >= minimum, clock.now - since >= quiet {
                return
            }
        }
        throw Failure("results did not settle (\(results.matches.count) rows)")
    }

    private func waitForAnswer(_ query: String) async throws {
        try await Task.sleep(for: .milliseconds(600))
        for _ in 0..<720 {
            if !core.query(query, limit: 50).1 { break }
            try await Task.sleep(for: .milliseconds(250))
        }
        try await settle(minimum: 2, quiet: .milliseconds(1200), timeout: .seconds(20))
    }

    // MARK: - Capture

    private func scene(_ name: String, _ body: @MainActor () async throws -> Void) async {
        guard wanted(name) else { return }
        do {
            try await body()
            print("captured \(name)")
        } catch {
            failures.append(name)
            print("scene \(name) failed: \(error)")
        }
    }

    /// Top-left-origin points around the panel, with room for its shadow. Stills use the
    /// panel's current height; recordings pass a fixed one because the panel grows as rows arrive.
    private func region(around window: NSWindow, height: CGFloat? = nil, margin: CGFloat = 44) -> String {
        let screen = window.screen ?? NSScreen.main!
        let frame = window.frame
        let fullHeight = (height ?? frame.height) + margin * 2
        let top = screen.frame.maxY - frame.maxY - margin
        return [frame.minX - margin, top, frame.width + margin * 2, fullHeight]
            .map { String(Int($0.rounded())) }
            .joined(separator: ",")
    }

    private func still(_ name: String, window: NSWindow? = nil) async throws {
        // Without the insertion point, macOS's caret-attached hints (a Universal Clipboard offer,
        // say) cannot float over the panel in a published screenshot.
        if window == nil { panel.makeFirstResponder(nil) }
        try await Task.sleep(for: .milliseconds(400))
        if window == nil, let found = leaks(results.matches).first {
            throw Failure("refusing to capture: \(found) points outside the demo home")
        }
        let target = output.appendingPathComponent("\(name).png").path
        let process = try launch(["-x", "-t", "png", "-R", region(around: window ?? panel), target])
        try await finish(process, "screenshot \(name)")
        if window == nil { panel.makeFirstResponder(field) }
    }

    private func record(_ name: String, seconds: Int, height: CGFloat, _ body: @MainActor () async throws -> Void) async throws {
        let target = work.appendingPathComponent("\(name).mov").path
        let process = try launch(["-x", "-v", "-V", String(seconds), "-R", region(around: panel, height: height), target])
        // screencapture takes about a second to start recording; the script trims this lead-in.
        try await Task.sleep(for: .milliseconds(1500))
        let started = ContinuousClock.now
        var leaked = false
        let watchdog = Task { @MainActor in
            while process.isRunning, !Task.isCancelled {
                if !self.leaks(self.results.matches).isEmpty { leaked = true }
                try? await Task.sleep(for: .milliseconds(50))
            }
        }
        defer { watchdog.cancel() }
        try await body()
        if process.isRunning == false {
            print("warning: \(name) ran \(ContinuousClock.now - started), longer than its \(seconds) s recording")
        }
        try await finish(process, "recording \(name)")
        if leaked {
            try? FileManager.default.removeItem(atPath: target)
            throw Failure("deleted the recording: a row pointed outside the demo home while recording")
        }
    }

    private func launch(_ arguments: [String]) throws -> Process {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        process.arguments = arguments
        try process.run()
        return process
    }

    private func finish(_ process: Process, _ what: String) async throws {
        while process.isRunning { try await Task.sleep(for: .milliseconds(100)) }
        guard process.terminationStatus == 0 else { throw Failure("\(what) exited \(process.terminationStatus)") }
    }

    private func dashboard() async throws {
        panel.standDown()
        backdrop.level = .normal
        backdrop.orderFrontRegardless()
        let settings = SettingsWindow(core: core)
        settings.show()
        try await Task.sleep(for: .milliseconds(800))
        guard let window = NSApp.windows.first(where: { $0.isVisible && $0 !== backdrop && $0 !== panel && $0.frame.width > 300 }) else {
            throw Failure("settings window did not open")
        }
        window.center()
        window.orderFrontRegardless()
        guard open(page: "Index", in: window) else {
            throw Failure("no Index page control found")
        }
        // The dashboard samples the index on a timer; give it time to fill in.
        try await Task.sleep(for: .seconds(4))
        try await still("index-dashboard", window: window)
        window.close()
    }

    private func open(page title: String, in window: NSWindow) -> Bool {
        guard let content = window.contentView,
              let button = find(in: content, where: { ($0 as? NSButton)?.identifier?.rawValue == "settings.section." + title }) as? NSButton else { return false }
        button.performClick(nil)
        return true
    }

    private func find(in view: NSView, where matches: (NSView) -> Bool) -> NSView? {
        if matches(view) { return view }
        for subview in view.subviews {
            if let found = find(in: subview, where: matches) { return found }
        }
        return nil
    }
}

private extension Duration {
    var seconds: Double {
        let (seconds, attoseconds) = components
        return Double(seconds) + Double(attoseconds) / 1e18
    }
}

/// A calm wallpaper stand-in, so the glass panel has something to refract that is not the
/// user's desktop.
private final class Backdrop: NSView {
    override func draw(_ dirtyRect: NSRect) {
        NSGradient(colors: [
            NSColor(srgbRed: 0.07, green: 0.08, blue: 0.16, alpha: 1),
            NSColor(srgbRed: 0.16, green: 0.10, blue: 0.24, alpha: 1),
            NSColor(srgbRed: 0.05, green: 0.14, blue: 0.22, alpha: 1),
        ])?.draw(in: bounds, angle: -30)
        let glows: [(CGFloat, CGFloat, CGFloat, NSColor)] = [
            (0.30, 0.72, 0.42, NSColor(srgbRed: 0.95, green: 0.45, blue: 0.30, alpha: 0.55)),
            (0.72, 0.62, 0.38, NSColor(srgbRed: 0.35, green: 0.45, blue: 0.95, alpha: 0.50)),
            (0.52, 0.25, 0.45, NSColor(srgbRed: 0.20, green: 0.75, blue: 0.70, alpha: 0.35)),
        ]
        for (x, y, radius, color) in glows {
            let center = NSPoint(x: bounds.width * x, y: bounds.height * y)
            NSGradient(starting: color, ending: color.withAlphaComponent(0))?
                .draw(fromCenter: center, radius: 0, toCenter: center, radius: bounds.width * radius, options: [])
        }
    }
}
