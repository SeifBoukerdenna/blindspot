import AppKit
import Vision

/// On-device text recognition, shared by clipboard images and Copy Text from Screen.
enum TextRecognition {
    /// The text in an image, in reading order, via Vision's on-device recogniser.
    ///
    /// `.accurate`, not `.fast`: on a 3024 × 1964 screenshot, fast took 16ms and returned
    /// `errorlE05991` and dropped French accents; accurate took 99ms warm and returned
    /// `error[E0599]` and "vérifiez" intact. The whole decode-encode-recognise step measured
    /// 367ms on a cold first run. It happens once per copied image, off the main thread,
    /// before the clip is stored — slower than a keystroke, faster than copy-then-open.
    /// English and French because this is a bilingual machine; detection picks between them.
    ///
    /// On-device text recognition, not an AI feature: deterministic, no network, and the
    /// same recogniser behind Live Text. Chosen deliberately against CLAUDE.md's "No AI".
    static func text(in image: CGImage) -> String {
        let request = VNRecognizeTextRequest()
        request.recognitionLevel = .accurate
        request.recognitionLanguages = ["en-US", "fr-FR"]
        request.automaticallyDetectsLanguage = true
        request.usesLanguageCorrection = true
        guard (try? VNImageRequestHandler(cgImage: image).perform([request])) != nil else {
            return ""
        }
        // Vision does not promise reading order, and the first line becomes the title, so
        // sort top to bottom, then left to right. Its coordinates put y = 0 at the bottom.
        let lines = (request.results ?? []).sorted { a, b in
            let ay = a.boundingBox.midY, by = b.boundingBox.midY
            if abs(ay - by) > 0.01 { return ay > by }
            return a.boundingBox.minX < b.boundingBox.minX
        }
        return lines.compactMap { $0.topCandidates(1).first?.string }.joined(separator: "\n")
    }
}

/// "Copy Text from Screen": the standard macOS area selection, Vision's on-device recogniser,
/// then the clipboard. The screenshot is a temporary file, deleted as soon as it has been read.
@MainActor
enum TextCapture {
    static func run() async {
        guard CGPreflightScreenCaptureAccess() else {
            // Asking lists Blindspot in Privacy & Security and shows the system prompt once.
            _ = CGRequestScreenCaptureAccess()
            let alert = NSAlert()
            alert.messageText = "Allow Screen Recording"
            alert.informativeText = "To read text from the screen, allow Blindspot under System Settings → Privacy & Security → Screen & System Audio Recording, then try again."
            alert.addButton(withTitle: "Open Privacy Settings")
            alert.addButton(withTitle: "Cancel")
            NSApp.activate()
            if alert.runModal() == .alertFirstButtonReturn,
               let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture") {
                NSWorkspace.shared.open(url)
            }
            return
        }
        let file = FileManager.default.temporaryDirectory
            .appendingPathComponent("blindspot-capture-\(UUID().uuidString).png")
        defer { try? FileManager.default.removeItem(at: file) }
        // Escape during the selection leaves no file, which is a quiet cancel.
        guard await select(into: file),
              let source = CGImageSourceCreateWithURL(file as CFURL, nil),
              let image = CGImageSourceCreateImageAtIndex(source, 0, nil) else { return }
        let text = await Task.detached(priority: .userInitiated) { TextRecognition.text(in: image) }.value
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            Toast.show("No text found in that area")
            return
        }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(text, forType: .string)
        let words = text.split(whereSeparator: \.isWhitespace).count
        Toast.show("Copied \(words) word\(words == 1 ? "" : "s")")
    }

    private static func select(into file: URL) async -> Bool {
        await withCheckedContinuation { continuation in
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
            // -i: the system's own area and window selection; -x: no shutter sound.
            process.arguments = ["-i", "-x", file.path]
            process.terminationHandler = { _ in
                continuation.resume(returning: FileManager.default.fileExists(atPath: file.path))
            }
            do {
                try process.run()
            } catch {
                continuation.resume(returning: false)
            }
        }
    }
}

/// A short confirmation near the top of the screen that needs no click and takes no focus, for
/// actions that finish after the panel has closed.
@MainActor
enum Toast {
    private static var shown: NSPanel?

    static func show(_ message: String) {
        shown?.orderOut(nil)
        guard let screen = NSScreen.main else { return }
        let label = NSTextField(labelWithString: message)
        label.font = .systemFont(ofSize: 13, weight: .medium)
        label.sizeToFit()
        let size = NSSize(width: ceil(label.frame.width) + 32, height: 36)
        let frame = NSRect(x: screen.visibleFrame.midX - size.width / 2, y: screen.visibleFrame.maxY - size.height - 40,
                           width: size.width, height: size.height)
        let panel = NSPanel(contentRect: frame, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.level = .statusBar
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.ignoresMouseEvents = true
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .transient]
        let plate = NSVisualEffectView(frame: NSRect(origin: .zero, size: size))
        plate.material = .hudWindow
        plate.state = .active
        plate.wantsLayer = true
        plate.layer?.cornerRadius = 10
        label.frame.origin = NSPoint(x: 16, y: (size.height - label.frame.height) / 2)
        plate.addSubview(label)
        panel.contentView = plate
        panel.orderFrontRegardless()
        shown = panel
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(1600))
            guard shown === panel else { return }
            panel.orderOut(nil)
            shown = nil
        }
    }
}
