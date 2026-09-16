//! Owns the opt-in indexing and query lifetimes; all disk access stays on workers.

pub mod passage_engine;
pub mod controls;
pub mod inspection;
use controls::{Work, HealthRequest, Health, Recovery};

use crate::{
    config::Content,
    content::{ContentStore, SearchPage},
    content_indexer::{self, Policy, Progress},
    process_job::{Failure, Latest},
};
use crate::semantic::{self, search::Helpers};
use passage_engine::Engine;
use crate::content::passage_search::{Match as PassageMatch, fuse as fuse_passages};

type SharedEngine = Arc<Mutex<Option<Engine>>>;

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Disabled,
    NeedsRoots,
    Paused,
    Indexing,
    Ready,
    Partial,
    Failed,
    Erasing,
    Erased,
    EraseFailed,
    Compacting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseReason {
    None,
    LowPower,
    Thermal,
    Battery,
    Unavailable,
    Manual,
}

impl PauseReason {
    fn from_raw(value: u8) -> Self {
        match value { 1 => Self::LowPower, 2 => Self::Thermal, 3 => Self::Battery, 4 => Self::Unavailable, _ => Self::None }
    }

    fn label(self) -> &'static str {
        match self { Self::None => "", Self::LowPower => "Low Power Mode", Self::Thermal => "Thermal protection", Self::Battery => "Battery power", Self::Unavailable => "Power state unavailable", Self::Manual => "Paused by you" }
    }
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub phase: Phase,
    pub progress: Progress,
    pub semantic_status: String,
    pub pause_reason: PauseReason,
    pub current_path: Option<PathBuf>,
    /// What the running pass is doing, for the Settings dashboard rather than control flow.
    pub stage: &'static str,
    pub pass_started: Option<std::time::SystemTime>,
    /// Wall-clock end and duration of the last pass that ran to completion.
    pub last_pass: Option<(std::time::SystemTime, std::time::Duration)>,
    pub semantic_progress: Option<semantic::indexing::Progress>,
    pub recovery: Recovery,
    pub last_compact: Option<crate::content::CompactReport>,
    pub compact_error: Option<&'static str>,
    semantic_enabled: bool,
    semantic_revision: u64,
    /// The last semantic pass finished; cleared by configuration changes and manual refresh.
    semantic_clean: bool,
    epoch: u64,
    ready: bool,
    roots: Vec<PathBuf>,
    exclusions: Vec<PathBuf>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            phase: Phase::Disabled,
            progress: Progress::default(),
            semantic_status: String::new(),
            pause_reason: PauseReason::None,
            current_path: None,
            stage: "",
            pass_started: None,
            last_pass: None,
            semantic_progress: None,
            recovery: Recovery::default(),
            last_compact: None,
            compact_error: None,
            semantic_enabled: false,
            semantic_revision: 0,
            semantic_clean: false,
            epoch: 0,
            ready: false,
            roots: Vec::new(),
            exclusions: Vec::new(),
        }
    }
}

#[derive(Clone)]
struct IndexRequest {
    work: Work,
    epoch: u64,
    erase: bool,
    compact: bool,
    stop: bool,
    changed_paths: Option<Vec<PathBuf>>,
    helpers: Option<Helpers>,
    query_engine: SharedEngine,
    path: PathBuf,
    config: Content,
    snapshot: Arc<Mutex<Snapshot>>,
}
impl PartialEq for IndexRequest {
    fn eq(&self, other: &Self) -> bool {
        self.epoch == other.epoch
    }
}
impl Eq for IndexRequest {}

#[derive(Clone, PartialEq, Eq)]
struct SearchRequest {
    epoch: u64,
    settled: bool,
    path: PathBuf,
    query: String,
    filter: crate::content::SearchFilter,
    roots: Vec<PathBuf>,
    exclusions: Vec<PathBuf>,
}

#[derive(Clone)]
struct SemanticRequest {
    search: SearchRequest,
    revision: u64,
    engine: SharedEngine,
}
impl PartialEq for SemanticRequest {
    fn eq(&self,other:&Self)->bool { self.search==other.search && self.revision==other.revision }
}
impl Eq for SemanticRequest {}

struct Control {
    manual_pause: bool,
    work: Work,
    health_request: Option<HealthRequest>,
    health_serial: u64,
    config: Content,
    epoch: u64,
    paused: bool,
    pause_reason: PauseReason,
    configured: bool,
    request: Option<IndexRequest>,
}

pub struct ContentService {
    health: Latest<HealthRequest, Health>,
    path: Option<PathBuf>,
    control: Mutex<Control>,
    snapshot: Arc<Mutex<Snapshot>>,
    indexing: Latest<IndexRequest, ()>,
    searching: Latest<SearchRequest, Vec<PassageMatch>>,
    semantic_searching: Latest<SemanticRequest, Vec<PassageMatch>>,
    helpers: Option<Helpers>,
    query_engine: SharedEngine,
    activity: Mutex<std::collections::VecDeque<(std::time::Instant, PathBuf)>>,
}

const ACTIVITY_WINDOW: std::time::Duration = std::time::Duration::from_secs(300);

