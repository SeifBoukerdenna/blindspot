use blindspot_core::{index::AppEntry, matching::Ranker};
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let count = std::env::args()
        .nth(1)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(10_000);
    assert!((1..=10_000_000).contains(&count));
    let start = Instant::now();
    let entries: Vec<_> = (0..count)
        .map(|i| {
            let name = format!("document-{i:08}.txt");
            AppEntry::new(name.clone(), PathBuf::from(format!("/synthetic/{name}")))
        })
        .collect();
    println!(
        "records={count} fixture_ms={:.3}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    let mut ranker = Ranker::new();
    for query in ["doc", "document-99999", "zzzz"] {
        let start = Instant::now();
        let results = ranker.rank(query, &entries, 50);
        println!(
            "query={query} results={} elapsed_ms={:.3}",
            results.len(),
            start.elapsed().as_secs_f64() * 1000.0
        );
        assert!(results.len() <= 50);
    }
}
