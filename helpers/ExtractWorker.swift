import Foundation
import PDFKit
import AppKit
import Darwin
import Vision

// Two independent modes in one binary:
//
//   blindspot-extract <absolute path>
//     Legacy path mode used by shell/Actions.swift for the "Extract Text" file action.
//     Prints plain UTF-8 text to stdout. Exit codes 2 (bad args) / 3 (open/stat failed) /
//     4 (unreadable/locked PDF or empty text) / 5 (I/O error). Behavior here is preserved
//     byte-for-byte from before this file grew an indexing mode.
//
//   blindspot-extract --index <kind> <maxBytes> <maxPages>
//     Indexing mode used by the Rust content indexer. The document is never named on the
//     command line: fd 0 is an already-opened, read-only regular file supplied by the
//     caller. Emits exactly one JSON object followed by "\n" on stdout and exits 0, except
//     for a bad-argument exit(2) (no stdout) and a watchdog exit(3) (no stdout).

@main
enum ExtractWorker {
    static func main() {
        let arguments = CommandLine.arguments
        if arguments.count >= 2, arguments[1] == "--index" {
            IndexMode.run(Array(arguments.dropFirst(2)))
        } else {
            LegacyMode.run(arguments)
        }
    }
}

// MARK: - Legacy path mode (unchanged)

