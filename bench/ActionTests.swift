import AppKit

@main
@MainActor
enum ActionTests {
    final class Probe: ResultActionProvider {
        let id = "test.provider"
        var performed = 0
        var available = true
        func actions(for result: Match) -> [ResultAction] {
            guard available else { return [] }
            return [ResultAction(id: ResultActionID(provider: id, operation: "dangerous"),
                                 label: "Test", rank: 0, confirmation: "Confirm", permission: nil, shortcut: "")]
        }
        func perform(_ action: ResultActionID, on result: Match) async throws -> ActionEffect {
            performed += 1
            return .none
        }
    }

    static func main() async {
        let registry = ActionRegistry()
        let probe = Probe()
        precondition(registry.register(probe))
        precondition(!registry.register(probe))
        let result = Match(id: 1, name: "fixture", kind: .file, path: "/synthetic/fixture.txt",
                           score: 0, timestamp: 0, width: 0, height: 0, detail: "", highlights: [])
        let action = probe.actions(for: result)[0]
        do {
            try await registry.perform(action, on: result, confirmed: false)
            preconditionFailure("Unconfirmed action ran")
        } catch {}
        precondition(probe.performed == 0)
        let forged = ResultAction(id: action.id, label: action.label, rank: 0,
                                  confirmation: nil, permission: nil, shortcut: "")
        do {
            try await registry.perform(forged, on: result, confirmed: false)
            preconditionFailure("Caller downgraded confirmation")
        } catch {}
        precondition(probe.performed == 0)
        do { try await registry.perform(action, on: result, confirmed: true) }
        catch { preconditionFailure("Confirmed action failed") }
        precondition(probe.performed == 1)
        precondition(LauncherContext.isTextCommand("rewrite professionally"))
        precondition(LauncherContext.isTextCommand("summarize"))
        precondition(!LauncherContext.isTextCommand("summarize.pdf"))
        precondition(LocalRequest.parse("kill the process using port 3000") == .terminatePort(3000))
        precondition(LocalRequest.parse("show everything listening on localhost")?.query == ":localhost")
        precondition(LocalRequest.parse("documents about genetec")?.query == ":content kind:documents genetec")
        precondition(LocalRequest.parse("my schedule") == .schedule && LocalRequest.parse(":schedule") == .schedule)
        precondition(LocalRequest.parse("join my next meeting") == .schedule && LocalRequest.parse("schedule a meeting") == nil)
        precondition(ScheduleProvider.meetingURL(in: ["Dial in: us02web.zoom.us/j/123?pwd=abc"])?.absoluteString == "https://us02web.zoom.us/j/123?pwd=abc")
        precondition(ScheduleProvider.meetingURL(in: ["https://evil.example/zoom.us/j/1", "notes https://example.com"]) == nil)
        precondition(ScheduleProvider.service(for: URL(string: "https://meet.google.com/abc-defg-hij")!) == "Google Meet")
        precondition(LocalRequest.parse("open current project") == .currentProject)
        precondition(LocalRequest.parse(":3000") == nil)
        let selected = LauncherContext(applicationID: "test", processID: 0,
                                       selectedText: .available("fixture"), windowTitle: .unavailable,
                                       documentURL: .unavailable)
        precondition(selected.hasTextObject && selected.textForAI == "fixture")
        let unavailable = LauncherContext(applicationID: nil, processID: 0,
                                          selectedText: .permissionRequired, windowTitle: .unavailable,
                                          documentURL: .unavailable)
        precondition(!unavailable.hasTextObject && unavailable.textForAI == nil)
        probe.available = false
        do {
            try await registry.perform(action, on: result, confirmed: true)
            preconditionFailure("Unavailable action ran")
        } catch {}
        let actions = registry.actions(for: result)
        precondition(actions.last?.label == "Move to Trash")
        precondition(actions.last?.confirmation != nil)
        precondition(probe.performed == 1)
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("blindspot-actions-" + UUID().uuidString)
        for (kind, path) in [(MatchKind.command, ":ports"), (MatchKind.setting, "agent.model")] {
            let row = Match(id: 77, name: "Navigation fixture", kind: kind, path: path,
                            score: 0, timestamp: 0, width: 0, height: 0, detail: "", highlights: [])
            guard let action = registry.actions(for: row).first else { preconditionFailure("Missing navigation action") }
            do {
                let effect = try await registry.perform(action, on: row, confirmed: false)
                switch (kind, effect) {
                case (.command, .query(let query)): precondition(query == path)
                case (.setting, .setting(let key)): precondition(key == path)
                default: preconditionFailure("Wrong navigation effect")
                }
            } catch { preconditionFailure("Navigation failed") }
        }
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            defer { try? FileManager.default.removeItem(at: directory) }
            let source = directory.appendingPathComponent("source.txt")
            let destination = directory.appendingPathComponent("copy.txt")
            try Data("private fixture".utf8).write(to: source)
            try await NativeFileMutation.perform(.copy, source: source, destination: destination)
            let original = try Data(contentsOf: source)
            let copied = try Data(contentsOf: destination)
            precondition(original == copied)
            do {
                try await NativeFileMutation.perform(.move, source: source, destination: destination)
                preconditionFailure("Overwrote existing file")
            } catch {}
            precondition(FileManager.default.fileExists(atPath: source.path))
            for invalid in ["", ".", "..", "../escape", "a/b", "a:b", "a\0b"] {
                do {
                    _ = try NativeFileMutation.renamedURL(source, name: invalid)
                    preconditionFailure("Accepted invalid filename")
                } catch {}
            }
            let extracted = try await FileText.read(source)
            precondition(extracted == "private fixture")
            let link = directory.appendingPathComponent("link.txt")
            try FileManager.default.createSymbolicLink(at: link, withDestinationURL: source)
            do {
                _ = try await FileText.read(link)
                preconditionFailure("Read symbolic link")
            } catch {}
        } catch { preconditionFailure("File action regression: \(error)") }
        print("Action safety and native file operations: 11 passed; contextual commands and permission fallback: 2 passed; typed navigation: 2 passed")
    }
}