impl ContentService {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self::with_helpers(path,None)
    }

    pub fn with_helpers(path: Option<PathBuf>, helpers: Option<Helpers>) -> Self {
        let query_engine=Arc::new(Mutex::new(helpers.clone().map(|helpers| Engine::new(helpers, Content::default().embedding_host))));
        Self {
            health: Latest::default(),
            path,
            control: Mutex::new(Control {
                manual_pause: false,
                work: Work::Reconcile,
                health_request: None,
                health_serial: 0,
                config: Content::default(),
                epoch: 0,
                paused: true,
                pause_reason: PauseReason::Unavailable,
                configured: false,
                request: None,
            }),
            snapshot: Arc::new(Mutex::new(Snapshot::default())),
            indexing: Latest::default(),
            searching: Latest::default(),
            semantic_searching: Latest::default(),
            helpers,
            query_engine,
            activity: Mutex::new(std::collections::VecDeque::new()),
        }
    }

    pub fn configure(&self, config: &Content) {
        let mut control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
        if (control.config == *config && control.configured) || self.pending_maintenance(&control) {
            return;
        }
        control.config = config.clone();
        control.configured = true;
        control.work = Work::Reconcile;
        control.health_request = None;
        self.health.cancel();
        self.queue(&mut control, false, None);
    }

    pub fn set_paused(&self, paused: bool, reason: u8) {
        let mut control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
        let reason = PauseReason::from_raw(reason);
        if control.paused == paused && control.pause_reason == reason {
            return;
        }
        control.paused = paused;
        control.pause_reason = if paused { reason } else { PauseReason::None };
        if paused {
            self.health.cancel();
            control.health_request = None;
        }
        self.queue(&mut control, true, None);
    }

    pub fn refresh(&self) {
        let mut control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
        control.work = Work::Reconcile;
        self.snapshot.lock().unwrap_or_else(PoisonError::into_inner).semantic_clean = false;
        self.queue(&mut control, true, None);
    }

    pub fn refresh_paths(&self, paths: Vec<PathBuf>) {
        let mut control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
        control.work = Work::Reconcile;
        {
            let mut activity = self.activity.lock().unwrap_or_else(PoisonError::into_inner);
            let now = std::time::Instant::now();
            for path in paths.iter().take(128) {
                if let Some(parent) = path.parent() { activity.push_back((now, parent.to_path_buf())); }
            }
            while activity.len() > 2048 || activity.front().is_some_and(|(at, _)| now.duration_since(*at) > ACTIVITY_WINDOW) {
                activity.pop_front();
            }
        }
        let ready = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner).phase == Phase::Ready;
        let selective = ready && !paths.is_empty() && paths.len() <= 128 && paths.iter().all(|path|
            path.is_absolute() && !path.components().any(|c| matches!(c, std::path::Component::ParentDir)));
        self.queue(&mut control, true, selective.then_some(paths));
    }

    fn queue(&self, control: &mut Control, retain: bool, changed_paths: Option<Vec<PathBuf>>) {
        if self.pending_maintenance(control) { return; }
        self.indexing.cancel();
        self.searching.cancel();
        self.semantic_searching.cancel();
        control.epoch = control.epoch.wrapping_add(1);
        control.request = None;
        let mut snapshot = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
        if !retain {
            let (last_pass, last_compact) = (snapshot.last_pass, snapshot.last_compact);
            *snapshot = Snapshot::default();
            (snapshot.last_pass, snapshot.last_compact) = (last_pass, last_compact);
        }
        snapshot.epoch = control.epoch;
        snapshot.progress = Progress::default();
        snapshot.semantic_enabled=control.config.semantic && control.config.enabled;
        snapshot.semantic_revision=0;
        snapshot.semantic_status=if snapshot.semantic_enabled {"Semantic indexing queued".into()} else {String::new()};
        snapshot.pause_reason = if control.manual_pause { PauseReason::Manual } else if control.paused { control.pause_reason } else { PauseReason::None };
        snapshot.current_path = None;
        snapshot.stage = "";
        snapshot.pass_started = None;
        snapshot.semantic_progress = None;
        snapshot.phase = if !control.config.enabled {
            Phase::Disabled
        } else if control.config.roots.is_empty() && control.config.code_roots.is_empty() {
            Phase::NeedsRoots
        } else if control.paused || control.manual_pause {
            Phase::Paused
        } else {
            Phase::Indexing
        };
        if matches!(snapshot.phase, Phase::Disabled | Phase::NeedsRoots) {
            snapshot.ready = false;
        }
        if snapshot.phase != Phase::Indexing {
            if !snapshot.semantic_enabled {
                let request=IndexRequest {work:Work::Reconcile,epoch:control.epoch,erase:false,compact:false,stop:true,changed_paths:None,
                    helpers:self.helpers.clone(),query_engine:Arc::clone(&self.query_engine),
                    path:self.path.clone().unwrap_or_default(),config:control.config.clone(),snapshot:Arc::clone(&self.snapshot)};
                control.request=Some(request.clone());
                drop(snapshot);
                self.indexing.search_shared(request,run_index);
            }
            return;
        }
        let Some(path) = &self.path else {
            snapshot.phase = Phase::Failed;
            snapshot.ready = false;
            return;
        };
        let request = IndexRequest {
            work: control.work.clone(),
            epoch: control.epoch,
            erase: false,
            compact: false,
            stop: false,
            changed_paths,
            helpers: self.helpers.clone(),
            query_engine: Arc::clone(&self.query_engine),
            path: path.clone(),
            config: control.config.clone(),
            snapshot: Arc::clone(&self.snapshot),
        };
        control.request = Some(request.clone());
        drop(snapshot);
        self.indexing.search_shared(request, run_index);
    }

    fn pending_erase(&self, control: &Control) -> bool {
        control.request.as_ref().is_some_and(|request| request.erase && self.indexing.results(request).1)
    }

    pub fn erasing(&self) -> bool {
        let control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
        self.pending_erase(&control)
    }

    pub fn erase(&self) -> Result<(), &'static str> {
        let mut control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
        if control.config.enabled { return Err("Disable content indexing before erasing its stored data"); }
        if self.pending_erase(&control) { return Ok(()); }
        if self.pending_maintenance(&control) { return Err("Wait for compaction to finish before erasing"); }
        let Some(path) = &self.path else { return Err("Content index location unavailable"); };
        self.indexing.cancel();
        self.searching.cancel();
        self.semantic_searching.cancel();
        control.epoch = control.epoch.wrapping_add(1);
        *self.snapshot.lock().unwrap_or_else(PoisonError::into_inner) = Snapshot {
            epoch: control.epoch, phase: Phase::Erasing, ..Snapshot::default()
        };
        let request = IndexRequest {
            work: Work::Reconcile,
            epoch: control.epoch, erase: true, compact: false, stop: false, changed_paths: None, path: path.clone(),
            helpers: self.helpers.clone(), query_engine: Arc::clone(&self.query_engine),
            config: control.config.clone(), snapshot: Arc::clone(&self.snapshot),
        };
        control.request = Some(request.clone());
        self.indexing.search_shared(request, run_erase);
        Ok(())
    }

    /// Starts a user-requested compaction: retired vectors removed, the word index merged and the
    /// database rewritten. Indexing and content search wait until it finishes.
    pub fn compact(&self) -> Result<(), &'static str> {
        let mut control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
        if self.pending_maintenance(&control) {
            return Err("Wait for the current erase or compaction to finish");
        }
        let Some(path) = &self.path else { return Err("Content index location unavailable") };
        if !path.exists() {
            return Err("There is no content index to compact yet");
        }
        self.indexing.cancel();
        self.searching.cancel();
        self.semantic_searching.cancel();
        control.epoch = control.epoch.wrapping_add(1);
        {
            let mut snapshot = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
            snapshot.epoch = control.epoch;
            snapshot.phase = Phase::Compacting;
            snapshot.stage = "Starting";
            snapshot.current_path = None;
            snapshot.compact_error = None;
        }
        let request = IndexRequest {
            work: Work::Reconcile,
            epoch: control.epoch, erase: false, compact: true, stop: false, changed_paths: None, path: path.clone(),
            helpers: self.helpers.clone(), query_engine: Arc::clone(&self.query_engine),
            config: control.config.clone(), snapshot: Arc::clone(&self.snapshot),
        };
        control.request = Some(request.clone());
        self.indexing.search_shared(request, run_compact);
        Ok(())
    }

    pub fn compacting(&self) -> bool {
        let control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
        control.request.as_ref().is_some_and(|request| request.compact && self.indexing.results(request).1)
    }

    fn pending_maintenance(&self, control: &Control) -> bool {
        control.request.as_ref().is_some_and(|request| (request.erase || request.compact) && self.indexing.results(request).1)
    }

    /// A handle for answering questions from indexed passages on another thread, when the index is usable.
    pub fn retriever(&self) -> Option<Retriever> {
        let snapshot = self.snapshot();
        let path = self.path.clone()?;
        if !snapshot.ready || matches!(snapshot.phase, Phase::Disabled | Phase::NeedsRoots | Phase::Erasing | Phase::Erased | Phase::Compacting) {
            return None;
        }
        Some(Retriever {
            path,
            roots: snapshot.roots,
            exclusions: snapshot.exclusions,
            engine: Arc::clone(&self.query_engine),
            semantic: snapshot.semantic_enabled,
        })
    }

    pub fn cancel_erase(&self) {
        let mut control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
        if !self.pending_erase(&control) { return; }
        self.indexing.cancel();
        control.request = None;
        control.epoch = control.epoch.wrapping_add(1);
        let mut snapshot = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
        snapshot.epoch = control.epoch;
        snapshot.phase = Phase::EraseFailed;
    }

    pub fn snapshot(&self) -> Snapshot {
        let control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
        let failed = control
            .request
            .as_ref()
            .is_some_and(|request| matches!(self.indexing.results(request).0, Some(Err(_))));
        let mut snapshot = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
        if failed && snapshot.phase == Phase::Indexing {
            snapshot.phase = Phase::Failed;
            snapshot.current_path = None;
        } else if failed && snapshot.phase == Phase::Erasing {
            snapshot.phase = Phase::EraseFailed;
        }
        snapshot.clone()
    }

    pub fn status(&self) -> String {
        let snapshot = self.snapshot();
        let base: String=match snapshot.phase {
            Phase::Disabled => "Off — stored index retained".into(),
            Phase::NeedsRoots => "Choose folders to index".into(),
            Phase::Paused => {
                let reason = snapshot.pause_reason.label();
                if snapshot.pause_reason == PauseReason::Manual { "Paused by you — existing results remain searchable".into() }
                else if reason.is_empty() { "Paused by resource policy".into() } else { format!("Paused by resource policy · {reason}") }
            },
            Phase::Indexing => format!(
                "Indexing · {} visited · {} updated · {} source bytes",
                snapshot.progress.visited, snapshot.progress.indexed, snapshot.progress.indexed_bytes
            ),
            Phase::Ready => format!(
                "Ready · {} indexed · {} unchanged",
                snapshot.progress.indexed, snapshot.progress.unchanged
            ),
            Phase::Partial => format!(
                "Partial scan · {} failures · prior records retained",
                snapshot.progress.failed
            ),
            Phase::Failed => "Index unavailable — ordinary search remains available".into(),
            Phase::Erasing => format!("Erasing · {} records removed", snapshot.progress.removed),
            Phase::Erased => "Index erased · indexing remains off".into(),
            Phase::EraseFailed => "Erasure incomplete — retry to finish removing stored data".into(),
            Phase::Compacting => format!("Compacting · {}", snapshot.stage),
        };
        if snapshot.semantic_enabled && !snapshot.semantic_status.is_empty() {format!("{base} · {}",snapshot.semantic_status)} else {base}
    }

    /// A short phase for Status rows; the Index page shows the detail.
    pub fn headline(&self) -> String {
        let snapshot = self.snapshot();
        match snapshot.phase {
            Phase::Indexing if !snapshot.stage.is_empty() => format!("Indexing · {}", snapshot.stage),
            Phase::Indexing => "Indexing".into(),
            Phase::Ready => "Up to date".into(),
            Phase::Partial => "Partly indexed".into(),
            Phase::Paused => match snapshot.pause_reason.label() { "" => "Paused".into(), reason => format!("Paused · {reason}") },
            Phase::Disabled => "Off".into(),
            Phase::NeedsRoots => "Choose folders to index".into(),
            Phase::Failed => "Unavailable".into(),
            Phase::Erasing => "Erasing".into(),
            Phase::Erased => "Erased".into(),
            Phase::EraseFailed => "Erase incomplete".into(),
            Phase::Compacting => "Compacting".into(),
        }
    }

    /// Folders whose files changed at least 20 times in the last five minutes, busiest first, so
    /// the Index page can offer to exclude a machine-written folder that keeps indexing busy.
    pub fn busy_folders(&self) -> Vec<(PathBuf, usize)> {
        let activity = self.activity.lock().unwrap_or_else(PoisonError::into_inner);
        let now = std::time::Instant::now();
        let mut counts = std::collections::HashMap::<&std::path::Path, usize>::new();
        for (at, folder) in activity.iter() {
            if now.duration_since(*at) <= ACTIVITY_WINDOW { *counts.entry(folder.as_path()).or_default() += 1; }
        }
        let mut busy: Vec<_> = counts.into_iter().filter(|(_, count)| *count >= 20).map(|(path, count)| (path.to_path_buf(), count)).collect();
        busy.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        busy.truncate(3);
        busy
    }

    pub fn search(&self, query: &str) -> (Option<Result<SearchPage, Failure>>, bool) {
        self.search_filtered(query, crate::content::SearchFilter::default())
    }

    /// The best passage of each matching file, for content rows that open where the match is.
    /// Empty rather than an error: the row falls back to a whole-document match, which is what a
    /// file indexed before chunking still has.
    pub fn passages(&self, query: &str, limit: usize) -> Vec<crate::content::Passage> {
        self.search_matches(query, crate::content::SearchFilter::default()).0
            .and_then(Result::ok).unwrap_or_default().into_iter().take(limit).map(|item| item.passage).collect()
    }

    pub fn search_filtered(&self, query: &str, filter: crate::content::SearchFilter) -> (Option<Result<SearchPage, Failure>>, bool) {
        let (result,pending) = self.search_matches(query,filter);
        (result.map(|result| result.map(|matches| {
            let mut seen=std::collections::HashSet::new();
            SearchPage { hits: matches.into_iter().filter(|item| seen.insert(item.passage.document_id)).map(|item| crate::content::Hit {
                id:item.passage.document_id, identity:item.passage.document_id.to_string(),path:item.passage.path,title:item.passage.title,
                rank:item.passage.rank,revision:item.revision,related:!item.words,snippet:Some(item.passage.text),
            }).collect(), limited:false }
        })),pending)
    }

    pub fn search_matches(&self, query: &str, filter: crate::content::SearchFilter) -> (Option<Result<Vec<PassageMatch>, Failure>>, bool) {
        let snapshot = self.snapshot();
        let Some(path) = &self.path else {
            return (None, false);
        };
        if !snapshot.ready || query.trim().chars().count() < 2 || query.len() > 4096 {
            self.searching.cancel();
            self.semantic_searching.cancel();
            return (None, false);
        }
        let request = SearchRequest {
            epoch: snapshot.epoch,
            settled: snapshot.phase != Phase::Indexing,
            path: path.clone(),
            query: query.into(),
            filter,
            roots: snapshot.roots,
            exclusions: snapshot.exclusions,
        };
        self.searching.search_shared(request.clone(), run_search);
        let (lexical,lexical_pending)=self.searching.results(&request);
        let (semantic,semantic_pending)=if snapshot.semantic_enabled {
            let request=SemanticRequest {search:request,revision:snapshot.semantic_revision,engine:Arc::clone(&self.query_engine)};
            self.semantic_searching.search_shared(request.clone(),run_semantic_search);
            self.semantic_searching.results(&request)
        } else {self.semantic_searching.cancel();(None,false)};
        let page=match (lexical,semantic) {
            (Some(Ok(page)),Some(Ok(hits)))=>Some(Ok(fuse_passages(page,hits,2))),
            (Some(Ok(page)),_)=>Some(Ok(fuse_passages(page,Vec::new(),2))),
            (_,Some(Ok(hits))) if !hits.is_empty()=>Some(Ok(fuse_passages(Vec::new(),hits,2))),
            (Some(Err(error)),_)=>Some(Err(error)),
            _=>None,
        };
        (page,lexical_pending || semantic_pending)
    }

    pub fn cancel_search(&self) {
        self.searching.cancel();
        self.semantic_searching.cancel();
    }

    pub fn stored_passage(&self,id:i64,path:&std::path::Path)->Option<String> {
        let snapshot=self.snapshot();
        if !snapshot.ready || matches!(snapshot.phase,Phase::Disabled|Phase::NeedsRoots|Phase::Erasing|Phase::Erased|Phase::Compacting) {return None;}
        let policy=Policy {excluded_paths:snapshot.exclusions.clone(),..Policy::default()};
        if !within_scope(path,&snapshot.roots,&policy) {return None;}
        let reader=ContentStore::open_reader(self.path.as_ref()?).ok()?;
        let text=reader.stored_passage(id,path,&snapshot.roots,&snapshot.exclusions,Arc::new(AtomicBool::new(false))).ok()??;
        (self.snapshot().epoch==snapshot.epoch).then_some(text)
    }

    pub fn watched_roots(&self) -> Vec<String> {
        self.snapshot().roots.into_iter().filter_map(|path|path.into_os_string().into_string().ok()).collect()
    }

    pub fn event_relevant(&self, path:&std::path::Path) -> bool {
        let snapshot=self.snapshot();
        if !snapshot.ready {return false;}
        let policy=Policy {excluded_paths:snapshot.exclusions,..Policy::default()};
        within_scope(path,&snapshot.roots,&policy)
    }
}

