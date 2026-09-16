import Foundation

@_silgen_name("archive_read_new") private func archiveNew() -> OpaquePointer?
@_silgen_name("archive_read_support_format_zip") private func archiveZip(_ archive: OpaquePointer) -> Int32
@_silgen_name("archive_read_open_memory") private func archiveOpen(_ archive: OpaquePointer, _ data: UnsafeRawPointer, _ size: Int) -> Int32
@_silgen_name("archive_read_next_header") private func archiveNext(_ archive: OpaquePointer, _ entry: UnsafeMutablePointer<OpaquePointer?>) -> Int32
@_silgen_name("archive_entry_pathname_utf8") private func archiveName(_ entry: OpaquePointer) -> UnsafePointer<CChar>?
@_silgen_name("archive_entry_size") private func archiveSize(_ entry: OpaquePointer) -> Int64
@_silgen_name("archive_entry_is_encrypted") private func archiveEncrypted(_ entry: OpaquePointer) -> Int32
@_silgen_name("archive_read_data") private func archiveData(_ archive: OpaquePointer, _ buffer: UnsafeMutableRawPointer, _ size: Int) -> Int
@_silgen_name("archive_read_data_skip") private func archiveSkip(_ archive: OpaquePointer) -> Int32
@_silgen_name("archive_read_free") private func archiveFree(_ archive: OpaquePointer) -> Int32

enum OfficeExtract {
    struct Output { var text: String; var partial: Bool }
    private enum Failure: Error { case invalid }

    static func extract(_ data: Data, slides: Bool, maxBytes: Int, maxSections: Int) -> Output? {
        do {
            let entries = try unpack(data, prefix: slides ? "ppt/" : "xl/")
            let base = slides ? "ppt/" : "xl/"
            let main = slides ? "presentation.xml" : "workbook.xml"
            guard let body = entries[base + main], let relations = entries[base + "_rels/" + main + ".rels"] else { return nil }
            let tree = try XML.read(body)
            var targets: [String: String] = [:]
            for node in try XML.read(relations).descendants("Relationship") {
                guard node.attributes["TargetMode"] != "External", let id = node.attributes["Id"],
                      let target = node.attributes["Target"], let path = resolve(target, base: base) else { continue }
                targets[id] = path
            }
            var shared: [String] = []
            if !slides, let strings = entries["xl/sharedStrings.xml"] {
                shared = try XML.read(strings).descendants("si").map { $0.descendants("t").map(\.text).joined() }
            }
            let sections = tree.descendants(slides ? "sldId" : "sheet")
            var partial = sections.count > maxSections
            var output = ""
            for (index, section) in sections.prefix(maxSections).enumerated() {
                guard let id = section.attributes["r:id"], let path = targets[id],
                      path.hasPrefix(base + (slides ? "slides/" : "worksheets/")), let xml = entries[path] else {
                    partial = true
                    if slides { output += index == 0 ? "" : "\u{c}" }
                    continue
                }
                let root = try XML.read(xml)
                var text: String
                if slides {
                    text = (index == 0 ? "" : "\u{c}") + root.descendants("p").map { $0.descendants("t").map(\.text).joined() }.joined(separator: "\n")
                } else {
                    let name = clean(section.attributes["name"] ?? "\(index + 1)")
                    text = (index == 0 ? "" : "\u{c}") + "# Sheet \(name)\n\n"
                    for row in root.descendants("row") {
                        var values: [String] = []
                        for cell in row.children where cell.name == "c" {
                            let raw = cell.descendants("v").first?.text ?? ""
                            let value: String
                            switch cell.attributes["t"] {
                            case "s": value = Int(raw).flatMap { shared.indices.contains($0) ? shared[$0] : nil } ?? ""
                            case "inlineStr": value = cell.descendants("t").map(\.text).joined()
                            default: value = raw
                            }
                            if !value.isEmpty { values.append("\(clean(cell.attributes["r"] ?? "cell")): \(clean(value))") }
                        }
                        if !values.isEmpty { text += "Sheet \(name) · Row \(clean(row.attributes["r"] ?? "?")): " + values.joined(separator: " | ") + "\n\n" }
                        if text.utf8.count >= maxBytes { partial = true; break }
                    }
                }
                let remaining = maxBytes - output.utf8.count
                if text.utf8.count > remaining {
                    var bytes = Array(text.utf8.prefix(max(0, remaining)))
                    while String(bytes: bytes, encoding: .utf8) == nil { bytes.removeLast() }
                    text = String(decoding: bytes, as: UTF8.self)
                    partial = true
                }
                output += text
                if output.utf8.count >= maxBytes { partial = partial || index + 1 < sections.count; break }
            }
            return Output(text: output, partial: partial)
        } catch { return nil }
    }

    private static func clean(_ text: String) -> String {
        String(text.unicodeScalars.map { $0.value < 32 && $0 != "\n" && $0 != "\t" ? " " : Character($0) })
    }

