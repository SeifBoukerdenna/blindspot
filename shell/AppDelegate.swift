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
    /// The optional second hotkey, which opens the agent directly.
    private var agentHotKey: HotKey?
    private var watcher: ClipboardWatcher?
    /// Built once and reused: re-reading the settings is cheap, rebuilding a window is not.
    private var settingsWindow: SettingsWindow?
    /// The way in that cannot break. See `StatusItem`.
    private var statusItem: StatusItem?
    /// Live only while the configured hotkey is one Spotlight is holding.
    private var conflictWatcher: Task<Void, Never>?

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

        let window = SettingsWindow(core: core)
        window.onChange = { [weak self] in self?.applySettings() }
        window.onPalette = { [weak self] in self?.rebuildPanel() }
        settingsWindow = window
        statusItem = StatusItem(
            onSettings: { [weak self] in self?.openSettings() },
            onReindex: { [weak core] in core?.reindex() })

        let settings = core.startup
        reconcileLoginItem(wanted: settings.launch_at_login)

        let hotKey = HotKey(
            keyCode: settings.hotkey_key_code, modifiers: settings.hotkey_modifiers, id: 1
            // `[weak self]`, not `[weak panel]`: this closure outlives any one panel, and
            // a palette change rebuilds it. Capturing the object would leave the hotkey
            // firing at a window that is no longer on screen.
        ) { [weak self] in self?.panel?.toggle() }
        self.hotKey = hotKey

        let wantsCommandSpace =
            hotKey.keyCode == UInt32(kVK_Space) && hotKey.modifiers == UInt32(cmdKey)

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

        // A second chord for the agent, when config.toml asks for one. Its failure is not
        // fatal the way the main hotkey's is: blindspot still opens, and `>` still works.
        if settings.agent_hotkey_key_code != 0 || settings.agent_hotkey_modifiers != 0 {
            let agentHotKey = HotKey(
                keyCode: settings.agent_hotkey_key_code,
                modifiers: settings.agent_hotkey_modifiers,
                id: 2
            ) { [weak self] in self?.panel?.toggleAgent() }
            self.agentHotKey = agentHotKey
            if !agentHotKey.register() {
                NSLog("blindspot: could not register the agent hotkey")
            }
        }

        // Deferred one main-loop turn so `applicationDidFinishLaunching` returns and the
        // hotkey is live before this spends ~40ms loading and flattening icons. Doing it
        // at all is what removes the 108ms stall the first panel used to pay.
        Task { @MainActor in
            IconCache.prewarm(core.allPaths())
        }

        // Last, once the hotkey is live. Watching reads no contents until the user next
        // copies something, so if a future macOS enforces the documented pasteboard alert,
        // it would appear then rather than at login. And only if history is on — off means
        // the poller never starts, not that it records and discards.
        applyClipSettings(settings)

        // Registered regardless — Carbon accepts a chord Spotlight holds and then never
        // delivers it — so this only explains the silence, and watches for it to end.
        if wantsCommandSpace, SpotlightConflict().ownsCommandSpace() {
            warnSpotlightOwnsCommandSpace()
            // Registered already, and registered again the moment Spotlight lets go — so the
            // switch in System Settings is the only step, with no restart after it.
            conflictWatcher = SpotlightConflict().watch { [weak self] in
                self?.hotKey?.reinstall()
                NSLog("blindspot: Spotlight released \u{2318}Space; the hotkey is blindspot's")
            }
        }

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

    /// Re-reads the settings and applies the ones the shell holds a copy of rather than
    /// asking the core for per use.
    ///
    /// Called after every write from the settings window. The core has already swapped its
    /// own config; this is the other half — the two hotkeys that went into
    /// `RegisterEventHotKey`, the row count the panel snapshotted, and the login item,
    /// which is system state rather than anything blindspot holds.
    func applySettings() {
        guard let core else { return }
        let settings = core.startup
        hotKey?.rebind(
            keyCode: settings.hotkey_key_code, modifiers: settings.hotkey_modifiers)
        rebindAgentHotKey(to: settings)
        reconcileLoginItem(wanted: settings.launch_at_login)
        applyClipSettings(settings)
        panel?.applySettings()
    }

    /// Builds the panel again in the current palette.
    ///
    /// Rebuilt, not repainted: `ResultsView` pools its row views — CLAUDE.md says to keep
    /// that pool — and a `CALayer` bakes its colour when it is built, so nothing already
    /// made would change. About forty milliseconds, paid once, off the hotkey path.
    ///
    /// Safe only because the hotkey closures resolve the panel through `self` rather than
    /// capturing one; capturing would leave the chord firing at a window that is gone.
    private func rebuildPanel() {
        guard let core, let watcher else { return }
        panel?.orderOut(nil)
        panel = Panel(core: core, watcher: watcher)
        // Icons are cached in their palette-independent colour and a drained copy, so the
        // cache survives — nothing in `IconCache` reads `Theme`.
    }

    /// Turning clipboard history off stops the poller outright rather than recording and
    /// discarding: off means nothing is read from the pasteboard at all.
    private func applyClipSettings(_ settings: BsStartup) {
        guard let watcher else { return }
        watcher.recordsImages = settings.clips_images
        watcher.readsImageText = settings.clips_ocr
        if settings.clips_enabled {
            watcher.start()
        } else {
            watcher.stop()
        }
    }

    /// The second hotkey can be added, changed or taken away, and only the middle one is a
    /// rebind: an empty `agent_hotkey` means there should be no registration at all, and
    /// dropping the object is what unregisters it.
    private func rebindAgentHotKey(to settings: BsStartup) {
        let wanted = settings.agent_hotkey_key_code != 0 || settings.agent_hotkey_modifiers != 0
        guard wanted else {
            agentHotKey = nil
            return
        }
        if let existing = agentHotKey {
            existing.rebind(
                keyCode: settings.agent_hotkey_key_code,
                modifiers: settings.agent_hotkey_modifiers)
            return
        }
        let added = HotKey(
            keyCode: settings.agent_hotkey_key_code,
            modifiers: settings.agent_hotkey_modifiers,
            id: 2
        ) { [weak self] in self?.panel?.toggleAgent() }
        agentHotKey = added
        if !added.register() {
            NSLog("blindspot: could not register the agent hotkey")
        }
    }

    /// Reopening the app — double-clicking it in Finder, or `open -a Blindspot` — brings
    /// up the settings window.
    ///
    /// Belt and braces for the status item, which is *not* guaranteed to be visible:
    /// measured on this machine, a full menu bar puts a newly added item under the notch.
    /// `NSScreen.auxiliaryTopLeftArea` reported the notch spanning x 663–848 and the item's
    /// own window landed at x 784, `isVisible` true and entirely hidden. An accessory app
    /// has no Dock icon, so this is the one way in that no amount of menu-bar clutter can
    /// take away.
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows: Bool) -> Bool {
        openSettings()
        return true
    }

    /// ⌘, from anywhere, and the status item's first entry.
    ///
    /// No target on the menu item, so this is reached through the responder chain — which
    /// is what lets it work while the panel is up, since the panel hands an unrecognised
    /// ⌘ chord to `super` and `super` walks the main menu.
    @objc func openSettings() {
        // Before the window activates, not after: the panel tears itself down from
        // `windowDidResignKey`, and letting the two race is how a window ends up behind
        // the thing that was meant to get out of its way.
        panel?.standDown()
        settingsWindow?.show()
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
    private func warnSpotlightOwnsCommandSpace() {
        let alert = NSAlert()
        alert.messageText = "Spotlight still owns \u{2318}Space."
        alert.informativeText =
            "blindspot is set to \u{2318}Space, but macOS gives it to Spotlight first, so the "
            + "hotkey will do nothing until you turn Spotlight's off: Keyboard Shortcuts > "
            + "Spotlight > Show Spotlight search. blindspot takes the key the moment you do; "
            + "there is no need to restart it."
        alert.addButton(withTitle: "Open Keyboard Shortcuts")
        alert.addButton(withTitle: "Later")
        NSApp.activate()
        if alert.runModal() == .alertFirstButtonReturn {
            // The Keyboard pane; the Shortcuts sheet is one click in, which no URL opens.
            if let settings = URL(
                string: "x-apple.systempreferences:com.apple.Keyboard-Settings.extension")
            {
                NSWorkspace.shared.open(settings)
            }
        }
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
