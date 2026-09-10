//! Query-path benchmark.
//!
//! Measures match + rank + truncate only. Indexing is deliberately outside the timed
//! closure: it happens once at launch and on a background rescan, never on the path
//! between a keystroke and a frame.
//!
//! The fixture is synthetic rather than a scan of this machine's `/Applications`, for
//! two reasons: the `/bench` skill requires 500+ entries and a real Mac has fewer, and
//! a benchmark whose corpus changes when you install an app produces numbers that
//! cannot be compared against a committed baseline.

use std::hint::black_box;
use std::path::PathBuf;

use blindspot_core::index::{AppEntry, Index};
use blindspot_core::matching::Ranker;
use criterion::{Criterion, criterion_group, criterion_main};

const PREFIXES: &[&str] = &[
    "Adobe", "Apple", "Blue", "Cloud", "Core", "Data", "Deep", "Fast", "Focus", "Green", "Hyper",
    "Iron", "Light", "Live", "Micro", "Night", "Open", "Pixel", "Quick", "Red", "Sharp", "Smart",
    "Solar", "Sound", "Swift", "Tiny", "Ultra", "Vector", "Wave", "Zen",
];

const SUFFIXES: &[&str] = &[
    "Board", "Cast", "Deck", "Desk", "Edit", "Flow", "Forge", "Frame", "Hub", "Lab", "Mail",
    "Mind", "Note", "Player", "Scope", "Sheet", "Studio", "Sync", "Term", "Vault",
];

/// A handful of real names so the ranking assertions in the report stay recognisable.
const REAL: &[&str] = &[
    "Activity Monitor",
    "App Store",
    "Calendar",
    "Console",
    "Disk Utility",
    "Finder",
    "Keychain Access",
    "Mail",
    "Messages",
    "Notes",
    "Photos",
    "Preview",
    "Safari",
    "Slack",
    "System Settings",
    "Terminal",
    "TextEdit",
    "Visual Studio Code",
    "Xcode",
    "zoom.us",
];

/// 620 entries: 600 generated plus the real ones above.
fn fixture() -> Index {
    let mut entries: Vec<AppEntry> = Vec::with_capacity(PREFIXES.len() * SUFFIXES.len() + REAL.len());
    for prefix in PREFIXES {
        for suffix in SUFFIXES {
            let name = format!("{prefix}{suffix}");
            let path = PathBuf::from(format!("/Applications/{name}.app"));
            entries.push(AppEntry::new(name, path));
        }
    }
    for name in REAL {
        let path = PathBuf::from(format!("/Applications/{name}.app"));
        entries.push(AppEntry::new((*name).to_owned(), path));
    }

    let index = Index::new();
    index.replace(entries);
    index
}

fn bench_query(c: &mut Criterion) {
    let index = fixture();
    let entries = index.snapshot();
    assert!(
        entries.len() >= 500,
        "fixture must be a realistic index size, got {}",
        entries.len()
    );

    let mut group = c.benchmark_group("query");

    // The Ranker is long-lived in production, so reusing one here is the honest shape.
    // A fresh Ranker per iteration would measure allocation we never pay per keystroke.
    let mut ranker = Ranker::new();

    // Named for what each costs, not what the user typed. The single character is the
    // worst case: almost every entry matches, so the sort runs over the whole corpus.
    for (label, query) in [
        ("empty", ""),
        ("one_char_broad", "s"),
        ("two_chars", "sl"),
        ("typical", "term"),
        ("subsequence", "actmon"),
        ("full_word", "terminal"),
        ("no_match", "qqqqzzzz"),
    ] {
        group.bench_function(label, |b| {
            b.iter(|| black_box(ranker.rank(black_box(query), black_box(&entries), 8)))
        });
    }

    group.finish();
}

criterion_group!(benches, bench_query);
criterion_main!(benches);