    private static func resolve(_ target: String, base: String) -> String? {
        guard target.utf8.count <= 4096, !target.contains(":"), !target.contains("\\"), !target.contains("%"), !target.contains("\0") else { return nil }
        let path = target.hasPrefix("/") ? String(target.dropFirst()) : base + target
        guard path.split(separator: "/").allSatisfy({ $0 != ".." && $0 != "." }) else { return nil }
        return path
    }

    private static func unpack(_ data: Data, prefix: String) throws -> [String: Data] {
        guard let archive = archiveNew() else { throw Failure.invalid }
        defer { _ = archiveFree(archive) }
        guard archiveZip(archive) == 0 else { throw Failure.invalid }
        return try data.withUnsafeBytes { bytes in
            guard let base = bytes.baseAddress, archiveOpen(archive, base, bytes.count) == 0 else { throw Failure.invalid }
            var files: [String: Data] = [:]
            var seen = Set<String>()
            var expanded = 0
            for _ in 0..<4096 {
                var entry: OpaquePointer?
                let status = archiveNext(archive, &entry)
                if status == 1 { return files }
                guard status == 0, let entry, archiveEncrypted(entry) == 0,
                      let pointer = archiveName(entry), let name = String(validatingCString: pointer),
                      name.utf8.count <= 4096, !name.hasPrefix("/"), !name.contains("\\"),
                      !name.split(separator: "/").contains(".."), seen.insert(name).inserted else { throw Failure.invalid }
                guard name.hasPrefix(prefix), name.hasSuffix(".xml") || name.hasSuffix(".rels") else {
                    guard archiveSkip(archive) == 0 else { throw Failure.invalid }
                    continue
                }
                let size = archiveSize(entry)
                guard (0...4_194_304).contains(size), expanded + Int(size) <= 32 * 1024 * 1024 else { throw Failure.invalid }
                var content = Data(count: Int(size) + 1)
                let count = content.withUnsafeMutableBytes { bytes -> Int in
                    var offset = 0
                    while offset < bytes.count {
                        let n = archiveData(archive, bytes.baseAddress!.advanced(by: offset), bytes.count - offset)
                        if n < 0 { return -1 }
                        if n == 0 { return offset }
                        offset += n
                    }
                    return offset
                }
                guard count == Int(size) else { throw Failure.invalid }
                content.count = count
                expanded += count
                files[name] = content
            }
            throw Failure.invalid
        }
    }

    private final class XML: NSObject, XMLParserDelegate {
        final class Node {
            let name: String; let attributes: [String: String]
            var text = ""; var children: [Node] = []
            init(_ name: String, _ attributes: [String: String] = [:]) { self.name = name; self.attributes = attributes }
            func descendants(_ name: String) -> [Node] {
                (self.name == name ? [self] : []) + children.flatMap { $0.descendants(name) }
            }
        }
        let root = Node("root")
        var stack: [Node] = []
        var nodes = 0
        var characters = 0
        var invalid = false
        static func read(_ data: Data) throws -> Node {
            guard let source = String(data: data, encoding: .utf8) ?? String(data: data, encoding: .utf16),
                  !source.contains("<!DOCTYPE"), !source.contains("<!ENTITY") else { throw Failure.invalid }
            let delegate = XML()
            delegate.stack = [delegate.root]
            let parser = XMLParser(data: data)
            parser.shouldResolveExternalEntities = false
            parser.delegate = delegate
            guard parser.parse(), !delegate.invalid else { throw Failure.invalid }
            return delegate.root
        }
        func parser(_ parser: XMLParser, didStartElement name: String, namespaceURI: String?, qualifiedName: String?, attributes: [String: String]) {
            nodes += 1
            guard nodes <= 100_000, stack.count < 64 else { invalid = true; parser.abortParsing(); return }
            let node = Node(String(name.split(separator: ":").last ?? Substring(name)), attributes)
            stack.last?.children.append(node); stack.append(node)
        }
        func parser(_ parser: XMLParser, didEndElement: String, namespaceURI: String?, qualifiedName: String?) { if stack.count > 1 { stack.removeLast() } }
        func parser(_ parser: XMLParser, foundCharacters text: String) {
            characters += text.utf8.count
            guard characters <= 4_194_304 else { invalid = true; parser.abortParsing(); return }
            stack.last?.text += clean(text)
        }
        func parser(_ parser: XMLParser, foundInternalEntityDeclarationWithName: String, value: String?) { invalid = true; parser.abortParsing() }
        func parser(_ parser: XMLParser, foundExternalEntityDeclarationWithName: String, publicID: String?, systemID: String?) { invalid = true; parser.abortParsing() }
        func parser(_ parser: XMLParser, resolveExternalEntityName: String, systemID: String?) -> Data? { invalid = true; parser.abortParsing(); return nil }
    }
}
