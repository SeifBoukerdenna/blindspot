use super::*;
use std::time::{Duration, Instant};
use serde::Serialize;

#[derive(Clone, Default, PartialEq, Eq)]
pub(super) enum Work {
    #[default]
    Reconcile,
    Folder(PathBuf),
    RetryFailures,
    SemanticOnly,
}

#[derive(Debug, Clone, Default)]
pub struct Recovery {
    pub needed: bool,
    pub attempts: u32,
    pub next: Option<Instant>,
    pub health: Option<Health>,
}

impl Recovery {
    pub(super) fn defer(&mut self) {
        self.needed = true;
        self.health = None;
        self.next = Some(Instant::now() + backoff(self.attempts));
        self.attempts = self.attempts.saturating_add(1);
    }
    pub(super) fn clear(&mut self) {
        self.needed = false;
        self.next = None;
        self.attempts = 0;
    }
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(30u64.saturating_mul(1u64 << attempt.min(5)).min(900))
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct HealthRequest {
    serial: u64,
    host: String,
    name: String,
    helper: Option<PathBuf>,
    database: Option<PathBuf>,
    automatic: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub available: bool,
    pub model: String,
    pub dimensions: Option<usize>,
    pub milliseconds: u128,
    pub message: String,
}

impl ContentService {
    pub fn index_action(&self, action: &str, folder: &str) -> Result<(), &'static str> {
        if action == "check" { return self.check_model(false); }
        if action == "tick" { self.recovery_tick(); return Ok(()); }
        let mut control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
        if self.pending_maintenance(&control) { return Err("Wait for index maintenance to finish"); }
        if !control.config.enabled { return Err("Enable indexing in Content settings first"); }
        match action {
            "pause" | "resume" => {
                let paused = action == "pause";
                if paused == control.manual_pause { return Ok(()); }
                control.manual_pause = paused;
                self.health.cancel();
                control.health_request = None;
                self.queue(&mut control, true, None);
            }
            "retry" | "folder" => {
                let snapshot = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
                if control.paused || control.manual_pause { return Err("Resume indexing and clear the power or thermal pause first"); }
                if snapshot.phase == Phase::Indexing { return Err("Wait for the current pass to finish"); }
                let work = if action == "folder" {
                    let path = PathBuf::from(folder);
                    if !snapshot.roots.contains(&path) { return Err("Choose a watched folder from the Index page"); }
                    Work::Folder(path)
                } else { Work::RetryFailures };
                drop(snapshot);
                control.work = work;
                self.snapshot.lock().unwrap_or_else(PoisonError::into_inner).semantic_clean = false;
                self.queue(&mut control, true, None);
            }
            _ => return Err("Unknown indexing action"),
        }
        Ok(())
    }

    fn check_model(&self, automatic: bool) -> Result<(), &'static str> {
        let mut control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
        if self.pending_maintenance(&control) { return Err("Wait for index maintenance to finish"); }
        if control.health_request.is_some() { return Ok(()); }
        control.health_serial = control.health_serial.wrapping_add(1);
        let request = HealthRequest {
            serial: control.health_serial, host: control.config.embedding_host.clone(),
            name: control.config.embedding_model.clone(), helper: self.helpers.as_ref().map(|h| h.embedding.clone()),
            database: self.path.clone(), automatic,
        };
        control.health_request = Some(request.clone());
        self.health.search_shared(request, probe);
        Ok(())
    }

    pub fn recovery_tick(&self) {
        let mut control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(request) = control.health_request.clone() {
            let (result, pending) = self.health.results(&request);
            if !pending {
                control.health_request = None;
                let mut snapshot = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(Ok(health)) = result {
                    let recovered = health.available && snapshot.recovery.needed;
                    if request.automatic && !health.available { snapshot.recovery.defer(); }
                    snapshot.recovery.health = Some(health);
                    if recovered { snapshot.recovery.next = Some(Instant::now()); }
                } else if request.automatic { snapshot.recovery.defer(); }
            }
        }
        let snapshot = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
        let idle = matches!(snapshot.phase, Phase::Ready | Phase::Partial | Phase::Failed);
        let due = snapshot.recovery.needed && snapshot.recovery.next.is_none_or(|at| Instant::now() >= at);
        if !control.config.enabled || !control.config.semantic || control.manual_pause || control.paused
            || !idle || !due || control.health_request.is_some() || self.pending_maintenance(&control) { return; }
        let available = snapshot.recovery.health.as_ref().is_some_and(|health| health.available);
        drop(snapshot);
        if available {
            control.work = Work::SemanticOnly;
            {
                let mut snapshot = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
                snapshot.semantic_clean = false;
            }
            self.queue(&mut control, true, None);
        } else {
            drop(control);
            let _ = self.check_model(true);
        }
    }

    pub fn controls_state(&self) -> serde_json::Value {
        let control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
        let snapshot = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
        let progress = snapshot.semantic_progress.as_ref();
        let done = progress.map_or(0, |p| p.written + p.failed + p.stale);
        let total = progress.and_then(|p| p.pending_total);
        serde_json::json!({
            "manualPause": control.manual_pause,
            "policyPause": control.paused,
            "canPause": control.config.enabled && !matches!(snapshot.phase, Phase::Erasing | Phase::Erased | Phase::Compacting | Phase::NeedsRoots),
            "canRun": control.config.enabled && !control.manual_pause && !control.paused && matches!(snapshot.phase, Phase::Ready | Phase::Partial | Phase::Failed),
            "checking": control.health_request.is_some(),
            "health": snapshot.recovery.health,
            "retrySeconds": snapshot.recovery.next.map(|at| at.saturating_duration_since(Instant::now()).as_secs()),
            "recoveryNeeded": snapshot.recovery.needed,
            "embeddingTotal": total, "embeddingDone": done,
            "embeddingRemaining": total.map(|n| n.saturating_sub(done)),
            "roots": snapshot.roots.iter().map(|path| path.to_string_lossy()).collect::<Vec<_>>()
        })
    }
}

