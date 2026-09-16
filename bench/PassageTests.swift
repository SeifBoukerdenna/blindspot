import AppKit
import PDFKit

@main
@MainActor
enum PassageTests {
    static func main() async throws {
        NSApplication.shared.setActivationPolicy(.accessory)
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("blindspot-passage-" + UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: directory) }
        let image = NSImage(size: NSSize(width: 1000, height: 500))
        image.lockFocus()
        NSColor.white.setFill()
        NSRect(x: 0, y: 0, width: 1000, height: 500).fill()
        ("Genetec camera renewal deadline" as NSString).draw(at: NSPoint(x: 40, y: 250),
            withAttributes: [.font: NSFont.systemFont(ofSize: 40), .foregroundColor: NSColor.black])
        image.unlockFocus()
        let document = PDFDocument()
        document.insert(PDFPage(image: image)!, at: 0)
        document.insert(PDFPage(image: image)!, at: 1)
        let url = directory.appendingPathComponent("scan.pdf")
        precondition(document.write(to: url))
        let preview = Preview()
        var closed = 0
        preview.onClose = { closed += 1 }
        precondition(preview.toggle(url.path, page: 2))
        var shown: NSWindow?
        for _ in 0..<100 {
            shown = NSApplication.shared.windows.first { $0.title == "scan.pdf · page 2" && $0.isVisible }
            if let view = shown?.contentView as? PDFView, let page = view.currentPage {
                precondition(view.document?.index(for: page) == 1)
                break
            }
            try await Task.sleep(for: .milliseconds(20))
        }
        precondition(shown?.contentView is PDFView, "PDF preview did not load")
        shown?.close()
        precondition(!preview.isShowing && closed == 1)
        precondition(preview.toggle(url.path, page: 1))
        precondition(!preview.toggle(url.path, page: 1))
        try await Task.sleep(for: .milliseconds(100))
        precondition(!preview.isShowing && closed == 2, "stale loader reopened preview")
        if CommandLine.arguments.count == 2 {
            for budget in [0, 1] {
                let process = Process()
                process.executableURL = URL(fileURLWithPath: CommandLine.arguments[1])
                process.arguments = ["--index", "pdf", "2097152", "100", String(budget)]
                let input = try FileHandle(forReadingFrom: url)
                defer { try? input.close() }
                process.standardInput = input
                let output = Pipe()
                process.standardOutput = output
                try process.run()
                let data = output.fileHandleForReading.readDataToEndOfFile()
                process.waitUntilExit()
                precondition(process.terminationStatus == 0)
                let result = try JSONSerialization.jsonObject(with: data) as! [String: Any]
                if budget == 0 { precondition(result["status"] as? String == "empty") }
                else {
                    precondition((result["text"] as? String)?.lowercased().contains("genetec camera renewal deadline") == true)
                    precondition(result["ocrPages"] as? [Int] == [1])
                    precondition(result["partial"] as? Bool == true)
                }
            }
        }
        print("PASS: PDF page navigation, preview close/cancellation, OCR opt-in and page budget")
    }
}
