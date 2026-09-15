import Foundation

/// Deterministic interpretations never pass executable text to a shell or model.
enum LocalRequest: Equatable {
    case search(String)
    case terminatePort(UInt16)
    case currentProject
    case schedule

    static func parse(_ text: String) -> LocalRequest? {
        let normalized = text.lowercased().split(whereSeparator: { $0.isWhitespace }).joined(separator: " ")
        if [":schedule", ":today", ":calendar"].contains(normalized) { return .schedule }
        if !text.hasPrefix(":"), !text.hasPrefix("?"), !text.hasPrefix(";"), asksAboutCalendar(normalized) {
            return .schedule
        }
        guard !text.hasPrefix(":"), !text.hasPrefix(">"), !text.hasPrefix("?"), !text.hasPrefix(";") else { return nil }
        for prefix in ["kill the process using port ", "close whatever is using port ", "terminate the process using port ", "kill process on port "] {
            if normalized.hasPrefix(prefix), let port = UInt16(normalized.dropFirst(prefix.count)), port > 0 {
                return .terminatePort(port)
            }
        }
        switch normalized {
        case "show everything listening on localhost", "show localhost ports": return .search(":localhost")
        case "show listening ports", "show all ports": return .search(":ports")
        case "find screenshots from this week larger than 5 mb": return .search("?Screenshot kind:image modified:week size:>5MB")
        case "show me files i haven't touched in six months", "find files not opened in six months": return .search("?used:>180d")
        case "open current project", "open this project": return .currentProject
        case "my schedule", "schedule", "today's schedule", "todays schedule", "what's on today", "whats on today",
             "what's on my calendar", "whats on my calendar", "my calendar", "my meetings", "meetings today",
             "next meeting", "my next meeting", "join meeting", "join my next meeting":
            return .schedule
        default: break
        }
        for prefix in ["ask my documents ", "ask my notes ", "ask my files ", "ask my docs "] where normalized.hasPrefix(prefix) {
            let question = String(normalized.dropFirst(prefix.count))
            guard !question.isEmpty, question.utf8.count <= 512 else { return nil }
            return .search(">docs " + question)
        }
        for (prefix, target) in [
            ("find documents about ", ":content kind:documents "),
            ("documents about ", ":content kind:documents "),
            ("find the document about ", ":content kind:documents "),
            ("document about ", ":content kind:documents "),
            ("find files about ", ":content "),
            ("files about ", ":content "),
            ("find code mentioning ", ":content kind:code "),
        ] {
            if normalized.hasPrefix(prefix) {
                let words = String(normalized.dropFirst(prefix.count))
                guard !words.isEmpty, words.utf8.count <= 512 else { return nil }
                return .search(target + words)
            }
        }
        return nil
    }

    /// A calendar question in everyday words, typed plainly or after `>`. The local model has no
    /// calendar access and would only say so, so the question is answered from EventKit instead.
    ///
    /// It needs a calendar word and a question or time cue. Requests to create or change events,
    /// and anything about notes, files or summaries, are left to search and the model.
    static func asksAboutCalendar(_ normalized: String) -> Bool {
        var text = normalized.replacingOccurrences(of: "\u{2019}", with: "'")
        if text.hasPrefix(">") { text = text.dropFirst().trimmingCharacters(in: .whitespaces) }
        guard !text.isEmpty, text.utf8.count <= 160 else { return false }
        let words = Set(text.split { !$0.isLetter && !$0.isNumber && $0 != "'" }.map(String.init))
        let subjects: Set = ["calendar", "calendars", "schedule", "agenda", "meeting", "meetings", "appointment", "appointments", "events"]
        let excluded: Set = [
            "notes", "note", "file", "files", "document", "documents", "doc", "docs", "pdf", "transcript", "recording",
            "email", "summarize", "summary", "create", "add", "book", "cancel", "delete", "move", "reschedule",
            "invite", "remind", "reminder", "set", "port",
        ]
        let cues: Set = [
            "today", "tomorrow", "tonight", "morning", "afternoon", "evening", "next", "upcoming", "now", "left",
            "anything", "something", "smth", "any", "have", "got", "what's", "whats", "when", "busy", "free", "my",
        ]
        guard !words.isDisjoint(with: subjects), words.isDisjoint(with: excluded), !words.isDisjoint(with: cues) else { return false }
        return !["schedule a ", "schedule an ", "schedule the ", "schedule time", "schedule with "].contains { text.hasPrefix($0) }
    }

    var query: String? {
        switch self {
        case .search(let query): query
        case .terminatePort(let port): ":\(port)"
        case .currentProject: nil
        case .schedule: ":schedule"
        }
    }

    var status: String {
        switch self {
        case .terminatePort: "SELECT PROCESS · ↩ REVIEW TERMINATION"
        case .search: "LOCAL SEARCH · ACTIONS ⌘K"
        case .currentProject: "CURRENT FOLDER · ↩ OPEN"
        case .schedule: "SCHEDULE · ↩ JOIN OR OPEN"
        }
    }
}
