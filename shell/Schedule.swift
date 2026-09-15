import AppKit
import EventKit

/// `:schedule`: today's and tomorrow's events with their meeting links, read through EventKit only
/// after the user allows calendar access. Nothing is stored or sent anywhere; rows are rebuilt
/// from the calendar each time they are shown.
@MainActor
final class ScheduleProvider {
    static let requestAccessPath = "blindspot:calendar-access"
    static let openSettingsPath = "blindspot:calendar-settings"
    static let calendarPath = "blindspot:calendar"
    nonisolated static let meetingServices: [(host: String, name: String)] = [
        ("zoom.us", "Zoom"), ("meet.google.com", "Google Meet"), ("teams.microsoft.com", "Microsoft Teams"),
        ("teams.live.com", "Microsoft Teams"), ("webex.com", "Webex"), ("whereby.com", "Whereby"),
        ("meet.jit.si", "Jitsi Meet"), ("gotomeeting.com", "GoTo Meeting"), ("chime.aws", "Amazon Chime"),
        ("facetime.apple.com", "FaceTime"),
    ]

    var onChange: (() -> Void)?
    private var store: EKEventStore?
    private var requesting = false

    func rows(now: Date = Date()) -> [Match] {
        switch EKEventStore.authorizationStatus(for: .event) {
        case .fullAccess:
            return events(now: now)
        case .notDetermined:
            return [Self.row(1, "Allow Calendar Access",
                             "Blindspot shows today's and tomorrow's events on this Mac · ↩ to allow", Self.requestAccessPath)]
        default:
            return [Self.row(1, "Calendar Access Is Off",
                             "Allow Blindspot in System Settings → Privacy & Security → Calendars · ↩ to open", Self.openSettingsPath)]
        }
    }

    func requestAccess() {
        guard !requesting else { return }
        requesting = true
        let store = eventStore()
        Task { @MainActor [weak self] in
            _ = try? await store.requestFullAccessToEvents()
            guard let self else { return }
            self.requesting = false
            // A store created before access was granted can keep answering with an empty calendar.
            self.store = nil
            self.onChange?()
        }
    }

    private func eventStore() -> EKEventStore {
        if let store { return store }
        let created = EKEventStore()
        store = created
        return created
    }

    private func events(now: Date) -> [Match] {
        let store = eventStore()
        let calendar = Calendar.current
        let today = calendar.startOfDay(for: now)
        guard let tomorrow = calendar.date(byAdding: .day, value: 1, to: today),
              let end = calendar.date(byAdding: .day, value: 2, to: today) else { return [] }
        let upcoming = store.events(matching: store.predicateForEvents(withStart: today, end: end, calendars: nil))
            .filter { $0.endDate > now }
            .sorted { ($0.startDate, $0.isAllDay ? 0 : 1) < ($1.startDate, $1.isAllDay ? 0 : 1) }
        guard !upcoming.isEmpty else {
            return [Self.row(1, "Nothing else today or tomorrow", "No remaining events in your calendars · ↩ opens Calendar", Self.calendarPath)]
        }
        var rows: [Match] = []
        var todaySection: Bool?
        for (index, event) in upcoming.prefix(24).enumerated() {
            let isToday = event.startDate < tomorrow
            if todaySection != isToday {
                todaySection = isToday
                rows.append(Match(id: 0, name: isToday ? "Today" : "Tomorrow", kind: .header, path: "", score: 0,
                                  timestamp: 0, width: 0, height: 0, detail: "", highlights: []))
            }
            let texts = [event.url?.absoluteString, event.location, event.notes.map { String($0.prefix(20_000)) }].compactMap { $0 }
            let link = Self.meetingURL(in: texts)
            let title = (event.title ?? "").isEmpty ? "Untitled event" : (event.title ?? "")
            rows.append(Self.row(UInt64(index + 2), title, Self.describe(event, now: now, link: link),
                                 link?.absoluteString ?? Self.calendarPath))
        }
        return rows
    }

    private static func describe(_ event: EKEvent, now: Date, link: URL?) -> String {
        var parts: [String] = []
        if event.isAllDay {
            parts.append("All day")
        } else {
            let time = DateFormatter()
            time.dateStyle = .none
            time.timeStyle = .short
            if event.startDate <= now {
                parts.append("Now")
            } else if event.startDate.timeIntervalSince(now) < 3600 {
                parts.append("In \(max(1, Int(event.startDate.timeIntervalSince(now) / 60))) min")
            }
            parts.append("\(time.string(from: event.startDate))–\(time.string(from: event.endDate))")
        }
        if let link {
            parts.append(service(for: link))
        } else if let location = event.location, !location.isEmpty {
            parts.append(String(location.prefix(60)))
        }
        return parts.joined(separator: " · ")
    }

    /// The first link to a known meeting service in an event's URL, location or notes, upgraded to
    /// https. Other links are ignored, so a stray URL in the notes is never offered as "Join".
    nonisolated static func meetingURL(in texts: [String]) -> URL? {
        guard let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue) else { return nil }
        for text in texts {
            for found in detector.matches(in: text, range: NSRange(text.startIndex..., in: text)) {
                guard let url = found.url, let scheme = url.scheme?.lowercased(), scheme == "https" || scheme == "http",
                      let host = url.host?.lowercased(),
                      meetingServices.contains(where: { host == $0.host || host.hasSuffix("." + $0.host) }),
                      var components = URLComponents(url: url, resolvingAgainstBaseURL: false) else { continue }
                components.scheme = "https"
                return components.url
            }
        }
        return nil
    }

    nonisolated static func service(for url: URL) -> String {
        let host = url.host?.lowercased() ?? ""
        return meetingServices.first { host == $0.host || host.hasSuffix("." + $0.host) }?.name ?? "Meeting link"
    }

    private static func row(_ id: UInt64, _ name: String, _ detail: String, _ path: String) -> Match {
        Match(id: id, name: name, kind: .event, path: path, score: 0, timestamp: 0, width: 0, height: 0,
              detail: detail, highlights: [])
    }
}
