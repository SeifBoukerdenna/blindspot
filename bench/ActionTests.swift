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
        if let editor = Editor.preferred() {
            let (tool, arguments) = editor.command("/tmp/notes.rs", line: 340)
            precondition(FileManager.default.isExecutableFile(atPath: tool), tool)
            precondition(arguments.contains { $0.contains("/tmp/notes.rs:340") || $0 == "340" }, "\(arguments)")
        }
        let passage = Match(id: 3, name: "store.rs", kind: .file, path: "/tmp/store.rs", score: 0,
                            timestamp: 0, width: 0, height: 0, detail: "", highlights: [], line: 42)
        let plain = Match(id: 4, name: "store.rs", kind: .file, path: "/tmp/store.rs", score: 0,
                          timestamp: 0, width: 0, height: 0, detail: "", highlights: [])
        // Through the registry, which is how the panel reaches the file provider.
        let files = ActionRegistry()
        let opensAt = { (row: Match) in
            files.actions(for: row).contains { $0.label.hasPrefix("Open at Line") }
        }
        precondition(opensAt(passage) == (Editor.preferred() != nil), "a passage offers its line")
        precondition(!opensAt(plain), "a whole-file row does not")
        precondition(LocalRequest.parse("join my next meeting") == .schedule)
        func draft(_ text: String) -> EventDraft? {
            if case .createEvent(let draft) = LocalRequest.parse(text) { return draft }
            return nil
        }
        let calendar = Calendar.current
        let hour = { (date: Date?) in date.map { calendar.component(.hour, from: $0) } }
        for typed in ["schedule a meeting today about an exam at 6pm", ">schedule a meeting today about an exam at 6pm"] {
            let exam = draft(typed)
            precondition(exam?.title == "Meeting about exam" && hour(exam?.start) == 18 && exam?.allDay == false, typed)
            precondition(exam.flatMap { $0.start }.map(calendar.isDateInToday) == true && exam.flatMap { d in d.end.map { $0.timeIntervalSince(d.start!) } } == 3600)
        }
        let tomorrow = draft("schedule a meeting for tomorrow at 6pm")
        precondition(tomorrow?.title == "Meeting" && hour(tomorrow?.start) == 18 && tomorrow.flatMap { $0.start }.map(calendar.isDateInTomorrow) == true)
        precondition(draft("add dentist appointment tomorrow at 3pm")?.title == "Dentist appointment")
        let lunch = draft("book lunch with Sarah friday at noon")
        precondition(lunch?.title == "Lunch with Sarah" && hour(lunch?.start) == 12 && lunch.flatMap { $0.start }.map { calendar.component(.weekday, from: $0) } == 6)
        let study = draft("schedule study session tomorrow 2-4pm")
        precondition(study?.title == "Study session" && study.flatMap { d in d.end.map { $0.timeIntervalSince(d.start!) } } == 7200)
        let call = draft("schedule call with Alex on Sept 20 at 10:30 for 30 minutes")
        precondition(call?.title == "Call with Alex" && call.flatMap { d in d.end.map { $0.timeIntervalSince(d.start!) } } == 1800)
        let exam = draft("schedule exam tomorrow")
        precondition(exam?.title == "Exam" && exam?.allDay == true)
        let undated = draft("schedule a meeting")
        precondition(undated?.title == "Meeting" && undated?.start == nil)
        for other in ["create a file tomorrow", "add milk to the list", "schedule today", "my schedule", "find meeting notes tomorrow", "set a timer for 10 minutes"] {
            precondition(draft(other) == nil, other)
        }
        precondition(LocalRequest.parse("schedule today") == .schedule)
        let path = ScheduleProvider.addPath(EventDraft(title: "Meeting about exam", start: Date(timeIntervalSince1970: 1_800_000_000),
                                                       end: Date(timeIntervalSince1970: 1_800_003_600), allDay: false),
                                            start: Date(timeIntervalSince1970: 1_800_000_000), end: Date(timeIntervalSince1970: 1_800_003_600))
        precondition(path.hasPrefix(ScheduleProvider.addEventPrefix) && path.contains("title=Meeting%20about%20exam") && path.contains("start=1800000000"), path)
        for question in [">do i have smth on my calendar today", "Do I have anything on my calendar tomorrow?",
                         "when is my next meeting", "> any meetings this afternoon", "what\u{2019}s on my agenda today"] {
            precondition(LocalRequest.parse(question) == .schedule, question)
        }
        for other in ["schedule a meeting with alex tomorrow", "find my meeting notes", ">summarize the meeting transcript",
                      ">docs what did the meeting decide", "move my meeting to friday", "meeting", "?calendar today"] {
            precondition(LocalRequest.parse(other) != .schedule, other)
        }
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
