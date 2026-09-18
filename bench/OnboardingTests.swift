import AppKit
import SwiftUI

private actor ProgressLog {
    var updates: [SetupDownloadProgress] = []
    func add(_ update: SetupDownloadProgress) { updates.append(update) }
    func completed() -> Bool { updates.contains { $0.status == "Download complete" } }
}

@main
@MainActor
enum OnboardingTests {
    static var checks = 0
    static func expect(_ value: @autoclosure () -> Bool, _ message: String) {
        precondition(value(), message)
        checks += 1
    }

    static func main() {
        if CommandLine.arguments.count == 3, CommandLine.arguments[1] == "--fresh-core" {
            let home = CommandLine.arguments[2]
            precondition(ProcessInfo.processInfo.environment["HOME"] == home)
            do {
                let (_, _, show) = try SetupStore.prepare(home: home)
                precondition(show)
                let config = URL(fileURLWithPath: home).appendingPathComponent("fixture.toml")
                try "app_paths = []\n".write(to: config, atomically: true, encoding: .utf8)
                guard let core = Core(configPath: config.path) else { fatalError("Fixture core failed") }
                precondition(!core.startup.clips_enabled && !core.startup.launch_at_login)
                precondition(core.contentState?.enabled == false && core.contentState?.roots.isEmpty == true)
                precondition(core.settings().first { $0.key == "agent.model" }?.value == "")
                print("Fresh core loaded prepared overrides; indexing, capture and startup disabled")
                exit(0)
            } catch { fatalError("Fresh core fixture: \(error)") }
        }
        precondition(ProcessInfo.processInfo.environment["HOME"] == nil, "Run with HOME unset; all stores use disposable fixtures")
        NSApplication.shared.setActivationPolicy(.accessory)
        Task { @MainActor in
            do {
                try lifecycle()
                try await network()
                try await screens()
                print("Onboarding: \(checks) assertions passed; lifecycle, preservation, model policy, bounded loopback requests, cloud aliases, download completion/cancellation, seven pages in light/dark; no live stores or models used")
                exit(0)
            } catch { fatalError("Onboarding fixture failed: \(error)") }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 90) { fatalError("Onboarding fixtures timed out") }
        NSApp.run()
    }

