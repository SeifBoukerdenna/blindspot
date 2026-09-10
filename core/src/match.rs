//! Fuzzy matching and ranking, wrapping `nucleo-matcher`.
//!
//! Deliberately not the `nucleo` crate. Its worker threadpool and `Injector` exist to
//! keep a UI responsive while millions of paths stream in from a filesystem walk; our
//! corpus is a couple of hundred entries already in memory, so that machinery would
//! only add a thread and snapshot polling to the query path we are trying to keep
//! inside one frame.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};

use crate::index::AppEntry;

const CASE: CaseMatching = CaseMatching::Ignore;
const NORMALIZE: Normalization = Normalization::Smart;

/// A match against the entry at `index` in the snapshot that was ranked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ranked {
    pub index: usize,
    pub score: u32,
}

/// Reused across queries. `Matcher` owns a scratch matrix and `Pattern` owns its atom
/// list, so constructing either per keystroke would allocate on the hot path.
pub struct Ranker {
    matcher: Matcher,
    pattern: Pattern,
    /// Scratch for `Utf32Str`. Only touched for non-ASCII names; ASCII takes a
    /// borrow-the-bytes fast path inside nucleo and never allocates.
    buf: Vec<char>,
    candidates: Vec<Ranked>,
}

impl Default for Ranker {
    fn default() -> Self {
        Self::new()
    }
}

impl Ranker {
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(MatcherConfig::DEFAULT),
            pattern: Pattern::parse("", CASE, NORMALIZE),
            buf: Vec::new(),
            candidates: Vec::new(),
        }
    }

    /// Ranks `entries` against `query`, best first, capped at `limit`.
    ///
    /// An empty query is not a match of everything at score zero — it is the state
    /// before the user has typed, so it yields the first `limit` entries in index
    /// order, which is alphabetical.
    pub fn rank(&mut self, query: &str, entries: &[AppEntry], limit: usize) -> Vec<Ranked> {
        if limit == 0 {
            return Vec::new();
        }
        if query.trim().is_empty() {
            return (0..entries.len().min(limit))
                .map(|index| Ranked { index, score: 0 })
                .collect();
        }

        self.pattern.reparse(query, CASE, NORMALIZE);

        // Destructured so the buffer and the matcher can be borrowed at once.
        let Self {
            matcher,
            pattern,
            buf,
            candidates,
        } = self;

        candidates.clear();
        for (index, entry) in entries.iter().enumerate() {
            let haystack = Utf32Str::new(&entry.name, buf);
            if let Some(score) = pattern.score(haystack, matcher) {
                candidates.push(Ranked { index, score });
            }
        }

        // A full sort of a few hundred candidates is well inside budget, and it keeps
        // the tie-break honest; a partial select would leave equal scores unordered.
        // Ties fall back to index order, which `Index::replace` made alphabetical.
        candidates.sort_unstable_by(|a, b| b.score.cmp(&a.score).then(a.index.cmp(&b.index)));
        candidates.truncate(limit);
        candidates.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entries(names: &[&str]) -> Vec<AppEntry> {
        names
            .iter()
            .map(|n| AppEntry::new((*n).to_owned(), PathBuf::from(format!("/Applications/{n}.app"))))
            .collect()
    }

    fn ranked_names<'a>(entries: &'a [AppEntry], ranked: &[Ranked]) -> Vec<&'a str> {
        ranked
            .iter()
            .filter_map(|r| entries.get(r.index))
            .map(|e| e.name.as_str())
            .collect()
    }

    #[test]
    fn matches_are_case_insensitive() {
        let apps = entries(&["Slack"]);
        let got = Ranker::new().rank("slack", &apps, 8);
        assert_eq!(ranked_names(&apps, &got), ["Slack"]);
    }

    #[test]
    fn non_matching_entries_are_excluded() {
        let apps = entries(&["Slack", "Terminal"]);
        let got = Ranker::new().rank("zzz", &apps, 8);
        assert!(got.is_empty());
    }

    #[test]
    fn subsequence_matching_works() {
        let apps = entries(&["Activity Monitor", "Terminal"]);
        let got = Ranker::new().rank("actmon", &apps, 8);
        assert_eq!(ranked_names(&apps, &got), ["Activity Monitor"]);
    }

    #[test]
    fn a_prefix_match_outranks_a_late_one() {
        let apps = entries(&["Books", "Slack"]);
        let got = Ranker::new().rank("s", &apps, 8);
        assert_eq!(
            ranked_names(&apps, &got).first(),
            Some(&"Slack"),
            "leading-character match should win over an interior one"
        );
    }

    #[test]
    fn results_are_capped_at_the_limit() {
        let apps = entries(&["Safari", "Slack", "Stocks", "System Settings"]);
        assert_eq!(Ranker::new().rank("s", &apps, 2).len(), 2);
        assert!(Ranker::new().rank("s", &apps, 0).is_empty());
    }

    #[test]
    fn empty_query_returns_the_head_of_the_index() {
        let apps = entries(&["Calendar", "Mail", "Notes"]);
        let got = Ranker::new().rank("", &apps, 2);
        assert_eq!(ranked_names(&apps, &got), ["Calendar", "Mail"]);
        assert_eq!(Ranker::new().rank("   ", &apps, 2).len(), 2);
    }

    #[test]
    fn empty_index_never_panics() {
        assert!(Ranker::new().rank("anything", &[], 8).is_empty());
        assert!(Ranker::new().rank("", &[], 8).is_empty());
    }

    #[test]
    fn a_reused_ranker_gives_the_same_answer_as_a_fresh_one() {
        let apps = entries(&["Safari", "Slack", "Terminal"]);
        let mut reused = Ranker::new();
        let _ = reused.rank("term", &apps, 8);
        let _ = reused.rank("saf", &apps, 8);
        assert_eq!(reused.rank("s", &apps, 8), Ranker::new().rank("s", &apps, 8));
    }

    #[test]
    fn non_ascii_names_match() {
        let apps = entries(&["Café", "Terminal"]);
        let got = Ranker::new().rank("café", &apps, 8);
        assert_eq!(ranked_names(&apps, &got), ["Café"]);
    }
}
