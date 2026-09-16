import AppKit
import CoreServices
import IOKit.ps
import os

@MainActor
protocol ContentMonitoring: AnyObject {
    var contentState: ContentRuntime? { get }
    func pauseContent(_ paused: Bool, reason: ContentPauseReason)
    func refreshContent()
    func refreshContent(paths: [String])
    func contentEventRelevant(_ path: String) -> Bool
    func recoverContentModel()
}

extension ContentMonitoring {
    func refreshContent(paths: [String]) { refreshContent() }
    func recoverContentModel() {}
}

extension Core: ContentMonitoring {}

@MainActor
final class ContentWatcher {
    fileprivate final class CallbackOwner {
        weak var watcher: ContentWatcher?
        init(_ watcher: ContentWatcher) { self.watcher = watcher }
    }

    private let core: any ContentMonitoring
    private let policyInterval: Duration
    private var stream: FSEventStreamRef?
    private var watched: [String] = []
    private var powerSource: CFRunLoopSource?
    private var powerOwner: CallbackOwner?
    private var observers: [NSObjectProtocol] = []
    private var debounce: Task<Void, Never>?
    private var polling: Task<Void, Never>?
    private var retry: Task<Void, Never>?
    private var policyTask: Task<Void, Never>?
    private var dirty = false
    private var fullScan = false
    private var changedPaths: Set<String> = []
    private var lastFlush: ContinuousClock.Instant?
    /// Event-driven passes start at least this far apart. A folder rewritten every second (a
    /// logger or data feed) otherwise keeps indexing permanently busy; the pass that follows
    /// still includes every change that arrived while waiting.
    private static let minimumInterval: Duration = .seconds(10)
    private let log = Logger(subsystem: "com.seifboukerdenna.blindspot", category: "ContentMonitor")
    private var enabled = false
    private var wasIndexing = false
    private(set) var receivedEvents = 0
    var isMonitoring: Bool { stream != nil && !watched.isEmpty }
    var onIndexChanged: (() -> Void)?

    init(core: any ContentMonitoring, policyInterval: Duration = .seconds(30)) {
        self.core = core
        self.policyInterval = policyInterval
    }
    isolated deinit { stop() }

