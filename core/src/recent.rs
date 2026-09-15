//! Recently used files, for the welcome screen and the bare `?` browse mode.
//!
//! Spotlight's `kMDItemLastUsedDate` rather than a history of blindspot's own: macOS
//! records every open through Launch Services — Finder, an app's Open dialog, `open` — so
//! the list is right on the first day, before blindspot has launched anything.
//!
//! Fetched in the background and cached between panel shows, so opening the panel paints
//! instantly from the last list and never waits on `mdfind`.

use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use crate::index::AppEntry;

/// Two weeks, and never an app — apps have their own section, ranked by usage instead.
const QUERY: &str = "kMDItemLastUsedDate >= $time.today(-14) \
                     && kMDItemContentType != \"com.apple.application-bundle\"";

/// Enough for the `?` browse mode to scroll through; the welcome screen shows a handful.
const CAP: usize = 50;

/// A fetch younger than this is reused. Backspacing to an empty field, or flicking between
/// browse modes, would otherwise start an `mdfind` each time for a list that has not moved.
const MIN_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct RecentFiles {
    list: RwLock<Arc<Vec<AppEntry>>>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    refreshing: bool,
    fetched_at: Option<Instant>,
}

impl RecentFiles {
    pub fn snapshot(&self) -> Arc<Vec<AppEntry>> {
        match self.list.read() {
            Ok(guard) => Arc::clone(&guard),
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }

    /// True while a fetch is running, so the caller can report `pending` and have the
    /// shell repaint when the fresh list lands.
    pub fn is_refreshing(&self) -> bool {
        self.state().refreshing
    }

    /// Starts a background fetch, unless one is running or the list is fresh enough.
    pub fn refresh(self: &Arc<Self>) {
        {
            let mut state = self.state();
            let fresh = state
                .fetched_at
                .is_some_and(|at| at.elapsed() < MIN_INTERVAL);
            if state.refreshing || fresh {
                return;
            }
            state.refreshing = true;
        }
        let this = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("blindspot-recent".to_owned())
            .spawn(move || {
                // Clears `refreshing` however the fetch ends, panic included: left set, the
                // shell would poll for a list that is never coming.
                let _done = Done(Arc::clone(&this));
                if let Some(list) = fetch() {
                    let next = Arc::new(list);
                    match this.list.write() {
                        Ok(mut guard) => *guard = next,
                        Err(poisoned) => *poisoned.into_inner() = next,
                    }
                }
            });
        if spawned.is_err() {
            self.state().refreshing = false;
        }
    }

    #[cfg(test)]
    pub(crate) fn with(list: Vec<AppEntry>) -> Self {
        Self {
            list: RwLock::new(Arc::new(list)),
            state: Mutex::new(State {
                refreshing: false,
                fetched_at: Some(Instant::now()),
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct Done(Arc<RecentFiles>);

impl Drop for Done {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.refreshing = false;
        state.fetched_at = Some(Instant::now());
    }
}

/// `None` if `mdfind` could not run at all, so a failure keeps the last good list rather
/// than blanking the section.
fn fetch() -> Option<Vec<AppEntry>> {
    let output = crate::process_job::capture_prefix(
        Command::new("/usr/bin/mdfind").args(["-attr", "kMDItemLastUsedDate", QUERY]),
        &AtomicBool::new(false),
        8 << 20,
    )
    .ok()?;
    Some(parse(&String::from_utf8_lossy(&output)))
}

/// Most recent first; system files, anything inside a bundle or a hidden folder, and
/// entries with no date dropped.
///
/// Dropped, where `?` search only demotes them: a search has to find `Desktop Pictures`
/// if that is what you typed, but nobody opened the panel hoping to see a font cache.
pub fn parse(output: &str) -> Vec<AppEntry> {
    let mut entries: Vec<AppEntry> = output
        .lines()
        .filter_map(crate::files::entry_for)
        .filter(|e| e.last_used.is_some() && !crate::relevance::is_noise(&e.path))
        .collect();
    entries.sort_by(|a, b| {
        b.last_used
            .cmp(&a.last_used)
            .then_with(|| a.name.cmp(&b.name))
    });
    entries.truncate(CAP);
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTPUT: &str = "\
/Users/seif/Documents/resume.pdf   kMDItemLastUsedDate = 2026-09-10 14:00:00 +0000
/Users/seif/Downloads   kMDItemLastUsedDate = 2026-09-11 09:30:00 +0000
/Users/seif/Library/Caches/thing.db   kMDItemLastUsedDate = 2026-09-11 10:00:00 +0000
/System/Library/Fonts/SFNS.ttf   kMDItemLastUsedDate = 2026-09-11 10:00:00 +0000
/Users/seif/.cache/blob   kMDItemLastUsedDate = 2026-09-11 10:00:00 +0000
/Users/seif/notes.md   kMDItemLastUsedDate = (null)
/Users/seif/Desktop/capstone plan.key   kMDItemLastUsedDate = 2026-09-08 08:00:00 +0000
";

    #[test]
    fn newest_first_with_noise_and_undated_dropped() {
        let names: Vec<String> = parse(OUTPUT).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["Downloads", "resume.pdf", "capstone plan.key"]);
    }

    #[test]
    fn capped() {
        let many: String = (0..80)
            .map(|i| {
                format!("/Users/seif/f{i}   kMDItemLastUsedDate = 2026-09-11 10:00:00 +0000\n")
            })
            .collect();
        assert_eq!(parse(&many).len(), CAP);
    }

    #[test]
    fn a_fresh_list_is_not_refetched() {
        let recent = Arc::new(RecentFiles::with(Vec::new()));
        recent.refresh();
        assert!(
            !recent.is_refreshing(),
            "fetched just now, so no new mdfind"
        );
    }
}
