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
        await explanations()
        print("Action safety and native file operations: 11 passed; contextual commands and permission fallback: 2 passed; typed navigation: 2 passed")
    }

    static func explanations() async {
        func row(_ detail: String = "", highlights: [Int] = [], kind: MatchKind = .file,
                 path: String = "/fixture/Glacier permits.pdf") -> Match {
            Match(id: 19, name: "Glacier permits.pdf", kind: kind, path: path, score: .max,
                  timestamp: 0, width: 0, height: 0, detail: detail, highlights: highlights, page: 7)
        }
        let evidence: [(Match, ResultExplanation.Evidence)] = [
            (row(highlights: [0, 1]), .filename),
            (row(highlights: [0], kind: .app), .application),
            (row("Words · “overnight permits”"), .words),
            (row("Meaning · “overnight permits”"), .meaning),
            (row("Words + meaning · “overnight permits”", highlights: [0]), .blended),
            (row("“overnight permits”"), .contentWords),
            (row("Content match"), .contentWords),
            (row("Content match · limited relevance; refine query"), .contentWords),
            (row("Related by meaning · “overnight permits”"), .contentMeaning),
            (row("Related by meaning · approximate, no exact word match"), .contentMeaning),
            (row(), .unknown),
            (row("Wordsworth notes"), .unknown),
            (row("Meaning · forged", kind: .tool), .unknown),
            (row("Words + meaning · forged", kind: .app), .unknown),
        ]
        for (result, expected) in evidence {
            precondition(ResultExplanation.evidence(for: result) == expected, result.detail)
            let body = ResultExplanation.body(for: result, inspection: nil, passageAvailable: nil)
            precondition(!body.contains("4294967295") && !body.contains("%"))
        }
        func inspection(_ title: String, detail: String = "Fixture status", next: String = "Check again.") -> FileInspection {
            FileInspection(title: title, detail: detail, next: next, setting: "", root: "/fixture", passages: 1, embedded: 1)
        }
        let passage = row("Words + meaning · “overnight permits”")
        let fresh = inspection("Indexed for word search")
        let statuses = [
            (fresh, "Source metadata matches"),
            (inspection("Partially indexed"), "only part of the file"),
            (inspection("Indexed copy is out of date"), "out of date"),
            (inspection("File unavailable to the indexer"), "unavailable"),
            (inspection("Outside your indexed folders"), "Outside your indexed folders"),
            (inspection("Extraction did not succeed", detail: "Older passages may be stale."), "may be stale"),
            (inspection("Index could not be read"), "could not be read"),
        ]
        for (inspection, expected) in statuses {
            let body = ResultExplanation.body(for: passage, inspection: inspection, passageAvailable: true)
            precondition(body.contains(expected), body)
            if inspection.title != "Indexed for word search" && inspection.title != "Partially indexed" {
                precondition(!body.contains("metadata matches"))
            }
        }
        let missing = ResultExplanation.body(for: passage, inspection: fresh, passageAvailable: false)
        precondition(missing.contains("selected passage is no longer available") && missing.contains("Search again"))
        precondition(ResultExplanation.body(for: passage, inspection: nil, passageAvailable: nil).contains("Could not verify"))
        let bounded = ResultExplanation.body(for: row("Meaning · text"), inspection:
            inspection("Failure", detail: String(repeating: "a", count: 10_000), next: String(repeating: "b", count: 10_000)), passageAvailable: false)
        precondition(bounded.count < 2000)

        let provider = SearchExplanationActions(inspect: { path in
            precondition(!Thread.isMainThread && path == "/fixture/Glacier permits.pdf")
            return fresh
        }, passageExists: { id, path in
            precondition(!Thread.isMainThread && id == 19 && path == "/fixture/Glacier permits.pdf")
            return false
        })
        let registry = ActionRegistry()
        precondition(registry.actions(for: passage).filter { $0.label == "Why this result?" }.count == 1)
        for bad in [row(kind: .tool), row(kind: .header), row(path: "relative.pdf"), row(path: "/invalid\0.pdf"), row(path: "/" + String(repeating: "a", count: 4096))] {
            precondition(provider.actions(for: bad).isEmpty)
        }
        do {
            let action = provider.actions(for: passage)[0]
            let effect = try await provider.perform(action.id, on: passage)
            guard case .message(let title, let body) = effect else { preconditionFailure("Not an explanation") }
            precondition(title == "Why this result?" && body.contains("Words + meaning") && body.contains("Search again"))
            let fallback = try await registry.perform(action, on: passage, confirmed: false)
            guard case .message(_, let fallbackBody) = fallback else { preconditionFailure("Missing fallback") }
            precondition(fallbackBody.contains("Could not verify"))
            let app = row(highlights: [0], kind: .app)
            let appEffect = try await provider.perform(action.id, on: app)
            guard case .message(_, let appBody) = appEffect else { preconditionFailure("Missing app explanation") }
            precondition(appBody.contains("Not applicable"))
        } catch { preconditionFailure("Explanation failed: \(error)") }

        let delayed = SearchExplanationActions(inspect: { _ in
            Thread.sleep(forTimeInterval: 0.08)
            return fresh
        })
        let cancelled = Task { try await delayed.perform(delayed.actions(for: passage)[0].id, on: passage) }
        try? await Task.sleep(for: .milliseconds(20))
        cancelled.cancel()
        do {
            _ = try await cancelled.value
            preconditionFailure("Cancelled explanation was delivered")
        } catch is CancellationError {} catch { preconditionFailure("Wrong cancellation error") }
        print("Search explanations: \(evidence.count) provenance cases, \(statuses.count) freshness cases, missing passage, bounded output, action dispatch, worker-thread checks and cancellation passed")

        if CommandLine.arguments.contains("--explanation-ui") {
            let app = NSApplication.shared
            app.setActivationPolicy(.accessory)
            let alert = NSAlert()
            alert.messageText = "Why this result?"
            alert.informativeText = ResultExplanation.body(for: passage, inspection: fresh, passageAvailable: true)
            alert.addButton(withTitle: "Done")
            alert.layout()
            let window = alert.window
            window.appearance = NSAppearance(named: .darkAqua)
            window.center()
            window.makeKeyAndOrderFront(nil)
            guard let view = window.contentView else { preconditionFailure("No explanation view") }
            view.layoutSubtreeIfNeeded()
            precondition(window.frame.height < (window.screen?.visibleFrame.height ?? 800))
            guard let bitmap = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { preconditionFailure("No capture") }
            view.cacheDisplay(in: view.bounds, to: bitmap)
            guard let png = bitmap.representation(using: .png, properties: [:]) else { preconditionFailure("No PNG") }
            try! png.write(to: URL(fileURLWithPath: "build/search-explanation.png"))
            window.close()
            precondition(!window.isVisible)
            print("Native explanation dialog: layout, screenshot and close passed")
        }
    }
}