/// What a background agent thread needs to pull passages for a question without borrowing the
/// service: the database, the scope in force and the shared semantic engine.
pub struct Retriever {
    path: PathBuf,
    roots: Vec<PathBuf>,
    exclusions: Vec<PathBuf>,
    engine: SharedEngine,
    semantic: bool,
}

/// One excerpt handed to the local model, with the file it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Passage {
    pub title: String,
    pub path: String,
    pub text: String,
    pub page: u32,
    pub line: u32,
}

impl Retriever {
    /// Up to five passages and about 7,000 characters: any-word matches for the question's
    /// distinctive terms fused with semantic neighbours, limited to the indexed scope.
    pub fn passages(&self, question: &str, cancel: Arc<AtomicBool>) -> Result<Vec<Passage>, &'static str> {
        let reader = ContentStore::open_reader(&self.path).map_err(|_| "The content index is unavailable")?;
        let filter=crate::content::SearchFilter::default();
        let lexical = reader.search_passages(question,&filter,&self.roots,&self.exclusions,Arc::clone(&cancel)).map_err(|_| "Searching the index did not finish")?;
        let semantic = if self.semantic {
            let mut owner = self.engine.lock().unwrap_or_else(PoisonError::into_inner);
            owner.as_mut().and_then(|engine| engine.search(&self.path, question, &filter,&self.roots,&self.exclusions,Arc::clone(&cancel)).ok()).unwrap_or_default()
        } else {
            Vec::new()
        };
        let fused = fuse_passages(lexical, semantic,2);
        let policy = Policy { excluded_paths: self.exclusions.clone(), ..Policy::default() };
        let mut passages = Vec::new();
        let mut budget = 7_000usize;
        for hit in fused.iter().map(|item| &item.passage).filter(|hit| within_scope(std::path::Path::new(&hit.path), &self.roots, &policy)) {
            if passages.len() == 5 || budget < 400 || cancel.load(Ordering::Acquire) {
                break;
            }
            let end=hit.text.floor_char_boundary(budget.min(1_800).min(hit.text.len()));
            let text=hit.text[..end].to_owned();
            budget=budget.saturating_sub(text.len());
            let page_kind=if hit.path.to_ascii_lowercase().ends_with(".pptx") {"slide"} else {"p."};
            let location=if hit.page>0 {format!(" · {page_kind} {}",hit.page)} else if hit.line>0 {format!(" · line {}",hit.line)} else {String::new()};
            passages.push(Passage {title:format!("{}{location}",hit.title),path:hit.path.clone(),text,
                page:u32::try_from(hit.page).unwrap_or(0),line:u32::try_from(hit.line).unwrap_or(0)});
        }
        Ok(passages)
    }
}

