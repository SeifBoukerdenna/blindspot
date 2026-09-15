import AppKit

@MainActor
private final class FixtureCore: ContentMonitoring {
    let root: String
    var refreshes = 0
    var pauses: [Bool] = []
    init(root: String) { self.root = root }
    var contentState: ContentRuntime? {
        ContentRuntime(enabled: true, onBattery: true, roots: [root], watchRoots: [root], indexing: false, status: "Fixture")
    }
    func pauseContent(_ paused: Bool, reason: ContentPauseReason) { pauses.append(paused) }
    func refreshContent() { refreshes += 1 }
    func contentEventRelevant(_ path: String) -> Bool {
        return path.hasPrefix(root + "/") && !path.hasPrefix(root + "/.ignored")
    }
}

@main
struct ContentWatcherTests {
    @MainActor
    static func main() {
        Task { @MainActor in
            do { try await run(); exit(0) }
            catch { FileHandle.standardError.write(Data("Content monitor test failed: \(error)\n".utf8)); exit(1) }
        }
        CFRunLoopRun()
    }

    @MainActor
    private static func run() async throws {
        precondition(!ContentWatcher.shouldPause(lowPower: false, thermal: .nominal, onBattery: false, allowBattery: false))
        precondition(ContentWatcher.pauseReason(lowPower: false, thermal: .nominal, onBattery: false, powerAvailable: false, allowBattery: false) == .unavailable)
        precondition(ContentWatcher.pauseReason(lowPower: false, thermal: .nominal, onBattery: false, powerAvailable: false, allowBattery: true) == .none)
        precondition(ContentWatcher.shouldPause(lowPower: false, thermal: .nominal, onBattery: true, allowBattery: false))
        precondition(!ContentWatcher.shouldPause(lowPower: false, thermal: .fair, onBattery: true, allowBattery: true))
        precondition(ContentWatcher.shouldPause(lowPower: true, thermal: .nominal, onBattery: false, allowBattery: true))
        precondition(ContentWatcher.shouldPause(lowPower: false, thermal: .serious, onBattery: false, allowBattery: true))
        precondition(ContentWatcher.shouldPause(lowPower: false, thermal: .critical, onBattery: false, allowBattery: true))

        let fm = FileManager.default
        let root = fm.temporaryDirectory.appendingPathComponent("blindspot-fsevents-" + UUID().uuidString)
        try fm.createDirectory(at: root.appendingPathComponent(".ignored"), withIntermediateDirectories: true)
        guard let resolved = root.path.withCString({ realpath($0, nil) }) else {
            throw CocoaError(.fileReadUnknown)
        }
        let canonical = String(cString: resolved)
        free(resolved)
        defer { try? fm.removeItem(at: root) }
        let core = FixtureCore(root: canonical)
        var watcher: ContentWatcher? = ContentWatcher(core: core)
        weak let lifetime = watcher
        watcher?.configure()
        precondition(watcher?.isMonitoring == true, "The native event stream must start")
        try await Task.sleep(for: .seconds(1))
        try Data("excluded".utf8).write(to: root.appendingPathComponent(".ignored/cache.txt"))
        try await Task.sleep(for: .seconds(4))
        precondition(core.refreshes == 0, "Excluded writes must not trigger indexing")
        for index in 0..<8 {
            try Data("fixture".utf8).write(to: root.appendingPathComponent("note\(index).txt"))
        }
        let deadline = ContinuousClock.now.advanced(by: .seconds(10))
        while core.refreshes == 0 && ContinuousClock.now < deadline { try await Task.sleep(for: .milliseconds(100)) }
        if core.refreshes != 1 {
            FileHandle.standardError.write(Data("refreshes=\(core.refreshes) events=\(watcher?.receivedEvents ?? -1) monitoring=\(watcher?.isMonitoring ?? false)\n".utf8))
        }
        precondition(core.refreshes == 1, "Native FSEvents must deliver and coalesce relevant changes")
        watcher?.stop()
        watcher = nil
        precondition(lifetime == nil, "Watcher callbacks must not retain the owner")
        try Data("after stop".utf8).write(to: root.appendingPathComponent("stopped.txt"))
        try await Task.sleep(for: .seconds(4))
        precondition(core.refreshes == 1, "Stopped watchers must not deliver refreshes")
        precondition(core.pauses.last == true)
        print("Content monitoring: 6 resource-policy checks; native exclusion, event coalescing, stop, and owner release passed")
    }
}
