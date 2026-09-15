//! End-to-end vector worker measurements over disposable synthetic database records.
use blindspot_core::{content::{ContentStore,Document,MAX_BATCH},semantic::{Model,cache,indexing::{model_key,maintain_cache},vectors::{Artifact,Client}}};
use std::{path::{Path,PathBuf},sync::{Arc,atomic::AtomicBool},time::Instant};
use std::os::unix::fs::DirBuilderExt;

fn noise(seed:u64)->Vec<f32> {
    let mut state=seed.wrapping_add(1);
    let mut values:Vec<_>=(0..512).map(|_|{
        state^=state<<13; state^=state>>7; state^=state<<17;
        ((state>>40) as f32/8_388_608.0)-1.0
    }).collect();
    let norm=values.iter().map(|value|value*value).sum::<f32>().sqrt();
    for value in &mut values {*value/=norm;}
    values
}
fn vector(seed:u64)->Vec<f32> {
    let mut values=noise(seed);
    let center=noise(seed%128+1_000_000_000);
    for (value,center) in values.iter_mut().zip(center) {*value=0.35 * *value+0.85*center;}
    let norm=values.iter().map(|value|value*value).sum::<f32>().sqrt();
    for value in &mut values {*value/=norm;}
    values
}

fn main()->Result<(),Box<dyn std::error::Error>> {
    let mut arguments=std::env::args().skip(1);
    let count:usize=arguments.next().ok_or("Expected count and helper path")?.parse()?;
    let helper=PathBuf::from(arguments.next().ok_or("Expected helper path")?);
    if !(100..=1_000_000).contains(&count) || !helper.is_absolute() || arguments.next().is_some() {return Err("Expected 100..1000000 records and absolute helper path".into());}
    let directory=std::env::temp_dir().join(format!("blindspot-vector-roundtrip-{}",std::process::id()));
    std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let result=(||->Result<(),Box<dyn std::error::Error>> {
        let database=directory.join("content.sqlite");
        let mut store=ContentStore::open(&database)?;
        let model=Model {identifier:"synthetic-clustered".into(),revision:1,dimensions:512};
        let key=model_key(&model).map_err(|_|"Invalid fixture model")?;
        let scan=store.begin_scan(Path::new("/fixture"))?;
        let started=Instant::now();
        for offset in (0..count).step_by(MAX_BATCH) {
            let paths:Vec<_>=(offset..(offset+MAX_BATCH).min(count)).map(|id|format!("/fixture/{id}.txt")).collect();
            let documents:Vec<_>=paths.iter().map(|path|Document {
                identity:path,path:Path::new(path),title:path,body:"Synthetic vector fixture",modified_ns:1,changed_ns:1,bytes:24,
                extraction: blindspot_core::content::Extraction::Text,
            }).collect();
            let ids=store.put_batch(&scan,&documents)?;
            for (i,(id,revision)) in ids.into_iter().enumerate() {store.put_embedding(id,revision,&key,&vector((offset+i) as u64))?;}
        }
        println!("documents={count} prepare_ms={:.3}",started.elapsed().as_secs_f64()*1000.0);
        let cancel=Arc::new(AtomicBool::new(false));
        let started=Instant::now();
        let progress=maintain_cache(&store,&database,&helper,&model,Arc::clone(&cancel),|_|{})
            .map_err(|_|"Cache construction failed")?;
        let build_ms=started.elapsed().as_secs_f64()*1000.0;
        assert_eq!(progress.vectors_added,count);
        let artifacts:Vec<_>=store.vector_catalog(Arc::clone(&cancel))?.into_iter().map(|shard|Artifact {
            token:shard.token,count:shard.count,bytes:shard.bytes,checksum:shard.checksum,
        }).collect();
        println!("shards={} build_ms={build_ms:.3} graph_bytes={}",artifacts.len(),artifacts.iter().map(|artifact|artifact.bytes).sum::<u64>());
        let started=Instant::now();
        let reused=maintain_cache(&store,&database,&helper,&model,Arc::clone(&cancel),|_|{})
            .map_err(|_|"Cache reuse failed")?;
        assert_eq!(reused.reused,artifacts.len());
        assert_eq!(reused.built,0);
        assert_eq!(reused.vectors_added,0);
        println!("reuse_ms={:.3} reused_shards={}",started.elapsed().as_secs_f64()*1000.0,reused.reused);
        let mut client=Client::new(helper,cache::prepare(&database)?);
        let mut timings=Vec::new();
        for query in 0..21 {
            let seed=(query*count/21) as u64;
            let values=vector(seed);
            let started=Instant::now();
            let found=client.search(&values,&artifacts,10,&cancel).map_err(|_|"Vector query failed")?;
            let hits=store.resolve_embeddings(&key,512,&found.candidates,Arc::clone(&cancel))?;
            let elapsed=started.elapsed().as_secs_f64()*1000.0;
            assert_eq!(found.unavailable,0);
            assert_eq!(hits.len(),10);
            assert!(hits.iter().any(|hit|hit.path==format!("/fixture/{seed}.txt")),"Indexed query must retrieve itself");
            if query==0 {println!("cold_query_ms={elapsed:.3}");} else {timings.push(elapsed);}
        }
        timings.sort_by(f64::total_cmp);
        println!("warm_query_median_ms={:.3} warm_query_max_ms={:.3}",timings[timings.len()/2],timings.last().ok_or("Missing timing")?);
        client.close();
        store.check_integrity()?;
        let started=Instant::now();
        store.erase(Arc::clone(&cancel),|_|{})?;
        let removed=cache::prune(&database,&[],&cancel)?;
        assert_eq!(removed as usize,artifacts.len());
        assert!(store.vector_catalog(Arc::clone(&cancel))?.is_empty());
        println!("erase_ms={:.3} removed_shards={removed}",started.elapsed().as_secs_f64()*1000.0);
        store.check_integrity()?;
        Ok(())
    })();
    std::fs::remove_dir_all(directory)?;
    result
}
