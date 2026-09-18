import Foundation
import Darwin

enum SetupPage: Int, CaseIterable, Codable {
    case welcome, shortcut, documents, clipboard, accessibility, ai, ready

    var title: String {
        switch self {
        case .welcome: "Welcome"
        case .shortcut: "Your shortcut"
        case .documents: "Document search"
        case .clipboard: "Clipboard"
        case .accessibility: "Accessibility"
        case .ai: "Local AI"
        case .ready: "Make it yours"
        }
    }

    var symbol: String {
        switch self {
        case .welcome: "sparkle"
        case .shortcut: "command"
        case .documents: "doc.text.magnifyingglass"
        case .clipboard: "clipboard"
        case .accessibility: "hand.point.up.left"
        case .ai: "sparkles"
        case .ready: "checkmark.circle"
        }
    }
}

struct SetupRecord: Codable, Equatable {
    var version = 1
    var initializing = false
    var presented = false
    var page = SetupPage.welcome
    var visited: Set<Int> = []
    var testedModel: String?
}

struct SetupStore {
    let directory: URL?
    var file: URL? { directory?.appendingPathComponent("setup.json") }

    static let initialOverrides = """
    "launch_at_login" = "false"
    "content.enabled" = "false"
    "clips.enabled" = "false"
    "clips.images" = "false"
    "clips.ocr" = "false"
    "agent.model" = ""
    "agent.question_model" = ""
    "content.roots" = []
    """ + "\n"

    static func prepare(home: String?) throws -> (SetupStore, SetupRecord, Bool) {
        guard let home, !home.isEmpty else { return (SetupStore(directory: nil), SetupRecord(presented: true), false) }
        let root = URL(fileURLWithPath: home, isDirectory: true)
        let directory = root.appendingPathComponent(".local/share/blindspot", isDirectory: true)
        let store = SetupStore(directory: directory)
        let fm = FileManager.default
        if fm.fileExists(atPath: store.file!.path) {
            var record = try store.read()
            if record.initializing {
                try store.finishPreparation()
                record.initializing = false
                try store.save(record)
            }
            return (store, record, !record.presented)
        }
        if fm.fileExists(atPath: directory.path)
            || fm.fileExists(atPath: root.appendingPathComponent(".config/blindspot").path) {
            return (store, SetupRecord(presented: true), false)
        }
        try fm.createDirectory(at: directory.deletingLastPathComponent(), withIntermediateDirectories: true)
        let staging = directory.deletingLastPathComponent().appendingPathComponent(".blindspot-setup-\(UUID().uuidString)")
        try fm.createDirectory(at: staging, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        defer { try? fm.removeItem(at: staging) }
        let prepared = SetupStore(directory: staging)
        let record = SetupRecord()
        try prepared.finishPreparation()
        try prepared.save(record)
        try fm.moveItem(at: staging, to: directory)
        return (store, record, true)
    }

    private func finishPreparation() throws {
        guard let directory else { return }
        let url = directory.appendingPathComponent("overrides.toml")
        let data = Data(Self.initialOverrides.utf8)
        if FileManager.default.fileExists(atPath: url.path) {
            let handle = try FileHandle(forReadingFrom: url)
            defer { try? handle.close() }
            guard try handle.read(upToCount: data.count + 1) == data else {
                throw SetupFailure("Setup was interrupted and the settings changed. Your settings were preserved. Reopen Blindspot after resolving setup.json's initializing state.")
            }
        } else {
            try data.write(to: url, options: .withoutOverwriting)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
        }
    }

    func read() throws -> SetupRecord {
        guard let file else { return SetupRecord(presented: true) }
        let handle = try FileHandle(forReadingFrom: file)
        defer { try? handle.close() }
        let data = try handle.read(upToCount: 16_384) ?? Data()
        guard data.count < 16_384 else { throw SetupFailure("Setup history is too large. Existing settings have not been changed.") }
        let record = try JSONDecoder().decode(SetupRecord.self, from: data)
        guard record.version == 1 else { throw SetupFailure("This setup history requires a newer Blindspot version.") }
        return record
    }

    func save(_ record: SetupRecord) throws {
        guard let file else { return }
        try FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true,
                                              attributes: [.posixPermissions: 0o700])
        try JSONEncoder().encode(record).write(to: file, options: .atomic)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: file.path)
    }
}

struct SetupFailure: LocalizedError {
    let message: String
    init(_ message: String) { self.message = message }
    var errorDescription: String? { message }
}

struct SetupModel: Identifiable, Equatable, Sendable {
    let id: String
    let title: String
    let bytes: Int64
    let note: String

    static let choices = [
        SetupModel(id: "qwen3.5:0.8b", title: "Qwen 3.5 · 0.8B", bytes: 1_000_000_000, note: "Smallest download · simpler answers"),
        SetupModel(id: "qwen3.5:2b", title: "Qwen 3.5 · 2B", bytes: 2_700_000_000, note: "Lightweight · leaves room for your apps"),
        SetupModel(id: "qwen3.5:4b", title: "Qwen 3.5 · 4B", bytes: 3_400_000_000, note: "A balanced starting point"),
        SetupModel(id: "qwen3.5:9b", title: "Qwen 3.5 · 9B", bytes: 6_600_000_000, note: "More capable · uses more memory"),
    ]
    static let embedding = SetupModel(id: "embeddinggemma:300m", title: "EmbeddingGemma", bytes: 622_000_000, note: "Find related passages · separate from your answer model")
    static func recommended(memory: UInt64) -> SetupModel {
        let gb = memory / 1_073_741_824
        return choices[gb < 12 ? 1 : gb < 24 ? 2 : 3]
    }
    var size: String { ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file) }
}

struct SetupHardware: Sendable {
    let chip: String
    let memory: UInt64

    static func current() -> SetupHardware {
        var size = 0
        sysctlbyname("machdep.cpu.brand_string", nil, &size, nil, 0)
        var buffer = [CChar](repeating: 0, count: max(1, min(size, 256)))
        if size > 0 && size <= 256 { sysctlbyname("machdep.cpu.brand_string", &buffer, &size, nil, 0) }
        let chip = buffer.first == 0 ? "Apple silicon" : String(decoding: buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self)
        return SetupHardware(chip: chip, memory: ProcessInfo.processInfo.physicalMemory)
    }
    var description: String { "\(chip) · \(memory / 1_073_741_824) GB memory" }
}
