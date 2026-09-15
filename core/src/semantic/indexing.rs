//! Incremental embedding passes over bounded document pages; called by background owners.

use super::{Batch, Client, Failure, Model};
use crate::content::{ContentStore, EmbeddingDocument};
use std::path::Path;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::Duration;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Progress {
    pub examined: u64,
    pub current: u64,
    pub written: u64,
    pub stale: u64,
    pub excluded: u64,
    pub failed: u64,
    pub lexical_only: u64,
}

#[derive(Debug)]
pub enum Error {
    Storage(crate::content::Error),
    Worker(Failure),
    Cancelled,
}
impl From<crate::content::Error> for Error {
    fn from(error: crate::content::Error) -> Self {
        match error { crate::content::Error::Cancelled => Self::Cancelled, other => Self::Storage(other) }
    }
}
impl From<Failure> for Error {
    fn from(error: Failure) -> Self {
        match error { Failure::Cancelled => Self::Cancelled, other => Self::Worker(other) }
    }
}

pub fn model_key(model: &Model) -> Result<String, Failure> {
    let key = serde_json::to_string(&(model.identifier.as_str(),model.revision,model.dimensions))
        .map_err(|_|Failure::InvalidResponse)?;
    if key.len()>128 || model.identifier.is_empty() || !model.identifier.is_ascii()
        || model.identifier.chars().any(char::is_control) || model.revision==0 || !(1..=2048).contains(&model.dimensions) {
        return Err(Failure::InvalidResponse);
    }
    Ok(key)
}

pub fn run(
    store: &mut ContentStore,
    client: &mut Client,
    cancel: Arc<AtomicBool>,
    eligible: impl Fn(&Path) -> bool,
    mut report: impl FnMut(&Progress),
) -> Result<Progress, Error> {
    let model = client.probe(&cancel)?;
    let key = model_key(&model)?;
    let mut progress = Progress::default();
    let mut after = 0;
    loop {
        check(&cancel)?;
        let page = store.embedding_page(after,&key,model.dimensions,Arc::clone(&cancel))?;
        if page.scanned == 0 { return Ok(progress); }
        after = page.after;
        progress.examined += page.scanned as u64;
        progress.lexical_only += page.lexical_only as u64;
        progress.current += page.scanned.saturating_sub(page.pending.len() + page.lexical_only) as u64;
        let mut pending = Vec::with_capacity(page.pending.len());
        for document in page.pending {
            if eligible(Path::new(&document.path)) { pending.push(document); }
            else { progress.excluded += 1; }
        }
        for documents in pending.chunks(4) {
            check(&cancel)?;
            let texts: Vec<_> = documents.iter().map(|document|passage(&document.text)).collect();
            match client.embed(&texts,&cancel) {
                Ok(batch) => persist(store,documents,&model,&key,batch,&cancel,&mut progress)?,
                Err(Failure::EmbeddingUnavailable | Failure::InvalidInput) => {
                    for document in documents {
                        check(&cancel)?;
                        match client.embed(&[passage(&document.text)],&cancel) {
                            Ok(batch) => persist(store,std::slice::from_ref(document),&model,&key,batch,&cancel,&mut progress)?,
                            Err(Failure::EmbeddingUnavailable | Failure::InvalidInput) => progress.failed += 1,
                            Err(error) => return Err(error.into()),
                        }
                    }
                }
                Err(error) => return Err(error.into()),
            }
            report(&progress);
            check(&cancel)?;
            std::thread::sleep(Duration::from_millis(10));
        }
        report(&progress);
    }
}

const PASSAGE_BYTES: usize = 1200;

/// The contextual model reads at most 256 tokens, so the start of a document (title first) is
/// embedded as one whitespace-normalized passage instead of letting the helper truncate raw text.
/// One passage per document measured nearly as well as three on real notes, without a schema change.
pub fn passage(text: &str) -> String {
    let mut passage = String::with_capacity(PASSAGE_BYTES);
    for word in text.split_whitespace() {
        if passage.len() + word.len() + 1 > PASSAGE_BYTES {
            if passage.is_empty() {
                passage.push_str(&word[..word.floor_char_boundary(PASSAGE_BYTES)]);
            }
            break;
        }
        if !passage.is_empty() {
            passage.push(' ');
        }
        passage.push_str(word);
    }
    passage
}

pub struct ShardRange {
    pub token: String,
    pub after: i64,
    pub through: i64,
}

