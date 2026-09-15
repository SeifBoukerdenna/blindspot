//! File search, by shelling out to `mdfind`.
//!
//! Spotlight's index is already there, already maintained, and already better than
//! anything this project would build. What it is not is fast: measured on this machine,
//! `mdfind -name report` takes 121ms and `mdfind -name a` takes 5.1 seconds for 177,333
//! results, against a 0.4ms app-query path. Everything here exists to keep that cost off
//! the keystroke path — a worker thread, a generation counter, and a child process that
//! gets killed the moment its answer stops mattering.

use crate::process_job::{Failure, Latest, capture_prefix};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

use crate::index::AppEntry;

/// A query beginning with this searches the filesystem instead of only the app index.
///
/// Explicit rather than automatic, so no subprocess ever runs unless it was asked for.
pub const PREFIX: char = '?';

/// Characters required after the prefix before anything is spawned.
///
/// The safety valve on the measured cliff: `?a` is the 5.1-second, 177,333-result case.
/// Deliberately low, since the prefix already means the user asked for this.
const MIN_QUERY: usize = 2;

/// Candidates handed to the keystroke-path ranker, chosen as the best of everything read.
///
/// Chosen, not merely the first to arrive: `mdfind` returns paths in no useful order, so
/// taking its first N made the real answer luck. With a first-50 cap `~/blindspot`
/// (position 81 of 1,645) never reached the ranker; with first-500, `Blindspot.app` still
/// lost its place to build artefacts. Selecting in the worker costs the keystroke nothing.
const RESULT_CAP: usize = 500;

/// Most lines read from one run. A safety valve for broad queries, not a relevance cut —
/// the selection above is what decides relevance.
const READ_CAP: usize = 20_000;

/// The query text if this asked to be a file search, prefix removed.
///
/// Returns `Some("")` for a bare prefix, so the caller can tell "wants files, not ready"
/// apart from "not a file query at all".
///
/// Trimmed at both ends because `mdfind -name` matches the text literally, trailing space
/// included — and the field turns a pasted line's final newline into exactly that.
pub fn strip_prefix(query: &str) -> Option<&str> {
    query.strip_prefix(PREFIX).map(str::trim)
}

/// `$HOME`, if set.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// The entries directly inside `home`, found by listing it — not by asking Spotlight.
///
/// This exists because Spotlight never returns `~/Documents` or `~/Downloads`, even
/// though `mdls` shows both indexed. Results from folders macOS privacy-protects are
/// filtered out for a caller without access, and listing `~` itself is not protected. So
/// this finds your top-level folders instantly, on the keystroke, while `mdfind` catches
/// up with everything deeper.
///
/// One level only, and never a `stat`: descending into `~/Documents` would read a
/// protected folder and raise a permission prompt, which nothing here is worth.
pub fn home_entries(home: &Path) -> Vec<AppEntry> {
    let Ok(entries) = std::fs::read_dir(home) else {
        return Vec::new();
    };
    entries
        .flatten()
        .take(4096)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            // Dotfiles are configuration, not something you open by name.
            (!name.starts_with('.')).then(|| AppEntry::new(name, entry.path()))
        })
        .collect()
}

#[derive(Default)]
pub struct FileSearch {
    job: Latest<String, Vec<AppEntry>>,
    home: Latest<(PathBuf, u64), Vec<AppEntry>>,
    home_cache: std::sync::Mutex<(Option<PathBuf>, Vec<AppEntry>)>,
}