fn run_compact(request: &IndexRequest, cancel: Arc<AtomicBool>) -> Result<(), Failure> {
    crate::ffi::index_native::background_priority();
    close_query_engine(&request.query_engine);
    let outcome = (|| -> Result<crate::content::CompactReport, crate::content::Error> {
        let mut store = ContentStore::open(&request.path)?;
        let retired=store.retire_passage_models(&cancel)?;
        let mut report = store.compact(&request.path, semantic::MODEL_IDENTIFIER, Arc::clone(&cancel),
            |stage| publish(request, &cancel, |snapshot| snapshot.stage = stage))?;
        report.removed_vectors+=retired;
        let mut catalog = store.vector_catalog(Arc::clone(&cancel))?;
        catalog.extend(store.passage_catalog(None,Arc::clone(&cancel))?);
        semantic::cache::prune(&request.path, &catalog, &cancel)?;
        Ok(report)
    })();
    let resting = if request.config.enabled { Phase::Ready } else { Phase::Disabled };
    match outcome {
        Ok(report) => {
            publish(request, &cancel, |snapshot| {
                snapshot.phase = resting;
                snapshot.stage = "";
                snapshot.last_compact = Some(report);
            });
            Ok(())
        }
        Err(crate::content::Error::Cancelled) => Err(Failure::Cancelled),
        Err(error) => {
            let reason = match error {
                crate::content::Error::Invalid(reason) => reason,
                _ => "Compaction did not finish; no indexed document was lost",
            };
            publish(request, &cancel, |snapshot| {
                snapshot.phase = resting;
                snapshot.stage = "";
                snapshot.compact_error = Some(reason);
            });
            Err(Failure::Unavailable)
        }
    }
}

