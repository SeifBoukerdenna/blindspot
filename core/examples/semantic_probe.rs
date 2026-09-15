//! Retrieval probe for judging ranking on real data.
//!
//! `semantic_probe <database> <helper-directory> [--scan <root>]... [--index] <query>...`
//!
//! Without `--scan`/`--index` it only reads. Both write the given database (PDF extraction,
//! migration, embeddings), so they refuse the live index location: use a scratch database or a
//! `.backup` copy. Paths are printed, so keep the output in a local terminal.
use blindspot_core::{
    content::ContentStore,
    content_indexer::{Policy, scan_root},
    semantic::{
        Client, indexing,
        search::{Engine, Helpers, fuse},
    },
};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1).peekable();
    let database = PathBuf::from(
        arguments
            .next()
            .ok_or("Expected database, helper directory, then queries")?,
    );
    let helpers = PathBuf::from(arguments.next().ok_or("Expected helper directory")?);
    let cancel = Arc::new(AtomicBool::new(false));
    let writes = arguments
        .peek()
        .is_some_and(|argument| argument == "--scan" || argument == "--index");
    if writes
        && database
            .to_string_lossy()
            .contains("/.local/share/blindspot/")
    {
        return Err(
            "Refusing to write the live index; use a scratch database or backup copy".into(),
        );
    }
    while arguments
        .peek()
        .is_some_and(|argument| argument == "--scan")
    {
        arguments.next();
        let root = PathBuf::from(arguments.next().ok_or("Expected a folder after --scan")?)
            .canonicalize()?;
        let mut store = ContentStore::open(&database)?;
        let policy = Policy {
            documents: true,
            extractor: Some(helpers.join("blindspot-extract")),
            ..Policy::default()
        };
        let started = Instant::now();
        let progress = scan_root(&mut store, &root, &policy, &cancel, |_| {})?;
        println!("scan {progress:?} in {:?}", started.elapsed());
    }
    if arguments
        .peek()
        .is_some_and(|argument| argument == "--index")
    {
        arguments.next();
        let mut store = ContentStore::open(&database)?;
        let started = Instant::now();
        let mut client = Client::new(helpers.join("blindspot-semantic"));
        let progress = indexing::run(
            &mut store,
            &mut client,
            Arc::clone(&cancel),
            |_| true,
            |_| {},
        )
        .map_err(|error| format!("{error:?}"))?;
        println!("embedding {progress:?} in {:?}", started.elapsed());
        let model = client
            .probe(&cancel)
            .map_err(|error| format!("{error:?}"))?;
        client.close();
        let started = Instant::now();
        let cache = indexing::maintain_cache(
            &store,
            &database,
            &helpers.join("blindspot-vectors"),
            &model,
            Arc::clone(&cancel),
            |_| {},
        )
        .map_err(|error| format!("{error:?}"))?;
        println!("cache {cache:?} in {:?}", started.elapsed());
    }
    let mut engine = Engine::new(Helpers {
        embedding: helpers.join("blindspot-semantic"),
        vectors: helpers.join("blindspot-vectors"),
    });
    for query in arguments {
        let reader = ContentStore::open_reader(&database)?;
        let started = Instant::now();
        let lexical = reader
            .search(&query, 100, Arc::clone(&cancel))
            .unwrap_or_default();
        let lexical_time = started.elapsed();
        let started = Instant::now();
        let semantic = engine
            .search(&database, &query, Arc::clone(&cancel))
            .map_err(|error| format!("{error:?}"))?;
        println!(
            "== {query}  lexical {} in {lexical_time:?} · semantic {} in {:?}",
            lexical.hits.len(),
            semantic.len(),
            started.elapsed()
        );
        for hit in fuse(lexical, semantic).hits.iter().take(12) {
            println!("  {} {}", if hit.related { "~" } else { "=" }, hit.path);
        }
    }
    Ok(())
}
