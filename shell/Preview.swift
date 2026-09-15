import AppKit
import Quartz

/// Quick Look for the selected result, without leaving the panel.
///
/// **Bound to ⌘Y, not space.** The drawing asked for space, and Finder does use it — but
/// blindspot's field always holds focus, which is the whole interface, so space is a space.
/// ⌘Y is the other thing Finder binds to Quick Look, and it is free here.
///
/// The panel deliberately does not dismiss while this is up: the preview takes key status,
/// which would otherwise reach `windowDidResignKey` and tear the panel down with the query
/// still in it. `Panel.previewing` holds that off, and this reports the close so the panel
/// can take key back.
@MainActor
final class Preview: NSObject, @MainActor QLPreviewPanelDataSource {
    private var url: URL?
    private var watching: (any NSObjectProtocol)?

    /// Raised when the preview goes away, by whatever route — ⌘Y again, Esc, or its own
    /// close button.
    var onClose: (() -> Void)?

    var isShowing: Bool {
        QLPreviewPanel.sharedPreviewPanelExists() && QLPreviewPanel.shared().isVisible
    }

    /// Shows `path`, or hides what is showing. Returns whether a preview is now up.
    @discardableResult
    func toggle(_ path: String) -> Bool {
        guard let quickLook = QLPreviewPanel.shared() else { return false }
        if isShowing {
            quickLook.orderOut(nil)
            return false
        }
        url = URL(fileURLWithPath: path)
        // Set directly rather than through `QLPreviewPanelController`: that protocol exists
        // so several views can hand the shared panel back and forth up the responder
        // chain, and there is only ever one thing here that wants it.
        quickLook.dataSource = self
        quickLook.reloadData()
        quickLook.makeKeyAndOrderFront(nil)
        listen(to: quickLook)
        return true
    }

    /// Notification rather than the controller protocol's `endPreviewPanelControl`, which
    /// is only called for a controller that took the panel through the responder chain.
    private func listen(to panel: QLPreviewPanel) {
        guard watching == nil else { return }
        watching = NotificationCenter.default.addObserver(
            forName: NSWindow.willCloseNotification, object: panel, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.url = nil
                self.onClose?()
            }
        }
    }

    /// `isolated` so this runs on the main actor and may read `watching`, which is
    /// `NSObjectProtocol` and therefore not `Sendable` — the same reason `HotKey`'s and
    /// `ChordWell`'s deinits are isolated.
    isolated deinit {
        if let watching { NotificationCenter.default.removeObserver(watching) }
    }

    // MARK: - QLPreviewPanelDataSource

    func numberOfPreviewItems(in panel: QLPreviewPanel!) -> Int {
        url == nil ? 0 : 1
    }

    func previewPanel(_ panel: QLPreviewPanel!, previewItemAt index: Int) -> (any QLPreviewItem)! {
        url as NSURL?
    }
}
