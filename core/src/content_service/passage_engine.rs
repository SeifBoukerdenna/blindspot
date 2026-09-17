use crate::{content::{ContentStore, SearchFilter, passage_search::{Match,Scope}, vectors::{Shard,MAX_DELTA}},
    semantic::{self, Embedder, Model, Failure, cache, indexing::{self,model_key}, vectors, search::Helpers}};
use std::{path::{Path,PathBuf},sync::{Arc,atomic::{AtomicBool,Ordering}}};

pub struct Engine {
    helpers: Helpers,
    host: String,
    embedder: Option<(String,Embedder)>,
    vectors: Option<vectors::Client>,
    code_roots: Vec<PathBuf>,
}

impl Engine {
    pub fn new(helpers: Helpers, host: String) -> Self { Self { helpers,host,embedder:None,vectors:None,code_roots:Vec::new() } }
    pub fn with_code_roots(mut self, roots:Vec<PathBuf>)->Self {self.code_roots=roots;self}
    pub fn close(&mut self) {
        if let Some((_,embedder))=&mut self.embedder { embedder.close(); }
        self.embedder=None;
        self.vectors=None;
    }
    pub fn search(&mut self, database: &Path, query: &str, filter: &SearchFilter, roots: &[PathBuf], exclusions: &[PathBuf], cancel: Arc<AtomicBool>) -> Result<Vec<Match>,Failure> {
        self.search_with_scope(database, query, filter, Scope { roots, exclusions }, cancel, false)
    }

