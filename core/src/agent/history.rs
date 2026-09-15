//! What the agent has been asked, and what came of it.
//!
//! A record kept for you to read, in the panel and on disk. One JSON object per line in
//! `~/.local/share/blindspot/agent-history.jsonl`: appendable without rewriting, greppable by
//! hand, and trivially recoverable when a line is ever corrupted — a malformed line is skipped
//! rather than taking the file with it.
//!
//! Separate from `agent.log`, which is a running commentary for debugging. This is the record
//! the panel shows.

use std::io::Write;
use std::path::PathBuf;

/// Entries kept. Older ones are dropped when the file is next written, so the file cannot grow
/// without bound and reading it stays instant.
const KEEP: usize = 200;

/// One request and what became of it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    /// Unix seconds.
    pub at: u64,
    pub request: String,
    pub model: String,
    /// Where the commands ran, `~` shortened. Empty for an answer.
    pub dir: String,
    /// The prose answer, when the model answered rather than proposed work.
    #[serde(default)]
    pub answer: String,
    #[serde(default)]
    pub commands: Vec<String>,
    /// What happened, in one word: `answered`, `ran`, `failed`, `blocked`, `stopped`.
    pub outcome: String,
}

impl Entry {
    /// The one-line summary the panel shows under the request.
    pub fn summary(&self) -> String {
        let what = match self.commands.len() {
            0 => self.outcome.clone(),
            1 => format!("{} · 1 command", self.outcome),
            many => format!("{} · {many} commands", self.outcome),
        };
        if self.dir.is_empty() {
            format!("{what} · {}", self.model)
        } else {
            format!("{what} · {} · {}", self.dir, self.model)
        }
    }
}

fn path() -> Option<PathBuf> {
    crate::store::data_dir()
        .ok()
        .map(|dir| dir.join("agent-history.jsonl"))
}

/// Appends one entry, best effort: a launcher must not fail a task because its diary is full.
pub fn record(entry: &Entry) {
    if cfg!(test) {
        return;
    }
    let Some(path) = path() else { return };
    append(&path, entry);
}

fn append(path: &std::path::Path, entry: &Entry) {
    let Ok(line) = serde_json::to_string(entry) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
    trim(path);
}

/// Keeps the file to [`KEEP`] entries, rewriting only when it has drifted well past — so the
/// common case is one append and nothing else.
fn trim(path: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= KEEP * 2 {
        return;
    }
    let kept: String = lines[lines.len() - KEEP..]
        .iter()
        .map(|line| format!("{line}\n"))
        .collect();
    let _ = std::fs::write(path, kept);
}

/// The most recent entries, newest first.
pub fn recent(limit: usize) -> Vec<Entry> {
    if cfg!(test) {
        return Vec::new();
    }
    let Some(path) = path() else {
        return Vec::new();
    };
    read(&path, limit)
}

fn read(path: &std::path::Path, limit: usize) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .rev()
        // A line that will not parse is one lost entry, not a lost history.
        .filter_map(|line| serde_json::from_str::<Entry>(line).ok())
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(request: &str, at: u64) -> Entry {
        Entry {
            at,
            request: request.to_owned(),
            model: "small:4b".to_owned(),
            dir: "~/dev".to_owned(),
            answer: String::new(),
            commands: vec!["mkdir -p notes".to_owned()],
            outcome: "ran".to_owned(),
        }
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("blindspot-history-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("a scratch directory");
            Self(path)
        }

        fn file(&self) -> PathBuf {
            self.0.join("agent-history.jsonl")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn entries_come_back_newest_first() {
        let scratch = Scratch::new("order");
        for (at, request) in [(1, "first"), (2, "second"), (3, "third")] {
            append(&scratch.file(), &entry(request, at));
        }
        let names: Vec<String> = read(&scratch.file(), 10)
            .into_iter()
            .map(|e| e.request)
            .collect();
        assert_eq!(names, ["third", "second", "first"]);
        assert_eq!(read(&scratch.file(), 2).len(), 2, "the limit is honoured");
    }

    #[test]
    fn a_corrupted_line_costs_one_entry_not_the_file() {
        let scratch = Scratch::new("corrupt");
        append(&scratch.file(), &entry("good", 1));
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(scratch.file())
                .expect("the file");
            writeln!(file, "{{not json").expect("write");
        }
        append(&scratch.file(), &entry("later", 3));
        let names: Vec<String> = read(&scratch.file(), 10)
            .into_iter()
            .map(|e| e.request)
            .collect();
        assert_eq!(names, ["later", "good"]);
    }

    #[test]
    fn the_file_does_not_grow_without_bound() {
        let scratch = Scratch::new("trim");
        for at in 0..(KEEP * 2 + 5) as u64 {
            append(&scratch.file(), &entry(&format!("request {at}"), at));
        }
        let lines = std::fs::read_to_string(scratch.file())
            .expect("the file")
            .lines()
            .count();
        assert!(lines <= KEEP * 2, "{lines} lines kept");
        // And the newest survive the trim.
        assert_eq!(
            read(&scratch.file(), 1)[0].request,
            format!("request {}", KEEP * 2 + 4)
        );
    }

    #[test]
    fn the_summary_says_what_happened() {
        let mut one = entry("make notes", 1);
        assert_eq!(one.summary(), "ran · 1 command · ~/dev · small:4b");
        one.commands.push("touch a".to_owned());
        assert_eq!(one.summary(), "ran · 2 commands · ~/dev · small:4b");
        let answered = Entry {
            commands: Vec::new(),
            dir: String::new(),
            outcome: "answered".to_owned(),
            ..one
        };
        assert_eq!(answered.summary(), "answered · small:4b");
    }
}
