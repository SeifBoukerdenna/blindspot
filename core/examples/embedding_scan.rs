//! Native embedding throughput over a disposable synthetic content database.
use blindspot_core::{content::{ContentStore, Document, MAX_BATCH}, semantic::{Client, indexing}};
use std::{path::{Path, PathBuf}, sync::{Arc, atomic::AtomicBool}, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let count: usize = arguments.next().ok_or("Expected document count and absolute helper path")?.parse()?;
    let helper = PathBuf::from(arguments.next().ok_or("Expected absolute helper path")?);
    if !(1..=100_000).contains(&count) || !helper.is_absolute() || arguments.next().is_some() {
        return Err("Expected 1..100000 documents and an absolute helper path".into());
    }
    let directory = std::env::temp_dir().join(format!("blindspot-embedding-bench-{}",std::process::id()));
    std::fs::create_dir(&directory)?;
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut store = ContentStore::open(&directory.join("content.sqlite"))?;
        let scan = store.begin_scan(Path::new("/fixture"))?;
        for offset in (0..count).step_by(MAX_BATCH) {
            let paths: Vec<_> = (offset..(offset+MAX_BATCH).min(count)).map(|id|format!("/fixture/{id}.txt")).collect();
            let documents: Vec<_> = paths.iter().map(|path|document(path,false)).collect();
            store.put_batch(&scan,&documents)?;
        }
        let mut client = Client::new(helper);
        for phase in ["initial","unchanged","updated"] {
            if phase=="updated" {
                for offset in (0..count).step_by(MAX_BATCH*10) {
                    let paths: Vec<_> = (offset..(offset+MAX_BATCH*10).min(count)).step_by(10).map(|id|format!("/fixture/{id}.txt")).collect();
                    let documents: Vec<_> = paths.iter().map(|path|document(path,true)).collect();
                    store.put_batch(&scan,&documents)?;
                }
            }
            let started = Instant::now();
            let progress = indexing::run(&mut store,&mut client,Arc::new(AtomicBool::new(false)), |_|true, |_|{})
                .map_err(|_|"Embedding pass failed")?;
            println!("phase={phase} documents={count} elapsed_ms={:.3} examined={} current={} written={} failed={}",
                started.elapsed().as_secs_f64()*1000.0,progress.examined,progress.current,progress.written,progress.failed);
            assert_eq!(progress.examined,count as u64);
            assert_eq!(progress.failed+progress.excluded+progress.stale,0);
            let expected = match phase { "initial" => count, "updated" => count.div_ceil(10), _ => 0 };
            assert_eq!(progress.written,expected as u64);
            assert_eq!(progress.current,(count-expected) as u64);
        }
        store.check_integrity()?;
        println!("database_bytes={}",std::fs::metadata(directory.join("content.sqlite"))?.len());
        Ok(())
    })();
    std::fs::remove_dir_all(directory)?;
    result
}

fn document(path: &str, updated: bool) -> Document<'_> {
    let body = if updated { "Updated notes about compiler optimization and memory usage." }
        else { "Synthetic document about relational database transactions and local file search." };
    Document { identity:path,path:Path::new(path),title:path.rsplit('/').next().unwrap(),body,
        modified_ns:if updated {2} else {1},changed_ns:if updated {2} else {1},bytes:body.len() as u64,
        extraction: blindspot_core::content::Extraction::Text }
}
