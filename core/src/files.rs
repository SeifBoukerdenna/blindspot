//! File search, by shelling out to `mdfind`.
//!
//! Spotlight's index is already there, already maintained, and already better than
//! anything this project would build. What it is not is fast: measured on this machine,
//! `mdfind -name report` takes 121ms and `mdfind -name a` takes 5.1 seconds for 177,333
//! results, against a 0.4ms app-query path. Everything here exists to keep that cost off
//! the keystroke path — a worker thread, a generation counter, and a child process that
//! gets killed the moment its answer stops mattering.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

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

/// Most paths taken from one run. `mdfind` will happily produce six figures of them.
const RESULT_CAP: usize = 50;

/// The query text if this asked to be a file search, prefix removed.
///
/// Returns `Some("")` for a bare prefix, so the caller can tell "wants files, not ready"
/// apart from "not a file query at all".
pub fn strip_prefix(query: &str) -> Option<&str> {
    query.strip_prefix(PREFIX).map(str::trim_start)
}

#[derive(Default)]
struct Shared {
    /// The query `results` belong to.
    query: String,
    results: Vec<AppEntry>,
    pending: bool,
    /// Bumped on every new query. A worker whose generation has moved on throws its
    /// output away instead of publishing a stale answer.
    generation: u64,
    /// Held so a superseding query can kill it. Only the worker owning the current
    /// generation may take it.
    child: Option<Child>,
}

#[derive(Default)]
pub struct FileSearch {
    shared: Arc<Mutex<Shared>>,
}

impl FileSearch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a search for `query` unless one is already running or finished for it.
    ///
    /// Returns immediately. Idempotent, which is what lets the caller invoke it on every
    /// keystroke and on every poll without thinking about it.
    pub fn search(&self, query: &str) {
        let mut shared = lock(&self.shared);
        if shared.query == query {
            return;
        }

        // Supersede: the old answer is now worthless, and so is the process producing it.
        shared.generation += 1;
        let generation = shared.generation;
        query.clone_into(&mut shared.query);
        shared.results.clear();
        if let Some(mut child) = shared.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }

        if query.chars().count() < MIN_QUERY {
            shared.pending = false;
            return;
        }
        shared.pending = true;
        drop(shared);

        let handle = Arc::clone(&self.shared);
        let query = query.to_owned();
        if std::thread::Builder::new()
            .name("blindspot-mdfind".to_owned())
            .spawn(move || run(&handle, generation, &query))
            .is_err()
        {
            // Nothing will publish a result, so do not leave the caller polling forever.
            lock(&self.shared).pending = false;
        }
    }

    /// What is known for `query`, and whether a search for it is still running.
    pub fn results(&self, query: &str) -> (Vec<AppEntry>, bool) {
        let shared = lock(&self.shared);
        if shared.query != query {
            return (Vec::new(), false);
        }
        (shared.results.clone(), shared.pending)
    }
}

/// Poison recovery rather than propagation, as everywhere else in this crate: a panic
/// while holding this lock must not cost the user their launcher.
fn lock(shared: &Arc<Mutex<Shared>>) -> std::sync::MutexGuard<'_, Shared> {
    match shared.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn run(shared: &Arc<Mutex<Shared>>, generation: u64, query: &str) {
    // `query` is an argument, never a shell string, so there is nothing to escape and
    // no injection to worry about — `Command` does not go through a shell.
    let spawned = Command::new("mdfind")
        .arg("-name")
        .arg(query)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();

    let Ok(mut child) = spawned else {
        finish(shared, generation, Vec::new());
        return;
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        finish(shared, generation, Vec::new());
        return;
    };

    {
        let mut guard = lock(shared);
        if guard.generation != generation {
            // Superseded between spawning and publishing the handle; clean up our own.
            drop(guard);
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        guard.child = Some(child);
    }

    // Read without the lock held: this is the slow part, and a poll must never block on it.
    let mut entries = Vec::with_capacity(RESULT_CAP);
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if entries.len() >= RESULT_CAP {
            break;
        }
        if let Some(entry) = entry_for(PathBuf::from(line)) {
            entries.push(entry);
        }
    }

    finish(shared, generation, entries);
}

fn finish(shared: &Arc<Mutex<Shared>>, generation: u64, entries: Vec<AppEntry>) {
    let mut guard = lock(shared);
    // A moved generation means `child` now belongs to a newer search. Touching it would
    // kill someone else's process.
    if guard.generation != generation {
        return;
    }
    if let Some(mut child) = guard.child.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    guard.results = entries;
    guard.pending = false;
}

fn entry_for(path: PathBuf) -> Option<AppEntry> {
    // Lossy rather than skipping: a file whose name is not valid UTF-8 should still be
    // findable, even if one character renders as a replacement.
    let name = path.file_name()?.to_string_lossy().into_owned();
    (!name.is_empty()).then(|| AppEntry::new(name, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_prefixed_query_asks_for_files() {
        assert_eq!(strip_prefix("?report"), Some("report"));
        assert_eq!(strip_prefix("?  report"), Some("report"));
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
        let first = lock(&search.shared).generation;
        search.search("blindspot-stable-xyzzy");
        assert_eq!(lock(&search.shared).generation, first, "idempotent");
    }

    #[test]
    fn entries_take_their_name_from_the_filename() {
        let entry = entry_for(PathBuf::from("/Users/x/Documents/report q3.pdf")).expect("named");
        assert_eq!(entry.name, "report q3.pdf");
        assert_eq!(
            entry.path,
            PathBuf::from("/Users/x/Documents/report q3.pdf")
        );
        assert!(entry_for(PathBuf::from("/")).is_none());
    }
}
