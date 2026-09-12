import AppKit

/// The menu an accessory app never shows but still needs.
///
/// An accessory app has no menu bar, yet AppKit still routes key equivalents through the
/// main menu — so the standard shortcuts exist only if their items do. With no menu at all,
/// ⌘Q did nothing; with only the app menu, ⌘V, ⌘C and ⌘A did nothing in the search field,
/// since text editing is driven by the Edit items' actions rather than by the text view
/// itself. A menu is less plumbing than intercepting each keystroke.
@MainActor
enum MainMenu {
    static func make() -> NSMenu {
        let appMenu = NSMenu()
        // No target, like the Edit items: the action walks the responder chain and the app
        // delegate answers it. That is also what makes ⌘, work while the panel is up —
        // `Panel.performKeyEquivalent` does not know the comma and hands it to `super`,
        // which walks this menu.
        appMenu.addItem(
            withTitle: "Settings…", action: Selector(("openSettings")), keyEquivalent: ",")
        appMenu.addItem(.separator())
        appMenu.addItem(
            withTitle: "Hide blindspot", action: #selector(NSApplication.hide(_:)),
            keyEquivalent: "h")
        appMenu.addItem(
            withTitle: "Quit blindspot", action: #selector(NSApplication.terminate(_:)),
            keyEquivalent: "q")

        // No target: each action goes to the first responder, which is the field editor
        // while the panel is up.
        let editMenu = NSMenu(title: "Edit")
        editMenu.addItem(withTitle: "Undo", action: Selector(("undo:")), keyEquivalent: "z")
        editMenu.addItem(withTitle: "Redo", action: Selector(("redo:")), keyEquivalent: "Z")
        editMenu.addItem(.separator())
        editMenu.addItem(withTitle: "Cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        editMenu.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        editMenu.addItem(
            withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        editMenu.addItem(
            withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")

        let main = NSMenu()
        for submenu in [appMenu, editMenu] {
            let item = NSMenuItem()
            item.submenu = submenu
            main.addItem(item)
        }
        return main
    }
}