private enum LegacyMode {
    static func run(_ arguments: [String]) {
        guard arguments.count == 2 else { exit(2) }
        let path = arguments[1]
        guard path.hasPrefix("/"), path.utf8.count <= 4096 else { exit(2) }
        let descriptor = path.withCString { Darwin.open($0, O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC) }
        guard descriptor >= 0 else { exit(3) }
        let file = FileHandle(fileDescriptor: descriptor, closeOnDealloc: true)
        defer { try? file.close() }
        var metadata = stat()
        guard fstat(descriptor, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFREG,
              metadata.st_size > 0, metadata.st_size <= 16 * 1024 * 1024 else { exit(3) }
        do {
            let data = try file.read(upToCount: 16 * 1024 * 1024) ?? Data()
            guard let document = PDFDocument(data: data), !document.isLocked else { exit(4) }
            var text = Data()
            for index in 0..<min(document.pageCount, 50) {
                let page = (document.page(at: index)?.string ?? "") + "\n"
                text.append(contentsOf: page.utf8.prefix(16_000 - text.count))
                if text.count >= 16_000 { break }
            }
            guard !String(decoding: text, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { exit(4) }
            try FileHandle.standardOutput.write(contentsOf: text)
        } catch { exit(5) }
    }
}

// MARK: - Indexing mode

private enum DocumentKind: String { case pdf, docx, doc, rtf, odt, pptx, xlsx }

private struct IndexResult: Encodable {
    var status: String
    var text: String?
    var pages: Int?
    var partial: Bool = false
    var ocrPages: [Int] = []
}

@_silgen_name("sandbox_init")
private func sandboxInit(_ profile: UnsafePointer<CChar>, _ flags: Int, _ error: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> Int32

private enum IndexMode {
    private static let maxInputBytes = 128 * 1024 * 1024
    private static let maxResponseBytes = 16 * 1024 * 1024
    private static let watchdogSeconds: TimeInterval = 20

    static func run(_ arguments: [String]) {
        guard (3...4).contains(arguments.count),
              let kind = DocumentKind(rawValue: arguments[0]),
              let maxBytes = Int(arguments[1]), (1...2_097_152).contains(maxBytes),
              let maxPages = Int(arguments[2]), (1...1_000).contains(maxPages)
        else { exit(2) }
        let ocrPages = arguments.count == 4 ? (Int(arguments[3]) ?? -1) : 0
        guard (0...100).contains(ocrPages) else { exit(2) }

        guard denyNetwork() else {
            emit(IndexResult(status: "unreadable"))
            exit(0)
        }
        startWatchdog()
        _ = pthread_set_qos_class_self_np(QOS_CLASS_BACKGROUND, 0)

        let result = autoreleasepool { extract(kind: kind, maxBytes: maxBytes, maxPages: maxPages, ocrBudget: ocrPages) }
        emit(result)
        exit(0)
    }

    /// Removes network access before any document is parsed. Measured: Apple's importers made no
    /// connection for external images, templates, hyperlinks or INCLUDEPICTURE fields in DOCX, DOC,
    /// RTF or ODT; this keeps a future importer change from fetching remote content a document names.
    private static func denyNetwork() -> Bool {
        var failure: UnsafeMutablePointer<CChar>?
        // "no-network" is kSBXProfileNoNetwork and 1 is SANDBOX_NAMED from <sandbox.h>.
        let status = "no-network".withCString { sandboxInit($0, 1, &failure) }
        if let failure { free(failure) }
        return status == 0
    }

    // Never allowed to hang the caller: PDFKit / TextEdit's importers can spin
    // indefinitely on pathological input. A sibling thread is the only way to bound
    // that from inside the same process; the watchdog never fires on the happy path
    // because `run` calls `exit(0)` itself as soon as extraction returns.
    private static func startWatchdog() {
        let thread = Thread {
            Thread.sleep(forTimeInterval: watchdogSeconds)
            exit(3)
        }
        thread.qualityOfService = .background
        thread.stackSize = 64 * 1024
        thread.start()
    }

    private static func extract(kind: DocumentKind, maxBytes: Int, maxPages: Int, ocrBudget: Int) -> IndexResult {
        var metadata = stat()
        guard fstat(0, &metadata) == 0,
              metadata.st_mode & S_IFMT == S_IFREG,
              metadata.st_size > 0,
              metadata.st_size <= maxInputBytes else {
            return IndexResult(status: "unreadable")
        }
        guard let data = readInput(size: Int(metadata.st_size)) else {
            return IndexResult(status: "unreadable")
        }
        switch kind {
        case .pdf:
            return extractPDF(data: data, maxBytes: maxBytes, maxPages: maxPages, ocrBudget: ocrBudget)
        case .docx, .doc, .rtf, .odt:
            return extractOffice(kind: kind, data: data, maxBytes: maxBytes)
        case .pptx, .xlsx:
            guard let result = OfficeExtract.extract(data, slides: kind == .pptx, maxBytes: maxBytes, maxSections: maxPages) else {
                return IndexResult(status: "unreadable")
            }
            return IndexResult(status: result.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? "empty" : "text", text: result.text, partial: result.partial)
        }
    }

    // MARK: Input

    private static func readInput(size: Int) -> Data? {
        if let mapped = mmapInput(size: size) { return mapped }
        return readAllFallback(size: size)
    }

    private static func mmapInput(size: Int) -> Data? {
        guard size > 0 else { return nil }
        guard let base = mmap(nil, size, PROT_READ, MAP_PRIVATE, 0, 0),
              base != UnsafeMutableRawPointer(bitPattern: -1) else { return nil }
        return Data(bytesNoCopy: base, count: size, deallocator: .custom { pointer, length in
            munmap(pointer, length)
        })
    }

    private static func readAllFallback(size: Int) -> Data? {
        var data = Data(count: size)
        let readCount: Int? = data.withUnsafeMutableBytes { rawBuffer -> Int? in
            guard let base = rawBuffer.baseAddress else { return size == 0 ? 0 : nil }
            var total = 0
            while total < size {
                let n = Darwin.read(0, base.advanced(by: total), size - total)
                if n < 0 {
                    if errno == EINTR { continue }
                    return nil
                }
                if n == 0 { break }
                total += n
            }
            return total
        }
        guard let count = readCount else { return nil }
        return data.prefix(count)
    }

    // MARK: PDF

    private static func extractPDF(data: Data, maxBytes: Int, maxPages: Int, ocrBudget: Int) -> IndexResult {
        guard let document = PDFDocument(data: data) else { return IndexResult(status: "unreadable") }
        if document.isLocked || (document.isEncrypted && !document.unlock(withPassword: "")) {
            return IndexResult(status: "locked")
        }
        var text = Data()
        var pagesExamined = 0
        var ocrPages: [Int] = []
        var attempted = 0
        var partial = document.pageCount > maxPages
        let upperBound = min(document.pageCount, maxPages)
        for index in 0..<upperBound {
            pagesExamined += 1
            if index > 0 { appendTruncated("\u{c}", into: &text, budget: maxBytes) }
            guard let page = document.page(at: index) else { partial = true; continue }
            var pageText = page.string ?? ""
            if pageText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                if attempted < ocrBudget {
                    attempted += 1
                    pageText = autoreleasepool { recognize(page) }
                    if !pageText.isEmpty { ocrPages.append(index + 1) }
                }
                if pageText.isEmpty { partial = true }
            }
            let sanitized = sanitize(pageText)
            if sanitized.utf8.count > maxBytes - text.count { partial = true }
            appendTruncated(sanitized, into: &text, budget: maxBytes)
            if text.count >= maxBytes { partial = partial || index + 1 < document.pageCount; break }
        }
        let extracted = String(decoding: text, as: UTF8.self)
        guard !extracted.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return IndexResult(status: "empty", pages: pagesExamined)
        }
        return IndexResult(status: "text", text: extracted, pages: pagesExamined, partial: partial, ocrPages: ocrPages)
    }

    private static func recognize(_ page: PDFPage) -> String {
        let bounds = page.bounds(for: .mediaBox)
        guard bounds.width.isFinite, bounds.height.isFinite, bounds.width > 0, bounds.height > 0 else { return "" }
        let scale = min(2.0, 2048 / max(bounds.width, bounds.height))
        let image = page.thumbnail(of: NSSize(width: bounds.width * scale, height: bounds.height * scale), for: .mediaBox)
        var rect = NSRect(origin: .zero, size: image.size)
        guard let cgImage = image.cgImage(forProposedRect: &rect, context: nil, hints: nil) else { return "" }
        let request = VNRecognizeTextRequest()
        request.recognitionLevel = .accurate
        request.usesLanguageCorrection = true
        request.automaticallyDetectsLanguage = true
        do { try VNImageRequestHandler(cgImage: cgImage).perform([request]) } catch { return "" }
        return (request.results ?? []).compactMap { $0.topCandidates(1).first?.string }.joined(separator: "\n")
    }

    // MARK: Office documents

    private static func extractOffice(kind: DocumentKind, data: Data, maxBytes: Int) -> IndexResult {
        let documentType: NSAttributedString.DocumentType
        switch kind {
        case .docx: documentType = .officeOpenXML
        case .doc: documentType = .docFormat
        case .rtf: documentType = .rtf
        case .odt: documentType = .openDocument
        case .pdf, .pptx, .xlsx: preconditionFailure("Handled separately")
        }
        guard let attributed = try? NSAttributedString(
            data: data, options: [.documentType: documentType], documentAttributes: nil
        ) else {
            return IndexResult(status: "unreadable")
        }
        let sanitized = sanitize(attributed.string)
        let truncated = truncated(sanitized, toUTF8ByteCount: maxBytes)
        guard !truncated.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return IndexResult(status: "empty")
        }
        return IndexResult(status: "text", text: truncated, partial: sanitized.utf8.count > maxBytes)
    }

