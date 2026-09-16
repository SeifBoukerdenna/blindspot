use blindspot_core::{content::ContentStore, content_indexer::{self, Policy},
    content_service::passage_engine, semantic::{indexing, search::Helpers}};
use std::{path::PathBuf, sync::{Arc, atomic::AtomicBool}, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args=std::env::args().skip(1);
    let root=PathBuf::from(args.next().ok_or("Expected root, new scratch directory, helpers, model")?).canonicalize()?;
    let directory=PathBuf::from(args.next().ok_or("Expected new scratch directory")?);
    let helpers=PathBuf::from(args.next().ok_or("Expected helpers directory")?).canonicalize()?;
    let model=args.next().unwrap_or_else(||"embeddinggemma:300m".into());
    if args.next().is_some() || !directory.is_absolute() || directory.exists() {return Err("Scratch directory must be new and absolute".into());}
    std::fs::create_dir(&directory)?;
    let database=directory.join("content.sqlite");
    let cancel=Arc::new(AtomicBool::new(false));
    let mut store=ContentStore::open(&database)?;
    let policy=Policy {batch_pause:Duration::ZERO,..Policy::default()};
    let scan=content_indexer::scan_root(&mut store,&root,&policy,&cancel, |_|{})?;
    println!("Scan: {} documents, complete={}",scan.indexed,scan.complete);
    if !scan.complete {return Err("Incomplete scan".into());}
    let helpers=Helpers {embedding:helpers.join("blindspot-semantic"),vectors:helpers.join("blindspot-vectors")};
    let (mut embedder,model,fallback)=passage_engine::select(&helpers,"127.0.0.1:11434",&model,&cancel).map_err(|e|format!("{e:?}"))?;
    println!("Model: {} / {} dimensions / fallback={fallback}",model.identifier,model.dimensions);
    let key=indexing::model_key(&model).map_err(|e|format!("{e:?}"))?;
    store.begin_passage_model(&key,model.dimensions)?;
    let progress=indexing::run_chunks(&mut store,&mut embedder,Arc::clone(&cancel), |_|true, |_|{}).map_err(|e|format!("{e:?}"))?;
    println!("Embeddings: written={} current={} failed={} stale={}",progress.written,progress.current,progress.failed,progress.stale);
    if progress.failed>0 || progress.stale>0 {return Err("Incomplete embeddings".into());}
    passage_engine::maintain(&store,&database,&helpers.vectors,&model,Arc::clone(&cancel),|built,reused|println!("Shards: built={built} reused={reused}")).map_err(|e|format!("{e:?}"))?;
    store.activate_passage_model(&key,&cancel)?;
    store.check_integrity()?;
    println!("Ready: {} bytes",store.storage_bytes()?);
    Ok(())
}
