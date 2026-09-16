import AppKit
import Quartz
@preconcurrency import PDFKit

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
final class Preview: NSObject, @MainActor QLPreviewPanelDataSource, NSWindowDelegate {
    private var url: URL?
    private var watching: (any NSObjectProtocol)?
    private var pdfPanel: NSPanel?
    private var pdfTask: Task<Void, Never>?
    private var generation = UUID()
    private final class Loaded: @unchecked Sendable {
        let document: PDFDocument
        init(_ document: PDFDocument) { self.document = document }
    }

    /// Raised when the preview goes away, by whatever route — ⌘Y again, Esc, or its own
    /// close button.
    var onClose: (() -> Void)?

    var isShowing: Bool {
        pdfPanel?.isVisible == true || (QLPreviewPanel.sharedPreviewPanelExists() && QLPreviewPanel.shared().isVisible)
    }

    /// Shows `path`, or hides what is showing. Returns whether a preview is now up.
    @discardableResult
    func toggle(_ path: String, page: Int = 0) -> Bool {
        if let pdfPanel { pdfPanel.close(); return false }
        if page > 0, URL(fileURLWithPath: path).pathExtension.lowercased() == "pdf" {
            if QLPreviewPanel.sharedPreviewPanelExists() { QLPreviewPanel.shared().orderOut(nil) }
            return showPDF(path, page: page)
        }
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

    private func showPDF(_ path: String, page: Int) -> Bool {
        let panel = PDFPanel(contentRect: NSRect(x: 0, y: 0, width: 760, height: 800),
                             styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        panel.isReleasedWhenClosed = false
        panel.title = "\(URL(fileURLWithPath: path).lastPathComponent) · page \(page)"
        panel.delegate = self
        let label = NSTextField(labelWithString: "Loading PDF…")
        label.frame = NSRect(x: 24, y: 24, width: 700, height: 30)
        panel.contentView?.addSubview(label)
        pdfPanel = panel
        generation = UUID()
        let expected = generation
        panel.center()
        panel.makeKeyAndOrderFront(nil)
        pdfTask = Task { @MainActor [weak self, weak panel] in
            let worker = Task.detached(priority: .userInitiated) { () -> Loaded? in
                let descriptor = path.withCString { Darwin.open($0, O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC) }
                guard descriptor >= 0 else { return nil }
                let file = FileHandle(fileDescriptor: descriptor, closeOnDealloc: true)
                defer { try? file.close() }
                var metadata = stat()
                guard fstat(descriptor, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFREG,
                      metadata.st_size > 0, metadata.st_size <= 128 * 1024 * 1024, !Task.isCancelled,
                      let data = try? file.read(upToCount: 128 * 1024 * 1024), !Task.isCancelled,
                      let document = PDFDocument(data: data), !document.isLocked else { return nil }
                return Loaded(document)
            }
            let loaded = await withTaskCancellationHandler { await worker.value } onCancel: { worker.cancel() }
            guard !Task.isCancelled, let self, let panel, self.generation == expected, panel.isVisible else { return }
            guard let loaded, let destination = loaded.document.page(at: page - 1) else {
                label.stringValue = "This page is unavailable. Use Open to view the file in its default application."
                return
            }
            let view = PDFView(frame: panel.contentView?.bounds ?? .zero)
            view.autoresizingMask = [.width, .height]
            view.autoScales = true
            view.document = loaded.document
            panel.contentView = view
            view.go(to: destination)
        }
        return true
    }

    func windowWillClose(_ notification: Notification) {
        guard let window = notification.object as? NSWindow, window === pdfPanel else { return }
        generation = UUID()
        pdfTask?.cancel()
        pdfTask = nil
        pdfPanel = nil
        onClose?()
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

@MainActor
private final class PDFPanel: NSPanel {
    override func cancelOperation(_ sender: Any?) { close() }
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        if event.modifierFlags.intersection(.deviceIndependentFlagsMask) == .command,
           event.charactersIgnoringModifiers == "y" { close(); return true }
        return super.performKeyEquivalent(with: event)
    }
}
