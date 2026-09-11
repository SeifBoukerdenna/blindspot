//! macOS's own record of which apps you use, from Spotlight's `kMDItemUsedDates`.
//!
//! blindspot's frecency only knows launches made through blindspot. macOS records every
//! launch — Dock, Finder, `open` — as one date per day of use, and Spotlight exposes it. On
//! this machine 63 apps carry that history. Folding it in means a fresh install already
//! ranks the apps you actually use, instead of starting from alphabetical.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use std::sync::{Arc, RwLock};

use crate::index::id_for_path;

/// The current scores behind a cheap snapshot, like the app index: a refresh swaps the map
/// in whole, and a keystroke clones one `Arc` rather than waiting on it.
#[derive(Default)]
pub struct UsageScores {
    inner: RwLock<Arc<HashMap<u64, f64>>>,
}

impl UsageScores {
    pub fn snapshot(&self) -> Arc<HashMap<u64, f64>> {
        match self.inner.read() {
            Ok(guard) => Arc::clone(&guard),
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }

    pub fn replace(&self, scores: HashMap<u64, f64>) {
        let next = Arc::new(scores);
        match self.inner.write() {
            Ok(mut guard) => *guard = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
    }
}

const QUERY: &str =
    r#"kMDItemContentType == "com.apple.application-bundle" && kMDItemUseCount > 0"#;
const ATTRIBUTE: &str = "kMDItemUsedDates";

/// Spotlight's usage score at or above which an app counts as recently used: one use four
/// weeks ago at the default 14-day half-life (`0.5^(28/14)`).
pub const RECENT_SCORE: f64 = 0.25;

/// Runs the query and scores the result. Blocks for the query — about 84ms on this machine
/// — so it is only ever called off the main thread.
pub fn fetch_scores(now: u64, half_life_days: f64) -> HashMap<u64, f64> {
    let output = Command::new("mdfind")
        .args(["-attr", ATTRIBUTE, QUERY])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    match output {
        Ok(out) => scores(
            &parse(&String::from_utf8_lossy(&out.stdout)),
            now,
            half_life_days,
        ),
        // No Spotlight, no usage history: ranking falls back to blindspot's own, which is
        // exactly what it was before this module existed.
        Err(_) => HashMap::new(),
    }
}

/// Each app's usage history scored with frecency's decay, keyed by the same path-hash id
/// the app index uses.
pub fn scores(
    usage: &HashMap<PathBuf, Vec<u64>>,
    now: u64,
    half_life_days: f64,
) -> HashMap<u64, f64> {
    usage
        .iter()
        .map(|(path, dates)| {
            (
                id_for_path(path),
                crate::frecency::decayed_count(dates, now, half_life_days),
            )
        })
        .filter(|(_, score)| *score > 0.0)
        .collect()
}

/// Parses `mdfind -attr kMDItemUsedDates` output.
///
/// Arrays do not fit `mdfind`'s one-line-per-result format. Captured from this machine:
///
/// ```text
/// /Applications/Safari.app   kMDItemUsedDates = (
///     "2026-07-06 04:00:00 +0000",
///     "2026-09-07 04:00:00 +0000"
/// )
/// ```
///
/// So a record begins at a line starting with `/` — every path is absolute — and its dates
/// follow on indented lines until a bare `)`. An unparseable date is skipped, not fatal.
pub fn parse(output: &str) -> HashMap<PathBuf, Vec<u64>> {
    let marker = format!("   {ATTRIBUTE} = ");
    let mut usage = HashMap::new();
    let mut current: Option<(PathBuf, Vec<u64>)> = None;

    for line in output.lines() {
        if line.starts_with('/') {
            if let Some((path, dates)) = current.take() {
                usage.insert(path, dates);
            }
            let Some((path, rest)) = line.rsplit_once(&marker) else {
                continue;
            };
            // `(null)` means no history; only `(` opens a list worth collecting.
            if rest.trim() == "(" {
                current = Some((PathBuf::from(path), Vec::new()));
            }
        } else if line.trim() == ")" {
            if let Some((path, dates)) = current.take() {
                usage.insert(path, dates);
            }
        } else if let Some((_, dates)) = current.as_mut() {
            let text = line.trim().trim_end_matches(',').trim_matches('"');
            if let Some(epoch) = crate::civil::parse_spotlight(text) {
                dates.push(epoch);
            }
        }
    }
    if let Some((path, dates)) = current {
        usage.insert(path, dates);
    }
    usage.retain(|_, dates| !dates.is_empty());
    usage
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two records captured verbatim from this machine, Chrome's shortened, plus a `(null)`.
    const CAPTURED: &str = "/Applications/Safari.app   kMDItemUsedDates = (
    \"2026-07-06 04:00:00 +0000\",
    \"2026-07-23 04:00:00 +0000\",
    \"2026-09-03 04:00:00 +0000\",
    \"2026-09-07 04:00:00 +0000\"
)
/Applications/Google Chrome.app   kMDItemUsedDates = (
    \"2026-04-21 04:00:00 +0000\",
    \"2026-04-22 04:00:00 +0000\"
)
/System/Applications/Chess.app   kMDItemUsedDates = (null)
";

    #[test]
    fn multi_line_records_parse_including_paths_with_spaces() {
        let usage = parse(CAPTURED);
        assert_eq!(usage.len(), 2, "the (null) record carries no history");
        assert_eq!(usage[&PathBuf::from("/Applications/Safari.app")].len(), 4);
        assert_eq!(
            usage[&PathBuf::from("/Applications/Google Chrome.app")].len(),
            2
        );
        assert_eq!(
            usage[&PathBuf::from("/Applications/Safari.app")][0],
            crate::civil::parse_spotlight("2026-07-06 04:00:00 +0000").expect("valid")
        );
    }

    #[test]
    fn a_truncated_record_still_yields_what_it_had() {
        let usage = parse("/A.app   kMDItemUsedDates = (\n    \"2026-07-06 04:00:00 +0000\",\n");
        assert_eq!(usage[&PathBuf::from("/A.app")].len(), 1);
    }

    #[test]
    fn recent_use_outscores_old_use() {
        let now = crate::civil::parse_spotlight("2026-09-10 04:00:00 +0000").expect("valid");
        let usage = parse(CAPTURED);
        let scores = scores(&usage, now, 14.0);
        let safari = scores[&id_for_path(std::path::Path::new("/Applications/Safari.app"))];
        let chrome = scores[&id_for_path(std::path::Path::new("/Applications/Google Chrome.app"))];
        assert!(
            safari > chrome,
            "used three days ago beats used in April: {safari} vs {chrome}"
        );
        assert!(safari >= RECENT_SCORE && chrome < RECENT_SCORE);
    }

    #[test]
    fn garbage_parses_to_nothing_rather_than_panicking() {
        assert!(parse("").is_empty());
        assert!(parse("not mdfind output\n)\n(\n").is_empty());
    }
}