fn publish(request: &IndexRequest, cancel: &AtomicBool, update: impl FnOnce(&mut Snapshot)) {
    let mut snapshot = request
        .snapshot
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if snapshot.epoch == request.epoch && !cancel.load(Ordering::Acquire) {
        update(&mut snapshot);
    }
}

fn run_index(request: &IndexRequest, cancel: Arc<AtomicBool>) -> Result<(), Failure> {
    crate::ffi::index_native::background_priority();
    if request.stop || !request.config.semantic {
        close_query_engine(&request.query_engine);
        if request.stop {return Ok(());}
    }
    let config = &request.config;
    if config.roots.len() + config.code_roots.len() > 32
        || config.excluded_paths.len() > 128
        || !(1..=16).contains(&config.max_file_mb)
    {
        return Err(Failure::Unavailable);
    }
    let mut roots = Vec::new();
    let mut failed = 0;
    for root in config.expanded_roots() {
        if cancel.load(Ordering::Acquire) {
            return Err(Failure::Cancelled);
        }
        if !root.is_absolute()
            || root
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            failed += 1;
            continue;
        }
        match root.canonicalize() {
            Ok(path) => roots.push(path),
            Err(_) => failed += 1,
        }
    }
    roots.sort();
    roots.dedup();
    let mut unique = Vec::<PathBuf>::new();
    for root in roots {
        if !unique.iter().any(|parent| root.starts_with(parent)) {
            unique.push(root);
        }
    }
    let roots = unique;
    if roots.is_empty() {
        return Err(Failure::Unavailable);
    }
    let mut exclusions = Vec::new();
    for path in config.expanded_exclusions() {
        if !path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(Failure::Unavailable);
        }
        exclusions.push(path.canonicalize().unwrap_or(path));
    }
    let started = std::time::Instant::now();
    let mut store = ContentStore::open(&request.path).map_err(|_| Failure::Unavailable)?;
    {
        let mut engine=request.query_engine.lock().unwrap_or_else(PoisonError::into_inner);
        *engine=request.helpers.clone().map(|helpers| Engine::new(helpers,config.embedding_host.clone())
            .with_code_roots(config.expanded_code_roots().into_iter().filter_map(|root|root.canonicalize().ok()).collect()));
    }
    publish(request, &cancel, |snapshot| {
        snapshot.ready = true;
        snapshot.stage = match request.work {
            Work::RetryFailures => "Retrying recorded extraction failures",
            Work::SemanticOnly => "Checking embedding model",
            _ if config.documents => "Scanning folders and extracting documents",
            _ => "Scanning folders",
        };
        snapshot.pass_started = Some(std::time::SystemTime::now());
        snapshot.roots = roots.clone();
        snapshot.exclusions = exclusions.clone();
    });
    let policy = Policy {
        excluded_paths: exclusions,
        documents: config.documents,
        max_document_bytes: config.max_document_mb.saturating_mul(1_048_576),
        extractor: request.helpers.as_ref().map(|helpers| helpers.embedding.with_file_name("blindspot-extract")),
        max_file_bytes: config.max_file_mb * 1_048_576,
        storage_budget_bytes: config.index_budget_mb.clamp(256,32768) * 1_048_576,
        ocr_pages: if config.ocr {config.ocr_pages.clamp(1,100)} else {0},
        batch_pause: if config.low_impact { std::time::Duration::from_millis(50) } else { std::time::Duration::from_millis(10) },
        ..Policy::default()
    };
    let mut total = Progress {
        failed,
        ..Progress::default()
    };
    for root in &roots {
        if matches!(request.work, Work::RetryFailures | Work::SemanticOnly)
            || matches!(&request.work, Work::Folder(folder) if folder != root) { continue; }
        if request.changed_paths.as_ref().is_some_and(|paths| !paths.iter().any(|path| path.starts_with(root) || root.starts_with(path))) {
            continue;
        }
        publish(request, &cancel, |snapshot| snapshot.current_path = Some(root.clone()));
        let mut current = Progress::default();
        let on_progress = |progress: &Progress| {
            current = progress.clone();
            let mut combined = total.clone();
            add(&mut combined, &current);
            publish(request, &cancel, |snapshot| snapshot.progress = combined);
        };
        // Event-driven refreshes of a complete index re-read only what changed; a change at or above
        // the root itself still needs the full reconciliation.
        let targeted = request.changed_paths.as_ref()
            .filter(|paths| !paths.iter().any(|path| root.starts_with(path)))
            .map(|paths| paths.iter().filter(|path| path.starts_with(root)).cloned().collect::<Vec<_>>());
        let result = match &targeted {
            Some(paths) => content_indexer::scan_paths(&mut store, root, paths, &policy, &cancel, on_progress),
            None => content_indexer::scan_root(&mut store, root, &policy, &cancel, on_progress),
        };
        match result {
            Ok(progress) => {
                if !progress.complete {
                    failed += 1;
                }
                add(&mut total, &progress);
            }
            Err(crate::content::Error::Cancelled) => return Err(Failure::Cancelled),
            Err(_) => {
                failed += 1;
                total.failed += 1;
            }
        }
    }
    total.complete = failed == 0;
    if request.work == Work::RetryFailures {
        total = controls::retry_extraction(request, &mut store, &roots, &policy, &cancel)?;
        total.complete &= failed == 0;
    }
    if total.complete && request.work == Work::Reconcile {
        total.removed += store.retain_roots(&roots, &cancel).map_err(|error| match error {
            crate::content::Error::Cancelled => Failure::Cancelled,
            _ => Failure::Unavailable,
        })?;
    }
    // Semantic work follows only passes that wrote embeddable documents, or a semantic pass that
    // has not finished cleanly. Otherwise a folder of machine-written data files restarts the model
    // helper and re-validates the cache on every save (measured: 34 passes in 20 s, 15-28% CPU).
    let semantic_clean = request.snapshot.lock().unwrap_or_else(PoisonError::into_inner).semantic_clean;
    if config.semantic && !total.budget_exhausted && (total.semantic_updates > 0 || !semantic_clean) {
        let outcome=(||->Result<(),semantic::indexing::Error> {
            publish(request,&cancel,|snapshot| {
                snapshot.stage="Checking embedding model";
                snapshot.current_path=None;
                snapshot.pass_started=Some(std::time::SystemTime::now());
            });
            let helpers=request.helpers.as_ref().ok_or(semantic::Failure::Unavailable)?;
            let (mut client,model,fallback)=passage_engine::select(helpers,&config.embedding_host,&config.embedding_model,&cancel)?;
            if fallback && !config.embedding_model.is_empty() && request.work == Work::SemanticOnly {
                return Err(semantic::Failure::Unavailable.into());
            }
            let key=semantic::indexing::model_key(&model)?;
            store.begin_passage_model(&key,model.dimensions)?;
            let code_roots:Vec<_>=config.expanded_code_roots().into_iter().filter_map(|root|root.canonicalize().ok()).collect();
            publish(request,&cancel,|snapshot| snapshot.stage="Counting pending embeddings");
            let progress=semantic::indexing::run_chunks_with_budget(&mut store,&mut client,Arc::clone(&cancel),
                |path|within_scope(path,&roots,&policy) && passage_engine::eligible_path(path,&code_roots)
                    && !matches!(&request.work, Work::Folder(folder) if !path.starts_with(folder)),|progress|publish(request,&cancel,|snapshot| {
                    if snapshot.stage!="Embedding passages" {snapshot.pass_started=Some(std::time::SystemTime::now());}
                    snapshot.stage="Embedding passages";
                    snapshot.semantic_status=format!("Embedding · {} written · {} current",progress.written,progress.current);
                    snapshot.semantic_progress=Some(progress.clone());
                }),policy.storage_budget_bytes)?;
            client.close();
            passage_engine::maintain_with_budget(&store,&request.path,&helpers.vectors,&model,Arc::clone(&cancel),|built,reused| {
                publish(request,&cancel,|snapshot| {
                    if snapshot.stage!="Building semantic cache" {snapshot.pass_started=Some(std::time::SystemTime::now());}
                    snapshot.stage="Building semantic cache";
                    snapshot.semantic_status=format!("Passage cache · {built} built · {reused} reused");
                    snapshot.semantic_revision=built as u64+1;
                });
            },policy.storage_budget_bytes)?;
            if progress.stale==0 && progress.current+progress.written>0 { store.activate_passage_model(&key,&cancel)?; }
            publish(request,&cancel,|snapshot|{snapshot.semantic_progress=Some(progress.clone());snapshot.semantic_clean=progress.stale==0;snapshot.semantic_status=if progress.failed>0 {
                format!("Semantic ready · {} text items unavailable",progress.failed)
            } else {format!("Semantic ready · {}{}",model.identifier,if fallback {" · Apple fallback"} else {""})};});
            publish(request,&cancel,|snapshot| {
                if fallback && !config.embedding_model.is_empty() {snapshot.recovery.defer();}
                else {snapshot.recovery.clear();}
            });
            Ok(())
        })();
        match outcome {
            Err(semantic::indexing::Error::Cancelled)=>return Err(Failure::Cancelled),
            Err(semantic::indexing::Error::Storage(crate::content::Error::Invalid("Storage budget reached"|"Index storage budget reached")))=>{
                total.budget_exhausted=true;
                total.complete=false;
                publish(request,&cancel,|snapshot|{snapshot.semantic_clean=false;snapshot.semantic_status="Storage budget reached; text search and prior vectors retained".into();});
            },
            Err(_)=>publish(request,&cancel,|snapshot|{snapshot.semantic_clean=false;snapshot.semantic_status="Semantic unavailable; text search ready".into();snapshot.recovery.defer();}),
            Ok(())=>{},
        }
    }
    publish(request, &cancel, |snapshot| {
        let exhausted=total.budget_exhausted;
        snapshot.phase = if total.complete {
            Phase::Ready
        } else {
            Phase::Partial
        };
        snapshot.progress = total;
        snapshot.current_path = None;
        snapshot.stage = if exhausted {"Storage budget reached; increase the budget or Compact"} else {""};
        snapshot.last_pass = Some((std::time::SystemTime::now(), started.elapsed()));
    });
    Ok(())
}

