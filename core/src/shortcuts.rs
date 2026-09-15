//! Quick links and text snippets saved from the launcher.
//!
//! Stored in `shortcuts.toml` beside the other window-written state rather than in config.toml,
//! which stays the user's. Entries are validated on the way in and again on load, so a
//! hand-edited file cannot introduce a non-web URL or an unbounded snippet.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

pub const MAX_ENTRIES: usize = 256;
pub const MAX_SNIPPET_BYTES: usize = 16 * 1024;
const MAX_URL_BYTES: usize = 2048;
pub const SNIPPET_PREFIX: char = '!';

/// AI commands that work on selected text without any setup. Saved commands are invoked as
/// `ai KEYWORD` instead, so a keyword such as `mail` can never turn app search into an AI request.
pub const BUILT_IN_PROMPTS: &[(&str, &str)] = &[
    ("fix grammar", "Fix spelling, grammar and punctuation. Keep the meaning, tone and language. Return only the corrected text."),
    ("make shorter", "Make this shorter while keeping every important fact, the tone and the language. Return only the rewritten text."),
    ("make longer", "Expand this with a little more detail and clarity without inventing facts. Keep the language. Return only the rewritten text."),
    ("make friendlier", "Rewrite this in a warmer, friendlier tone. Keep the meaning and language. Return only the rewritten text."),
    ("make formal", "Rewrite this in a clear, formal, professional tone. Keep the meaning and language. Return only the rewritten text."),
    ("bullet points", "Turn this into concise bullet points. Return only the bullet points."),
    ("explain simply", "Explain this in simple words that a newcomer would understand."),
    ("write reply", "Write a short, polite reply to this message in the same language. Return only the reply."),
];
const MAX_PROMPT_BYTES: usize = 2048;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Data {
    links: BTreeMap<String, String>,
    snippets: BTreeMap<String, String>,
    prompts: BTreeMap<String, String>,
}