impl FileSearch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn search(&self, query: &str) {
        let Ok(parsed) = crate::query::FileQuery::parse(query) else {
            self.job.cancel();
            return;
        };
        if !parsed.filtered() && parsed.name.chars().count() < MIN_QUERY {
            self.job.cancel();
            return;
        }
        self.job
            .search(query.to_owned(), |query, cancel| run(query, cancel));
    }

    pub fn cancel(&self) {
        self.job.cancel();
    }

    pub fn home_entries(&self, path: &Path) -> (Vec<AppEntry>, bool) {
        let key = (path.to_path_buf(), crate::relevance::unix_now() / 5);
        self.home.search(key.clone(), |(path, _), cancel| {
            if cancel.load(std::sync::atomic::Ordering::Acquire) {
                return Err(Failure::Cancelled);
            }
            Ok(home_entries(path))
        });
        let (result, pending) = self.home.results(&key);
        let mut cache = self
            .home_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cache.0.as_deref() != Some(path) {
            *cache = (Some(path.to_path_buf()), Vec::new());
        }
        if let Some(Ok(entries)) = result {
            cache.1 = entries;
        }
        (cache.1.clone(), pending)
    }

    pub fn results(&self, query: &str) -> (Vec<AppEntry>, bool) {
        let (result, pending) = self.job.results(&query.to_owned());
        (result.and_then(Result::ok).unwrap_or_default(), pending)
    }
}

fn run(query: &str, cancel: &AtomicBool) -> Result<Vec<AppEntry>, Failure> {
    let parsed = crate::query::FileQuery::parse(query).map_err(|_| Failure::Unavailable)?;
    let mut command = Command::new("/usr/bin/mdfind");
    command.args(["-attr", LAST_USED]);
    if parsed.filtered() {
        command.arg(parsed.spotlight_predicate());
    } else {
        command.arg("-name").arg(&parsed.name);
    }
    // `query` is an argument, never a shell string, so there is nothing to escape and
    // no injection to worry about — `Command` does not go through a shell.
    // `-attr` adds each result's last-opened date to the same line, so recency costs no
    // second process: of 233 matches for `desktop`, only 3 had ever been opened.
    let output = capture_prefix(&mut command, cancel, 8 << 20)?;

    // Read without the lock held: this is the slow part, and a poll must never block on it.
    // Every line is scored cheaply — place and match quality, no fuzzy matching — and only
    // the best `RESULT_CAP` survive, so the keystroke ranks good candidates rather than
    // whichever ones `mdfind` happened to print first.
    let home = home_dir();
    let now = crate::relevance::unix_now();
    let mut scored: Vec<(crate::relevance::Key, AppEntry)> = Vec::new();
    let text = String::from_utf8_lossy(&output);
    for line in text.lines().take(READ_CAP) {
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(Failure::Cancelled);
        }
        let Some(entry) = entry_for(line) else {
            continue;
        };
        let candidate = crate::relevance::Candidate {
            name: &entry.name,
            path: &entry.path,
            is_app: false,
            fuzzy: 0,
            used: crate::relevance::recency(entry.last_used, now),
        };
        let key = crate::relevance::key(&candidate, &parsed.name, home.as_deref());
        scored.push((key, entry));
        // Bounded memory on a broad query: trim back whenever the pile grows well past
        // what will be kept.
        if scored.len() >= RESULT_CAP * 4 {
            keep_best(&mut scored);
        }
    }
    keep_best(&mut scored);

    Ok(scored.into_iter().map(|(_, e)| e).collect())
}

fn keep_best(scored: &mut Vec<(crate::relevance::Key, AppEntry)>) {
    scored.sort_unstable_by_key(|(key, _)| std::cmp::Reverse(*key));
    scored.truncate(RESULT_CAP);
}

const LAST_USED: &str = "kMDItemLastUsedDate";

