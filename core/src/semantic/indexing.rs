//! Incremental embedding passes over bounded document pages; called by background owners.

use super::{Batch, Client, Embedder, Failure, Model};
use crate::content::{ContentStore, EmbeddingDocument, PendingChunk};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
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
        match error {
            crate::content::Error::Cancelled => Self::Cancelled,
            other => Self::Storage(other),
        }
    }
}
impl From<Failure> for Error {
    fn from(error: Failure) -> Self {
        match error {
            Failure::Cancelled => Self::Cancelled,
            other => Self::Worker(other),
        }
    }
}

pub fn model_key(model: &Model) -> Result<String, Failure> {
    let key = serde_json::to_string(&(model.identifier.as_str(), model.revision, model.dimensions))
        .map_err(|_| Failure::InvalidResponse)?;
    if key.len() > 128
        || model.identifier.is_empty()
        || !model.identifier.is_ascii()
        || model.identifier.chars().any(char::is_control)
        || model.revision == 0
        || !(1..=2048).contains(&model.dimensions)
    {
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
        let page = store.embedding_page(after, &key, model.dimensions, Arc::clone(&cancel))?;
        if page.scanned == 0 {
            return Ok(progress);
        }
        after = page.after;
        progress.examined += page.scanned as u64;
        progress.lexical_only += page.lexical_only as u64;
        progress.current +=
            page.scanned
                .saturating_sub(page.pending.len() + page.lexical_only) as u64;
        let mut pending = Vec::with_capacity(page.pending.len());
        for document in page.pending {
            if eligible(Path::new(&document.path)) {
                pending.push(document);
            } else {
                progress.excluded += 1;
            }
        }
        for documents in pending.chunks(4) {
            check(&cancel)?;
            let texts: Vec<_> = documents
                .iter()
                .map(|document| passage(&document.text))
                .collect();
            match client.embed(&texts, &cancel) {
                Ok(batch) => persist(
                    store,
                    documents,
                    &model,
                    &key,
                    batch,
                    &cancel,
                    &mut progress,
                )?,
                Err(Failure::EmbeddingUnavailable | Failure::InvalidInput) => {
                    for document in documents {
                        check(&cancel)?;
                        match client.embed(&[passage(&document.text)], &cancel) {
                            Ok(batch) => persist(
                                store,
                                std::slice::from_ref(document),
                                &model,
                                &key,
                                batch,
                                &cancel,
                                &mut progress,
                            )?,
                            Err(Failure::EmbeddingUnavailable | Failure::InvalidInput) => {
                                progress.failed += 1
                            }
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

/// Embeds passages that have no current vector for this model, a bounded page at a time.
///
/// Shaped like [`run`], with three differences that matter: the unit is a chunk rather than a
/// document, the batch size comes from the backend rather than being fixed at four, and the text
/// is sent as stored. [`passage`] is deliberately not applied — its 1,200-byte cap exists to keep a
/// whole document inside the contextual model's window, and a chunk is already sized to fit.
pub fn run_chunks(
    store: &mut ContentStore,
    embedder: &mut Embedder,
    cancel: Arc<AtomicBool>,
    eligible: impl Fn(&Path) -> bool,
    report: impl FnMut(&Progress),
) -> Result<Progress, Error> {
    run_chunks_with_budget(store,embedder,cancel,eligible,report,u64::MAX)
}

pub fn run_chunks_with_budget(
    store: &mut ContentStore,
    embedder: &mut Embedder,
    cancel: Arc<AtomicBool>,
    eligible: impl Fn(&Path) -> bool,
    mut report: impl FnMut(&Progress),
    budget: u64,
) -> Result<Progress, Error> {
    let model = embedder.probe(&cancel)?;
    let key = model_key(&model)?;
    let size = embedder.batch().max(1);
    let mut progress = Progress::default();
    let mut after = 0;
    loop {
        check(&cancel)?;
        let page =
            store.chunk_embedding_page(after, &key, model.dimensions, Arc::clone(&cancel))?;
        if page.scanned == 0 {
            return Ok(progress);
        }
        after = page.after;
        progress.examined += page.scanned as u64;
        progress.lexical_only += page.lexical_only as u64;
        progress.current +=
            page.scanned
                .saturating_sub(page.pending.len() + page.lexical_only) as u64;
        let mut pending = Vec::with_capacity(page.pending.len());
        for chunk in page.pending {
            if eligible(Path::new(&chunk.path)) {
                pending.push(chunk);
            } else {
                progress.excluded += 1;
            }
        }
        for chunks in pending.chunks(size) {
            check(&cancel)?;
            if store.storage_bytes()? >= budget {
                return Err(crate::content::Error::Invalid("Index storage budget reached").into());
            }
            let texts: Vec<_> = chunks.iter().map(|chunk| chunk.text.clone()).collect();
            match embedder.embed(&texts, &cancel) {
                Ok(batch) => {
                    persist_chunks(store, chunks, &model, &key, batch, &cancel, &mut progress)?
                }
                // One passage the model cannot read must not cost the rest of the batch, so the
                // batch is retried a passage at a time and only the bad ones are counted failed.
                Err(Failure::EmbeddingUnavailable | Failure::InvalidInput) => {
                    for chunk in chunks {
                        check(&cancel)?;
                        match embedder.embed(std::slice::from_ref(&chunk.text), &cancel) {
                            Ok(batch) => persist_chunks(
                                store,
                                std::slice::from_ref(chunk),
                                &model,
                                &key,
                                batch,
                                &cancel,
                                &mut progress,
                            )?,
                            Err(Failure::EmbeddingUnavailable | Failure::InvalidInput) => {
                                progress.failed += 1
                            }
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
) -> Result<Option<super::vectors::Artifact>, Error> {
    let result = (|| {
        check(&cancel)?;
        let key = model_key(model)?;
        let count = store.embedding_count(
            range.after,
            range.through,
            &key,
            model.dimensions,
            Arc::clone(&cancel),
        )?;
        if count == 0 {
            return Ok(None);
        }
        client.begin(&range.token, model.dimensions, count, &cancel)?;
        let mut after = range.after;
        let mut added = 0;
        while after < range.through {
            check(&cancel)?;
            let page = store.embedding_vectors(
                after,
                range.through,
                &key,
                model.dimensions,
                Arc::clone(&cancel),
            )?;
            if page.scanned == 0 {
                break;
            }
            after = page.after;
            if !page.vectors.is_empty() {
                client.add(&page.vectors, &cancel)?;
                added += page.vectors.len();
                report(added);
                check(&cancel)?;
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        check(&cancel)?;
        Ok(Some(client.finish(&cancel)?))
    })();
    if result.is_err() {
        client.close();
    }
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
) -> Result<CacheProgress, Error> {
    use super::{
        cache,
        vectors::{Artifact, Client as VectorClient},
    };
    use crate::content::vectors::{MAX_SHARDS, Shard};
    check(&cancel)?;
    let key = model_key(model)?;
    let directory = cache::prepare(database)?;
    let mut client = VectorClient::new(worker.into(), directory);
    let mut progress = CacheProgress::default();
    for shard in store.vector_catalog(Arc::clone(&cancel))? {
        check(&cancel)?;
        // Stale shards of the current model keep serving until their replacement is published.
        if shard.model != key || shard.dimensions != model.dimensions {
            store.remove_vector_shard(&shard, Arc::clone(&cancel))?;
        }
    }
    let catalog = store.vector_catalog(Arc::clone(&cancel))?;
    progress.removed_files += cache::prune(database, &catalog, &cancel)?;
    let high = store.embedding_watermark()?;
    let mut after = 0;
    let mut visited = Vec::new();
    let mut probe = vec![0.0; model.dimensions];
    if let Some(value) = probe.first_mut() {
        *value = 1.0;
    }
    while let Some((start, end)) =
        store.next_vector_range(after, high, &key, model.dimensions, Arc::clone(&cancel))?
    {
        check(&cancel)?;
        if progress.built + progress.reused >= MAX_SHARDS {
            return Err(crate::content::Error::Invalid("Vector cache capacity exceeded").into());
        }
        after = end;
        visited.push(start);
        if let Some(shard) = catalog.iter().find(|shard| shard.after == start)
            && cache::available(database, shard)?
            && store.vector_shard_reusable(shard, end >= high, Arc::clone(&cancel))?
        {
            let artifact = Artifact {
                token: shard.token.clone(),
                count: shard.count,
                bytes: shard.bytes,
                checksum: shard.checksum.clone(),
            };
            let checked = client.search(&probe, &[artifact], 1, &cancel)?;
            if checked.unavailable == 0 && !checked.candidates.is_empty() {
                progress.reused += 1;
                report(&progress);
                continue;
            }
        }
        let mut added = 0;
        let range = ShardRange {
            token: cache::token()?,
            after: start,
            through: end,
        };
        let Some(artifact) = build_shard(
            store,
            &mut client,
            model,
            &range,
            Arc::clone(&cancel),
            |count| {
                progress.vectors_added += count - added;
                added = count;
                report(&progress);
            },
        )?
        else {
            continue;
        };
        let shard = Shard {
            after: start,
            through: end,
            model: key.clone(),
            dimensions: model.dimensions,
            token: artifact.token,
            checksum: artifact.checksum,
            count: artifact.count,
            bytes: artifact.bytes,
        };
        if !store.publish_vector_shard(&shard, Arc::clone(&cancel))? {
            return Err(
                crate::content::Error::Invalid("Vector source changed during publication").into(),
            );
        }
        progress.built += 1;
        report(&progress);
    }
    client.close();
    for shard in store.vector_catalog(Arc::clone(&cancel))? {
        if !visited.contains(&shard.after) {
            store.remove_vector_shard(&shard, Arc::clone(&cancel))?;
        }
    }
    progress.removed_files += cache::prune(
        database,
        &store.vector_catalog(Arc::clone(&cancel))?,
        &cancel,
    )?;
    check(&cancel)?;
    report(&progress);
    Ok(progress)
}

fn persist(
    store: &mut ContentStore,
    documents: &[EmbeddingDocument],
    model: &Model,
    key: &str,
    batch: Batch,
    cancel: &AtomicBool,
    progress: &mut Progress,
) -> Result<(), Error> {
    if &batch.model != model || batch.vectors.len() != documents.len() {
        return Err(Error::Worker(Failure::InvalidResponse));
    }
    for (document, vector) in documents.iter().zip(batch.vectors) {
        check(cancel)?;
        if store.put_embedding(document.id, document.revision, key, &vector)? {
            progress.written += 1;
        } else {
            progress.stale += 1;
        }
    }
    Ok(())
}

/// Stores one batch of passage vectors. A chunk rewritten while the model was working is counted
/// `stale`, not failed: its replacement is already waiting in a later page.
fn persist_chunks(
    store: &mut ContentStore,
    chunks: &[PendingChunk],
    model: &Model,
    key: &str,
    batch: Batch,
    cancel: &AtomicBool,
    progress: &mut Progress,
) -> Result<(), Error> {
    if &batch.model != model || batch.vectors.len() != chunks.len() {
        return Err(Error::Worker(Failure::InvalidResponse));
    }
    for (chunk, vector) in chunks.iter().zip(batch.vectors) {
        check(cancel)?;
        if store.put_chunk_embedding(chunk.id, key, &vector)? {
            progress.written += 1;
        } else {
            progress.stale += 1;
        }
    }
    Ok(())
}

fn check(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Acquire) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod pass_tests {
    use super::*;
    use crate::content::{Document, Extraction};
    use std::io::{BufRead, Write};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicUsize;

    /// A loopback stand-in for Ollama that answers every request, so a whole pass can run without
    /// a model installed. Replies with one unit vector per input, counting the requests it served.
    fn serve(dimensions: usize, refuse: bool) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let host = listener.local_addr().expect("an address").to_string();
        let served = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&served);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let mut reader = std::io::BufReader::new(stream.try_clone().expect("a clone"));
                let mut request = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    request.push_str(&line);
                }
                let length: usize = request
                    .lines()
                    .find_map(|l| l.strip_prefix("Content-Length: "))
                    .and_then(|n| n.trim().parse().ok())
                    .unwrap_or(0);
                let mut body = vec![0u8; length];
                let _ = std::io::Read::read_exact(&mut reader, &mut body);
                let body = String::from_utf8_lossy(&body);
                // Count the elements of "input":[…] itself. Counting separators anywhere in the
                // body would see the comma in {"model":"m","input":…} and reply with one vector
                // too many, which the client rightly refuses as a count mismatch.
                let inputs = body
                    .split_once("\"input\":[")
                    .and_then(|(_, rest)| rest.split_once(']'))
                    .map_or(0, |(array, _)| array.matches('"').count() / 2);
                let count = counter.fetch_add(1, Ordering::Relaxed);
                // The first request is the probe; `refuse` then rejects only multi-text batches,
                // so the pass must fall back to embedding one passage at a time.
                let reply = if refuse && count > 0 && inputs > 1 {
                    "{\"error\":\"batch too large\"}".to_owned()
                } else {
                    let mut vector = vec!["0.0"; dimensions];
                    vector[0] = "1.0";
                    let one = format!("[{}]", vector.join(","));
                    format!(
                        "{{\"embeddings\":[{}]}}",
                        std::iter::repeat_n(one.as_str(), inputs)
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                };
                let mut stream = stream;
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                        reply.len()
                    )
                    .as_bytes(),
                );
            }
        });
        (host, served)
    }

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new(name: &str) -> Self {
            Self(
                std::env::temp_dir()
                    .join(format!("blindspot-pass-{}-{name}", std::process::id()))
                    .join("content.sqlite"),
            )
        }
        fn open(&self) -> ContentStore {
            ContentStore::open(&self.0).expect("open")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Some(parent) = self.0.parent() {
                let _ = std::fs::remove_dir_all(parent);
            }
        }
    }

    fn fill(store: &mut ContentStore, count: usize) {
        let scan = store.begin_scan(Path::new("/fixture")).expect("scan");
        let paths: Vec<_> = (0..count).map(|i| format!("/fixture/{i}.md")).collect();
        let documents: Vec<_> = paths
            .iter()
            .map(|path| Document {
                identity: path,
                path: Path::new(path),
                title: path,
                body: "a passage worth embedding",
                modified_ns: 1,
                changed_ns: 1,
                bytes: 25,
                extraction: Extraction::Text,
            })
            .collect();
        store.put_batch(&scan, &documents).expect("put");
    }

    /// How many passages still lack a vector, read through the page the pass itself uses rather
    /// than by reaching into the store's connection.
    ///
    /// The key must be the one the pass wrote under — `model_key`'s identifier/revision/dimensions
    /// tuple, not the bare model name — or every passage would look pending and the assertions
    /// would hold for the wrong reason.
    fn awaiting(store: &ContentStore, identifier: &str, dimensions: usize) -> usize {
        let key = model_key(&Model {
            identifier: identifier.to_owned(),
            revision: 1,
            dimensions,
        })
        .expect("a key");
        let mut after = 0;
        let mut total = 0;
        loop {
            let page = store
                .chunk_embedding_page(after, &key, dimensions, Arc::new(AtomicBool::new(false)))
                .expect("page");
            if page.scanned == 0 {
                return total;
            }
            after = page.after;
            total += page.pending.len();
        }
    }

    #[test]
    fn a_pass_embeds_every_eligible_passage_and_resumes_without_repeating_work() {
        let fixture = Fixture::new("embeds");
        let mut store = fixture.open();
        fill(&mut store, 40);
        let (host, served) = serve(4, false);
        let mut embedder = Embedder::Ollama(crate::semantic::ollama::Client::new(
            host,
            "embeddinggemma:300m".into(),
        ));
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = run_chunks(
            &mut store,
            &mut embedder,
            Arc::clone(&cancel),
            |_| true,
            |_| {},
        )
        .expect("a pass");
        assert_eq!(progress.written, 40, "{progress:?}");
        assert_eq!(progress.failed, 0);
        assert_eq!(awaiting(&store, "embeddinggemma:300m", 4), 0);
        let first = served.load(Ordering::Relaxed);

        // Everything already has a current vector, so a second pass sends no batches at all.
        let again = run_chunks(&mut store, &mut embedder, cancel, |_| true, |_| {}).expect("again");
        assert_eq!(again.written, 0);
        assert_eq!(again.current, 40, "{again:?}");
        assert_eq!(
            served.load(Ordering::Relaxed) - first,
            1,
            "only the probe should reach the model on a settled index"
        );
    }

    #[test]
    fn a_batch_the_model_refuses_is_retried_one_passage_at_a_time() {
        let fixture = Fixture::new("refused");
        let mut store = fixture.open();
        fill(&mut store, 5);
        let (host, _) = serve(4, true);
        let mut embedder = Embedder::Ollama(crate::semantic::ollama::Client::new(host, "m".into()));
        let progress = run_chunks(
            &mut store,
            &mut embedder,
            Arc::new(AtomicBool::new(false)),
            |_| true,
            |_| {},
        )
        .expect("a pass");
        assert_eq!(progress.written, 5, "one bad batch must not lose the rest");
        assert_eq!(awaiting(&store, "m", 4), 0);
    }

    #[test]
    fn passages_outside_the_chosen_folders_are_excluded_and_cancellation_stops_the_pass() {
        let fixture = Fixture::new("excluded");
        let mut store = fixture.open();
        fill(&mut store, 3);
        let (host, served) = serve(4, false);
        let mut embedder = Embedder::Ollama(crate::semantic::ollama::Client::new(host, "m".into()));
        let progress = run_chunks(
            &mut store,
            &mut embedder,
            Arc::new(AtomicBool::new(false)),
            |_| false,
            |_| {},
        )
        .expect("a pass");
        assert_eq!(progress.excluded, 3);
        assert_eq!(progress.written, 0);
        assert_eq!(awaiting(&store, "m", 4), 3, "nothing was embedded");
        assert_eq!(
            served.load(Ordering::Relaxed),
            1,
            "an excluded passage must not reach the model"
        );

        assert!(matches!(
            run_chunks(
                &mut store,
                &mut embedder,
                Arc::new(AtomicBool::new(true)),
                |_| true,
                |_| {},
            ),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn each_backend_declares_the_batch_size_its_transport_accepts() {
        // Apple's helper refuses more than eight texts; handing it Ollama's thirty-two would make
        // every batch fail as InvalidInput and fall back to one-at-a-time embedding.
        let apple = Embedder::Apple(Client::new(std::path::PathBuf::from("/nonexistent")));
        assert_eq!(apple.batch(), 8);
        let ollama = Embedder::Ollama(crate::semantic::ollama::Client::new(
            "127.0.0.1:1".into(),
            "m".into(),
        ));
        assert_eq!(ollama.batch(), 32);
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use std::io::{Seek, Write};

    #[test]
    #[ignore = "requires the built native vector helper and native descriptor access"]
    fn native_cache_recovers_corruption_reuses_commits_and_cleans_cancelled_work() {
        let root =
            std::env::temp_dir().join(format!("blindspot-cache-pass-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let database = root.join("content.sqlite");
        let worker = std::path::PathBuf::from(
            std::env::var_os("BLINDSPOT_VECTOR_WORKER").expect("helper path"),
        );
        let model = Model {
            identifier: "fixture".into(),
            revision: 1,
            dimensions: 2,
        };
        let key = model_key(&model).unwrap();
        let mut store = ContentStore::open(&database).unwrap();
        let scan = store.begin_scan(Path::new("/fixture")).unwrap();
        let paths: Vec<_> = (0..130).map(|i| format!("/fixture/{i}.txt")).collect();
        let docs: Vec<_> = paths
            .iter()
            .map(|path| crate::content::Document {
                identity: path,
                path: Path::new(path),
                title: "fixture",
                body: "fixture",
                modified_ns: 1,
                changed_ns: 1,
                bytes: 7,
                extraction: crate::content::Extraction::Text,
            })
            .collect();
        let ids = store.put_batch(&scan, &docs).unwrap();
        for (id, revision) in &ids {
            store
                .put_embedding(*id, *revision, &key, &[1.0, 0.0])
                .unwrap();
        }
        let cancel = Arc::new(AtomicBool::new(false));
        assert!(matches!(
            maintain_cache(
                &store,
                &database,
                &worker,
                &model,
                Arc::clone(&cancel),
                |progress| {
                    if progress.vectors_added >= 64 {
                        cancel.store(true, Ordering::Release);
                    }
                }
            ),
            Err(Error::Cancelled)
        ));
        assert!(
            store
                .vector_catalog(Arc::new(AtomicBool::new(false)))
                .unwrap()
                .is_empty()
        );
        cancel.store(false, Ordering::Release);
        let first = maintain_cache(
            &store,
            &database,
            &worker,
            &model,
            Arc::clone(&cancel),
            |_| {},
        )
        .unwrap();
        assert_eq!(
            (first.built, first.reused, first.vectors_added),
            (1, 0, 130)
        );
        drop(store);
        let store = ContentStore::open(&database).unwrap();
        let second = maintain_cache(
            &store,
            &database,
            &worker,
            &model,
            Arc::clone(&cancel),
            |_| {},
        )
        .unwrap();
        assert_eq!(
            (second.built, second.reused, second.vectors_added),
            (0, 1, 0)
        );
        let original = store.vector_catalog(Arc::clone(&cancel)).unwrap().remove(0);
        let directory = super::super::cache::prepare(&database).unwrap();
        let path = directory.join(format!("{}.ann", original.token));
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(std::io::SeekFrom::Start(0)).unwrap();
        file.write_all(b"broken!!").unwrap();
        file.sync_all().unwrap();
        drop(file);
        let repaired = maintain_cache(
            &store,
            &database,
            &worker,
            &model,
            Arc::clone(&cancel),
            |_| {},
        )
        .unwrap();
        assert_eq!((repaired.built, repaired.reused), (1, 0));
        assert!(!path.exists());
        assert_ne!(
            store.vector_catalog(Arc::clone(&cancel)).unwrap()[0].token,
            original.token
        );
        let mut store = store;
        store
            .put_embedding(ids[0].0, ids[0].1, &key, &[0.0, 1.0])
            .unwrap();
        let updated = maintain_cache(
            &store,
            &database,
            &worker,
            &model,
            Arc::clone(&cancel),
            |_| {},
        )
        .unwrap();
        assert_eq!((updated.built, updated.reused), (1, 0));
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        store.erase(Arc::clone(&cancel), |_| {}).unwrap();
        super::super::cache::prune(&database, &[], &cancel).unwrap();
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