/// A `:link` / `:snippet` family command typed into the launcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command<'a> {
    SaveLink { keyword: &'a str, url: &'a str },
    SaveSnippet { keyword: &'a str, text: String },
    RemoveLink(&'a str),
    RemoveSnippet(&'a str),
    ListLinks(&'a str),
    ListSnippets(&'a str),
    SavePrompt { keyword: &'a str, text: String },
    RemovePrompt(&'a str),
    ListPrompts(&'a str),
}

pub fn parse(query: &str) -> Option<Command<'_>> {
    let query = query.trim_start();
    let (head, rest) = match query.split_once(char::is_whitespace) {
        Some((head, rest)) => (head, rest.trim()),
        None => (query.trim_end(), ""),
    };
    match head {
        ":links" => Some(Command::ListLinks(rest)),
        ":snippets" => Some(Command::ListSnippets(rest)),
        ":prompts" => Some(Command::ListPrompts(rest)),
        ":prompt" if rest.is_empty() => Some(Command::ListPrompts("")),
        ":prompt" => Some(match rest.split_once(char::is_whitespace) {
            Some((keyword, text)) => Command::SavePrompt { keyword, text: text.trim().to_owned() },
            None => Command::SavePrompt { keyword: rest, text: String::new() },
        }),
        ":unprompt" => Some(Command::RemovePrompt(rest)),
        ":link" | ":snippet" if rest.is_empty() => Some(if head == ":link" { Command::ListLinks("") } else { Command::ListSnippets("") }),
        ":link" => Some(match rest.split_once(char::is_whitespace) {
            Some((keyword, url)) => Command::SaveLink { keyword, url: url.trim() },
            None => Command::SaveLink { keyword: rest, url: "" },
        }),
        ":snippet" => Some(match rest.split_once(char::is_whitespace) {
            Some((keyword, text)) => Command::SaveSnippet { keyword, text: text.trim().replace("\\n", "\n").replace("\\t", "\t") },
            None => Command::SaveSnippet { keyword: rest, text: String::new() },
        }),
        ":unlink" => Some(Command::RemoveLink(rest)),
        ":unsnippet" => Some(Command::RemoveSnippet(rest)),
        _ => None,
    }
}

pub fn valid_keyword(keyword: &str) -> bool {
    (1..=32).contains(&keyword.len())
        && keyword.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_')
}

/// Web pages only: a quick link is opened with one keystroke, so a `file:` or custom-scheme
/// target (which could launch an app or script handler) is refused.
pub fn valid_url(url: &str) -> bool {
    url.len() <= MAX_URL_BYTES
        && (url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).is_some_and(|rest| !rest.is_empty()))
        && !url.chars().any(|character| character.is_whitespace() || character.is_control())
}

/// Fills `{query}` with the percent-encoded search text, keeping only RFC 3986 unreserved bytes.
pub fn expand(template: &str, query: &str) -> String {
    let mut encoded = String::with_capacity(query.len() * 3);
    for byte in query.trim().bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    template.replace("{query}", &encoded)
}

/// `gh blindspot` → (`gh`, `blindspot`). The caller checks the keyword is a saved link.
pub fn split_link(query: &str) -> Option<(&str, &str)> {
    let (keyword, rest) = query.split_once(' ')?;
    valid_keyword(keyword).then_some((keyword, rest.trim()))
}

pub struct Library {
    path: Option<PathBuf>,
    data: Mutex<Data>,
}

impl Library {
    pub fn open(path: Option<PathBuf>) -> Self {
        let data = path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| toml::from_str::<Data>(&text).ok())
            .map(sanitized)
            .unwrap_or_default();
        Self { path, data: Mutex::new(data) }
    }

    fn data(&self) -> MutexGuard<'_, Data> {
        self.data.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn links(&self) -> Vec<(String, String)> {
        self.data().links.iter().map(|(keyword, url)| (keyword.clone(), url.clone())).collect()
    }

    pub fn snippets(&self) -> Vec<(String, String)> {
        self.data().snippets.iter().map(|(keyword, text)| (keyword.clone(), text.clone())).collect()
    }

    pub fn link(&self, keyword: &str) -> Option<String> {
        self.data().links.get(keyword).cloned()
    }

    pub fn save_link(&self, keyword: &str, url: &str) -> Result<(), &'static str> {
        if !valid_keyword(keyword) {
            return Err("Keywords are 1–32 lowercase letters, digits, - or _");
        }
        if !valid_url(url) {
            return Err("Quick links must be http:// or https:// URLs; put {query} where the search text goes");
        }
        self.update(|data| {
            if !data.links.contains_key(keyword) && data.links.len() >= MAX_ENTRIES {
                return Err("Quick link limit reached");
            }
            data.links.insert(keyword.to_owned(), url.to_owned());
            Ok(())
        })
    }

    pub fn save_snippet(&self, keyword: &str, text: &str) -> Result<(), &'static str> {
        if !valid_keyword(keyword) {
            return Err("Keywords are 1–32 lowercase letters, digits, - or _");
        }
        if text.trim().is_empty() || text.len() > MAX_SNIPPET_BYTES || text.contains('\0') {
            return Err("Snippets hold 1 byte to 16 KiB of text");
        }
        self.update(|data| {
            if !data.snippets.contains_key(keyword) && data.snippets.len() >= MAX_ENTRIES {
                return Err("Snippet limit reached");
            }
            data.snippets.insert(keyword.to_owned(), text.to_owned());
            Ok(())
        })
    }

    pub fn prompts(&self) -> Vec<(String, String)> {
        self.data().prompts.iter().map(|(keyword, text)| (keyword.clone(), text.clone())).collect()
    }

    /// The AI command `text` names, as (title, instruction): a built-in phrase such as `fix grammar`
    /// (optionally after `ai `), or a saved keyword after `ai `.
    pub fn prompt(&self, text: &str) -> Option<(String, String)> {
        let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        let (named, name) = match normalized.strip_prefix("ai ") {
            Some(rest) => (true, rest.to_owned()),
            None => (false, normalized.clone()),
        };
        if let Some((title, instruction)) = BUILT_IN_PROMPTS.iter().find(|(title, _)| *title == name) {
            return Some(((*title).to_owned(), (*instruction).to_owned()));
        }
        if !named {
            return None;
        }
        self.data().prompts.get(&name).map(|instruction| (name.clone(), instruction.clone()))
    }

    pub fn save_prompt(&self, keyword: &str, text: &str) -> Result<(), &'static str> {
        if !valid_keyword(keyword) {
            return Err("Keywords are 1–32 lowercase letters, digits, - or _");
        }
        if text.trim().is_empty() || text.len() > MAX_PROMPT_BYTES || text.contains('\0') {
            return Err("AI command instructions hold 1 byte to 2 KiB of text");
        }
        self.update(|data| {
            if !data.prompts.contains_key(keyword) && data.prompts.len() >= MAX_ENTRIES {
                return Err("AI command limit reached");
            }
            data.prompts.insert(keyword.to_owned(), text.to_owned());
            Ok(())
        })
    }

    pub fn remove_prompt(&self, keyword: &str) -> Result<bool, &'static str> {
        self.update(|data| Ok(data.prompts.remove(keyword).is_some()))
    }

    pub fn remove_link(&self, keyword: &str) -> Result<bool, &'static str> {
        self.update(|data| Ok(data.links.remove(keyword).is_some()))
    }

    pub fn remove_snippet(&self, keyword: &str) -> Result<bool, &'static str> {
        self.update(|data| Ok(data.snippets.remove(keyword).is_some()))
    }

    fn update<T>(&self, change: impl FnOnce(&mut Data) -> Result<T, &'static str>) -> Result<T, &'static str> {
        let mut data = self.data();
        let mut next = data.clone();
        let result = change(&mut next)?;
        if next != *data {
            self.persist(&next)?;
            *data = next;
        }
        Ok(result)
    }

    /// Written to a private temporary file and renamed into place, so a crash mid-save leaves
    /// the previous shortcuts rather than a truncated file.
    fn persist(&self, data: &Data) -> Result<(), &'static str> {
        const FAILED: &str = "Could not save shortcuts";
        let Some(path) = &self.path else { return Ok(()) };
        let text = toml::to_string(data).map_err(|_| FAILED)?;
        let temporary = path.with_extension("toml.tmp");
        let mut file = std::fs::OpenOptions::new()
            .write(true).create(true).truncate(true).mode(0o600)
            .open(&temporary).map_err(|_| FAILED)?;
        file.write_all(text.as_bytes()).and_then(|()| file.sync_all()).map_err(|_| FAILED)?;
        std::fs::rename(&temporary, path).map_err(|_| FAILED)
    }
}