    func configure() {
        guard let state = core.contentState, state.enabled else { stop(); return }
        if !enabled {
            enabled = true
            let center = NotificationCenter.default
            for name in [ProcessInfo.thermalStateDidChangeNotification, Notification.Name.NSProcessInfoPowerStateDidChange] {
                observers.append(center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                    MainActor.assumeIsolated { self?.applyPowerPolicy() }
                })
            }
            let owner = CallbackOwner(self)
            powerOwner = owner
            if let source = IOPSNotificationCreateRunLoopSource({ context in
                guard let context else { return }
                let owner = Unmanaged<CallbackOwner>.fromOpaque(context).takeUnretainedValue()
                MainActor.assumeIsolated { owner.watcher?.applyPowerPolicy() }
            }, Unmanaged.passUnretained(owner).toOpaque())?.takeRetainedValue() {
                powerSource = source
                CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
            }
        }
        synchronizeStream(state.watchRoots)
        applyPowerPolicy()
        startPolicyRefresh()
    }

    func stop() {
        enabled = false
        debounce?.cancel(); debounce = nil
        polling?.cancel(); polling = nil
        retry?.cancel(); retry = nil
        policyTask?.cancel(); policyTask = nil
        dirty = false; fullScan = false; changedPaths.removeAll()
        stopStream()
        if let powerSource { CFRunLoopSourceInvalidate(powerSource) }
        powerSource = nil
        powerOwner = nil
        observers.forEach(NotificationCenter.default.removeObserver)
        observers.removeAll()
        core.pauseContent(true, reason: .unavailable)
    }

    static func shouldPause(lowPower: Bool, thermal: ProcessInfo.ThermalState, onBattery: Bool, allowBattery: Bool) -> Bool {
        pauseReason(lowPower: lowPower, thermal: thermal, onBattery: onBattery,
            powerAvailable: true, allowBattery: allowBattery) != .none
    }

    static func pauseReason(lowPower: Bool, thermal: ProcessInfo.ThermalState, onBattery: Bool,
        powerAvailable: Bool, allowBattery: Bool) -> ContentPauseReason {
        if lowPower { return .lowPower }
        if thermal == .serious || thermal == .critical { return .thermal }
        if !powerAvailable && !allowBattery { return .unavailable }
        if onBattery && !allowBattery { return .battery }
        return .none
    }

    private func applyPowerPolicy() {
        guard enabled, let state = core.contentState else { return }
        let info = IOPSCopyPowerSourcesInfo()?.takeRetainedValue()
        let source = info.flatMap { IOPSGetProvidingPowerSourceType($0)?.takeUnretainedValue() } as String?
        let onBattery = source == kIOPSBatteryPowerValue
        let process = ProcessInfo.processInfo
        let reason = Self.pauseReason(lowPower: process.isLowPowerModeEnabled,
            thermal: process.thermalState, onBattery: onBattery,
            powerAvailable: source != nil, allowBattery: state.onBattery)
        core.pauseContent(reason != .none, reason: reason)
        startPolling()
    }

    private func startPolicyRefresh() {
        policyTask?.cancel()
        let interval = policyInterval
        policyTask = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                do { try await Task.sleep(for: interval) } catch { return }
                guard let self, self.enabled else { return }
                self.applyPowerPolicy()
                self.core.recoverContentModel()
            }
        }
    }

    private func startPolling() {
        polling?.cancel()
        wasIndexing = core.contentState?.indexing ?? false
        polling = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(1)) } catch { return }
                guard let self, self.enabled, let state = self.core.contentState else { return }
                self.synchronizeStream(state.watchRoots)
                if self.wasIndexing && !state.indexing { self.onIndexChanged?() }
                self.wasIndexing = state.indexing
                if !state.indexing {
                    self.polling = nil
                    if self.dirty { self.scheduleFlush() }
                    return
                }
            }
        }
    }

    private func filesystemChanged(paths: [String]? = nil) {
        guard enabled else { return }
        dirty = true
        if let paths, !fullScan {
            changedPaths.formUnion(paths)
            if changedPaths.count > 128 { fullScan = true; changedPaths.removeAll() }
        } else { fullScan = true; changedPaths.removeAll() }
        scheduleFlush()
    }

    private func scheduleFlush() {
        guard debounce == nil else { return }
        let sinceLast = lastFlush.map { ContinuousClock.now - $0 } ?? Self.minimumInterval
        let delay = max(.seconds(2), Self.minimumInterval - sinceLast)
        debounce = Task { @MainActor [weak self] in
            do { try await Task.sleep(for: delay) } catch { return }
            guard let self, self.enabled else { return }
            self.debounce = nil
            self.flushChanges()
        }
    }

    private func flushChanges() {
        guard enabled, dirty else { return }
        if core.contentState?.indexing == true { startPolling(); return }
        let paths = changedPaths.sorted()
        let all = fullScan || paths.isEmpty
        dirty = false; fullScan = false; changedPaths.removeAll()
        lastFlush = .now
        if all { core.refreshContent() } else { core.refreshContent(paths: paths) }
        startPolling()
    }

    private func retryMonitoring() {
        guard retry == nil else { return }
        log.error("Filesystem monitoring unavailable; retrying with reconciliation in 30 seconds")
        retry = Task { @MainActor [weak self] in
            do { try await Task.sleep(for: .seconds(30)) } catch { return }
            guard let self, self.enabled, let state = self.core.contentState else { return }
            self.retry = nil
            self.synchronizeStream(state.watchRoots)
            self.filesystemChanged()
        }
    }

    private func synchronizeStream(_ roots: [String]) {
        guard roots != watched else { return }
        stopStream()
        guard !roots.isEmpty else { return }
        let owner = CallbackOwner(self)
        defer { withExtendedLifetime(owner) {} }
        var context = FSEventStreamContext(version: 0,
            info: Unmanaged.passUnretained(owner).toOpaque(),
            retain: retainContentContext, release: releaseContentContext, copyDescription: nil)
        let flags = FSEventStreamCreateFlags(kFSEventStreamCreateFlagUseCFTypes | kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagWatchRoot)
        guard let stream = FSEventStreamCreate(kCFAllocatorDefault, { _, context, count, paths, flags, _ in
            guard let context else { return }
            let owner = Unmanaged<CallbackOwner>.fromOpaque(context).takeUnretainedValue()
            MainActor.assumeIsolated {
                guard let watcher = owner.watcher, watcher.enabled else { return }
                watcher.receivedEvents += count
                if count > 128 { watcher.filesystemChanged(); return }
                let array = Unmanaged<CFArray>.fromOpaque(paths).takeUnretainedValue()
                let dropped = FSEventStreamEventFlags(kFSEventStreamEventFlagMustScanSubDirs | kFSEventStreamEventFlagUserDropped | kFSEventStreamEventFlagKernelDropped | kFSEventStreamEventFlagRootChanged)
                var relevant: [String] = []
                for index in 0..<min(count, CFArrayGetCount(array)) {
                    if flags[index] & dropped != 0 { watcher.filesystemChanged(); return }
                    guard let value = CFArrayGetValueAtIndex(array, index) else { continue }
                    let path = Unmanaged<CFString>.fromOpaque(value).takeUnretainedValue() as String
                    if watcher.core.contentEventRelevant(path) { relevant.append(path) }
                }
                if !relevant.isEmpty { watcher.filesystemChanged(paths: relevant) }
            }
        }, &context, roots as CFArray, FSEventStreamEventId(kFSEventStreamEventIdSinceNow), 1.0, flags) else { retryMonitoring(); return }
        self.stream = stream
        FSEventStreamSetDispatchQueue(stream, DispatchQueue.main)
        if FSEventStreamStart(stream) {
            watched = roots
            retry?.cancel(); retry = nil
        } else { stopStream(); retryMonitoring() }
    }

    private func stopStream() {
        if let stream {
            FSEventStreamStop(stream)
            FSEventStreamInvalidate(stream)
            FSEventStreamRelease(stream)
        }
        stream = nil
        watched = []
    }

}

    nonisolated private func retainContentContext(_ pointer: UnsafeRawPointer?) -> UnsafeRawPointer? {
        guard let pointer else { return nil }
        return UnsafeRawPointer(Unmanaged<ContentWatcher.CallbackOwner>.fromOpaque(pointer).retain().toOpaque())
    }

    nonisolated private func releaseContentContext(_ pointer: UnsafeRawPointer?) {
        guard let pointer else { return }
        Unmanaged<ContentWatcher.CallbackOwner>.fromOpaque(pointer).release()
    }
