//! Built-in macOS system commands offered by name. The core names and matches them; the shell runs
//! each through public APIs and asks first for any that end work in progress or delete data.

pub struct SystemCommand {
    pub id: &'static str,
    pub title: &'static str,
    pub detail: &'static str,
    keywords: &'static [&'static str],
}

pub const COMMANDS: &[SystemCommand] = &[
    SystemCommand { id: "lock", title: "Lock Screen", detail: "System · locks this Mac", keywords: &["lock"] },
    SystemCommand { id: "sleep", title: "Sleep", detail: "System · puts this Mac to sleep", keywords: &["sleep"] },
    SystemCommand { id: "sleep-displays", title: "Sleep Displays", detail: "System · turns the displays off", keywords: &["displays off", "screen off"] },
    SystemCommand { id: "screensaver", title: "Start Screen Saver", detail: "System", keywords: &["screen saver", "screensaver"] },
    SystemCommand { id: "capture-text", title: "Copy Text from Screen", detail: "System · select an area and copy its text, read on this Mac", keywords: &["ocr", "screenshot to text", "text from screen", "grab text", "capture text", "scan text", "copy text"] },
    SystemCommand { id: "restart", title: "Restart…", detail: "System · asks first; apps can cancel", keywords: &["restart", "reboot"] },
    SystemCommand { id: "shutdown", title: "Shut Down…", detail: "System · asks first; apps can cancel", keywords: &["shut down", "shutdown", "power off"] },
    SystemCommand { id: "logout", title: "Log Out…", detail: "System · asks first; apps can cancel", keywords: &["log out", "logout", "sign out"] },
    SystemCommand { id: "empty-trash", title: "Empty Trash…", detail: "System · asks first; deletes permanently", keywords: &["empty trash"] },
    SystemCommand { id: "eject", title: "Eject All Disks", detail: "System · ejects removable and network volumes", keywords: &["eject", "unmount"] },
    SystemCommand { id: "dark-mode", title: "Toggle Dark Mode", detail: "System · switches between light and dark appearance", keywords: &["dark mode", "light mode", "appearance"] },
    SystemCommand { id: "mute", title: "Toggle Mute", detail: "System · mutes or unmutes sound output", keywords: &["mute", "unmute"] },
    SystemCommand { id: "quit-all", title: "Quit All Apps…", detail: "System · asks first; apps may ask to save", keywords: &["quit all", "close all apps"] },
];

/// Commands whose title words or keywords start with `query` (three characters at least, so short
/// app searches such as `sl` for Slack are never interrupted).
pub fn matching(query: &str) -> Vec<&'static SystemCommand> {
    let query = query.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    if query.chars().count() < 3 {
        return Vec::new();
    }
    COMMANDS
        .iter()
        .filter(|command| {
            let title = command.title.trim_end_matches('…').to_lowercase();
            title.starts_with(&query)
                || title.split_whitespace().any(|word| word.starts_with(&query))
                || command.keywords.iter().any(|keyword| keyword.starts_with(&query) || query == *keyword)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_match_by_title_words_and_keywords_but_not_short_queries() {
        let ids = |query: &str| matching(query).iter().map(|command| command.id).collect::<Vec<_>>();
        assert_eq!(ids("sle"), ["sleep", "sleep-displays"]);
        assert_eq!(ids("reboot"), ["restart"]);
        assert_eq!(ids("dark"), ["dark-mode"]);
        assert_eq!(ids("empty trash"), ["empty-trash"]);
        assert!(ids("sl").is_empty());
        assert!(ids("safari").is_empty());
        assert!(COMMANDS.iter().all(|command| !command.id.is_empty() && command.detail.starts_with("System")));
    }
}