pub fn build_shard(
    store: &ContentStore,
    client: &mut super::vectors::Client,
    model: &Model,
    range: &ShardRange,
    cancel: Arc<AtomicBool>,
    mut report: impl FnMut(usize),
) -> Result<Option<super::vectors::Artifact>,Error> {
    let result=(|| {
        check(&cancel)?;
        let key=model_key(model)?;
        let count=store.embedding_count(range.after,range.through,&key,model.dimensions,Arc::clone(&cancel))?;
        if count==0 { return Ok(None); }
        client.begin(&range.token,model.dimensions,count,&cancel)?;
        let mut after=range.after;
        let mut added=0;
        while after<range.through {
            check(&cancel)?;
            let page=store.embedding_vectors(after,range.through,&key,model.dimensions,Arc::clone(&cancel))?;
            if page.scanned==0 { break; }
            after=page.after;
            if !page.vectors.is_empty() {
                client.add(&page.vectors,&cancel)?;
                added+=page.vectors.len();
                report(added);
                check(&cancel)?;
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        check(&cancel)?;
        Ok(Some(client.finish(&cancel)?))
    })();
    if result.is_err() { client.close(); }
    result
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CacheProgress {
    pub built: usize,
    pub reused: usize,
    pub vectors_added: usize,
    pub removed_files: u64,
}

pub fn maintain_cache(
    store: &ContentStore,
    database: &Path,
    worker: &Path,
    model: &Model,
    cancel: Arc<AtomicBool>,
    mut report: impl FnMut(&CacheProgress),
) -> Result<CacheProgress,Error> {
    use crate::content::vectors::{MAX_SHARDS, Shard};
    use super::{cache, vectors::{Artifact, Client as VectorClient}};
    check(&cancel)?;
    let key=model_key(model)?;
    let directory=cache::prepare(database)?;
    let mut client=VectorClient::new(worker.into(),directory);
    let mut progress=CacheProgress::default();
    for shard in store.vector_catalog(Arc::clone(&cancel))? {
        check(&cancel)?;
        // Stale shards of the current model keep serving until their replacement is published.
        if shard.model!=key || shard.dimensions!=model.dimensions {
            store.remove_vector_shard(&shard,Arc::clone(&cancel))?;
        }
    }
    let catalog=store.vector_catalog(Arc::clone(&cancel))?;
    progress.removed_files+=cache::prune(database,&catalog,&cancel)?;
    let high=store.embedding_watermark()?;
    let mut after=0;
    let mut visited=Vec::new();
    let mut probe=vec![0.0;model.dimensions];
    if let Some(value)=probe.first_mut() { *value=1.0; }
    while let Some((start,end))=store.next_vector_range(after,high,&key,model.dimensions,Arc::clone(&cancel))? {
        check(&cancel)?;
        if progress.built+progress.reused>=MAX_SHARDS {
            return Err(crate::content::Error::Invalid("Vector cache capacity exceeded").into());
        }
        after=end;
        visited.push(start);
        if let Some(shard)=catalog.iter().find(|shard|shard.after==start)
            && cache::available(database,shard)?
            && store.vector_shard_reusable(shard,end>=high,Arc::clone(&cancel))? {
            let artifact=Artifact {token:shard.token.clone(),count:shard.count,bytes:shard.bytes,checksum:shard.checksum.clone()};
            let checked=client.search(&probe,&[artifact],1,&cancel)?;
            if checked.unavailable==0 && !checked.candidates.is_empty() {
                progress.reused+=1;
                report(&progress);
                continue;
            }
        }
        let mut added=0;
        let range=ShardRange {token:cache::token()?,after:start,through:end};
        let Some(artifact)=build_shard(store,&mut client,model,&range,Arc::clone(&cancel),|count| {
            progress.vectors_added+=count-added;
            added=count;
            report(&progress);
        })? else { continue; };
        let shard=Shard {after:start,through:end,model:key.clone(),dimensions:model.dimensions,
            token:artifact.token,checksum:artifact.checksum,count:artifact.count,bytes:artifact.bytes};
        if !store.publish_vector_shard(&shard,Arc::clone(&cancel))? {
            return Err(crate::content::Error::Invalid("Vector source changed during publication").into());
        }
        progress.built+=1;
        report(&progress);
    }
    client.close();
    for shard in store.vector_catalog(Arc::clone(&cancel))? {
        if !visited.contains(&shard.after) { store.remove_vector_shard(&shard,Arc::clone(&cancel))?; }
    }
    progress.removed_files+=cache::prune(database,&store.vector_catalog(Arc::clone(&cancel))?,&cancel)?;
    check(&cancel)?;
    report(&progress);
    Ok(progress)
}

fn persist(store: &mut ContentStore, documents: &[EmbeddingDocument], model: &Model, key: &str,
    batch: Batch, cancel: &AtomicBool, progress: &mut Progress) -> Result<(), Error> {
    if &batch.model!=model || batch.vectors.len()!=documents.len() {
        return Err(Error::Worker(Failure::InvalidResponse));
    }
    for (document,vector) in documents.iter().zip(batch.vectors) {
        check(cancel)?;
        if store.put_embedding(document.id,document.revision,key,&vector)? { progress.written += 1; }
        else { progress.stale += 1; }
    }
    Ok(())
}

fn check(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Acquire) { Err(Error::Cancelled) } else { Ok(()) }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use std::io::{Seek, Write};

    #[test]
    #[ignore = "requires the built native vector helper and native descriptor access"]
    fn native_cache_recovers_corruption_reuses_commits_and_cleans_cancelled_work() {
        let root=std::env::temp_dir().join(format!("blindspot-cache-pass-{}",std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let database=root.join("content.sqlite");
        let worker=std::path::PathBuf::from(std::env::var_os("BLINDSPOT_VECTOR_WORKER").expect("helper path"));
        let model=Model {identifier:"fixture".into(),revision:1,dimensions:2};
        let key=model_key(&model).unwrap();
        let mut store=ContentStore::open(&database).unwrap();
        let scan=store.begin_scan(Path::new("/fixture")).unwrap();
        let paths:Vec<_>=(0..130).map(|i|format!("/fixture/{i}.txt")).collect();
        let docs:Vec<_>=paths.iter().map(|path|crate::content::Document {
            identity:path,path:Path::new(path),title:"fixture",body:"fixture",modified_ns:1,changed_ns:1,bytes:7,
            extraction: crate::content::Extraction::Text,
        }).collect();
        let ids=store.put_batch(&scan,&docs).unwrap();
        for (id,revision) in &ids { store.put_embedding(*id,*revision,&key,&[1.0,0.0]).unwrap(); }
        let cancel=Arc::new(AtomicBool::new(false));
        assert!(matches!(maintain_cache(&store,&database,&worker,&model,Arc::clone(&cancel),|progress| {
            if progress.vectors_added>=64 { cancel.store(true,Ordering::Release); }
        }),Err(Error::Cancelled)));
        assert!(store.vector_catalog(Arc::new(AtomicBool::new(false))).unwrap().is_empty());
        cancel.store(false,Ordering::Release);
        let first=maintain_cache(&store,&database,&worker,&model,Arc::clone(&cancel),|_|{}).unwrap();
        assert_eq!((first.built,first.reused,first.vectors_added),(1,0,130));
        drop(store);
        let store=ContentStore::open(&database).unwrap();
        let second=maintain_cache(&store,&database,&worker,&model,Arc::clone(&cancel),|_|{}).unwrap();
        assert_eq!((second.built,second.reused,second.vectors_added),(0,1,0));
        let original=store.vector_catalog(Arc::clone(&cancel)).unwrap().remove(0);
        let directory=super::super::cache::prepare(&database).unwrap();
        let path=directory.join(format!("{}.ann",original.token));
        let mut file=std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(std::io::SeekFrom::Start(0)).unwrap();
        file.write_all(b"broken!!").unwrap();file.sync_all().unwrap();drop(file);
        let repaired=maintain_cache(&store,&database,&worker,&model,Arc::clone(&cancel),|_|{}).unwrap();
        assert_eq!((repaired.built,repaired.reused),(1,0));
        assert!(!path.exists());
        assert_ne!(store.vector_catalog(Arc::clone(&cancel)).unwrap()[0].token,original.token);
        let mut store=store;
        store.put_embedding(ids[0].0,ids[0].1,&key,&[0.0,1.0]).unwrap();
        let updated=maintain_cache(&store,&database,&worker,&model,Arc::clone(&cancel),|_|{}).unwrap();
        assert_eq!((updated.built,updated.reused),(1,0));
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(),1);
        store.erase(Arc::clone(&cancel),|_|{}).unwrap();
        super::super::cache::prune(&database,&[],&cancel).unwrap();
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(),0);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
