//! Filesystem indexing benchmark over disposable synthetic text files.
use blindspot_core::{
    content::ContentStore,
    content_indexer::{Policy, scan_root},
};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let count: usize = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "10000".into())
        .parse()?;
    if !(1..=100_000).contains(&count) {
        return Err("Expected 1..100000 files".into());
    }
    let directory =
        std::env::temp_dir().join(format!("blindspot-scan-bench-{}", std::process::id()));
    std::fs::create_dir(&directory)?;
    let directory = directory.canonicalize()?;
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let root = directory.join("files");
        std::fs::create_dir(&root)?;
        for i in 0..count {
            std::fs::write(
                root.join(format!("document{i}.txt")),
                format!("Synthetic project {i}: database migrations and local search."),
            )?;
        }
        let mut store = ContentStore::open(&directory.join("content.sqlite"))?;
        let policy = Policy {
            batch_pause: Duration::ZERO,
            ..Policy::default()
        };
        let changed = count.div_ceil(10);
        for phase in ["initial", "unchanged", "updated", "removed"] {
            if phase == "updated" {
                for i in (0..count).step_by(10) {
                    std::fs::write(root.join(format!("document{i}.txt")), format!("Updated fixture {i}: cancellable maintenance."))?;
                }
            } else if phase == "removed" {
                for i in (0..count).step_by(10) {
                    std::fs::remove_file(root.join(format!("document{i}.txt")))?;
                }
            }
            let started = Instant::now();
            let progress = scan_root(&mut store, &root, &policy, &AtomicBool::new(false), |_| {})?;
            println!(
                "phase={phase} files={count} elapsed_ms={:.3} indexed={} unchanged={} removed={} complete={}",
                started.elapsed().as_secs_f64() * 1000.0,
                progress.indexed,
                progress.unchanged,
                progress.removed,
                progress.complete
            );
            assert!(progress.complete);
            assert_eq!(store.count()?, (if phase == "removed" { count - changed } else { count }) as u64);
            match phase {
                "initial" => assert_eq!(progress.indexed, count as u64),
                "unchanged" => assert_eq!(progress.unchanged, count as u64),
                "updated" => assert_eq!(progress.indexed, changed as u64),
                "removed" => assert_eq!(progress.removed, changed as u64),
                _ => unreachable!(),
            }
        }
        let started = Instant::now();
        let mut erased = 0;
        store.erase(Arc::new(AtomicBool::new(false)), |removed| erased = removed)?;
        println!("phase=erased elapsed_ms={:.3} removed={erased}", started.elapsed().as_secs_f64()*1000.0);
        assert_eq!(erased, (count-changed) as u64);
        assert_eq!(store.count()?, 0);
        assert_eq!(std::fs::read_dir(&root)?.count(), count-changed);
        store.check_integrity()?;
        Ok(())
    })();
    std::fs::remove_dir_all(directory)?;
    result
}
