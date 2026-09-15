//! Synthetic content-index benchmark. Never reads or indexes personal files.
use blindspot_core::content::{ContentStore, Document, MAX_BATCH, ScanOutcome};
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let count: usize = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "10000".into())
        .parse()?;
    if !(1..=10_000_000).contains(&count) {
        return Err("Expected 1..10000000 records".into());
    }
    let directory =
        std::env::temp_dir().join(format!("blindspot-content-scale-{}", std::process::id()));
    std::fs::create_dir(&directory)?;
    let path = directory.join("content.sqlite");
    let result = run(&path, count);
    std::fs::remove_dir_all(directory)?;
    result
}

fn run(path: &Path, count: usize) -> Result<(), Box<dyn std::error::Error>> {
    let mut store = ContentStore::open(path)?;
    let scan = store.begin_scan(Path::new("/synthetic"))?;
    let started = Instant::now();
    for offset in (0..count).step_by(MAX_BATCH) {
        let records: Vec<_> = (offset..(offset + MAX_BATCH).min(count)).map(|i| (
            format!("volume:{i}:1"), format!("/synthetic/document{i}.md"), format!("Document {i}"),
            format!("Project topic{} database migrations and transactional rollback. Invoice vendor{} receipt. Unique marker{i}", i % 1000, i % 10000)
        )).collect();
        let documents: Vec<_> = records
            .iter()
            .map(|(identity, path, title, body)| Document {
                identity,
                path: Path::new(path),
                title,
                body,
                modified_ns: 1,
                changed_ns: 1,
                bytes: body.len() as u64,
                extraction: blindspot_core::content::Extraction::Text,
            })
            .collect();
        store.put_batch(&scan, &documents)?;
    }
    store.finish_scan(&scan, ScanOutcome::Complete, &AtomicBool::new(false))?;
    let seconds = started.elapsed().as_secs_f64();
    println!(
        "records={count} index_seconds={seconds:.3} records_per_second={:.0}",
        count as f64 / seconds
    );
    assert_eq!(store.count()?, count as u64);
    let reader = ContentStore::open_reader(path)?;
    for query in [
        "marker42",
        "topic42",
        "vendor42",
        "database migrations",
        "absentterm",
    ] {
        let mut times = Vec::new();
        let mut cancellations = 0;
        let mut found = 0;
        let mut limited = false;
        for _ in 0..10 {
            let started = Instant::now();
            match reader.search(query, 50, Arc::new(AtomicBool::new(false))) {
                Ok(page) => { found = page.hits.len(); limited = page.limited; }
                Err(blindspot_core::content::Error::Cancelled) => cancellations += 1,
                Err(error) => return Err(error.into()),
            }
            times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        println!(
            "query={query:?} hits={found} limited={limited} median_ms={:.3} max_ms={:.3} cancelled={cancellations}/10",
            times[5], times[9]
        );
    }
    store.check_integrity()?;
    drop(reader);
    drop(store);
    println!("database_bytes={}", std::fs::metadata(path)?.len());
    Ok(())
}