fn run_erase(request: &IndexRequest, cancel: Arc<AtomicBool>) -> Result<(), Failure> {
    crate::ffi::index_native::background_priority();
    close_query_engine(&request.query_engine);
    if request.path.try_exists().map_err(|_| Failure::Unavailable)? {
        let mut store = ContentStore::open(&request.path).map_err(|_| Failure::Unavailable)?;
        store.erase(Arc::clone(&cancel), |removed| publish(request, &cancel, |snapshot| snapshot.progress.removed = removed))
            .map_err(|error| match error {
                crate::content::Error::Cancelled => Failure::Cancelled,
                _ => Failure::Unavailable,
            })?;
    }
    crate::semantic::cache::prune(&request.path,&[],&cancel).map_err(|error| match error {
        crate::content::Error::Cancelled => Failure::Cancelled,
        _ => Failure::Unavailable,
    })?;
    publish(request, &cancel, |snapshot| snapshot.phase = Phase::Erased);
    Ok(())
}

fn close_query_engine(engine:&SharedEngine) {
    if let Some(engine)=engine.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {engine.close();}
}

fn run_semantic_search(request:&SemanticRequest,cancel:Arc<AtomicBool>)->Result<Vec<PassageMatch>,Failure> {
    let mut owner=request.engine.lock().unwrap_or_else(PoisonError::into_inner);
    let engine=owner.as_mut().ok_or(Failure::Unavailable)?;
    let mut hits=engine.search(&request.search.path,&request.search.query,&request.search.filter,&request.search.roots,&request.search.exclusions,Arc::clone(&cancel)).map_err(|error|match error {
        semantic::Failure::Cancelled=>Failure::Cancelled,semantic::Failure::TimedOut=>Failure::TimedOut,_=>Failure::Unavailable,
    })?;
    let policy=Policy {excluded_paths:request.search.exclusions.clone(),..Policy::default()};
    hits.retain(|hit|within_scope(std::path::Path::new(&hit.passage.path),&request.search.roots,&policy));
    drop(owner);
    Ok(hits)
}