fn sanitized(mut data: Data) -> Data {
    data.links.retain(|keyword, url| valid_keyword(keyword) && valid_url(url));
    data.snippets.retain(|keyword, text| {
        valid_keyword(keyword) && !text.trim().is_empty() && text.len() <= MAX_SNIPPET_BYTES && !text.contains('\0')
    });
    data.prompts.retain(|keyword, text| valid_keyword(keyword) && !text.trim().is_empty() && text.len() <= MAX_PROMPT_BYTES && !text.contains('\0'));
    while data.prompts.len() > MAX_ENTRIES { data.prompts.pop_last(); }
    while data.links.len() > MAX_ENTRIES { data.links.pop_last(); }
    while data.snippets.len() > MAX_ENTRIES { data.snippets.pop_last(); }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn commands_parse_saves_lists_and_removals() {
        assert_eq!(parse(":link gh https://github.com/search?q={query}"),
            Some(Command::SaveLink { keyword: "gh", url: "https://github.com/search?q={query}" }));
        assert_eq!(parse(":link gh"), Some(Command::SaveLink { keyword: "gh", url: "" }));
        assert_eq!(parse(":link"), Some(Command::ListLinks("")));
        assert_eq!(parse(":links git"), Some(Command::ListLinks("git")));
        assert_eq!(parse(":snippet sig Best regards,\\nSeif"),
            Some(Command::SaveSnippet { keyword: "sig", text: "Best regards,\nSeif".into() }));
        assert_eq!(parse(":unsnippet sig"), Some(Command::RemoveSnippet("sig")));
        assert_eq!(parse(":linker"), None);
        assert_eq!(parse(":ports"), None);
    }

    #[test]
    fn ai_commands_resolve_built_ins_and_saved_keywords_only_after_ai() {
        let library = Library::open(None);
        assert_eq!(library.prompt("Fix  Grammar").map(|found| found.0).as_deref(), Some("fix grammar"));
        assert_eq!(library.prompt("ai make shorter").map(|found| found.0).as_deref(), Some("make shorter"));
        assert_eq!(parse(":prompt tldr Summarize in one sentence."), Some(Command::SavePrompt { keyword: "tldr", text: "Summarize in one sentence.".into() }));
        assert_eq!(parse(":prompts"), Some(Command::ListPrompts("")));
        library.save_prompt("tldr", "Summarize in one sentence.").unwrap();
        assert_eq!(library.prompt("ai tldr").map(|found| found.1).as_deref(), Some("Summarize in one sentence."));
        assert_eq!(library.prompt("tldr"), None);
        assert_eq!(library.prompt("mail"), None);
        assert!(library.save_prompt("bad key", "x").is_err());
        assert_eq!(library.remove_prompt("tldr"), Ok(true));
    }

    #[test]
    fn links_expand_with_encoding_and_refuse_non_web_targets() {
        assert_eq!(expand("https://x.test/?q={query}", " a b&c/é "), "https://x.test/?q=a%20b%26c%2F%C3%A9");
        assert_eq!(expand("https://x.test/home", "ignored"), "https://x.test/home");
        for bad in ["javascript:alert(1)", "file:///etc/passwd", "https://", "https://a b", "ftp://x", "blindspot://x"] {
            assert!(!valid_url(bad), "{bad}");
        }
        assert!(valid_url("http://127.0.0.1:3000/{query}"));
        for bad in ["", "GH", "with space", "a/b", &"x".repeat(33)] {
            assert!(!valid_keyword(bad), "{bad}");
        }
        assert_eq!(split_link("gh blindspot app"), Some(("gh", "blindspot app")));
        assert_eq!(split_link("Safari"), None);
    }

    #[test]
    fn library_persists_privately_and_drops_invalid_entries_on_load() {
        let directory = std::env::temp_dir().join(format!("blindspot-shortcuts-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("shortcuts.toml");
        let library = Library::open(Some(path.clone()));
        library.save_link("gh", "https://github.com/search?q={query}").unwrap();
        library.save_snippet("sig", "Best regards,\nSeif").unwrap();
        assert!(library.save_link("bad", "file:///etc").is_err());
        assert!(library.save_snippet("empty", "  ").is_err());
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let reopened = Library::open(Some(path.clone()));
        assert_eq!(reopened.link("gh").as_deref(), Some("https://github.com/search?q={query}"));
        assert_eq!(reopened.snippets(), vec![("sig".to_owned(), "Best regards,\nSeif".to_owned())]);
        assert_eq!(reopened.remove_link("gh"), Ok(true));
        assert_eq!(Library::open(Some(path.clone())).links(), Vec::new());
        std::fs::write(&path, "[links]\nevil = \"javascript:alert(1)\"\nok = \"https://example.com\"\n").unwrap();
        assert_eq!(Library::open(Some(path)).links(), vec![("ok".to_owned(), "https://example.com".to_owned())]);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
