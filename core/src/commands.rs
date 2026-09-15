//! First-party command descriptions and settings discovery. Descriptions carry no executable code.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub owner: String,
    pub invocation: String,
    pub help: String,
}

#[derive(Default)]
pub struct Registry {
    commands: Vec<Command>,
}

impl Registry {
    pub fn register(&mut self, command: Command) -> bool {
        if command.owner.is_empty()
            || command.invocation.len() > 512
            || !command.invocation.starts_with([':', '?', ';', '>'])
            || command.invocation.contains(['\0', '\n', '\r'])
            || self
                .commands
                .iter()
                .any(|registered| registered.invocation == command.invocation)
        {
            return false;
        }
        self.commands.push(command);
        self.commands
            .sort_by(|a, b| a.invocation.cmp(&b.invocation));
        true
    }

    pub fn complete(&self, query: &str) -> Option<Vec<&Command>> {
        let query = query.trim();
        if query == ":" || query == ":help" {
            return Some(self.commands.iter().collect());
        }
        if !query.starts_with(':')
            || query.len() < 2
            || self.commands.iter().any(|c| c.invocation.trim() == query)
        {
            return None;
        }
        let matches: Vec<_> = self
            .commands
            .iter()
            .filter(|c| c.invocation.starts_with(query))
            .collect();
        (!matches.is_empty()).then_some(matches)
    }
}

pub fn builtins() -> &'static Registry {
    static REGISTRY: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry = Registry::default();
        for (owner, invocation, help) in [
            ("process", ":ports", "Show TCP listeners and UDP sockets; :ports PID filters by owner"),
            ("process", ":pid ", "Inspect a visible process by PID"),
            ("process", ":children ", "Show immediate child processes of a PID"),
            (
                "process",
                ":processes",
                "Show visible running processes; :node filters by name",
            ),
            (
                "process",
                ":localhost",
                "Show TCP/UDP sockets bound to loopback interfaces",
            ),
            (
                "process",
                ":3000",
                "Find the process using a TCP/UDP port; replace 3000 with your port",
            ),
            (
                "settings",
                ":settings ",
                "Search Blindspot settings by label, section, or key",
            ),
            ("files", "?kind:pdf size:>5MB", "Find PDFs larger than 5 MB"),
            (
                "files",
                "?modified:week",
                "Find files changed in the past seven days",
            ),
            ("clipboard", ";", "Search local clipboard history"),
            ("content", ":content ", "Search text inside indexed files; filters kind:pdf, kind:documents, modified:month, size:>1MB"),
            ("shortcuts", ":link ", "Save a quick link: :link gh https://github.com/search?q={query}"),
            ("shortcuts", ":links", "List quick links; type a keyword and search text to open one"),
            ("shortcuts", ":snippet ", "Save a text snippet: :snippet sig Best regards,\\nSeif"),
            ("shortcuts", ":snippets", "List snippets; type !keyword to paste one"),
            ("shortcuts", ":prompt ", "Save an AI command: :prompt tldr Summarize in one sentence (then select text and type ai tldr)"),
            ("shortcuts", ":prompts", "List AI commands such as fix grammar and make shorter"),
            ("system", ":system", "System commands: lock, sleep, restart, empty Trash, eject, dark mode, mute"),
            ("calendar", ":schedule", "Today's and tomorrow's calendar events with meeting links"),
            (
                "agent",
                ">",
                "Ask the local model; Return submits the request",
            ),
        ] {
            registry.register(Command {
                owner: owner.into(),
                invocation: invocation.into(),
                help: help.into(),
            });
        }
        registry
    })
}

pub fn settings_request(query: &str) -> Option<&str> {
    let suffix = query.strip_prefix(":settings")?;
    (suffix.is_empty() || suffix.starts_with(char::is_whitespace)).then(|| suffix.trim())
}

pub fn content_request(query: &str) -> Option<&str> {
    let suffix = query.strip_prefix(":content")?;
    (suffix.is_empty() || suffix.starts_with(char::is_whitespace)).then(|| suffix.trim())
}

pub fn settings(query: &str, limit: usize) -> Vec<&'static crate::settings::SettingDef> {
    let query = query.to_lowercase();
    let words: Vec<_> = query.split_whitespace().collect();
    let mut found: Vec<_> = crate::settings::SCHEMA
        .iter()
        .filter(|setting| {
            let searchable = format!(
                "{} {} {} {}",
                setting.label,
                setting.key,
                setting.section.name(),
                setting.help
            )
            .to_lowercase();
            words.iter().all(|word| searchable.contains(word))
        })
        .collect();
    found.sort_by(|a, b| {
        let score = |setting: &crate::settings::SettingDef| {
            (
                setting.key.eq_ignore_ascii_case(&query),
                crate::relevance::tier(setting.label, &query),
            )
        };
        score(b).cmp(&score(a)).then(a.key.cmp(b.key))
    });
    found.truncate(limit);
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registration_is_unique_and_completions_do_not_replace_exact_commands() {
        let mut registry = Registry::default();
        let command = Command {
            owner: "test".into(),
            invocation: ":status".into(),
            help: "Status".into(),
        };
        assert!(registry.register(command.clone()));
        assert!(!registry.register(command));
        assert_eq!(
            registry.complete(":sta").expect("prefix")[0].invocation,
            ":status"
        );
        assert!(registry.complete(":status").is_none());
        assert!(registry.complete("status").is_none());
    }
    #[test]
    fn builtins_are_discoverable_and_process_filters_remain_available() {
        assert_eq!(builtins().complete(":help").expect("help").len(), 20);
        assert_eq!(
            builtins().complete(":po").expect("prefix")[0].invocation,
            ":ports"
        );
        assert!(builtins().complete(":node").is_none());
        assert!(builtins().complete("?mod").is_none());
        assert!(settings_request(":settingsd").is_none());
        assert_eq!(settings_request(":settings  hotkey"), Some("hotkey"));
    }
    #[test]
    fn settings_search_uses_schema_labels_keys_and_sections() {
        assert_eq!(settings("hotkey", 1)[0].key, "hotkey");
        assert_eq!(settings("agent.model", 1)[0].key, "agent.model");
        assert!(
            settings("clipboard", 50)
                .iter()
                .all(|s| s.section == crate::settings::Section::Clipboard)
        );
        assert!(!settings("clipboard", 50).is_empty());
        assert!(settings("no-such-setting-xyzzy", 50).is_empty());
        assert!(settings("", 0).is_empty());
    }
}