/// One `mdfind -attr kMDItemLastUsedDate` line: `path   kMDItemLastUsedDate = <date|(null)>`.
/// Split from the right, so a path containing three spaces still parses; a line without
/// the attribute is taken whole as a path.
pub(crate) fn entry_for(line: &str) -> Option<AppEntry> {
    let marker = format!("   {LAST_USED} = ");
    let (path, last_used) = match line.rsplit_once(&marker) {
        Some((path, value)) => (path, crate::civil::parse_spotlight(value)),
        None => (line, None),
    };
    let path = PathBuf::from(path);
    // Lossy rather than skipping: a file whose name is not valid UTF-8 should still be
    // findable, even if one character renders as a replacement.
    let name = path.file_name()?.to_string_lossy().into_owned();
    (!name.is_empty()).then(|| AppEntry {
        last_used,
        ..AppEntry::new(name, path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_prefixed_query_asks_for_files() {
        assert_eq!(strip_prefix("?report"), Some("report"));
        assert_eq!(strip_prefix("?  report"), Some("report"));
        assert_eq!(
            strip_prefix("?report "),
            Some("report"),
            "a pasted newline, as a space"
        );
        assert_eq!(strip_prefix("?"), Some(""));
        assert_eq!(strip_prefix("report"), None);
        assert_eq!(strip_prefix(""), None);
        // Only a *leading* prefix counts, so a question mark inside a name is literal.
        assert_eq!(strip_prefix("what?"), None);
    }

    #[test]
    fn a_query_under_the_floor_never_spawns_anything() {
        let search = FileSearch::new();
        for query in ["", "a"] {
            search.search(query);
            let (results, pending) = search.results(query);
            assert!(results.is_empty());
            assert!(!pending, "{query:?} must not leave the caller polling");
        }
    }

    #[test]
    fn an_unknown_query_reports_nothing_pending() {
        let search = FileSearch::new();
        let (results, pending) = search.results("never asked for");
        assert!(results.is_empty());
        assert!(!pending);
    }

    #[test]
    fn a_real_search_eventually_settles() {
        let search = FileSearch::new();
        // Deliberately a name nothing will match, so this stays fast and deterministic.
        let query = "blindspot-no-such-file-xyzzy";
        search.search(query);

        let mut settled = false;
        for _ in 0..200 {
            let (_, pending) = search.results(query);
            if !pending {
                settled = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(settled, "search should finish well inside five seconds");
        assert!(search.results(query).0.is_empty());
    }

    #[test]
    fn superseding_a_search_replaces_its_query() {
        let search = FileSearch::new();
        search.search("blindspot-first-xyzzy");
        search.search("blindspot-second-xyzzy");
        // The old query is no longer the one being tracked, so it reports nothing.
        assert!(!search.results("blindspot-first-xyzzy").1);
        assert!(search.results("blindspot-first-xyzzy").0.is_empty());
    }

    #[test]
    fn repeating_the_same_query_does_not_restart_it() {
        let search = FileSearch::new();
        search.search("blindspot-stable-xyzzy");
        let first = search.results("blindspot-stable-xyzzy");
        search.search("blindspot-stable-xyzzy");
        let second = search.results("blindspot-stable-xyzzy");
        assert!(first.1 || !second.1, "completed work never restarts");
    }

    #[test]
    fn a_last_used_date_rides_along_on_the_same_line() {
        let dated = entry_for("/Users/x/Desktop   kMDItemLastUsedDate = 2026-09-10 22:55:11 +0000")
            .expect("parses");
        assert_eq!(dated.path, PathBuf::from("/Users/x/Desktop"));
        assert_eq!(dated.name, "Desktop");
        assert!(dated.last_used.is_some());
        let never = entry_for("/Users/x/a b.log   kMDItemLastUsedDate = (null)").expect("parses");
        assert_eq!(never.name, "a b.log");
        assert_eq!(never.last_used, None);
    }

    #[test]
    fn home_entries_list_one_level_and_skip_dotfiles() {
        let dir = std::env::temp_dir().join(format!("blindspot-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Documents/deep")).expect("mkdir");
        std::fs::write(dir.join("notes.md"), b"x").expect("write");
        std::fs::write(dir.join(".zshrc"), b"x").expect("write");

        let mut names: Vec<String> = home_entries(&dir).into_iter().map(|e| e.name).collect();
        names.sort();
        assert_eq!(
            names,
            ["Documents", "notes.md"],
            "one level, no dotfiles, no descent"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn entries_take_their_name_from_the_filename() {
        let entry = entry_for("/Users/x/Documents/report q3.pdf").expect("named");
        assert_eq!(entry.name, "report q3.pdf");
        assert_eq!(
            entry.path,
            PathBuf::from("/Users/x/Documents/report q3.pdf")
        );
        assert!(entry_for("/").is_none());
    }
}
