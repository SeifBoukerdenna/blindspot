import AppKit

/// The menu-bar item.
///
/// The reason it exists is narrow and worth stating: blindspot is `LSUIElement`, so the
/// panel is the only way in — and the panel opens with a hotkey. A hotkey you have just
/// mistyped in the settings window, or that Spotlight has taken, would otherwise lock you
/// out of the one screen that fixes it. This is the way back in that cannot break.
@MainActor
final class StatusItem {
    /// Held for the process lifetime: `NSStatusBar` does not retain it, and a released
    /// item silently disappears from the menu bar.
    private let item: NSStatusItem

    init(onOpen: @escaping () -> Void = {}, onSetup: @escaping () -> Void = {}, onRestartSetup: @escaping () -> Void = {}, onSettings: @escaping () -> Void, onReindex: @escaping () -> Void) {
        item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        // The command glyph rather than a magnifying glass: this is a hotkey launcher, and
        // a magnifying glass in the menu bar is Spotlight's.
        item.button?.image = NSImage(
            systemSymbolName: "command", accessibilityDescription: "blindspot")
        item.button?.image?.isTemplate = true

        let menu = NSMenu()
        let open = NSMenuItem(title: "Open Blindspot", action: #selector(Actions.open), keyEquivalent: "")
        let containers = NSMenuItem(title: "Containers…", action: #selector(Actions.containers), keyEquivalent: "")
        let setup = NSMenuItem(title: "Set up Blindspot…", action: #selector(Actions.setup), keyEquivalent: "")
        let restartSetup = NSMenuItem(title: "Restart onboarding…", action: #selector(Actions.restartSetup), keyEquivalent: "")
        let settings = NSMenuItem(
            title: "Settings…", action: #selector(Actions.settings), keyEquivalent: ",")
        let reindex = NSMenuItem(
            title: "Reindex now", action: #selector(Actions.reindex), keyEquivalent: "")
        let actions = Actions(onOpen: onOpen, onSetup: onSetup, onRestartSetup: onRestartSetup, onSettings: onSettings, onReindex: onReindex)
        self.actions = actions
        for entry in [open, containers, setup, restartSetup, settings, reindex] {
            entry.target = actions
            menu.addItem(entry)
        }
        menu.addItem(.separator())
        menu.addItem(
            NSMenuItem(
                title: "Quit blindspot", action: #selector(NSApplication.terminate(_:)),
                keyEquivalent: "q"))
        item.menu = menu
    }

    /// A menu item's target is unowned, so the thing the selectors land on has to be kept
    /// alive by something. That is this.
    private var actions: Actions?

    @MainActor
    private final class Actions: NSObject {
        private let onOpen: () -> Void
        private let onSetup: () -> Void
        private let onRestartSetup: () -> Void
        private let onSettings: () -> Void
        private let onReindex: () -> Void

        init(onOpen: @escaping () -> Void = {}, onSetup: @escaping () -> Void = {}, onRestartSetup: @escaping () -> Void = {}, onSettings: @escaping () -> Void, onReindex: @escaping () -> Void) {
            self.onOpen = onOpen
            self.onSetup = onSetup
            self.onRestartSetup = onRestartSetup
            self.onSettings = onSettings
            self.onReindex = onReindex
        }

        @objc func open() { onOpen() }
        @objc func containers() { ContainerWindow.shared.show() }
        @objc func setup() { onSetup() }
        @objc func restartSetup() { onRestartSetup() }
        @objc func settings() { onSettings() }
        @objc func reindex() { onReindex() }
    }
}