    static func lifecycle() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("blindspot-onboarding-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let fresh = root.appendingPathComponent("fresh")
        let (store, record, show) = try SetupStore.prepare(home: fresh.path)
        expect(show && !record.initializing, "Fresh install should show setup after preparation")
        let overrides = store.directory!.appendingPathComponent("overrides.toml")
        expect(tryString(overrides) == SetupStore.initialOverrides, "Prepare must disable reads before Core starts")
        let again = try SetupStore.prepare(home: fresh.path)
        expect(again.2, "An unpresented welcome resumes")
        var saved = record
        saved.presented = true
        saved.page = .ai
        try store.save(saved)
        let resumed = try SetupStore.prepare(home: fresh.path)
        expect(!resumed.2 && resumed.1.page == .ai, "Do not repeat welcome after presentation; retain page")
        try Data("existing preferences".utf8).write(to: overrides)
        _ = try SetupStore.prepare(home: fresh.path)
        expect(tryString(overrides) == "existing preferences", "Never rewrite returning users' overrides")

        let prior = root.appendingPathComponent("prior")
        let priorState = prior.appendingPathComponent(".local/share/blindspot")
        try FileManager.default.createDirectory(at: priorState, withIntermediateDirectories: true)
        try Data("fixture index".utf8).write(to: priorState.appendingPathComponent("content.sqlite"))
        let existing = try SetupStore.prepare(home: prior.path)
        expect(!existing.2, "Legacy data without marker is not a fresh install")
        expect(!FileManager.default.fileExists(atPath: priorState.appendingPathComponent("overrides.toml").path), "No legacy preference changes")
        expect(tryString(priorState.appendingPathComponent("content.sqlite")) == "fixture index", "Preserve old index")

        let configured = root.appendingPathComponent("configured")
        try FileManager.default.createDirectory(at: configured.appendingPathComponent(".config/blindspot"), withIntermediateDirectories: true)
        let configuredState = try SetupStore.prepare(home: configured.path)
        expect(configuredState.2 == false, "A config directory is enough to preserve behavior")
        let interrupted = SetupStore(directory: root.appendingPathComponent("interrupted/.local/share/blindspot"))
        try interrupted.save(SetupRecord(initializing: true))
        let recovered = try SetupStore.prepare(home: root.appendingPathComponent("interrupted").path)
        expect(!recovered.1.initializing && recovered.2, "Recover interruption before override creation")
        try interrupted.save(SetupRecord(initializing: true))
        try Data("changed".utf8).write(to: interrupted.directory!.appendingPathComponent("overrides.toml"))
        do {
            _ = try SetupStore.prepare(home: root.appendingPathComponent("interrupted").path)
            fatalError("Ambiguous interrupted settings must stop startup")
        } catch { checks += 1 }
        expect(tryString(interrupted.directory!.appendingPathComponent("overrides.toml")) == "changed", "Interrupted changes are retained")
        let homeless = try SetupStore.prepare(home: nil)
        expect(homeless.0.directory == nil, "HOME-less fixtures do not use live state")
        let child = Process()
        let childHome = root.appendingPathComponent("core-home").path
        child.executableURL = URL(fileURLWithPath: CommandLine.arguments[0]).standardizedFileURL
        child.arguments = ["--fresh-core", childHome]
        var environment = ProcessInfo.processInfo.environment
        environment["HOME"] = childHome
        child.environment = environment
        try child.run()
        child.waitUntilExit()
        expect(child.terminationStatus == 0, "Real core consumes fresh overrides before content configuration")
        for (gb, name) in [(8, "2b"), (16, "4b"), (24, "9b"), (32, "9b"), (48, "9b"), (128, "9b")] {
            expect(SetupModel.recommended(memory: UInt64(gb) * 1_073_741_824).id == "qwen3.5:" + name, "Conservative memory tier")
        }
        for host in ["example.com:11434", "127.0.0.1:0", "user@localhost:11434", "localhost:11434/path", "localhost:11434?secret", "localhost:65536"] {
            expect(SetupOllama.endpoint(host) == nil, "Refuse nonlocal or malformed endpoints")
        }
        expect(SetupOllama.endpoint("localhost:11434")?.host == "127.0.0.1", "Avoid DNS")
        expect(SetupOllama.endpoint("[::1]:11434") != nil, "Support IPv6 loopback")
        var local: [String: Any] = ["details": ["format": "gguf"], "model_info": ["architecture": "fixture"], "capabilities": ["completion"]]
        expect(SetupOllama.isLocal(local, capability: "completion"), "Local metadata accepted")
        expect(!SetupOllama.isLocal(local, capability: "embedding"), "Wrong capability refused")
        local["remote_host"] = "https://example.invalid"
        expect(!SetupOllama.isLocal(local, capability: "completion"), "Cloud alias rejected")
        expect(!SetupOllama.isLocal([:], capability: "completion"), "Missing locality metadata rejected")
    }

    static func tryString(_ url: URL) -> String { (try? String(contentsOf: url, encoding: .utf8)) ?? "" }