fn add(total: &mut Progress, next: &Progress) {
    total.visited += next.visited;
    total.indexed += next.indexed;
    total.indexed_bytes += next.indexed_bytes;
    total.unchanged += next.unchanged;
    total.skipped += next.skipped;
    total.failed += next.failed;
    total.removed += next.removed;
    total.semantic_updates += next.semantic_updates;
    total.budget_exhausted |= next.budget_exhausted;
}

fn run_search(request: &SearchRequest, cancel: Arc<AtomicBool>) -> Result<Vec<PassageMatch>, Failure> {
    let reader = ContentStore::open_reader(&request.path).map_err(|_| Failure::Unavailable)?;
    let mut page = reader
        .search_passages(&request.query, &request.filter, &request.roots,&request.exclusions,Arc::clone(&cancel))
        .map_err(|error| match error {
            crate::content::Error::Cancelled => Failure::Cancelled,
            _ => Failure::Unavailable,
        })?;
    let policy=Policy {excluded_paths:request.exclusions.clone(),..Policy::default()};
    page.retain(|hit|within_scope(std::path::Path::new(&hit.passage.path),&request.roots,&policy));
    Ok(page)
}

fn within_scope(path:&std::path::Path,roots:&[PathBuf],policy:&Policy)->bool {
    roots.iter().any(|root| {
        let Ok(relative)=path.strip_prefix(root) else {return false;};
        let mut prefix=root.clone();
        for component in relative.components() {
            let std::path::Component::Normal(name)=component else {return false;};
            prefix.push(name);
            if content_indexer::excluded(&prefix,name,policy) {return false;}
        }
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn scope_filter_rejects_hidden_generated_excluded_and_traversal_paths() {
        let roots=vec![PathBuf::from("/fixture")];
        let policy=Policy {excluded_paths:vec!["/fixture/private".into()],..Policy::default()};
        for path in ["/fixture/.ssh/key.txt","/fixture/node_modules/cache.js","/fixture/private/a.txt","/fixture/../other.txt","/another/file.txt"] {
            assert!(!within_scope(std::path::Path::new(path),&roots,&policy));
        }
        assert!(within_scope(std::path::Path::new("/fixture/project/notes.md"),&roots,&policy));
    }
    pub(super) fn wait(service: &ContentService) {
        let started = std::time::Instant::now();
        while (matches!(service.snapshot().phase, Phase::Indexing | Phase::Erasing) || service.erasing())
            && started.elapsed() < Duration::from_secs(5)
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!matches!(service.snapshot().phase, Phase::Indexing | Phase::Erasing) && !service.erasing());
    }

    #[test]
    fn erasure_runs_while_paused_and_preserves_source_files() {
        let path = std::env::temp_dir().join(format!("blindspot-content-erasure-{}", std::process::id()));
        std::fs::create_dir(&path).expect("fixture");
        let path = path.canonicalize().expect("canonical");
        let root = path.join("root");
        std::fs::create_dir(&root).expect("root");
        std::fs::write(root.join("notes.txt"), "fixture content").expect("file");
        let database = path.join("content.sqlite");
        let service = ContentService::new(Some(database.clone()));
        let cache=crate::semantic::cache::prepare(&database).expect("cache fixture");
        let orphan=cache.join(format!("{}.ann",crate::semantic::cache::token().unwrap()));
        std::fs::write(&orphan,b"abandoned vector data").unwrap();
        service.configure(&Content { enabled: false, ..Content::default() });
        service.erase().expect("absent index");
        wait(&service);
        assert_eq!(service.snapshot().phase, Phase::Erased);
        assert!(!database.exists());
        assert!(!orphan.exists(),"orphaned vectors must be erased without recreating the database");
        let enabled = Content { enabled: true, roots: vec![root.to_str().expect("path").into()], ..Content::default() };
        service.configure(&enabled);
        service.set_paused(false, 0);
        wait(&service);
        assert_eq!(service.snapshot().phase, Phase::Ready);
        assert!(service.erase().is_err(), "enabled indexing must refuse erasure");
        service.configure(&Content { enabled: false, ..enabled.clone() });
        service.set_paused(true, 3);
        let guard = rusqlite::Connection::open(&database).expect("test writer");
        guard.execute_batch("BEGIN IMMEDIATE").expect("hold writer");
        service.erase().expect("erase");
        service.refresh();
        service.set_paused(false, 0);
        service.configure(&enabled);
        assert!(service.erasing(), "resource/config refresh must not cancel queued erasure");
        guard.execute_batch("ROLLBACK").expect("release writer");
        drop(guard);
        wait(&service);
        assert_eq!(service.snapshot().phase, Phase::Erased);
        assert_eq!(ContentStore::open_reader(&database).expect("reader").count().expect("empty"), 0);
        assert_eq!(service.search("fixture"), (None, false));
        assert!(root.join("notes.txt").exists());
        std::fs::remove_dir(&cache).unwrap();
        std::os::unix::fs::symlink(&root,&cache).unwrap();
        service.erase().expect("unsafe cache must fail asynchronously");
        wait(&service);
        assert_eq!(service.snapshot().phase,Phase::EraseFailed);
        assert!(root.join("notes.txt").exists());
        std::fs::remove_file(&cache).unwrap();
        service.erase().expect("retry after unsafe cache is removed");
        wait(&service);
        assert_eq!(service.snapshot().phase,Phase::Erased);
        service.configure(&enabled);
        wait(&service);
        assert_eq!(service.snapshot().phase, Phase::Ready);
        assert_eq!(ContentStore::open_reader(&database).expect("reader").count().expect("rebuilt"), 1);
        drop(service);
        std::fs::remove_dir_all(path).expect("cleanup");
    }

    #[test]
    fn unavailable_semantics_preserves_lexical_results_and_disabled_creates_no_vectors() {
        let root=std::env::temp_dir().join(format!("blindspot-semantic-fallback-{}",std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let source=root.join("source");std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("notes.txt"),"database migrations").unwrap();
        let database=root.join("content.sqlite");
        let service=ContentService::new(Some(database.clone()));
        service.configure(&Content {enabled:true,semantic:true,roots:vec![source.to_string_lossy().into()],..Content::default()});
        service.set_paused(false, 0);
        wait(&service);
        assert_eq!(service.snapshot().phase,Phase::Ready);
        assert!(service.status().contains("Semantic unavailable"));
        let started=std::time::Instant::now();
        loop {
            let (result,pending)=service.search("migrations");
            if !pending { assert_eq!(result.unwrap().unwrap().hits.len(),1);break; }
            assert!(started.elapsed()<Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!root.join("content-vectors").exists());
        service.configure(&Content { enabled: false, ..Content::default() });
        service.erase().unwrap();wait(&service);
        drop(service);std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "requires the bundled native vector helper and native descriptor access"]
    fn native_semantic_service_returns_text_early_cancels_stale_queries_and_erases_helpers_data() {
        use std::os::unix::fs::PermissionsExt;
        let root=std::env::temp_dir().join(format!("blindspot-semantic-service-{}",std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let source=root.join("source");std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("notes.txt"),"database migrations").unwrap();
        std::fs::write(source.join("fruit.txt"),"fruit and pears").unwrap();
        let embedding=root.join("embed");
        std::fs::write(&embedding,r#"#!/usr/bin/python3
import json, sys, time
for line in sys.stdin:
    request=json.loads(line)
    response={'version':1,'id':request['id'],'model':{'identifier':'apple-contextual-en','revision':1,'dimensions':2}}
    if request['operation']=='embed':
        if request['texts']==['migrations']: time.sleep(1)
        response['vectors']=[[0.0,1.0] if ('fruit' in text or text=='apples') else [1.0,0.0] for text in request['texts']]
    print(json.dumps(response),flush=True)
"#).unwrap();
        std::fs::set_permissions(&embedding,std::fs::Permissions::from_mode(0o700)).unwrap();
        let database=root.join("content.sqlite");
        let service=ContentService::with_helpers(Some(database.clone()),Some(Helpers {
            embedding,vectors:std::env::var_os("BLINDSPOT_VECTOR_WORKER").expect("helper").into(),
        }));
        service.configure(&Content {enabled:true,semantic:true,embedding_model:String::new(),roots:vec![source.to_string_lossy().into()],..Content::default()});
        service.set_paused(false, 0);wait(&service);
        assert_eq!(service.snapshot().phase,Phase::Ready);
        assert!(service.status().contains("Semantic ready"));
        let started=std::time::Instant::now();
        loop {
            let (result,pending)=service.search("migrations");
            if let Some(Ok(page))=result {
                assert!(pending,"lexical results must arrive before delayed semantics");
                assert_eq!(page.hits.len(),1);
                assert!(started.elapsed()<Duration::from_millis(500));
                break;
            }
            assert!(started.elapsed()<Duration::from_millis(500));
            std::thread::sleep(Duration::from_millis(5));
        }
        service.cancel_search();
        let started=std::time::Instant::now();
        loop {
            let (result,pending)=service.search("apples");
            if !pending {
                let hits=result.unwrap().unwrap().hits;
                assert!(hits.first().unwrap().path.ends_with("fruit.txt"));
                break;
            }
            assert!(started.elapsed()<Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(5));
        }
        service.configure(&Content {enabled:false,..Content::default()});
        service.erase().unwrap();wait(&service);
        assert_eq!(service.snapshot().phase,Phase::Erased);
        assert_eq!(std::fs::read_dir(root.join("content-vectors")).unwrap().count(),0);
        assert!(ContentStore::open_reader(&database).unwrap().vector_catalog(Arc::new(AtomicBool::new(false))).unwrap().is_empty());
        assert!(source.join("notes.txt").exists());
        drop(service);std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn removed_roots_are_cleaned_but_unavailable_roots_are_preserved() {
        let path = std::env::temp_dir().join(format!("blindspot-content-roots-{}", std::process::id()));
        std::fs::create_dir(&path).expect("fixture");
        let path = path.canonicalize().expect("canonical");
        let first = path.join("first");
        let second = path.join("second");
        for root in [&first, &second] {
            std::fs::create_dir(root).expect("root");
            std::fs::write(root.join("notes.txt"), "fixture content").expect("content");
        }
        let database = path.join("content.sqlite");
        let service = ContentService::new(Some(database.clone()));
        service.set_paused(false, 0);
        let mut config = Content {
            enabled: true,
            roots: vec![first.to_str().expect("path").into(), second.to_str().expect("path").into()],
            ..Content::default()
        };
        service.configure(&config);
        wait(&service);
        assert_eq!(service.snapshot().phase, Phase::Ready);
        assert_eq!(ContentStore::open_reader(&database).expect("reader").count().expect("count"), 2);
        std::fs::rename(&first, path.join("unavailable")).expect("simulate unavailable");
        service.refresh();
        wait(&service);
        assert_eq!(service.snapshot().phase, Phase::Partial);
        assert_eq!(ContentStore::open_reader(&database).expect("reader").count().expect("preserved"), 2);
        config.roots.remove(0);
        service.configure(&config);
        wait(&service);
        assert_eq!(service.snapshot().phase, Phase::Ready);
        assert_eq!(ContentStore::open_reader(&database).expect("reader").count().expect("removed"), 1);
        assert!(path.join("unavailable/notes.txt").exists(), "index cleanup must not touch source files");
        drop(service);
        std::fs::remove_dir_all(path).expect("cleanup");
    }

    #[test]
    fn disabled_creates_nothing_and_enabled_search_respects_reconfiguration() {
        let path =
            std::env::temp_dir().join(format!("blindspot-content-service-{}", std::process::id()));
        std::fs::create_dir(&path).expect("fixture");
        let path = path.canonicalize().expect("canonical");
        let root = path.join("root");
        std::fs::create_dir(&root).expect("root");
        std::fs::write(root.join("notes.txt"), "database migrations").expect("text");
        let database = path.join("content.sqlite");
        let service = ContentService::new(Some(database.clone()));
        service.configure(&Content::default());
        assert!(!database.exists());
        service.set_paused(false, 0);
        let mut config = Content {
            enabled: true,
            roots: vec![root.to_str().expect("path").into()],
            ..Content::default()
        };
        service.configure(&config);
        wait(&service);
        assert_eq!(service.snapshot().phase, Phase::Ready);
        let started = std::time::Instant::now();
        let result = loop {
            let result = service.search("migrations");
            if !result.1 {
                break result.0;
            }
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(result.expect("result").expect("search").hits.len(), 1);
        config
            .excluded_paths
            .push(root.to_str().expect("path").into());
        service.configure(&config);
        assert!(service.search("migrations").0.is_none());
        wait(&service);
        config.enabled = false;
        service.configure(&config);
        assert_eq!(service.snapshot().phase, Phase::Disabled);
        assert_eq!(service.search("migrations"), (None, false));
        drop(service);
        std::fs::remove_dir_all(path).expect("cleanup");
    }
}
