import Foundation

@MainActor
final class CommandHistory {
    static let shared = CommandHistory(home: ProcessInfo.processInfo.environment["HOME"])
    private let file: URL?
    private(set) var recent: [String] = []

    init(home: String?) {
        file = home.flatMap { $0.isEmpty ? nil : URL(fileURLWithPath: $0)
            .appendingPathComponent(".local/share/blindspot/recent-commands.json") }
        if let file, let data = try? Data(contentsOf: file), data.count <= 8192,
           let values = try? JSONDecoder().decode([String].self, from: data) {
            recent = Array(values.prefix(32))
        }
    }

    func record(_ command: String, registered: [String]) {
        guard registered.contains(command), command.hasPrefix(":"), command.utf8.count <= 128 else { return }
        recent.removeAll { $0 == command || !registered.contains($0) }
        recent.insert(command, at: 0)
        recent = Array(recent.prefix(32))
        guard let file, let data = try? JSONEncoder().encode(recent) else { return }
        try? FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? data.write(to: file, options: .atomic)
    }

    func ordered(_ commands: [Match]) -> [Match] {
        commands.enumerated().sorted { lhs, rhs in
            let a = recent.firstIndex(of: lhs.element.path) ?? Int.max
            let b = recent.firstIndex(of: rhs.element.path) ?? Int.max
            return a == b ? lhs.offset < rhs.offset : a < b
        }.map(\.element)
    }
}
