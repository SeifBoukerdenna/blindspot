import AppKit
import Carbon.HIToolbox

/// Whether Spotlight still holds ⌘Space, and when it stops.
///
/// This exists because ⌘Space cannot be taken programmatically: the binding lives in a
/// system-owned plist, `RegisterEventHotKey` reports success for a chord another process
/// already holds, and the key then simply never arrives. So blindspot registers anyway,
/// explains the silence once, and watches for the user to turn Spotlight's shortcut off.
///
/// `domain` is a parameter so this can be tested against a preference domain a harness owns.
/// Writing to `com.apple.symbolichotkeys` is the user's business, never blindspot's.
/// The preference domain macOS keeps its own keyboard shortcuts in.
private let systemDomain = "com.apple.symbolichotkeys"
/// Spotlight's "Show Spotlight search" entry within it.
private let spotlightEntry = "64"

@MainActor
struct SpotlightConflict {
    let domain: String

    init(domain: String = systemDomain) {
        self.domain = domain
    }

    /// True when the system's own ⌘Space is enabled — including when the entry is absent,
    /// which means it is at its default, and its default is enabled.
    func ownsCommandSpace() -> Bool {
        // The preferences daemon caches per process; without this, a change made while
        // blindspot is running would never be seen.
        CFPreferencesAppSynchronize(domain as CFString)
        let all =
            CFPreferencesCopyAppValue("AppleSymbolicHotKeys" as CFString, domain as CFString)
            as? [String: Any]
        guard let spotlight = all?[spotlightEntry] as? [String: Any] else { return true }
        guard spotlight["enabled"] as? Bool ?? true else { return false }
        let parameters = (spotlight["value"] as? [String: Any])?["parameters"] as? [Int]
        guard let parameters, parameters.count >= 3 else { return true }
        // [character, key code, NSEvent modifier flags]: 49 is Space, 1 << 20 is ⌘.
        return parameters[1] == kVK_Space && parameters[2] == 1 << 20
    }

    /// Calls `onRelease` once, as soon as Spotlight's ⌘Space is switched off, and stops.
    ///
    /// Polling, because macOS publishes no notification for symbolic hotkey changes that this
    /// could observe instead — and the alternative is telling the user to restart the app after
    /// flipping a switch. One preference read every few seconds, only while the clash lasts.
    /// Cancel the returned task to stop early.
    @discardableResult
    func watch(every interval: Duration = .seconds(3), onRelease: @escaping @MainActor () -> Void)
        -> Task<Void, Never>
    {
        Task { @MainActor in
            while !Task.isCancelled {
                try? await Task.sleep(for: interval)
                guard !Task.isCancelled else { return }
                if !ownsCommandSpace() {
                    onRelease()
                    return
                }
            }
        }
    }
}
