import AppKit

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
        app.mainMenu = makeMainMenu()
        app.delegate = owner
        app.run()
    }

    /// With no main menu at all, Cmd+Q does nothing — quitting is normally satisfied by
    /// the standard Quit item's key equivalent, and an accessory app has no menu bar to
    /// hold one. A two-item menu is less plumbing than intercepting the keystroke, and
    /// it picks up Cmd+H on the same principle rather than as a special case.
    private static func makeMainMenu() -> NSMenu {
        let appMenu = NSMenu()
        appMenu.addItem(
            withTitle: "Hide blindspot",
            action: #selector(NSApplication.hide(_:)),
            keyEquivalent: "h")
        appMenu.addItem(
            withTitle: "Quit blindspot",
            action: #selector(NSApplication.terminate(_:)),
            keyEquivalent: "q")

        let appItem = NSMenuItem()
        appItem.submenu = appMenu
        let main = NSMenu()
        main.addItem(appItem)
        return main
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

        let hotKey = HotKey { [weak panel] in panel?.toggle() }
        self.hotKey = hotKey

        guard hotKey.register() else {
            // Note this is *not* how a clash with another app shows up: Carbon returns
            // success for a combination another process already holds, and the hotkey
            // simply never fires. A failure here means Carbon refused outright.
            die(
                "blindspot could not register its hotkey.",
                "Carbon rejected \u{2318}\u{21E7}Space. If another copy of blindspot is "
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
