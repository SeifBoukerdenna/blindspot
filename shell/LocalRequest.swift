import Foundation

/// A calendar event typed as a request, such as "schedule a meeting tomorrow at 6pm". Parsed on this
/// Mac with NSDataDetector, never by a model. Nothing is added until its preview row is chosen, and
/// that row carries exactly these values.
struct EventDraft: Equatable, Sendable {
    let title: String
    /// Nil when the request named no date or time; the preview then asks for one.
    let start: Date?
    let end: Date?
    let allDay: Bool
}

/// Deterministic interpretations never pass executable text to a shell or model.
enum LocalRequest: Equatable {
    case search(String)
    case terminatePort(UInt16)
    case currentProject
    case schedule
    case createEvent(EventDraft)

    static func parse(_ text: String) -> LocalRequest? {
        let normalized = text.lowercased().split(whereSeparator: { $0.isWhitespace }).joined(separator: " ")
        if [":schedule", ":today", ":calendar"].contains(normalized) { return .schedule }
        if !text.hasPrefix(":"), !text.hasPrefix("?"), !text.hasPrefix(";") {
            if let draft = eventDraft(from: text) { return .createEvent(draft) }
            if asksAboutCalendar(normalized) { return .schedule }
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

    /// Built once: requests are parsed on every keystroke that starts with an event verb.
    private static let dates = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.date.rawValue)

    /// "schedule …" / "book …", or "add/create/put/plan/new/set up … meeting|event|appointment|call|calendar …".
    /// Cheap word checks come first, so ordinary searches never reach the date detector.
    static func eventDraft(from text: String, now: Date = Date(), calendar: Calendar = .current) -> EventDraft? {
        var request = text.trimmingCharacters(in: .whitespaces)
        if request.hasPrefix(">") { request = request.dropFirst().trimmingCharacters(in: .whitespaces) }
        guard !request.isEmpty, request.utf8.count <= 240 else { return nil }
        let words = request.lowercased().split { !$0.isLetter && !$0.isNumber }.map(String.init)
        guard let verb = words.first, words.count > 1 else { return nil }
        let nouns: Set = ["meeting", "event", "appointment", "call", "calendar"]
        switch verb {
        case "schedule":
            // "schedule today" and "schedule for tomorrow" ask to see the schedule, not to add to it.
            guard !["today", "tomorrow", "tonight", "for", "this", "next", "on", "at", "now"].contains(words[1]) else { return nil }
        case "book":
            break
        case "add", "create", "put", "plan", "new", "set":
            guard words.contains(where: nouns.contains) else { return nil }
        default:
            return nil
        }
        let excluded: Set = ["file", "files", "folder", "directory", "document", "note", "notes", "reminder", "reminders", "alarm", "timer", "port"]
        guard !words.contains(where: excluded.contains) else { return nil }

        let source = request as NSString
        var day: Date?
        var time: Date?
        var duration: TimeInterval = 0
        var removed: [NSRange] = []
        for match in dates?.matches(in: request, range: NSRange(location: 0, length: source.length)) ?? [] {
            guard let date = match.date else { continue }
            let piece = source.substring(with: match.range).lowercased()
            let timed = piece.contains(where: \.isNumber) || piece.contains("noon") || piece.contains("midnight")
            if timed, time == nil {
                time = date
                duration = match.duration
            } else if !timed, day == nil {
                day = date
            }
            removed.append(match.range)
        }
        var lowered = request.lowercased()
        if let explicit = lowered.range(of: #"\bfor (\d{1,3}) ?(minutes?|mins?|hours?|hrs?|h)\b"#, options: .regularExpression) {
            let phrase = String(lowered[explicit])
            let amount = Double(phrase.filter(\.isNumber)) ?? 0
            duration = phrase.contains("h") && !phrase.contains("min") ? amount * 3600 : amount * 60
            removed.append(NSRange(explicit, in: lowered))
            lowered = ""
        }

        var remaining = source
        for range in removed.sorted(by: { $0.location > $1.location }) {
            remaining = remaining.replacingCharacters(in: range, with: " ") as NSString
        }
        let title = Self.eventTitle(from: remaining as String)

        var start: Date?
        var end: Date?
        var allDay = false
        if let time {
            var moment = time
            if let day {
                let parts = calendar.dateComponents([.year, .month, .day], from: day)
                let clock = calendar.dateComponents([.hour, .minute], from: time)
                moment = calendar.date(from: DateComponents(year: parts.year, month: parts.month, day: parts.day,
                                                            hour: clock.hour, minute: clock.minute)) ?? time
            } else if moment < now, let next = calendar.date(byAdding: .day, value: 1, to: moment) {
                // "at 9am" typed in the afternoon means tomorrow morning.
                moment = next
            }
            start = moment
            end = moment.addingTimeInterval(duration > 0 ? min(duration, 24 * 3600) : 3600)
        } else if let day {
            let first = calendar.startOfDay(for: day)
            start = first
            end = calendar.date(byAdding: .day, value: 1, to: first)
            allDay = true
        }
        return EventDraft(title: title, start: start, end: end, allDay: allDay)
    }

    /// "schedule a meeting  about an exam at " → "Meeting about exam"; "book lunch with Sarah " → "Lunch with Sarah".
    static func eventTitle(from remainder: String) -> String {
        let fillers: Set = ["for", "at", "on", "by", "from", "to", "in", "this", "next"]
        let articles: Set = ["a", "an", "the", "my", "new", "up"]
        var tokens = remainder
            .replacingOccurrences(of: #"(?i)\b(to|on|in|into) (my|the) calendar\b"#, with: " ", options: .regularExpression)
            .split(whereSeparator: \.isWhitespace)
            .map { $0.trimmingCharacters(in: CharacterSet(charactersIn: "?!.,;:\"")) }
            .filter { !$0.isEmpty }
        if !tokens.isEmpty { tokens.removeFirst() }
        func tidy(_ part: [String]) -> [String] {
            var part = part
            while let first = part.first, articles.contains(first.lowercased()) || fillers.contains(first.lowercased()) { part.removeFirst() }
            while let last = part.last, fillers.contains(last.lowercased()) || articles.contains(last.lowercased()) { part.removeLast() }
            return part
        }
        var subject = tokens
        var topic: [String] = []
        if let about = tokens.firstIndex(where: { $0.lowercased() == "about" }) {
            subject = Array(tokens[..<about])
            topic = tidy(Array(tokens[(about + 1)...]))
        }
        subject = tidy(subject)
        let generic = subject.count == 1 && ["event", "calendar", "appointment"].contains(subject[0].lowercased())
        var title: String
        switch (subject.isEmpty || generic, topic.isEmpty) {
        case (true, true): title = subject.first.map { $0.lowercased() == "calendar" ? "Event" : $0 } ?? "Event"
        case (true, false): title = topic.joined(separator: " ")
        case (false, true): title = subject.joined(separator: " ")
        case (false, false): title = subject.joined(separator: " ") + " about " + topic.joined(separator: " ")
        }
        title = String(title.prefix(120))
        return title.prefix(1).uppercased() + title.dropFirst()
    }

    var query: String? {
        switch self {
        case .search(let query): query
        case .terminatePort(let port): ":\(port)"
        case .currentProject: nil
        case .schedule, .createEvent: ":schedule"
        }
    }

    var status: String {
        switch self {
        case .terminatePort: "SELECT PROCESS · ↩ REVIEW TERMINATION"
        case .search: "LOCAL SEARCH · ACTIONS ⌘K"
        case .currentProject: "CURRENT FOLDER · ↩ OPEN"
        case .schedule: "SCHEDULE · ↩ JOIN OR OPEN"
        case .createEvent: "NEW EVENT · ↩ ADD TO CALENDAR"
        }
    }
}
