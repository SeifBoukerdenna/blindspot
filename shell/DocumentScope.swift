import AppKit

@MainActor
final class DocumentScope: NSPopUpButton, NSMenuDelegate {
    private(set) var folder: String?
    var roots: (() -> [String])?
    var onChange: (() -> Void)?
    var onChoose: (() -> Void)?
    private var paths: [String] = []

    init() {
        super.init(frame: .zero, pullsDown: false)
        isBordered = false
        font = Theme.text(11)
        contentTintColor = Theme.muted
        translatesAutoresizingMaskIntoConstraints = false
        identifier = NSUserInterfaceItemIdentifier("documents.folder")
        setAccessibilityLabel("Document folder, Command L")
        target = self
        action = #selector(changed)
        rebuild()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    func selectFolder(_ path: String?) {
        folder = path
        rebuild()
        onChange?()
    }

    func menuNeedsUpdate(_ menu: NSMenu) { rebuild() }

    private func rebuild() {
        paths = Array((roots?() ?? []).prefix(32))
        if let folder, !paths.contains(folder) { paths.insert(folder, at: 0) }
        removeAllItems()
        addItem(withTitle: "All indexed folders")
        for path in paths {
            addItem(withTitle: (path as NSString).abbreviatingWithTildeInPath)
            lastItem?.toolTip = path
        }
        menu?.addItem(.separator())
        addItem(withTitle: "Choose subfolder…")
        lastItem?.tag = -1
        menu?.delegate = self
        selectItem(at: folder.flatMap { paths.firstIndex(of: $0) }.map { $0 + 1 } ?? 0)
        toolTip = folder.map { "Search within \($0) · ⌘L to change folder" }
            ?? "Search all indexed folders · ⌘L to choose a folder"
        (cell as? NSPopUpButtonCell)?.lineBreakMode = .byTruncatingMiddle
    }

    @objc private func changed() {
        if selectedItem?.tag == -1 {
            rebuild()
            onChoose?()
            return
        }
        let index = indexOfSelectedItem
        selectFolder(index > 0 && index <= paths.count ? paths[index - 1] : nil)
    }
}
