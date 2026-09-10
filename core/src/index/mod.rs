//! The in-memory application index.

pub mod apps;

use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// One launchable application bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    /// Derived from `path`, so it survives a rescan. Swift hands this back to
    /// `bs_activate` after the index may already have been swapped underneath it.
    pub id: u64,
    /// Display name, already resolved through the `CFBundleDisplayName` fallback chain.
    pub name: String,
    pub path: PathBuf,
}

impl AppEntry {
    pub fn new(name: String, path: PathBuf) -> Self {
        Self {
            id: id_for_path(&path),
            name,
            path,
        }
    }
}

/// FNV-1a. Chosen over `DefaultHasher` because that one's output is explicitly not
/// stable across Rust releases, and these ids are written to the frecency store at M3.
fn id_for_path(path: &std::path::Path) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in path.as_os_str().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Holds the current entry list behind an `RwLock<Arc<..>>` so a background rescan
/// never blocks a keystroke: a query clones the `Arc` and releases the lock at once.
#[derive(Debug, Default)]
pub struct Index {
    inner: RwLock<Arc<Vec<AppEntry>>>,
}

impl Index {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cheap. Clones one `Arc` and drops the read guard immediately.
    pub fn snapshot(&self) -> Arc<Vec<AppEntry>> {
        // A poisoned lock means some other thread panicked while holding it. The data
        // is still a consistent `Arc<Vec<..>>`, and losing the launcher is worse than
        // serving a possibly stale index, so we recover instead of propagating.
        match self.inner.read() {
            Ok(guard) => Arc::clone(&guard),
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }

    /// Swaps in a freshly scanned list. Sorted by name so that equal match scores
    /// break ties alphabetically without the ranker needing to look at names.
    pub fn replace(&self, mut entries: Vec<AppEntry>) {
        entries.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
        entries.dedup_by(|a, b| a.path == b.path);
        let next = Arc::new(entries);
        match self.inner.write() {
            Ok(mut guard) => *guard = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
    }

    pub fn len(&self) -> usize {
        self.snapshot().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, path: &str) -> AppEntry {
        AppEntry::new(name.to_owned(), PathBuf::from(path))
    }

    #[test]
    fn ids_are_stable_for_a_path_and_differ_between_paths() {
        let a = entry("Slack", "/Applications/Slack.app");
        let again = entry("Slack renamed", "/Applications/Slack.app");
        let other = entry("Slack", "/Applications/Slack2.app");
        assert_eq!(a.id, again.id, "id must depend only on the path");
        assert_ne!(a.id, other.id);
    }

    #[test]
    fn replace_sorts_by_name_and_drops_duplicate_paths() {
        let index = Index::new();
        index.replace(vec![
            entry("Terminal", "/Applications/Terminal.app"),
            entry("Calendar", "/Applications/Calendar.app"),
            entry("Terminal", "/Applications/Terminal.app"),
        ]);
        let snap = index.snapshot();
        let names: Vec<&str> = snap.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Calendar", "Terminal"]);
    }

    #[test]
    fn snapshot_is_unaffected_by_a_later_replace() {
        let index = Index::new();
        index.replace(vec![entry("Mail", "/Applications/Mail.app")]);
        let before = index.snapshot();
        index.replace(vec![entry("Notes", "/Applications/Notes.app")]);
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].name, "Mail");
        assert_eq!(index.snapshot()[0].name, "Notes");
    }

    #[test]
    fn a_new_index_is_empty() {
        assert!(Index::new().is_empty());
    }
}
