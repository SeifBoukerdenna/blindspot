import AppKit
import Carbon.HIToolbox
import ServiceManagement

@main
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    /// `NSApplication.delegate` is a weak reference, so something has to own the
    /// delegate for longer than `run()`.
    private static let owner = AppDelegate()

    private var core: Core?
    private var panel: Panel?
    private var hotKey: HotKey?
    private var watcher: ClipboardWatcher?

    static func main() {
        let app = NSApplication.shared
        // Redundant with `LSUIElement` in the bundle's Info.plist, but it also makes the
        // binary behave when run straight out of the build directory with no bundle.
        app.setActivationPolicy(.accessory)
        app.mainMenu = MainMenu.make()
        app.delegate = owner
        app.run()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        guard let core = Core() else {
            die(
                "blindspot could not start.",
                "The core failed to initialise. Run it from a terminal to see why.")
            return
        }
        self.core = core

        let watcher = ClipboardWatcher(sink: core.clipSink)
        self.watcher = watcher
        IconCache.clipThumbnails = { [weak core] id in core?.clipContent(id, part: .thumbnail) }

        // Built here, once, and never on the hotkey path. The initial scan already
        // happened inside `Core()`, so the first invocation has a populated index.
        let panel = Panel(core: core, watcher: watcher)
        self.panel = panel

        let settings = core.settings
        reconcileLoginItem(wanted: settings.launch_at_login)

        let hotKey = HotKey(
            keyCode: settings.hotkey_key_code, modifiers: settings.hotkey_modifiers
        ) { [weak panel] in panel?.toggle() }
        self.hotKey = hotKey

        // Registered regardless; the alert only explains why it is silent for now. Whether
        // the registration starts firing on its own once Spotlight lets go is untested, so
        // the alert asks for a restart, which is certain to work.
        if hotKey.keyCode == UInt32(kVK_Space), hotKey.modifiers == UInt32(cmdKey),
            spotlightOwnsCommandSpace()
        {
            warnSpotlightOwnsCommandSpace()
        }

        guard hotKey.register() else {
            // Note this is *not* how a clash with another app shows up: Carbon returns
            // success for a combination another process already holds, and the hotkey
            // simply never fires. A failure here means Carbon refused outright.
            die(
                "blindspot could not register its hotkey.",
                "Carbon rejected the configured hotkey. If another copy of blindspot is "
                    + "running, quit it and try again.")
            return
        }

        // Deferred one main-loop turn so `applicationDidFinishLaunching` returns and the
        // hotkey is live before this spends ~40ms loading and flattening icons. Doing it
        // at all is what removes the 108ms stall the first panel used to pay.
        Task { @MainActor in
            IconCache.prewarm(core.allPaths())
        }

        // Last, once the hotkey is live. Watching reads no contents until the user next
        // copies something, so if a future macOS enforces the documented pasteboard alert,
        // it would appear then rather than at login.
        watcher.start()
    }

    func applicationWillTerminate(_ notification: Notification) {
        watcher?.stop()
        // `core` is deliberately *not* released. Its deinit runs `bs_shutdown`, which frees
        // the handle, while a detached task may still be encoding a screenshot it is about
        // to hand to `bs_clip_add` — a use-after-free with nothing to order the two. The
        // process is exiting and the kernel reclaims everything; the FFI tests already
        // prove `bs_shutdown` frees correctly, which is what the explicit teardown was for.
        panel?.close()
        hotKey = nil
    }

    /// Keeps the login item in step with config.toml on every launch — registering when it
    /// is wanted, removing it when it is not. `SMAppService` records the bundle's current
    /// path, so an app that has moved simply re-registers here next time.
    private func reconcileLoginItem(wanted: Bool) {
        let service = SMAppService.mainApp
        do {
            switch (wanted, service.status) {
            case (true, .notRegistered), (true, .notFound):
                try service.register()
            case (false, .enabled), (false, .requiresApproval):
                try service.unregister()
            default:
                break
            }
        } catch {
            NSLog("blindspot: login item: %@", error.localizedDescription)
        }
        if service.status == .requiresApproval {
            NSLog("blindspot: approve blindspot in System Settings > General > Login Items")
        }
    }

    /// Whether Spotlight's own ⌘Space binding (symbolic hotkey 64) is still live.
    ///
    /// Read, never written: rebinding Spotlight is a deliberate manual step. An absent entry
    /// means the system default, which is enabled on ⌘Space. A present one is checked for
    /// the chord itself, so someone who moved Spotlight to another key is not warned wrongly.
    private func spotlightOwnsCommandSpace() -> Bool {
        let all =
            CFPreferencesCopyAppValue(
                "AppleSymbolicHotKeys" as CFString, "com.apple.symbolichotkeys" as CFString)
            as? [String: Any]
        guard let spotlight = all?["64"] as? [String: Any] else { return true }
        guard spotlight["enabled"] as? Bool ?? true else { return false }
        let parameters = (spotlight["value"] as? [String: Any])?["parameters"] as? [Int]
        guard let parameters, parameters.count >= 3 else { return true }
        // [character, key code, NSEvent modifier flags]: 49 is Space, 1 << 20 is ⌘.
        return parameters[1] == kVK_Space && parameters[2] == 1 << 20
    }

    private func warnSpotlightOwnsCommandSpace() {
        let alert = NSAlert()
        alert.messageText = "Spotlight still owns \u{2318}Space."
        alert.informativeText =
            "blindspot is set to \u{2318}Space, but macOS gives it to Spotlight first, so "
            + "the hotkey will do nothing until you turn Spotlight's off: System Settings > "
            + "Keyboard > Keyboard Shortcuts > Spotlight. Then quit and reopen blindspot."
        NSApp.activate()
        alert.runModal()
    }

    private func die(_ message: String, _ detail: String) {
        NSApp.activate()
        let alert = NSAlert()
        alert.alertStyle = .critical
        alert.messageText = message
        alert.informativeText = detail
        alert.runModal()
        NSApp.terminate(nil)
    }
}