    pub fn search_with_scope(&mut self, database: &Path, query: &str, filter: &SearchFilter, scope: Scope<'_>, cancel: Arc<AtomicBool>, folder_scoped: bool) -> Result<Vec<Match>,Failure> {
        let Scope { roots, exclusions } = scope;
        let reader=ContentStore::open_reader(database).map_err(storage)?;
        for (key,dimensions) in reader.passage_models().map_err(storage)? {
            if cancel.load(Ordering::Acquire) { return Err(Failure::Cancelled); }
            let (identifier,revision,width): (String,u32,usize)=serde_json::from_str(&key).map_err(|_|Failure::InvalidResponse)?;
            if width!=dimensions { return Err(Failure::InvalidResponse); }
            if self.embedder.as_ref().is_none_or(|(old,_)| old!=&key) {
                self.embedder=Some((key.clone(), if identifier==semantic::MODEL_IDENTIFIER {
                    Embedder::Apple(semantic::Client::new(self.helpers.embedding.clone()))
                } else { Embedder::Ollama(semantic::ollama::Client::new(self.host.clone(),identifier.clone())) }));
            }
            let batch=match self.embedder.as_mut().ok_or(Failure::Unavailable)?.1.embed(&[query.into()],&cancel) {
                Ok(batch)=>batch,
                Err(Failure::Cancelled)=>return Err(Failure::Cancelled),
                Err(_)=>{self.close();continue;}
            };
            if batch.model != (Model {identifier,revision,dimensions}) { self.close(); continue; }
            let mut query_vector=batch.vectors.into_iter().next().ok_or(Failure::InvalidResponse)?;
            let norm=query_vector.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>().sqrt();
            if !norm.is_finite() || norm<=0.0 { return Err(Failure::InvalidResponse); }
            for value in &mut query_vector { *value=(f64::from(*value)/norm) as f32; }
            if folder_scoped {
                return reader.rank_folder_passages(&key, &query_vector, filter, scope, cancel,
                    |path| eligible_path(path, &self.code_roots)).map_err(storage);
            }
            if !filter.is_empty() && let Some(vectors)=reader.scoped_passage_vectors(&key,dimensions,filter,roots,exclusions,Arc::clone(&cancel)).map_err(storage)? {
                let mut candidates:Vec<_>=vectors.into_iter().map(|vector| {
                    let similarity:f32=vector.values.iter().zip(&query_vector).map(|(a,b)|a*b).sum();
                    (vector.key,(1.0-similarity).clamp(0.0,2.0))
                }).collect();
                candidates.sort_by(|a,b|a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
                let mut found=Vec::new();
                for page in candidates.chunks(100) {
                    found.extend(reader.resolve_passage_vectors(&key,page,filter,roots,exclusions,Arc::clone(&cancel)).map_err(storage)?
                        .into_iter().filter(|item|eligible_path(Path::new(&item.passage.path),&self.code_roots)));
                    if found.len()>=100 {break;}
                }
                found.truncate(100);
                return Ok(found);
            }
            let shards=reader.passage_catalog(Some(&key),Arc::clone(&cancel)).map_err(storage)?;
            let covered=shards.iter().map(|shard|shard.through).max().unwrap_or(0);
            let mut candidates=Vec::new();
            if !shards.is_empty() {
                if self.vectors.is_none() { self.vectors=Some(vectors::Client::new(self.helpers.vectors.clone(),cache::prepare(database).map_err(storage)?)); }
                let artifacts:Vec<_>=shards.iter().map(artifact).collect();
                if let Some(client)=self.vectors.as_mut() {
                    match client.search(&query_vector,&artifacts,100,&cancel) {
                        Ok(found)=>candidates=found.candidates,
                        Err(Failure::Cancelled)=>return Err(Failure::Cancelled),
                        Err(_)=>{self.vectors=None;}
                    }
                }
            }
            let mut after=covered;
            let mut examined=0;
            while examined<MAX_DELTA {
                let page=reader.passage_vector_page(after,i64::MAX,&key,dimensions,Arc::clone(&cancel)).map_err(storage)?;
                if page.scanned==0 {break;}
                after=page.after; examined+=page.scanned;
                for vector in page.vectors {
                    let similarity:f32=vector.values.iter().zip(&query_vector).map(|(a,b)|a*b).sum();
                    candidates.push((vector.key,(1.0-similarity).clamp(0.0,2.0)));
                }
            }
            candidates.sort_by(|a,b|a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
            candidates.dedup_by_key(|item|item.0);
            candidates.truncate(100);
            let mut found=reader.resolve_passage_vectors(&key,&candidates,filter,roots,exclusions,cancel).map_err(storage)?;
            found.retain(|item| eligible_path(Path::new(&item.passage.path),&self.code_roots));
            return Ok(found);
        }
        if folder_scoped { Err(Failure::Unavailable) } else { Ok(Vec::new()) }
    }
}

pub fn eligible_path(path:&Path,code_roots:&[PathBuf])->bool {
    crate::content::semantic_eligible(path,crate::content::Extraction::Text)
        || path.extension().and_then(|s|s.to_str()).is_some_and(|s|matches!(s.to_ascii_lowercase().as_str(),"pdf"|"docx"|"doc"|"rtf"|"odt"|"pptx"))
        || (crate::content::chunk_eligible(path,crate::content::Extraction::Text) && code_roots.iter().any(|root|path.starts_with(root)))
}

pub fn select(helpers: &Helpers, host: &str, name: &str, cancel: &AtomicBool) -> Result<(Embedder,Model,bool),Failure> {
    if !name.trim().is_empty() {
        let mut embedder=Embedder::Ollama(semantic::ollama::Client::new(host.into(),name.into()));
        match embedder.probe(cancel) {
            Ok(model)=>return Ok((embedder,model,false)),
            Err(Failure::Cancelled)=>return Err(Failure::Cancelled),
            Err(_)=>embedder.close(),
        }
    }
    let mut embedder=Embedder::Apple(semantic::Client::new(helpers.embedding.clone()));
    let model=embedder.probe(cancel)?;
    Ok((embedder,model,true))
}

pub fn maintain(store: &ContentStore,database:&Path,worker:&Path,model:&Model,cancel:Arc<AtomicBool>,mut report:impl FnMut(usize,usize)) -> Result<(),indexing::Error> {
    maintain_with_budget(store,database,worker,model,cancel,&mut report,u64::MAX)
}

pub fn maintain_with_budget(store: &ContentStore,database:&Path,worker:&Path,model:&Model,cancel:Arc<AtomicBool>,mut report:impl FnMut(usize,usize),budget:u64) -> Result<(),indexing::Error> {
    let key=model_key(model)?;
    let catalog=store.passage_catalog(Some(&key),Arc::clone(&cancel))?;
    let mut client=vectors::Client::new(worker.into(),cache::prepare(database)?);
    let mut built=0;
    let mut reused=0;
    let mut probe=vec![0.0;model.dimensions]; probe[0]=1.0;
    for (after,through,count) in store.passage_vector_ranges(&key,Arc::clone(&cancel))? {
        if cancel.load(Ordering::Acquire) { return Err(indexing::Error::Cancelled); }
        if let Some(old)=catalog.iter().find(|s|s.after==after && s.through==through && s.count==count)
            && cache::available(database,old)? {
            let checked=client.search(&probe,&[artifact(old)],1,&cancel)?;
            if checked.unavailable==0 { reused+=1;report(built,reused);continue; }
        }
        if store.storage_bytes()?.saturating_add((count as u64).saturating_mul(model.dimensions as u64 * 4 + 256))>budget {
            return Err(crate::content::Error::Invalid("Storage budget reached").into());
        }
        let token=cache::token()?;
        client.begin(&token,model.dimensions,count,&cancel)?;
        let mut cursor=after;
        loop {
            let page=store.passage_vector_page(cursor,through,&key,model.dimensions,Arc::clone(&cancel))?;
            if page.scanned==0 {break;}
            cursor=page.after;
            client.add(&page.vectors,&cancel)?;
        }
        let artifact=client.finish(&cancel)?;
        let shard=Shard {after,through,model:key.clone(),dimensions:model.dimensions,token:artifact.token,checksum:artifact.checksum,count:artifact.count,bytes:artifact.bytes};
        if !store.publish_passage_shard(&shard,Arc::clone(&cancel))? { return Err(indexing::Error::Storage(crate::content::Error::Invalid("Passages changed during shard publication"))); }
        built+=1;report(built,reused);
    }
    client.close();
    store.remove_empty_passage_shards(&key)?;
    let mut retained=store.vector_catalog(Arc::clone(&cancel))?;
    retained.extend(store.passage_catalog(None,Arc::clone(&cancel))?);
    cache::prune(database,&retained,&cancel)?;
    Ok(())
}

fn artifact(shard:&Shard)->vectors::Artifact { vectors::Artifact {token:shard.token.clone(),checksum:shard.checksum.clone(),count:shard.count,bytes:shard.bytes} }
fn storage(error:crate::content::Error)->Failure { match error {crate::content::Error::Cancelled=>Failure::Cancelled,_=>Failure::Unavailable} }