fn probe(request: &HealthRequest, cancel: Arc<AtomicBool>) -> Result<Health, Failure> {
    crate::ffi::index_native::background_priority();
    let started = Instant::now();
    let mut client = if request.name.trim().is_empty() {
        semantic::Embedder::Apple(semantic::Client::new(request.helper.clone().unwrap_or_default()))
    } else { semantic::Embedder::Ollama(semantic::ollama::Client::new(request.host.clone(), request.name.clone())) };
    let result = client.probe(&cancel);
    client.close();
    if cancel.load(Ordering::Acquire) { return Err(Failure::Cancelled); }
    let mut health = Health { available: false, model: if request.name.is_empty() { semantic::MODEL_IDENTIFIER.into() } else {request.name.clone()}, dimensions: None, milliseconds: started.elapsed().as_millis(), message: String::new() };
    health.message = match result {
        Ok(model) => {
            let Ok(key) = semantic::indexing::model_key(&model) else {
                health.message = "Unsupported model identity or dimensions".into(); return Ok(health);
            };
            health.available = true;
            health.dimensions = Some(model.dimensions);
            let models = request.database.as_ref().and_then(|path| {
                if !path.exists() { return Some(Vec::new()); }
                ContentStore::open_reader(path).ok().and_then(|store| store.passage_models().ok())
            });
            if models.as_ref().is_some_and(|models| models.iter().any(|(existing, width)| existing == &key && *width == model.dimensions)) {
                "Available · matches active model name, revision and dimensions".into()
            } else if models.is_some() {
                "Available · supported; no matching active generation yet. Existing results are retained while embeddings are prepared".into()
            } else { "Available · supported dimensions; index compatibility could not be checked".into() }
        }
        Err(semantic::Failure::ModelUnavailable) => "Model not installed or unavailable. Choose an installed embedding model; Blindspot will not download one".into(),
        Err(semantic::Failure::InvalidResponse | semantic::Failure::InvalidInput) => "Incompatible embedding response or unsupported dimensions".into(),
        Err(semantic::Failure::TimedOut) => "Model check timed out; existing word search remains available".into(),
        Err(_) => "Local embedding service unavailable or request rejected. Start Ollama and check the selected embedding model".into(),
    };
    Ok(health)
}