    static func network() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("blindspot-ollama-fixture-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let port = root.appendingPathComponent("port")
        let server = Process()
        server.executableURL = URL(fileURLWithPath: "/usr/bin/python3")
        server.arguments = ["bench/OnboardingServer.py", port.path]
        server.standardOutput = FileHandle.nullDevice
        server.standardError = FileHandle.nullDevice
        try server.run()
        defer { server.terminate(); server.waitUntilExit() }
        for _ in 0..<100 {
            if FileManager.default.fileExists(atPath: port.path) { break }
            try await Task.sleep(for: .milliseconds(20))
        }
        let client = try SetupOllama(host: "127.0.0.1:" + tryString(port))
        let names = try await client.models()
        expect(names == ["fixture:local"], "Filter remote entries")
        try await client.test("fixture:local")
        checks += 1
        try await client.test("fixture:local", embedding: true)
        checks += 1
        for name in ["alias", "redirect", "huge"] {
            do { try await client.test(name); fatalError("Unsafe response accepted: \(name)") }
            catch { checks += 1 }
        }
        let log = ProgressLog()
        try await client.pull(SetupModel.choices[2]) { await log.add($0) }
        let complete = await log.completed()
        expect(complete, "A successful pull emits completion")
        do { try await client.pull(SetupModel.choices[0]) { _ in }; fatalError("Truncated pull accepted") }
        catch { checks += 1 }
        let task = Task { try await client.pull(SetupModel.choices[1]) { _ in } }
        try await Task.sleep(for: .milliseconds(100))
        task.cancel()
        do { try await task.value; fatalError("Cancelled pull accepted") }
        catch { checks += 1 }
    }

    static func screens() async throws {
        let model = Onboarding(core: nil, store: SetupStore(directory: nil), record: SetupRecord(),
                               hardware: SetupHardware(chip: "Apple M3", memory: 16 * 1_073_741_824))
        model.values = ["hotkey": "cmd+shift+space", "content.enabled": "false"]
        model.show()
        let directory = URL(fileURLWithPath: "build/onboarding-screens")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        guard let window = model.window, let view = window.contentView else { fatalError("No onboarding window") }
        expect(window.isVisible && window.styleMask.contains(.resizable), "Native resizable window")
        for (name, appearance) in [("light", NSAppearance.Name.aqua), ("dark", .darkAqua)] {
            window.appearance = NSAppearance(named: appearance)
            for page in SetupPage.allCases {
                model.navigate(page)
                try await Task.sleep(for: .milliseconds(350))
                view.layoutSubtreeIfNeeded()
                expect(!view.hasAmbiguousLayout, "Unambiguous setup layout")
                try snapshot(model, dark: name == "dark", directory.appendingPathComponent("\(name)-\(page.rawValue).png"))
            }
        }
        model.navigate(.ai)
        model.connected = true
        model.installed = ["qwen3.5:4b"]
        model.aiStatus = "Ollama is ready · 1 installed model"
        try await Task.sleep(for: .milliseconds(350))
        try snapshot(model, dark: true, directory.appendingPathComponent("model-choice.png"))
        model.busy = true
        model.progress = SetupDownloadProgress(status: "Downloading model layer", completed: 1_200_000_000, total: 3_400_000_000)
        model.aiStatus = "Downloading model layer"
        try await Task.sleep(for: .milliseconds(350))
        try snapshot(model, dark: true, directory.appendingPathComponent("download.png"))
        model.busy = false
        model.receivedShortcut()
        expect(model.shortcutWorked, "Actual hotkey delivery is observable")
        window.performClose(nil)
        expect(!window.isVisible, "Closing setup does not end app")
        model.show()
        expect(window.isVisible && model.page == .ai, "Setup resumes current page")
        window.performClose(nil)
    }

    static func snapshot(_ model: Onboarding, dark: Bool, _ url: URL) throws {
        guard ProcessInfo.processInfo.environment["BLINDSPOT_CAPTURE_SETUP"] == "1", let window = model.window else { return }
        window.displayIfNeeded()
        let capture = Process()
        capture.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        capture.arguments = ["-x", "-o", "-l", String(window.windowNumber), url.path]
        try capture.run()
        capture.waitUntilExit()
        guard capture.terminationStatus == 0 else { throw SetupFailure("Native setup capture requires Screen Recording access for the test runner.") }
    }
}