    // MARK: Text handling

    /// Replaces NUL and C0 control characters other than tab/newline with a space so the
    /// stored text can never smuggle raw control bytes into downstream storage/UI.
    private static func sanitize(_ text: String) -> String {
        var scalars = String.UnicodeScalarView()
        scalars.reserveCapacity(text.unicodeScalars.count)
        for scalar in text.unicodeScalars {
            if scalar.value <= 0x1F, scalar != "\n", scalar != "\t" {
                scalars.append(" ")
            } else {
                scalars.append(scalar)
            }
        }
        return String(scalars)
    }

    /// Appends as much of `text` as fits in the remaining `budget` (in UTF-8 bytes),
    /// stopping on a Character boundary rather than splitting a multi-byte scalar.
    private static func appendTruncated(_ text: String, into accumulator: inout Data, budget: Int) {
        let remaining = budget - accumulator.count
        guard remaining > 0 else { return }
        accumulator.append(contentsOf: truncated(text, toUTF8ByteCount: remaining).utf8)
    }

    private static func truncated(_ text: String, toUTF8ByteCount limit: Int) -> String {
        guard text.utf8.count > limit else { return text }
        guard limit > 0 else { return "" }
        var end = text.startIndex
        var used = 0
        for index in text.indices {
            let length = text[index].utf8.count
            if used + length > limit { break }
            used += length
            end = text.index(after: index)
        }
        return String(text[text.startIndex..<end])
    }

    // MARK: Output

    /// Encodes and writes exactly one JSON line. If the encoded response would exceed the
    /// protocol's response cap (escaping can inflate text — e.g. every byte a newline),
    /// the text is halved repeatedly until it fits; this is a defensive backstop since
    /// maxBytes is capped at 2 MiB by argument validation.
    private static func emit(_ result: IndexResult) {
        var candidate = result
        for _ in 0..<24 {
            if let data = try? JSONEncoder().encode(candidate), data.count <= maxResponseBytes - 1 {
                write(data)
                return
            }
            guard let text = candidate.text, !text.isEmpty else { break }
            let characters = Array(text)
            let shortened = String(characters.prefix(characters.count / 2))
            if shortened.isEmpty {
                candidate.text = nil
                candidate.status = "empty"
            } else {
                candidate.text = shortened
                candidate.partial = true
            }
        }
        write(Data(#"{"status":"unreadable"}"#.utf8))
    }

    private static func write(_ data: Data) {
        var output = data
        output.append(10)
        FileHandle.standardOutput.write(output)
    }
}