pub(super) fn retry_extraction(request: &IndexRequest, store: &mut ContentStore, roots: &[PathBuf], policy: &Policy, cancel: &Arc<AtomicBool>) -> Result<Progress, Failure> {
    use rusqlite::OpenFlags;
    let reader = rusqlite::Connection::open_with_flags(&request.path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW).map_err(|_| Failure::Unavailable)?;
    let cancelled = Arc::clone(cancel);
    reader.progress_handler(1000, Some(move || cancelled.load(Ordering::Acquire))).map_err(|_| Failure::Unavailable)?;
    let mut after = 0i64;
    let mut total = Progress { complete: true, ..Progress::default() };
    let policy = Policy { force_reload: true, ..policy.clone() };
    loop {
        if cancel.load(Ordering::Acquire) { return Err(Failure::Cancelled); }
        let mut statement = reader.prepare("SELECT id,substr(path,1,4097) FROM documents WHERE id>?1 AND (extraction>=2 OR partial=1) ORDER BY id LIMIT 128").map_err(|_| Failure::Unavailable)?;
        let rows = statement.query_map([after], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).map_err(|_| Failure::Unavailable)?;
        let page = rows.collect::<Result<Vec<_>, _>>().map_err(|_| Failure::Unavailable)?;
        if page.is_empty() { break; }
        after = page.last().unwrap().0;
        drop(statement);
        for root in roots {
            let paths: Vec<PathBuf> = page.iter().map(|(_, p)| PathBuf::from(p)).filter(|path|
                path.as_os_str().len() <= 4096 && path != root && path.starts_with(root) && within_scope(path, roots, &policy)).collect();
            if paths.is_empty() { continue; }
            publish(request, cancel, |snapshot| snapshot.current_path = Some(root.clone()));
            let progress = content_indexer::scan_paths(store, root, &paths, &policy, cancel, |progress| {
                let mut combined = total.clone(); add(&mut combined, progress);
                publish(request, cancel, |snapshot| snapshot.progress = combined);
            }).map_err(|e| if matches!(e, crate::content::Error::Cancelled) {Failure::Cancelled} else {Failure::Unavailable})?;
            total.complete &= progress.complete;
            add(&mut total, &progress);
            if total.budget_exhausted { return Ok(total); }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tests::wait;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("blindspot-controls-{}-{name}", std::process::id()));
            std::fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
        fn service(&self) -> ContentService {
            for name in ["first", "second"] {
                std::fs::create_dir(self.0.join(name)).unwrap();
                std::fs::write(self.0.join(name).join("notes.md"), name).unwrap();
            }
            let service = ContentService::new(Some(self.0.join("content.sqlite")));
            service.set_paused(false, 0);
            service.configure(&Content { enabled: true, roots: ["first", "second"].iter().map(|p|self.0.join(p).to_string_lossy().into()).collect(), ..Content::default() });
            wait(&service);
            service
        }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

    struct Server {
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Server {
        fn start(host: &str) -> Self {
            use std::io::{BufRead, Read, Write};
            let listener = std::net::TcpListener::bind(host).unwrap();
            listener.set_nonblocking(true).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let cancelled = Arc::clone(&stop);
            let thread = std::thread::spawn(move || {
                while !cancelled.load(Ordering::Acquire) {
                    let Ok((mut stream, _)) = listener.accept() else { std::thread::sleep(Duration::from_millis(5)); continue; };
                    stream.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                    let mut length = 0;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" { break; }
                        if let Some(value) = line.strip_prefix("Content-Length: ") { length = value.trim().parse().unwrap(); }
                    }
                    let mut body = vec![0; length];
                    if reader.read_exact(&mut body).is_err() { continue; }
                    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let count = value["input"].as_array().unwrap().len();
                    let reply = serde_json::json!({"embeddings": vec![vec![1.0,0.0,0.0,0.0]; count]}).to_string();
                    let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len());
                }
            });
            Self {stop, thread:Some(thread)}
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            self.thread.take().unwrap().join().unwrap();
        }
    }
    fn unused_host() -> String {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string()
    }

    #[test]
    fn model_check_reports_unavailable_then_dimensions_without_writing_an_index() {
        let fixture = Fixture::new("health");
        let host = unused_host();
        let request = HealthRequest {serial:1, host:host.clone(), name:"fixture-model".into(), helper:None,
            database:Some(fixture.0.join("absent.sqlite")), automatic:false};
        assert!(!probe(&request, Arc::new(AtomicBool::new(false))).unwrap().available);
        let _server = Server::start(&host);
        let health = probe(&request, Arc::new(AtomicBool::new(false))).unwrap();
        assert!(health.available);
        assert_eq!(health.dimensions, Some(4));
        assert!(health.message.contains("no matching active generation"));
        assert!(!fixture.0.join("absent.sqlite").exists());
        assert!(matches!(probe(&request, Arc::new(AtomicBool::new(true))), Err(Failure::Cancelled)));
    }

    #[test]
    #[ignore = "requires the built native vector helper"]
    fn native_recovery_when_model_starts_is_embedding_only_and_keeps_word_search() {
        let fixture = Fixture::new("native-recovery");
        let initial = fixture.service();
        drop(initial);
        let host = unused_host();
        let service = ContentService::with_helpers(Some(fixture.0.join("content.sqlite")), Some(Helpers {
            embedding:fixture.0.join("absent-helper"), vectors:std::env::var_os("BLINDSPOT_VECTOR_WORKER").unwrap().into(),
        }));
        service.set_paused(false, 0);
        service.configure(&Content {enabled:true,semantic:true,embedding_host:host.clone(),embedding_model:"fixture-model".into(),
            roots:["first","second"].iter().map(|p|fixture.0.join(p).to_string_lossy().into()).collect(),..Content::default()});
        wait(&service);
        assert!(service.snapshot().recovery.needed);
        assert!(service.retriever().is_some());
        std::fs::write(fixture.0.join("first/not-scanned.md"), "new document").unwrap();
        let _server = Server::start(&host);
        service.snapshot.lock().unwrap().recovery.next = Some(Instant::now());
        let started = Instant::now();
        loop {
            service.recovery_tick();
            if !service.snapshot().recovery.needed && service.snapshot().phase == Phase::Ready { break; }
            assert!(started.elapsed() < Duration::from_secs(10), "recovery did not finish: {}", service.status());
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(service.snapshot().progress.visited, 0);
        let reader = ContentStore::open_reader(&fixture.0.join("content.sqlite")).unwrap();
        assert_eq!(reader.count().unwrap(), 2, "recovery must not scan the new document");
        assert_eq!(reader.passage_models().unwrap().len(), 1);
        assert_eq!(service.snapshot().semantic_progress.unwrap().written, 2);
        assert!(service.snapshot().recovery.health.unwrap().available, "Recovery must retain the model-check result");
    }

    #[test]
    fn manual_pause_survives_policy_changes_and_resume_obeys_protection() {
        let fixture = Fixture::new("pause");
        let service = fixture.service();
        service.index_action("pause", "").unwrap();
        service.set_paused(true, 1);
        service.set_paused(false, 0);
        assert_eq!(service.snapshot().pause_reason, PauseReason::Manual);
        service.refresh();
        assert_eq!(service.snapshot().phase, Phase::Paused);
        assert!(service.retriever().is_some());
        service.set_paused(true, 2);
        service.index_action("resume", "").unwrap();
        assert_eq!(service.snapshot().phase, Phase::Paused);
        assert_eq!(service.snapshot().pause_reason, PauseReason::Thermal);
        service.set_paused(false, 0);
        wait(&service);
        assert_eq!(service.snapshot().phase, Phase::Ready);
    }

    #[test]
    fn folder_rescan_never_scans_or_removes_other_roots() {
        let fixture = Fixture::new("folder");
        let service = fixture.service();
        std::fs::write(fixture.0.join("first/notes.md"), "newfirst").unwrap();
        std::fs::write(fixture.0.join("second/notes.md"), "newsecond").unwrap();
        assert!(service.index_action("folder", "/outside").is_err());
        service.index_action("folder", fixture.0.join("first").to_str().unwrap()).unwrap();
        wait(&service);
        let store = ContentStore::open_reader(&fixture.0.join("content.sqlite")).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        assert_eq!(store.count().unwrap(), 2);
        assert_eq!(store.search("newfirst", 5, Arc::clone(&cancel)).unwrap().hits.len(), 1);
        assert!(store.search("newsecond", 5, cancel).unwrap().hits.is_empty());
        assert_eq!(service.snapshot().progress.indexed, 1);
    }

    #[test]
    fn retry_only_reloads_recorded_problem_files() {
        let fixture = Fixture::new("retry");
        let service = fixture.service();
        let connection = rusqlite::Connection::open(fixture.0.join("content.sqlite")).unwrap();
        connection.execute("UPDATE documents SET partial=1 WHERE path=?1", [fixture.0.join("first/notes.md").to_str().unwrap()]).unwrap();
        drop(connection);
        service.index_action("retry", "").unwrap();
        wait(&service);
        assert_eq!(service.snapshot().progress.visited, 1);
        assert_eq!(service.snapshot().progress.indexed, 1);
        assert_eq!(ContentStore::open_reader(&fixture.0.join("content.sqlite")).unwrap().count().unwrap(), 2);
    }

    #[test]
    fn recovery_is_backed_off_and_never_overrides_pause_or_disable() {
        assert_eq!((0..7).map(|n|backoff(n).as_secs()).collect::<Vec<_>>(), [30,60,120,240,480,900,900]);
        let fixture = Fixture::new("recovery");
        let service = fixture.service();
        service.index_action("pause", "").unwrap();
        {
            service.control.lock().unwrap().config.semantic = true;
            let mut snapshot = service.snapshot.lock().unwrap();
            snapshot.recovery.needed = true;
            snapshot.recovery.next = Some(Instant::now());
            snapshot.recovery.health = Some(Health {available:true,model:"m".into(),dimensions:Some(4),milliseconds:1,message:"ready".into()});
        }
        service.recovery_tick();
        assert!(service.control.lock().unwrap().health_request.is_none());
        assert_eq!(service.snapshot().phase, Phase::Paused);
        {
            let mut control = service.control.lock().unwrap();
            control.manual_pause = false;
            control.config.enabled = false;
            service.snapshot.lock().unwrap().phase = Phase::Ready;
        }
        service.recovery_tick();
        assert!(service.control.lock().unwrap().work == Work::Reconcile);
    }
}
