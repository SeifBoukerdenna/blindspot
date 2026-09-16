//! The C ABI surface.
//!
//! The only module in the crate where `unsafe` is permitted, and the only place where
//! a mistake is memory-unsafe rather than merely wrong.
//!
//! Two rules shape everything here. Rust owns every allocation it hands out, so every
//! struct that leaves has a matching `bs_free_*`. And no panic may cross the boundary:
//! `extern "C"` is `nounwind`, so an escaping panic aborts the process — which for a
//! background agent means the user silently loses their launcher. Every entry point
//! therefore wraps its body in `catch_unwind`.

use std::ffi::{CStr, c_char};
use std::os::unix::ffi::OsStrExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::clips::{ClipKind, Clips, NewClip, Part};
use crate::config::Config;
use crate::files::FileSearch;
use crate::frecency::Frecency;
use crate::index::{AppEntry, Index, apps};
use crate::matching::Ranker;
use crate::store::Store;

pub(crate) mod process_native;
pub mod index_native;
pub mod passage;

/// How often a rescan may actually run.
///
/// The panel asks for one on every show, and a walk of the configured paths costs about
/// 30ms of disk. Collapsing a burst of invocations into a single walk is free; the price
/// is that a newly installed app can take this long to surface. FSEvents is the real
/// answer to that and belongs at M4.
const RESCAN_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct ClipRetention { clips:Arc<Clips>, count:usize }
impl PartialEq for ClipRetention {fn eq(&self,other:&Self)->bool {self.count==other.count}}
impl Eq for ClipRetention {}
fn retain_clips(request:&ClipRetention,cancel:&AtomicBool)->Result<(),crate::process_job::Failure> {
    if cancel.load(Ordering::Acquire) {return Err(crate::process_job::Failure::Cancelled);}
    request.clips.keep(request.count).then_some(()).ok_or(crate::process_job::Failure::Unavailable)
}

/// Opaque to C. Swift only ever holds a `BsHandle *`.
pub struct BsHandle {
    /// The config in force: config.toml with the settings window's overrides laid over
    /// it. A `Mutex<Arc<_>>` rather than the bare struct because a set may swap the whole
    /// thing while a query is in flight. Readers take a snapshot and drop the lock at
    /// once, so no query ever holds it across ranking — see [`BsHandle::config`].
    config: Mutex<Arc<Config>>,
    /// config.toml as parsed, with nothing laid over it. Kept so a settings row can say
    /// whether its value came from the user's file or from the window, and so resetting a
    /// key has something to fall back to.
    file_config: Arc<Config>,
    overrides: Mutex<crate::settings::Overrides>,
    /// Where config.toml was looked for, and what went wrong reading it. Kept rather than
    /// only printed: "is my file being read" is the first question a layered config
    /// raises, and stderr is not where a user looks for the answer.
    config_note: (Option<PathBuf>, Option<String>),
    /// Whether the agent's host answered, the last time anything asked. `None` until
    /// [`bs_diagnostics_refresh`] has been called and come back.
    reachable: Arc<Mutex<Option<bool>>>,
    /// `Arc` because a background rescan thread outlives the call that started it and
    /// may still be walking when `bs_shutdown` drops the handle.
    index: Arc<Index>,
    /// Long-lived: the ranker owns the matcher's scratch matrix and the parsed pattern,
    /// and rebuilding those per keystroke is the allocation this design exists to avoid.
    /// A mutex rather than a `RefCell` because nothing in a C ABI stops Swift from
    /// calling on a second thread; the lock is uncontended in practice.
    ranker: Mutex<Ranker>,
    rescanning: Arc<AtomicBool>,
    /// Spotlight's record of app use, refreshed alongside every rescan.
    usage: Arc<crate::usage::UsageScores>,
    /// Files opened in the last two weeks, for the welcome screen and a bare `?`.
    recent: Arc<crate::recent::RecentFiles>,
    /// When the last rescan started. Seeded at init, which has already scanned.
    last_scan: Mutex<Option<Instant>>,
    frecency: Mutex<Frecency>,
    files: FileSearch,
    content: crate::content_service::ContentService,
    content_path: Option<PathBuf>,
    content_stats: Arc<Mutex<ContentStats>>,
    content_stats_running: Arc<AtomicBool>,
    resource_sample: Mutex<ResourceSample>,
    ports: crate::ports::PortSearch,
    /// Whose top-level folders `?` lists directly. Held rather than read from `$HOME` on
    /// each query so tests can point it somewhere controlled — otherwise their results
    /// depend on whatever is in the home folder of whoever runs them.
    home: Option<PathBuf>,
    /// `None` only if even an in-memory store could not be created. Clipboard history is
    /// then unavailable, and everything else carries on.
    clips: Option<Arc<Clips>>,
    clip_retention: crate::process_job::Latest<ClipRetention, ()>,
    /// `None` if the store could not be opened. Frecency then works for the session and
    /// is forgotten at exit, which is a far better failure than refusing to launch.
    store: Option<Arc<Store>>,
    /// `None` when `agent.enabled` is false, which is the only state in which blindspot
    /// opens no socket at all.
    agent: Option<crate::agent::session::Session>,
    shortcuts: crate::shortcuts::Library,
}

#[derive(Clone, Default)]
struct ContentStats {
    database_bytes: Option<u64>,
    wal_bytes: Option<u64>,
    shm_bytes: Option<u64>,
    vector_catalog_bytes: Option<u64>,
    documents: Option<u64>,
    embeddings: Option<u64>,
    extraction_counts: Option<[u64; 4]>,
    extracted: Option<u64>,
    semantic_eligible: Option<u64>,
    semantic_current: Option<u64>,
    partial_documents: Option<u64>,
    taken: Option<Instant>,
    reclaimable_bytes: Option<u64>,
    kinds: Vec<(String, u64, u64)>,
    folders: Vec<(String, u64, u64)>,
    attention: Vec<(String, u8)>,
    recent: Vec<(String, i64)>,
    sampled: bool,
}

#[derive(Default)]
struct ResourceSample {
    taken: Option<Instant>,
    cpu: std::collections::HashMap<u32, u64>,
    line: String,
    rows: Vec<(String, u64, Option<f64>)>,
}

#[derive(Default)]
struct Inventory {
    kinds: Vec<(String, u64, u64)>,
    folders: Vec<(String, u64, u64)>,
    attention: Vec<(String, u8)>,
    recent: Vec<(String, i64)>,
}

/// What the index holds, for the Index page: documents by kind and by top-level folder under each
/// root, documents whose text could not be extracted, and the most recently modified. One
/// read-only pass over paths and sizes (never bodies), bounded by the sampler's deadline.
fn inventory(connection: &rusqlite::Connection, roots: &[String]) -> Inventory {
    const KINDS: &[(&str, &[&str])] = &[
        ("Notes & text", &["txt", "md", "markdown", "rst"]),
        ("PDF & Office documents", &["pdf", "docx", "doc", "rtf", "odt", "pptx", "xlsx"]),
        ("Code", &["rs", "swift", "py", "js", "jsx", "ts", "tsx", "go", "java", "c", "h", "cpp", "hpp", "rb", "sh", "sql"]),
        ("Data & config", &["json", "toml", "yaml", "yml", "csv", "tsv", "xml"]),
        ("Web pages & styles", &["html", "css"]),
    ];
    let mut inventory = Inventory::default();
    let Ok(mut statement) = connection.prepare("SELECT path,bytes,extraction,modified_ns,partial FROM documents") else { return inventory; };
    let Ok(mut rows) = statement.query([]) else { return inventory; };
    let mut kinds = vec![(0u64, 0u64); KINDS.len() + 1];
    let mut folders = std::collections::HashMap::<String, (u64, u64)>::new();
    let mut recent: Vec<(i64, String)> = Vec::new();
    while let Ok(Some(row)) = rows.next() {
        let (Ok(path), Ok(bytes), Ok(extraction), Ok(modified)) =
            (row.get::<_, String>(0), row.get::<_, i64>(1), row.get::<_, i64>(2), row.get::<_, i64>(3)) else { continue; };
        let bytes = u64::try_from(bytes).unwrap_or(0);
        let extension = std::path::Path::new(&path).extension().and_then(|value| value.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
        let kind = KINDS.iter().position(|(_, extensions)| extensions.contains(&extension.as_str())).unwrap_or(KINDS.len());
        if let Some(slot) = kinds.get_mut(kind) {
            slot.0 += 1;
            slot.1 += bytes;
        }
        let root = roots.iter()
            .filter(|root| path.starts_with(root.as_str()) && path.as_bytes().get(root.len()) == Some(&b'/'))
            .max_by_key(|root| root.len());
        if let Some(root) = root && let Some(rest) = path.get(root.len() + 1..) {
            let folder = match rest.split_once('/') { Some((first, _)) => format!("{root}/{first}"), None => root.clone() };
            let entry = folders.entry(folder).or_default();
            entry.0 += 1;
            entry.1 += bytes;
        }
        let status=if row.get::<_,bool>(4).unwrap_or(false) && extraction<2 {6} else {extraction};
        if let Ok(status @ 2..=6) = u8::try_from(status) && inventory.attention.len() < 20 {
            inventory.attention.push((path.clone(), status));
        }
        recent.push((modified, path));
        if recent.len() >= 256 {
            recent.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.0));
            recent.truncate(8);
        }
    }
    recent.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.0));
    recent.truncate(8);
    inventory.recent = recent.into_iter().map(|(modified, path)| (path, modified)).collect();
    inventory.kinds = kinds.into_iter().enumerate().filter(|(_, (count, _))| *count > 0)
        .map(|(index, (count, bytes))| (KINDS.get(index).map_or("Other", |(label, _)| label).to_owned(), count, bytes)).collect();
    inventory.kinds.sort_by_key(|kind| std::cmp::Reverse(kind.1));
    let mut folders: Vec<_> = folders.into_iter().map(|(path, (count, bytes))| (path, count, bytes)).collect();
    folders.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    folders.truncate(10);
    inventory.folders = folders;
    inventory
}

/// Structured state for the Index page. The shell formats numbers, sizes and times; the core
/// decides what each count means, so the page cannot drift from the indexer.
fn content_overview(handle: &BsHandle, config: &Config, snapshot: &crate::content_service::Snapshot, stats: &ContentStats) -> serde_json::Value {
    use crate::content_service::Phase;
    use serde_json::json;
    let state = match snapshot.phase {
        Phase::Disabled => "off",
        Phase::NeedsRoots => "needsRoots",
        Phase::Paused => "paused",
        Phase::Indexing => "indexing",
        Phase::Ready => "ready",
        Phase::Partial => "partial",
        Phase::Failed => "failed",
        Phase::Erasing => "erasing",
        Phase::Erased => "erased",
        Phase::EraseFailed => "eraseFailed",
        Phase::Compacting => "compacting",
    };
    let elapsed = |time: std::time::SystemTime| time.elapsed().ok().map(|duration| duration.as_secs());
    let named = |path: &str| {
        let path = std::path::Path::new(path);
        (path.file_name().map_or_else(|| path.to_string_lossy().into_owned(), |name| name.to_string_lossy().into_owned()),
            path.parent().map_or_else(String::new, short_path))
    };
    let progress = &snapshot.progress;
    let semantic = snapshot.semantic_progress.as_ref();
    let database = [stats.database_bytes, stats.wal_bytes, stats.shm_bytes].into_iter().flatten().sum::<u64>();
    let mut overview = serde_json::Map::new();
    let mut put = |key: &str, value: serde_json::Value| { overview.insert(key.to_owned(), value); };
    put("state", json!(state));
    put("stage", json!(snapshot.stage));
    put("compact", json!(snapshot.last_compact.map(|report| json!({"before": report.before_bytes, "after": report.after_bytes, "removed": report.removed_vectors}))));
    put("compactError", json!(snapshot.compact_error));
    put("reclaimable", json!(stats.reclaimable_bytes));
    put("message", json!(handle.content.status()));
    put("stageSeconds", json!(snapshot.pass_started.filter(|_| snapshot.phase == Phase::Indexing).and_then(elapsed)));
    put("lastPassAgo", json!(snapshot.last_pass.and_then(|(finished, _)| elapsed(finished))));
    put("lastPassSeconds", json!(snapshot.last_pass.map(|(_, took)| took.as_secs_f64())));
    put("currentFolder", json!(snapshot.current_path.as_deref().map(short_path)));
    put("pass", json!({"checked": progress.visited, "updated": progress.indexed, "sourceBytes": progress.indexed_bytes,
        "unchanged": progress.unchanged, "skipped": progress.skipped, "unreadable": progress.failed, "removed": progress.removed}));
    put("roots", json!(handle.content.watched_roots().iter().map(|root| short_path(std::path::Path::new(root))).collect::<Vec<_>>()));
    put("semantic", json!({"enabled": config.content.semantic, "embedded": stats.semantic_current, "eligible": stats.semantic_eligible,
        "passEmbedded": semantic.map(|progress| progress.written), "passFailed": semantic.map(|progress| progress.failed)}));
    put("documents", json!(stats.documents));
    put("partialDocuments", json!(stats.partial_documents));
    put("budgetBytes", json!(config.content.index_budget_mb.saturating_mul(1_048_576)));
    put("pdfText", json!(stats.extracted));
    put("pdfIssues", json!(stats.extraction_counts));
    put("disk", json!({"database": database, "cache": stats.vector_catalog_bytes}));
    put("processes", json!(handle.resource_rows().into_iter()
        .map(|(name, memory, cpu)| json!({"name": name, "memory": memory, "cpu": cpu})).collect::<Vec<_>>()));
    put("pacing", json!({"lowImpact": config.content.low_impact, "battery": config.content.on_battery,
        "documents": config.content.documents, "documentLimitMB": config.content.max_document_mb}));
    put("busy", json!(handle.content.busy_folders().into_iter()
        .map(|(path, changes)| json!({"folder": short_path(&path), "path": path.to_string_lossy(), "changes": changes})).collect::<Vec<_>>()));
    put("sampled", json!(stats.sampled));
    put("sampleAgo", json!(stats.taken.map(|taken| taken.elapsed().as_secs())));
    put("kinds", json!(stats.kinds.iter()
        .map(|(label, count, bytes)| json!({"label": label, "count": count, "bytes": bytes})).collect::<Vec<_>>()));
    put("folders", json!(stats.folders.iter()
        .map(|(path, count, bytes)| json!({"folder": short_path(std::path::Path::new(path)), "path": path, "count": count, "bytes": bytes})).collect::<Vec<_>>()));
    put("attention", json!(stats.attention.iter().map(|(path, status)| {
        let (name, folder) = named(path);
        let reason = match status { 2 => "No text — scanned or image-only", 3 => "Locked", 4 => "Over the size limit", 6 => "Partly indexed — byte, passage, page or OCR limit", _ => "Unreadable" };
        json!({"name": name, "folder": folder, "path": path, "reason": reason})
    }).collect::<Vec<_>>()));
    put("recent", json!(stats.recent.iter().map(|(path, modified)| {
        let (name, folder) = named(path);
        json!({"name": name, "folder": folder, "path": path, "modified": modified / 1_000_000_000})
    }).collect::<Vec<_>>()));
    serde_json::Value::Object(overview)
}


/// A [`BsResult`] that is an application bundle.
pub const BS_KIND_APP: u8 = 0;
/// A [`BsResult`] that is a file found through Spotlight.
pub const BS_KIND_FILE: u8 = 1;
/// A [`BsResult`] that is a calculator answer. Its `path` is empty — there is nothing to
/// open — and `name` is the formatted result, which Swift copies on Enter.
pub const BS_KIND_CALC: u8 = 2;
/// A [`BsResult`] that is a text clip from clipboard history.
pub const BS_KIND_CLIP_TEXT: u8 = 3;
/// A [`BsResult`] that is an image clip from clipboard history.
pub const BS_KIND_CLIP_IMAGE: u8 = 4;
/// A [`BsResult`] from an SRE tool — an epoch, a unit conversion, an encoding. Like
/// [`BS_KIND_CALC`], no path, and Enter copies `name`; `detail` says which form it is.
pub const BS_KIND_TOOL: u8 = 5;
/// A [`BsResult`] that is the agent's "ask the model" row. Enter submits the request.
pub const BS_KIND_AGENT_PROMPT: u8 = 7;
/// A [`BsResult`] that is a command the agent proposes. Enter runs the whole plan.
pub const BS_KIND_AGENT_STEP: u8 = 8;
/// A [`BsResult`] that is a command blindspot refuses to run, or an error. `detail` says why.
pub const BS_KIND_AGENT_BLOCKED: u8 = 9;
/// A [`BsResult`] that is a command that ran and succeeded; `detail` is its last output line.
pub const BS_KIND_AGENT_OK: u8 = 10;
/// A [`BsResult`] that is a command that ran and failed.
pub const BS_KIND_AGENT_FAILED: u8 = 11;
/// A [`BsResult`] that is prose: the model answered a question. `name` is the whole answer,
/// which the shell wraps over as many lines as it needs. Enter copies it.
pub const BS_KIND_AGENT_ANSWER: u8 = 12;
/// A [`BsResult`] that is the command running right now. Enter leaves it running — a server
/// never exits, and waiting for it to is not a useful answer — and Esc stops it.
pub const BS_KIND_AGENT_RUNNING: u8 = 14;
/// A [`BsResult`] that is a past request from the agent's history. Enter puts it back in the
/// field, ready to ask again; `detail` says what became of it.
pub const BS_KIND_AGENT_PAST: u8 = 15;
/// A [`BsResult`] that is one model in the picker. Enter on it makes it the model to ask, and
/// `id` is its index for [`bs_agent_choose`].
pub const BS_KIND_AGENT_MODEL: u8 = 13;

/// A [`BsResult`] that is a process listening on the port you asked about. `id` is its
/// pid, `path` its executable, and `detail` the pid and address it is bound to. Enter
/// copies the pid; ⌃↩ asks it to stop.
pub const BS_KIND_PORT: u8 = 16;
/// A registered command whose path carries the query to complete.
pub const BS_KIND_COMMAND: u8 = 17;
/// A setting whose path carries its schema key.
pub const BS_KIND_SETTING: u8 = 18;
/// A `:link` / `:snippet` command that saves or removes a shortcut when run; `path` is the command.
pub const BS_KIND_SHORTCUT: u8 = 19;
/// A saved quick link; `path` is the URL, or the template when no search text was typed.
pub const BS_KIND_LINK: u8 = 20;
/// A saved text snippet; `path` is its text.
pub const BS_KIND_SNIPPET: u8 = 21;
/// A macOS system command; `path` is its identifier in [`crate::system::COMMANDS`].
pub const BS_KIND_SYSTEM: u8 = 22;
/// An AI command for selected text; `path` is what to type to run it.
pub const BS_KIND_PROMPT: u8 = 23;
/// A [`BsResult`] that is a section title on the welcome screen — "Suggested", "Recent
/// files". Not selectable; `name` is the title and everything else is empty.
pub const BS_KIND_HEADER: u8 = 6;

/// `part` for [`bs_clip_content`]: the full text, or the full PNG.
pub const BS_CLIP_FULL: u8 = 0;
/// `part` for [`bs_clip_content`]: the row thumbnail. Empty for text clips.
pub const BS_CLIP_THUMBNAIL: u8 = 1;

/// One ranked match.
///
/// `name` and `path` are pointer + length, not NUL-terminated: a bundle path is bytes
/// on macOS, not necessarily valid UTF-8, and a length keeps that honest. Swift reads
/// them with `String(decoding:as:)` over an `UnsafeRawBufferPointer`.
///
/// `path` is in the struct because Swift needs it twice — once for
/// `NSWorkspace.icon(forFile:)` and once to launch — and re-deriving it from `id` would
/// mean a second FFI call per row per keystroke.
#[repr(C)]
pub struct BsResult {
    pub id: u64,
    pub process_pid: u32,
    pub network_port: u16,
    pub name: *const u8,
    pub name_len: usize,
    pub path: *const u8,
    pub path_len: usize,
    pub score: u32,
    /// One of the `BS_KIND_*` constants. Swift needs it to decide what Enter does.
    pub kind: u8,
    /// Unix seconds a clip was last copied, so Swift can show "2 min ago" with its own
    /// localized formatter. Zero for anything that is not a clip.
    pub timestamp: u64,
    /// Pixel size of an image clip, zero otherwise. Swift compares it against the
    /// attached displays to call a full-screen capture a screenshot.
    pub width: u32,
    pub height: u32,
    /// Where a content passage sits in its file: page for an extracted document, line for text
    /// and code, zero when the row is not a passage. Swift shows it and opens there.
    pub page: u32,
    pub line: u32,
    /// A tool row's subtitle — "binary", "ISO 8601", "decoded". NULL for everything else.
    /// For an epoch, `timestamp` carries the instant too, because only Swift knows the
    /// local time zone to render it in.
    pub detail: *const u8,
    pub detail_len: usize,
    /// Which characters of `name` the query matched, as offsets into its `char`s —
    /// Unicode scalars, not bytes and not UTF-16 units. The shell sets them in the accent,
    /// which is the only thing colour does in a results list besides mark the selection.
    ///
    /// NULL for every row nothing was typed at: a calculated value, an agent row, a
    /// section title, and every row of a browse mode with an empty query.
    pub highlights: *const u32,
    pub highlights_len: usize,
}

/// A clip handed from Swift to Rust. Every pointer only has to live for the call:
/// [`bs_clip_add`] copies whatever it keeps before returning.
#[repr(C)]
pub struct BsClip {
    /// [`BS_KIND_CLIP_TEXT`] or [`BS_KIND_CLIP_IMAGE`].
    pub kind: u8,
    /// UTF-8 text, or PNG bytes.
    pub content: *const u8,
    pub content_len: usize,
    /// A small PNG for the row. May be NULL for text.
    pub thumbnail: *const u8,
    pub thumbnail_len: usize,
    /// For images, UTF-8 text recognised in the image, which becomes its name and makes it
    /// searchable. May be NULL. Ignored for text clips.
    pub text: *const u8,
    pub text_len: usize,
    pub width: u32,
    pub height: u32,
}

/// What the shell needs from the config the moment it starts: the two hotkeys to register
/// and whether to be a login item.
///
/// Named for when it is read, not for what it holds — [`bs_settings_list`] is the settings
/// surface, and two entry points called `bs_settings` with opposite lifetimes would be a
/// trap. A plain value struct: nothing here to free.
#[repr(C)]
pub struct BsStartup {
    /// Carbon virtual key code, e.g. `kVK_Space`.
    pub hotkey_key_code: u32,
    /// Carbon modifier mask — `cmdKey`, `shiftKey` and friends, not `NSEvent` flags.
    pub hotkey_modifiers: u32,
    /// False if config.toml's hotkey did not parse and the default is in use.
    pub hotkey_from_config: bool,
    /// A second hotkey that opens straight into the agent. Zero when none is configured —
    /// key code 0 is `kVK_ANSI_A`, which is never a hotkey on its own.
    pub agent_hotkey_key_code: u32,
    pub agent_hotkey_modifiers: u32,
    pub launch_at_login: bool,
    /// Whether to poll the pasteboard at all. Off means nothing is recorded.
    pub clips_enabled: bool,
    /// Whether to record image clips. Off means text only.
    pub clips_images: bool,
    /// Whether to read the text in an image. Only the shell can: Vision is AppKit-side.
    pub clips_ocr: bool,
}

/// One row of the settings window: what it is, what it holds, and where that came from.
///
/// Read-only diagnostics ride this same struct rather than earning their own — a
/// diagnostic is a setting with [`BS_SETTING_READONLY`] for a kind — so the shell has one
/// row renderer and this module one free function.
#[repr(C)]
pub struct BsSetting {
    /// The dotted key, e.g. `agent.model`. Stable, and what every setter takes.
    pub key: *const u8,
    pub key_len: usize,
    /// The page this belongs on, as its name — "General", "Agent". A string rather than a
    /// tag because the shell draws it; a numbering both sides had to agree on would be a
    /// constant table for nothing.
    pub section: *const u8,
    pub section_len: usize,
    pub label: *const u8,
    pub label_len: usize,
    /// One line, drawn under the control.
    pub help: *const u8,
    pub help_len: usize,
    /// The value in force. Scalars are in their canonical text form — `true`, `8`,
    /// `cmd+shift+space`; a [`BS_SETTING_PATHS`] value is its members separated by NUL,
    /// which is the one byte a POSIX pathname cannot contain.
    pub value: *const u8,
    pub value_len: usize,
    /// What this would be with the override removed — which is what resetting restores,
    /// and not necessarily the built-in default.
    pub fallback: *const u8,
    pub fallback_len: usize,
    /// One of the `BS_SETTING_*` constants.
    pub kind: u8,
    /// One of the `BS_SOURCE_*` constants.
    pub source: u8,
    /// False means the change needs a restart, which the row says.
    pub live: bool,
    /// What a stepper clamps to. Both zero for a kind that has no bounds.
    pub min: f64,
    pub max: f64,
}

/// `~` for the home folder, so a Status row does not spend half its width on `/Users/you`.
fn short_path(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    match crate::files::home_dir() {
        Some(home) => text
            .strip_prefix(&*home.to_string_lossy())
            .map_or_else(|| text.to_string(), |rest| format!("~{rest}")),
        None => text.to_string(),
    }
}

/// Bytes, at the precision a person reading a settings row wants.
fn human_bytes(bytes: u64) -> String {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a display string; the clip budget is bounded well under 2^53 anyway"
    )]
    let size = bytes as f64;
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.0} KB", size / 1024.0),
        _ => format!("{:.1} MB", size / 1_048_576.0),
    }
}

fn content_stats(path: Option<&PathBuf>, roots: &[String]) -> ContentStats {
    let Some(path) = path else { return ContentStats::default(); };
    let size = |suffix: &str| std::fs::metadata(PathBuf::from(format!("{}{}", path.display(), suffix))).ok().map(|m| m.len());
    let database_bytes = std::fs::metadata(path).ok().map(|m| m.len());
    let wal_bytes = size("-wal");
    let shm_bytes = size("-shm");
    let connection = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    ).or_else(|_| rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)).ok();
    let Some(connection) = connection else {
        return ContentStats { database_bytes, wal_bytes, shm_bytes, taken: Some(Instant::now()), sampled: true, ..Default::default() };
    };
    let _ = connection.busy_timeout(Duration::from_millis(100));
    let deadline = Instant::now() + Duration::from_secs(1);
    let _ = connection.progress_handler(1000, Some(move || Instant::now() >= deadline));
    let scalar = |sql: &str| connection.query_row(sql, [], |row| row.get::<_, i64>(0)).ok().and_then(|value| u64::try_from(value).ok());
    let mut extraction_counts = [0; 4];
    let mut extraction_ok = false;
    if let Ok(mut statement) = connection.prepare("SELECT extraction,count(*) FROM documents WHERE extraction BETWEEN 2 AND 5 GROUP BY extraction")
        && let Ok(mut rows) = statement.query([]) {
        loop {
            match rows.next() {
                Ok(Some(row)) => {
                    let (Ok(kind), Ok(count)) = (row.get::<_, i64>(0), row.get::<_, i64>(1)) else { break; };
                    let (Ok(index), Ok(count)) = (usize::try_from(kind - 2), u64::try_from(count)) else { break; };
                    if index >= extraction_counts.len() { break; }
                    extraction_counts[index] = count;
                }
                Ok(None) => { extraction_ok = true; break; }
                Err(_) => break,
            }
        }
    }
    let inventory = inventory(&connection, roots);
    ContentStats {
        database_bytes,
        wal_bytes,
        shm_bytes,
        vector_catalog_bytes: Some(path.parent().map(|parent|parent.join("content-vectors"))
            .filter(|directory|std::fs::symlink_metadata(directory).is_ok_and(|metadata|metadata.is_dir() && !metadata.file_type().is_symlink()))
            .and_then(|directory|std::fs::read_dir(directory).ok()).into_iter().flatten().flatten()
            .filter_map(|entry|std::fs::symlink_metadata(entry.path()).ok())
            .filter(|metadata|metadata.is_file() && !metadata.file_type().is_symlink()).map(|metadata|metadata.len()).sum()),
        documents: scalar("SELECT count(*) FROM documents"),
        embeddings: scalar("SELECT count(*) FROM chunk_embeddings"),
        extraction_counts: extraction_ok.then_some(extraction_counts),
        extracted: scalar("SELECT count(*) FROM documents WHERE extraction=1"),
        semantic_eligible: scalar("SELECT count(*) FROM chunks"),
        semantic_current: scalar("SELECT count(*) FROM chunk_embeddings e JOIN passage_models m ON m.model=e.model AND m.active=1"),
        partial_documents: scalar("SELECT count(*) FROM documents WHERE partial=1"),
        taken: Some(Instant::now()),
        reclaimable_bytes: reclaimable(&connection),
        kinds: inventory.kinds,
        folders: inventory.folders,
        attention: inventory.attention,
        recent: inventory.recent,
        sampled: true,
    }
}

fn spawn_content_stats(path: Option<PathBuf>, roots: Vec<String>, slot: Arc<Mutex<ContentStats>>, running: Arc<AtomicBool>) {
    if running.swap(true, Ordering::AcqRel) { return; }
    let worker_running = Arc::clone(&running);
    let result = std::thread::Builder::new().name("blindspot-content-stats".to_owned()).spawn(move || {
        let sample = content_stats(path.as_ref(), &roots);
        match slot.lock() {
            Ok(mut guard) => *guard = sample,
            Err(poisoned) => *poisoned.into_inner() = sample,
        }
        worker_running.store(false, Ordering::Release);
    });
    if result.is_err() { running.store(false, Ordering::Release); }
}

impl BsSetting {
    /// A row that only reports. No source and no bounds, because nothing set it.
    fn reading(key: &str, label: &str, help: &str, value: &str) -> Self {
        let (key, key_len) = leak_bytes(key.as_bytes());
        let (section, section_len) = leak_bytes(crate::settings::Section::Status.name().as_bytes());
        let (label, label_len) = leak_bytes(label.as_bytes());
        let (help, help_len) = leak_bytes(help.as_bytes());
        let (value, value_len) = leak_bytes(value.as_bytes());
        let (fallback, fallback_len) = leak_bytes(&[]);
        Self {
            key,
            key_len,
            section,
            section_len,
            label,
            label_len,
            help,
            help_len,
            value,
            value_len,
            fallback,
            fallback_len,
            kind: BS_SETTING_READONLY,
            source: BS_SOURCE_DEFAULT,
            live: true,
            min: 0.0,
            max: 0.0,
        }
    }

    fn of(row: &crate::settings::Resolved) -> Self {
        let (min, max) = row.def.kind.bounds();
        let (key, key_len) = leak_bytes(row.def.key.as_bytes());
        let (section, section_len) = leak_bytes(row.def.section.name().as_bytes());
        let (label, label_len) = leak_bytes(row.def.label.as_bytes());
        let (help, help_len) = leak_bytes(row.def.help.as_bytes());
        let (value, value_len) = leak_bytes(row.value.as_bytes());
        let (fallback, fallback_len) = leak_bytes(row.fallback.as_bytes());
        Self {
            key,
            key_len,
            section,
            section_len,
            label,
            label_len,
            help,
            help_len,
            value,
            value_len,
            fallback,
            fallback_len,
            kind: row.def.kind.tag(),
            source: row.source as u8,
            live: row.def.live,
            min,
            max,
        }
    }
}

/// A pointer and a length, as [`BsResults`] is. Must be passed to [`bs_free_settings`].
#[repr(C)]
pub struct BsSettingList {
    pub items: *mut BsSetting,
    pub len: usize,
}

/// A [`BsSetting`] holding `true` or `false`.
pub const BS_SETTING_FLAG: u8 = 0;
/// A whole number, bounded by `min` and `max`.
pub const BS_SETTING_COUNT: u8 = 1;
/// A real number, bounded by `min` and `max`.
pub const BS_SETTING_NUMBER: u8 = 2;
/// Free text.
pub const BS_SETTING_TEXT: u8 = 3;
/// A hotkey chord, validated by the same parser config.toml is read with.
pub const BS_SETTING_CHORD: u8 = 4;
/// A list of folders, NUL-separated in `value`.
pub const BS_SETTING_PATHS: u8 = 5;
/// A diagnostic. Never settable.
pub const BS_SETTING_READONLY: u8 = 6;

/// Nothing set this; it is the built-in.
pub const BS_SOURCE_DEFAULT: u8 = 0;
/// The user's hand-edited config.toml.
pub const BS_SOURCE_CONFIG: u8 = 1;
/// The settings window.
pub const BS_SOURCE_OVERRIDE: u8 = 2;

/// Bytes owned by Rust. Must be passed to [`bs_free_blob`].
#[repr(C)]
pub struct BsBlob {
    pub data: *const u8,
    pub len: usize,
}

/// A pointer and a length, so one query is one struct rather than a Swift-side array
/// copy per keystroke. Must be passed to [`bs_free_results`] on every path.
#[repr(C)]
pub struct BsResults {
    pub items: *mut BsResult,
    pub len: usize,
    /// True while a file search for this query is still running, meaning more results
    /// may follow. Swift polls until it goes false. Always false for a query with no
    /// `?` prefix, because nothing asynchronous was started.
    pub pending: bool,
}

impl BsResults {
    fn empty() -> Self {
        Self {
            items: std::ptr::null_mut(),
            len: 0,
            pending: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Loads the config, scans for applications, and returns a handle.
///
/// The initial scan is synchronous so that the very first query after launch sees a
/// populated index rather than an empty one — at M2 this happens before the hotkey is
/// even registered, so it costs the user nothing.
///
/// `config_path` may be NULL, meaning `~/.config/blindspot/config.toml`. A config that
/// cannot be read or parsed falls back to defaults and complains on stderr rather than
/// failing: a typo in a TOML file must not cost the user their launcher. Returns NULL
/// only if initialisation panicked.
///
/// # Safety
///
/// `config_path` must be NULL or a valid NUL-terminated C string that stays alive for
/// the duration of the call. The returned pointer must eventually be passed to
/// [`bs_shutdown`], and to nothing else.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_init(config_path: *const c_char) -> *mut BsHandle {
    // SAFETY: delegated to this function's own contract — the caller promises NULL or
    // a live NUL-terminated buffer. Done before `catch_unwind` because a bad pointer
    // here is undefined behaviour, not a panic, so there is nothing to catch.
    let explicit = unsafe { cstr_to_string(config_path) };

    catch_unwind(move || init(explicit)).unwrap_or(std::ptr::null_mut())
}

/// Ranked matches for `query`, best first, at most `limit` of them.
///
/// An empty query is the welcome screen: suggested apps and recent files under
/// [`BS_KIND_HEADER`] rows, with `pending` set while the recent list refreshes. A NULL
/// query is treated as an empty one; a NULL handle yields an empty result set rather
/// than a crash. A `limit` of zero yields no results; ask [`bs_max_results`] for the
/// configured cap.
///
/// # Safety
///
/// `handle` must be NULL or a live pointer from [`bs_init`] that has not been shut
/// down. `query` must be NULL or a valid NUL-terminated C string alive for the call.
/// The returned [`BsResults`] must be passed to [`bs_free_results`] exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_query(
    handle: *mut BsHandle,
    query: *const c_char,
    limit: usize,
) -> BsResults {
    if handle.is_null() {
        return BsResults::empty();
    }
    // SAFETY: per the contract above, `handle` is a live `bs_init` allocation and
    // `query` is NULL or a live NUL-terminated buffer. The borrow does not outlive
    // this call, and every field it reaches is behind a lock.
    let handle = unsafe { &*handle };
    let query = unsafe { cstr_to_string(query) }.unwrap_or_default();

    // `AssertUnwindSafe`: the only state shared across the boundary is behind a mutex
    // and an rwlock whose poisoning we recover from explicitly below, so a panic here
    // cannot leave a torn value visible to the next call. Result strings allocated
    // before an unwind would leak, which is the correct trade against aborting.
    catch_unwind(AssertUnwindSafe(|| handle.query(&query, limit)))
        .unwrap_or_else(|_| BsResults::empty())
}

/// Cancels transient searches when the panel closes.
///
/// # Safety
/// `handle` must be NULL or a live pointer from `bs_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_search_cancel(handle: *mut BsHandle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: the caller retains the live allocation throughout this call.
    let handle = unsafe { &*handle };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        handle.files.cancel();
        handle.content.cancel_search();
        handle.ports.cancel();
        if let Some(agent) = &handle.agent {
            agent.cancel();
        }
    }));
}

/// Queues a coalesced content reconciliation without waiting for disk work.
/// # Safety
/// `handle` must be NULL or a live pointer from `bs_init` retained for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_content_refresh(handle: *mut BsHandle) {
    if handle.is_null() {return;}
    // SAFETY: the caller retains a live handle throughout this call.
    let handle=unsafe {&*handle};
    let _=catch_unwind(AssertUnwindSafe(||handle.content.refresh()));
}

/// Reconciles affected configured roots; invalid or oversized input requests a full scan.
/// # Safety
/// `handle` must be live and retained. `paths` must point to `len` readable bytes when non-NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_content_refresh_paths(handle: *mut BsHandle, paths: *const u8, len: usize) {
    if handle.is_null() { return; }
    // SAFETY: the caller retains the handle for this call.
    let handle = unsafe { &*handle };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if paths.is_null() || len > 64 * 1024 { handle.content.refresh(); return; }
        // SAFETY: the caller supplies len readable bytes and the size is bounded above.
        let bytes = unsafe { std::slice::from_raw_parts(paths, len) };
        match serde_json::from_slice::<Vec<std::path::PathBuf>>(bytes) {
            Ok(paths) => handle.content.refresh_paths(paths),
            Err(_) => handle.content.refresh(),
        }
    }));
}

/// Applies the shell's power/thermal policy without waiting for the index worker.
/// # Safety
/// `handle` must be NULL or a live pointer from `bs_init` retained for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_content_pause(handle: *mut BsHandle, paused: bool, reason: u8) {
    if handle.is_null() {return;}
    // SAFETY: the caller retains a live handle throughout this call.
    let handle=unsafe {&*handle};
    let _=catch_unwind(AssertUnwindSafe(||handle.content.set_paused(paused, reason)));
}

/// Erases indexed content after explicit confirmation and disabling indexing.
/// Returns an empty blob when queued, otherwise a refusal; free with `bs_free_blob`.
/// # Safety
/// `handle` must be NULL or a live pointer retained for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_content_erase(handle: *mut BsHandle, confirmed: bool) -> BsBlob {
    if handle.is_null() { return leak_blob(b"Content index unavailable"); }
    // SAFETY: the caller retains a live handle throughout this call.
    let handle = unsafe { &*handle };
    catch_unwind(AssertUnwindSafe(|| {
        if !confirmed { return leak_blob(b"Erasing indexed content requires confirmation"); }
        match handle.content.erase() {
            Ok(()) => leak_blob(b""),
            Err(why) => leak_blob(why.as_bytes()),
        }
    })).unwrap_or_else(|_| leak_blob(b"Content erasure unavailable"))
}

/// Stops erasure between transactions; already removed records stay removed.
/// # Safety
/// `handle` must be NULL or a live pointer retained for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_content_erase_cancel(handle: *mut BsHandle) {
    if handle.is_null() { return; }
    // SAFETY: the caller retains a live handle throughout this call.
    let handle = unsafe { &*handle };
    let _ = catch_unwind(AssertUnwindSafe(|| handle.content.cancel_erase()));
}

/// Returns bounded JSON configuration/status; free it with `bs_free_blob`.
/// # Safety
/// `handle` must be NULL or a live pointer from `bs_init` retained for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_content_state(handle: *mut BsHandle) -> BsBlob {
    if handle.is_null() {return leak_blob(b"");}
    // SAFETY: the caller retains a live handle throughout this call.
    let handle=unsafe {&*handle};
    catch_unwind(AssertUnwindSafe(|| {
        let config=handle.config();let snapshot=handle.content.snapshot();
        let stats = match handle.content_stats.lock() { Ok(guard) => guard.clone(), Err(poisoned) => poisoned.into_inner().clone() };
        // Counts need SQL over the whole index, so they refresh in the background at most every 15 s;
        // live pass counters come from the snapshot instead.
        if stats.taken.is_none_or(|taken| taken.elapsed() > Duration::from_secs(15)) {
            spawn_content_stats(handle.content_path.clone(), handle.content.watched_roots(), Arc::clone(&handle.content_stats), Arc::clone(&handle.content_stats_running));
        }
        let overview = content_overview(handle, &config, &snapshot, &stats);
        let roots:Vec<_>=config.content.roots.iter().take(32).filter(|path|path.len()<=4096).collect();
        let progress=&snapshot.progress;
        let current_path=snapshot.current_path.as_ref().map(|path| short_path(path));
        let state=serde_json::json!({"enabled":config.content.enabled,"onBattery":config.content.on_battery,"roots":roots,
            "watchRoots":handle.content.watched_roots(),"indexing":snapshot.phase==crate::content_service::Phase::Indexing,"status":handle.content.status(),
            "phase":format!("{:?}",snapshot.phase),"visited":progress.visited,"indexed":progress.indexed,
            "indexedBytes":progress.indexed_bytes,"unchanged":progress.unchanged,"skipped":progress.skipped,"failed":progress.failed,"removed":progress.removed,
            "currentPath":current_path,
            "databaseBytes":stats.database_bytes,"walBytes":stats.wal_bytes,"shmBytes":stats.shm_bytes,
            "vectorCatalogBytes":stats.vector_catalog_bytes,
            "documents":stats.documents,"embeddings":stats.embeddings,"statsSampled":stats.sampled,
            "extractionCounts":stats.extraction_counts,"overview":overview,
            "erasing":handle.content.erasing(),"erased":snapshot.phase==crate::content_service::Phase::Erased,"eraseFailed":snapshot.phase==crate::content_service::Phase::EraseFailed});
        match serde_json::to_vec(&state) {Ok(bytes)=>leak_blob(&bytes),Err(_)=>leak_blob(b"")}
    })).unwrap_or_else(|_|leak_blob(b""))
}

/// Maps launcher file filters onto the content index. `used:` needs Spotlight's last-opened date,
/// which the index does not store, so it is refused rather than silently ignored.
fn content_filter(query: &crate::query::FileQuery) -> Result<crate::content::SearchFilter, &'static str> {
    use crate::query::{AgeFilter, Comparison, FileKind};
    const NOTES: &[&str] = &["txt", "md", "markdown", "rst"];
    const WORD: &[&str] = &["docx", "doc", "rtf", "odt"];
    const CODE: &[&str] = &["rs", "swift", "py", "js", "jsx", "ts", "tsx", "go", "java", "c", "h", "cpp", "hpp", "rb", "sh", "sql"];
    let mut filter = crate::content::SearchFilter::default();
    if query.used.is_some() {
        return Err("used: is not available for content search; use modified:");
    }
    match &query.kind {
        None => {}
        Some(FileKind::Extension(extension)) => {
            let list = |items: &[&str]| items.iter().map(|item| (*item).to_owned()).collect::<Vec<_>>();
            filter.extensions = match extension.to_ascii_lowercase().as_str() {
                "document" | "documents" | "docs" => [list(&["pdf","pptx","xlsx"]), list(WORD), list(NOTES)].concat(),
                "note" | "notes" | "text" => list(NOTES),
                "word" => list(WORD),
                "code" => list(CODE),
                other => vec![other.to_owned()],
            };
        }
        Some(_) => return Err("kind: in content search takes an extension, documents, notes, word or code"),
    }
    if let Some(size) = query.size {
        match size.comparison {
            Comparison::Greater => filter.min_bytes = Some(size.bytes.saturating_add(1)),
            Comparison::AtLeast => filter.min_bytes = Some(size.bytes),
            Comparison::Less => filter.max_bytes = Some(size.bytes.saturating_sub(1)),
            Comparison::AtMost => filter.max_bytes = Some(size.bytes),
            Comparison::Equal => { filter.min_bytes = Some(size.bytes); filter.max_bytes = Some(size.bytes); }
        }
    }
    if let Some(age) = query.modified {
        const DAY: i64 = 86_400;
        let midnight = local_midnight(i64::try_from(unix_now()).unwrap_or(i64::MAX));
        let ns = |seconds: i64| seconds.saturating_mul(1_000_000_000);
        match age {
            AgeFilter::Today => filter.modified_after_ns = Some(ns(midnight)),
            AgeFilter::Yesterday => { filter.modified_after_ns = Some(ns(midnight - DAY)); filter.modified_before_ns = Some(ns(midnight)); }
            AgeFilter::WithinDays(days) => filter.modified_after_ns = Some(ns(midnight - DAY * i64::from(days))),
            AgeFilter::OlderThanDays(days) => filter.modified_before_ns = Some(ns(midnight - DAY * i64::from(days))),
        }
    }
    Ok(filter)
}

/// Local midnight, matching Spotlight's `$time.today` so `modified:today` means the same thing in
/// file search and content search.
fn local_midnight(now: i64) -> i64 {
    let fallback = now - now.rem_euclid(86_400);
    let time: libc::time_t = now;
    // SAFETY: an all-zero `tm` is valid plain data (its zone pointer is NULL); localtime_r overwrites it.
    let mut parts: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers refer to live locals for the duration of the call.
    if unsafe { libc::localtime_r(&time, &mut parts) }.is_null() {
        return fallback;
    }
    parts.tm_hour = 0;
    parts.tm_min = 0;
    parts.tm_sec = 0;
    parts.tm_isdst = -1;
    // SAFETY: `parts` was filled by localtime_r and adjusted in place; mktime normalizes it.
    let midnight = unsafe { libc::mktime(&mut parts) };
    if midnight < 0 { fallback } else { midnight }
}

fn one_line(text: &str, limit: usize) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() <= limit { joined } else { format!("{}…", joined.chars().take(limit).collect::<String>()) }
}

fn link_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    format!("Quick link · {}", rest.split(['/', '?', '#']).next().unwrap_or(rest))
}

fn reclaimable(connection: &rusqlite::Connection) -> Option<u64> {
    let free: i64 = connection.query_row("SELECT page_size*freelist_count FROM pragma_page_size(), pragma_freelist_count()", [], |row| row.get(0)).ok()?;
    let vectors: i64 = connection.query_row(
        &format!("SELECT coalesce(sum(length(vector)),0) FROM embeddings WHERE model NOT GLOB '[[]\"{}\",*'", crate::semantic::MODEL_IDENTIFIER),
        [], |row| row.get(0)).ok()?;
    u64::try_from(free.saturating_add(vectors)).ok()
}

/// Checks an FSEvents path against the configured scope without accessing the filesystem.
/// # Safety
/// `handle` must be NULL or a live retained handle; `path` must be NULL or a valid terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_content_event_relevant(handle:*mut BsHandle,path:*const c_char)->bool {
    if handle.is_null() {return false;}
    // SAFETY: the caller retains a live handle throughout this call.
    let handle=unsafe {&*handle};
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: path follows this entry point's terminated-string contract.
        let Some(path)=(unsafe {cstr_to_string(path)}).filter(|path|path.len()<=4096) else {return false;};
        handle.content.event_relevant(std::path::Path::new(&path))
    })).unwrap_or(false)
}

/// Runs a `:link` / `:snippet` / `:unlink` / `:unsnippet` command. Returns an empty blob on success
/// or the reason it was refused; free it with `bs_free_blob`.
/// # Safety
/// `handle` must be NULL or a live pointer from `bs_init`; `command` must be NULL or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_shortcut_apply(handle: *mut BsHandle, command: *const c_char) -> BsBlob {
    if handle.is_null() {
        return leak_blob(b"Blindspot is not ready");
    }
    // SAFETY: the caller retains a live handle throughout this call.
    let handle = unsafe { &*handle };
    // SAFETY: `command` follows this entry point's terminated-string contract.
    let command = unsafe { cstr_to_string(command) }.unwrap_or_default();
    catch_unwind(AssertUnwindSafe(|| {
        use crate::shortcuts::Command;
        let found = |removed: bool, missing: &'static str| if removed { Ok(()) } else { Err(missing) };
        let outcome = match crate::shortcuts::parse(&command) {
            Some(Command::SaveLink { keyword, url }) => handle.shortcuts.save_link(keyword, url),
            Some(Command::SaveSnippet { keyword, text }) => handle.shortcuts.save_snippet(keyword, &text),
            Some(Command::RemoveLink(keyword)) => handle.shortcuts.remove_link(keyword).and_then(|removed| found(removed, "No quick link with that keyword")),
            Some(Command::RemoveSnippet(keyword)) => handle.shortcuts.remove_snippet(keyword).and_then(|removed| found(removed, "No snippet with that keyword")),
            Some(Command::SavePrompt { keyword, text }) => handle.shortcuts.save_prompt(keyword, &text),
            Some(Command::RemovePrompt(keyword)) => handle.shortcuts.remove_prompt(keyword).and_then(|removed| found(removed, "No saved AI command with that keyword")),
            Some(Command::ListLinks(_) | Command::ListSnippets(_) | Command::ListPrompts(_)) | None => Err("Not a shortcut command"),
        };
        leak_blob(outcome.err().unwrap_or("").as_bytes())
    }))
    .unwrap_or_else(|_| leak_blob(b"The shortcut could not be saved"))
}

/// Saves `len` bytes of UTF-8 `text` as snippet `keyword`. Returns an empty blob on success or the
/// reason it was refused; free it with `bs_free_blob`.
/// # Safety
/// `handle` must be NULL or live; `keyword` NULL or NUL-terminated; `text` NULL or valid for `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_snippet_save(handle: *mut BsHandle, keyword: *const c_char, text: *const u8, len: usize) -> BsBlob {
    if handle.is_null() {
        return leak_blob(b"Blindspot is not ready");
    }
    // SAFETY: the caller retains a live handle throughout this call.
    let handle = unsafe { &*handle };
    // SAFETY: `keyword` follows this entry point's terminated-string contract.
    let keyword = unsafe { cstr_to_string(keyword) }.unwrap_or_default();
    // SAFETY: the caller guarantees `text` is valid for `len` bytes for this call; NULL reads as empty.
    let bytes = unsafe { slice_or_empty(text, len) }.to_vec();
    catch_unwind(AssertUnwindSafe(|| {
        let outcome = std::str::from_utf8(&bytes).map_err(|_| "Snippets must be UTF-8 text")
            .and_then(|text| handle.shortcuts.save_snippet(&keyword, text));
        leak_blob(outcome.err().unwrap_or("").as_bytes())
    }))
    .unwrap_or_else(|_| leak_blob(b"The snippet could not be saved"))
}

/// The instruction for the AI command `text` names (`fix grammar`, or `ai KEYWORD` for a saved one),
/// or an empty blob when it names none; free it with `bs_free_blob`.
/// # Safety
/// `handle` must be NULL or a live pointer from `bs_init`; `text` must be NULL or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_prompt_resolve(handle: *mut BsHandle, text: *const c_char) -> BsBlob {
    if handle.is_null() {
        return leak_blob(b"");
    }
    // SAFETY: the caller retains a live handle throughout this call.
    let handle = unsafe { &*handle };
    // SAFETY: `text` follows this entry point's terminated-string contract.
    let text = unsafe { cstr_to_string(text) }.unwrap_or_default();
    catch_unwind(AssertUnwindSafe(|| {
        leak_blob(handle.shortcuts.prompt(&text).map(|(_, instruction)| instruction).unwrap_or_default().as_bytes())
    }))
    .unwrap_or_else(|_| leak_blob(b""))
}

/// Starts a user-confirmed compaction of the content index. Returns an empty blob when it started or
/// the reason it could not; free it with `bs_free_blob`.
/// # Safety
/// `handle` must be NULL or a live pointer from `bs_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_content_compact(handle: *mut BsHandle) -> BsBlob {
    if handle.is_null() {
        return leak_blob(b"Blindspot is not ready");
    }
    // SAFETY: the caller retains a live handle throughout this call.
    let handle = unsafe { &*handle };
    catch_unwind(AssertUnwindSafe(|| leak_blob(handle.content.compact().err().unwrap_or("").as_bytes())))
        .unwrap_or_else(|_| leak_blob(b"Compaction could not start"))
}

/// Asks the local model what to do about `request`, on a background thread.
///
/// Returns immediately. The answer arrives through [`bs_query`] on the same `>` request, whose
/// `pending` flag stays true until the model is done.
///
/// # Safety
///
/// `handle` must be NULL or a live pointer from [`bs_init`]. `request` must be NULL or a valid
/// NUL-terminated C string alive for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_agent_submit(handle: *mut BsHandle, request: *const c_char) {
    if handle.is_null() {
        return;
    }
    // SAFETY: per the contract above; the borrow does not outlive this call.
    let handle = unsafe { &*handle };
    let request = unsafe { cstr_to_string(request) }.unwrap_or_default();
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(session) = &handle.agent {
            let request = request.trim();
            match request.strip_prefix("docs").filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace)) {
                Some(question) => session.submit_documents(request, question.trim(), handle.content.retriever()),
                None => session.submit(request, handle.home.as_deref()),
            }
        }
    }));
}

/// Submits a request with ephemeral selected text, only after an explicit user action.
///
/// # Safety
/// `handle` must be NULL or live; strings must be NULL or NUL-terminated and live for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_agent_submit_context(
    handle: *mut BsHandle,
    request: *const c_char,
    selected: *const c_char,
) {
    if handle.is_null() {
        return;
    }
    // SAFETY: caller retains handle and both input buffers throughout the call.
    let handle = unsafe { &*handle };
    // SAFETY: input is NULL or NUL-terminated, as required by the contract.
    let request = unsafe { cstr_to_string(request) }.unwrap_or_default();
    // SAFETY: input is NULL or NUL-terminated, as required by the contract.
    let selected = unsafe { cstr_to_string(selected) };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(session) = &handle.agent {
            let instruction = handle.shortcuts.prompt(request.trim()).map(|(_, instruction)| instruction);
            session.submit_with_context(
                request.trim(),
                handle.home.as_deref(),
                selected.as_deref(),
                instruction.as_deref(),
            );
        }
    }));
}

/// Runs the commands the agent proposed, in order, on a background thread.
///
/// Does nothing unless a plan is waiting and every command in it passed the refusal rules.
///
/// # Safety
///
/// `handle` must be NULL or a live pointer from [`bs_init`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_agent_run(handle: *mut BsHandle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: per the contract above.
    let handle = unsafe { &*handle };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(session) = &handle.agent {
            session.run();
        }
    }));
}

/// Stops whatever the agent is doing: a generation mid-stream, or a running command.
///
/// # Safety
///
/// `handle` must be NULL or a live pointer from [`bs_init`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_agent_cancel(handle: *mut BsHandle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: per the contract above.
    let handle = unsafe { &*handle };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(session) = &handle.agent {
            session.cancel();
        }
    }));
}

/// Offers the models Ollama has installed, as rows the shell shows in place of the results.
///
/// # Safety
///
/// `handle` must be NULL or a live pointer from [`bs_init`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_agent_models(handle: *mut BsHandle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: per the contract above.
    let handle = unsafe { &*handle };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(session) = &handle.agent {
            session.pick();
        }
    }));
}

/// Picks the model at row `index` from the list [`bs_agent_models`] produced. Index 0 is
/// "Automatic", which restores the configured defaults. Remembered across restarts.
///
/// # Safety
///
/// `handle` must be NULL or a live pointer from [`bs_init`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_agent_choose(handle: *mut BsHandle, index: usize) {
    if handle.is_null() {
        return;
    }
    // SAFETY: per the contract above.
    let handle = unsafe { &*handle };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(session) = &handle.agent {
            session.choose(index);
        }
    }));
}

/// Stops waiting on the command in flight and leaves it running.
///
/// # Safety
///
/// `handle` must be NULL or a live pointer from [`bs_init`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_agent_detach(handle: *mut BsHandle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: per the contract above.
    let handle = unsafe { &*handle };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(session) = &handle.agent {
            session.detach();
        }
    }));
}

/// Records that the user launched `result_id`.
///
/// Updates the in-memory frecency score immediately and persists it on a detached
/// thread, so this returns without waiting on an fsync. Later queries rank a
/// frequently-launched app above an equally-good textual match.
///
/// An id that matches nothing is recorded anyway and simply never scores against a
/// result — ids are hashes of bundle paths, so a stale one belongs to an app that has
/// been uninstalled and may yet come back.
///
/// # Safety
///
/// `handle` must be NULL or a live pointer from [`bs_init`] that has not been shut down.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_activate(handle: *mut BsHandle, result_id: u64) {
    if handle.is_null() {
        return;
    }
    // SAFETY: as `bs_query` — a live handle, borrowed only for this call.
    let handle = unsafe { &*handle };
    let _ = catch_unwind(AssertUnwindSafe(|| handle.record_activation(result_id)));
}

/// Kicks off a rescan and returns immediately.
///
/// The walk runs on a detached thread and swaps the new list in when it finishes, so the
/// panel can call this on every show without a keystroke ever waiting on the filesystem.
/// Concurrent calls collapse into one walk.
///
/// Rate limited: at most one walk every 10 seconds, however often this is called. A
/// newly installed application can therefore take that long to appear.
///
/// # Safety
///
/// `handle` must be NULL or a live pointer from [`bs_init`] that has not been shut down.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_reindex(handle: *mut BsHandle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: as `bs_query`. The spawned thread captures `Arc` clones rather than this
    // borrow, so it stays valid even if the handle is shut down mid-walk.
    let handle = unsafe { &*handle };
    let _ = catch_unwind(AssertUnwindSafe(|| handle.reindex()));
}

/// The configured `max_results`, or 0 for a NULL handle.
///
/// Beyond the surface sketched in CLAUDE.md, and deliberately: without it the Swift
/// side has to hardcode a row count, which makes `max_results` in the config file a
/// lie. One getter is cheaper than that inconsistency.
///
/// # Safety
///
/// `handle` must be NULL or a live pointer from [`bs_init`] that has not been shut down.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_max_results(handle: *const BsHandle) -> usize {
    if handle.is_null() {
        return 0;
    }
    // SAFETY: as `bs_query`, read-only and not outliving the call.
    let handle = unsafe { &*handle };
    catch_unwind(AssertUnwindSafe(|| handle.config().max_results)).unwrap_or(0)
}

/// Records a clip copied by the user.
///
/// Swift has already refused anything carrying a privacy marker; this applies the size
/// caps, dedups against history, evicts what no longer fits, and persists. Safe to call
/// off the main thread — only brief in-memory sections are locked, never the disk write.
///
/// # Safety
///
/// `handle` must be NULL or a live pointer from [`bs_init`]. `clip` must be NULL or point
/// to a valid [`BsClip`] whose `content` and `thumbnail` are each NULL or valid for their
/// stated lengths, for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_clip_add(handle: *mut BsHandle, clip: *const BsClip) {
    if handle.is_null() || clip.is_null() {
        return;
    }
    // SAFETY: per the contract above, both pointers are live for this call. The byte
    // slices are built by `slice_or_empty`, which never hands `from_raw_parts` a NULL.
    let (handle, clip) = unsafe { (&*handle, &*clip) };
    let content = unsafe { slice_or_empty(clip.content, clip.content_len) };
    let thumbnail = unsafe { slice_or_empty(clip.thumbnail, clip.thumbnail_len) };
    let text = unsafe { slice_or_empty(clip.text, clip.text_len) };

    let kind = match clip.kind {
        BS_KIND_CLIP_TEXT => ClipKind::Text,
        BS_KIND_CLIP_IMAGE => ClipKind::Image,
        _ => return,
    };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(clips) = &handle.clips {
            clips.add(
                NewClip {
                    kind,
                    content,
                    thumbnail,
                    text,
                    width: clip.width,
                    height: clip.height,
                },
                unix_now(),
            );
        }
    }));
}

/// The full bytes of a clip, or its thumbnail. Empty if `id` is not a clip.
///
/// # Safety
///
/// `handle` must be NULL or a live pointer from [`bs_init`]. The returned [`BsBlob`] must
/// be passed to [`bs_free_blob`] exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_clip_content(handle: *mut BsHandle, id: u64, part: u8) -> BsBlob {
    let empty = BsBlob {
        data: std::ptr::null(),
        len: 0,
    };
    if handle.is_null() {
        return empty;
    }
    // SAFETY: as `bs_query` — a live handle, borrowed only for this call.
    let handle = unsafe { &*handle };
    let part = if part == BS_CLIP_THUMBNAIL {
        Part::Thumbnail
    } else {
        Part::Full
    };
    catch_unwind(AssertUnwindSafe(|| {
        let bytes = handle.clips.as_ref()?.content(id, part)?;
        let (data, len) = leak_bytes(&bytes);
        Some(BsBlob { data, len })
    }))
    .ok()
    .flatten()
    .unwrap_or(empty)
}

/// Forgets every clip, returning how many went.
///
/// Its own entry point rather than a settings row: "clear history now" has no value, no
/// default and nowhere for a provenance to come from. Clipboard history is its own file,
/// so this cannot touch launch history.
///
/// # Safety
///
/// `handle` must be NULL or a live pointer from [`bs_init`] that has not been shut down.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_clips_clear(handle: *mut BsHandle) -> u32 {
    if handle.is_null() {
        return 0;
    }
    // SAFETY: as `bs_query` — a live handle, borrowed only for this call.
    let handle = unsafe { &*handle };
    catch_unwind(AssertUnwindSafe(|| {
        u32::try_from(handle.clips.as_ref().map_or(0, |clips|clips.clear())).unwrap_or(u32::MAX)
    }))
    .unwrap_or(0)
}

/// Removes one clipboard item and all stored representations.
///
/// # Safety
/// `handle` must be null or a live handle retained until this call returns. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_clip_remove(handle: *mut BsHandle, id: u64) -> bool {
    if handle.is_null() { return false; }
    // SAFETY: the caller retains the live handle for this call.
    let handle = unsafe { &*handle };
    catch_unwind(AssertUnwindSafe(|| handle.clips.as_ref().is_some_and(|clips| clips.remove(id))))
        .unwrap_or(false)
}

/// Reads pin state without accessing disk.
/// # Safety
/// The handle is null or retained and live for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_clip_pinned(handle:*mut BsHandle,id:u64)->bool {
    if handle.is_null() {return false;}
    // SAFETY: caller retains the live handle for this call.
    let handle=unsafe {&*handle};
    catch_unwind(AssertUnwindSafe(||handle.clips.as_ref().is_some_and(|clips|clips.pinned(id)))).unwrap_or(false)
}

/// Writes pin state. Call off the UI thread.
/// # Safety
/// The handle is null or retained and live for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_clip_pin(handle:*mut BsHandle,id:u64,pinned:bool)->bool {
    if handle.is_null() {return false;}
    // SAFETY: caller retains the live handle for this call.
    let handle=unsafe {&*handle};
    catch_unwind(AssertUnwindSafe(||handle.clips.as_ref().is_some_and(|clips|clips.pin(id,pinned)))).unwrap_or(false)
}

/// Clears history, distinguishing failure from an already empty history. Call off the UI thread.
/// # Safety
/// The handle is null or retained and live for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_clips_clear_checked(handle:*mut BsHandle)->bool {
    if handle.is_null() {return false;}
    // SAFETY: caller retains the live handle for this call.
    let handle=unsafe {&*handle};
    catch_unwind(AssertUnwindSafe(||handle.clips.as_ref().is_some_and(|clips|clips.clear_checked().is_ok()))).unwrap_or(false)
}

/// Updates a clipboard item's recency without recording application launch history.
///
/// # Safety
/// `handle` must be null or a live handle retained until this call returns. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_clip_touch(handle: *mut BsHandle, id: u64) -> bool {
    if handle.is_null() { return false; }
    // SAFETY: the caller retains the live handle for this call.
    let handle = unsafe { &*handle };
    catch_unwind(AssertUnwindSafe(|| handle.clips.as_ref().is_some_and(|clips| clips.touch(id, unix_now()))))
        .unwrap_or(false)
}

/// Asks, on a thread, whether the agent's host answers.
///
/// Returns immediately; the answer shows up in the next [`bs_settings_list`], exactly as
/// the model picker's list does. Deliberately not a synchronous getter: with Ollama
/// stopped, opening the settings window would then hang for a connect timeout.
///
/// # Safety
///
/// `handle` must be NULL or a live pointer from [`bs_init`] that has not been shut down.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_diagnostics_refresh(handle: *mut BsHandle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: as `bs_query`.
    let handle = unsafe { &*handle };
    let _ = catch_unwind(AssertUnwindSafe(|| handle.probe_host()));
    let _ = catch_unwind(AssertUnwindSafe(|| spawn_content_stats(handle.content_path.clone(), handle.content.watched_roots(), Arc::clone(&handle.content_stats), Arc::clone(&handle.content_stats_running))));
}

/// Legacy PID-only signaling is unavailable; use bs_process_signal with a start time.
///
/// # Safety
/// Takes no pointers and always returns false, preserving the old ABI without signaling.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_port_kill(pid: u32) -> bool {
    let _ = pid;
    false
}

/// Signals only the same process instance shown in the result. Requires UI confirmation.
#[unsafe(no_mangle)]
pub extern "C" fn bs_process_signal(pid: u32, started: u64, force: bool) -> bool {
    catch_unwind(|| process_native::signal(pid, started, force)).unwrap_or(false)
}

/// Returns JSON for a still-current process; empty means unavailable. Free with bs_free_blob.
#[unsafe(no_mangle)]
pub extern "C" fn bs_process_inspect(pid: u32, started: u64) -> BsBlob {
    catch_unwind(|| {
        let Some(snapshot) = process_native::snapshot(pid, true).filter(|p| started == 0 || p.started == started) else {
            return leak_blob(&[]);
        };
        match serde_json::to_vec(&snapshot) {
            Ok(bytes) => {
                let (data, len) = leak_bytes(&bytes);
                BsBlob { data, len }
            }
            Err(_) => leak_blob(&[]),
        }
    }).unwrap_or_else(|_| leak_blob(&[]))
}

/// Spells a Carbon key code and modifier mask the way config.toml writes it.
///
/// The settings window's recorder turns an `NSEvent` into a code and a mask — genuinely
/// shell-side work, since Carbon and AppKit modifier bits share no positions — and this
/// turns that back into the one canonical string. The chord vocabulary therefore lives in
/// exactly one place, and `hotkey::format`'s round-trip test is what keeps it honest.
///
/// An empty blob means the key has no name blindspot could write into a config file.
///
/// # Safety
///
/// The returned [`BsBlob`] must be passed to [`bs_free_blob`] exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_hotkey_format(key_code: u32, modifiers: u32) -> BsBlob {
    let hotkey = crate::hotkey::Hotkey {
        key_code,
        modifiers,
    };
    catch_unwind(AssertUnwindSafe(|| {
        crate::hotkey::format(hotkey).map_or_else(|| leak_blob(b""), |s| leak_blob(s.as_bytes()))
    }))
    .unwrap_or_else(|_| leak_blob(b""))
}

/// Releases a [`BsBlob`].
///
/// # Safety
///
/// `blob` must be exactly what [`bs_clip_content`] returned, freed at most once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_free_blob(blob: BsBlob) {
    // SAFETY: the contract guarantees the pair came from `leak_bytes` in
    // `bs_clip_content` and has not been freed; `free_bytes` ignores NULL.
    let _ = catch_unwind(AssertUnwindSafe(move || unsafe {
        free_bytes(blob.data, blob.len)
    }));
}

/// The two hotkeys and the login-item flag, read from the config in force.
///
/// # Safety
///
/// `handle` must be NULL or a live pointer from [`bs_init`] that has not been shut down.
/// A NULL handle yields the defaults.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_startup(handle: *const BsHandle) -> BsStartup {
    let defaults = BsStartup {
        hotkey_key_code: crate::hotkey::DEFAULT.key_code,
        hotkey_modifiers: crate::hotkey::DEFAULT.modifiers,
        hotkey_from_config: false,
        agent_hotkey_key_code: 0,
        agent_hotkey_modifiers: 0,
        launch_at_login: true,
        clips_enabled: true,
        clips_images: true,
        clips_ocr: true,
    };
    if handle.is_null() {
        return defaults;
    }
    // SAFETY: as `bs_query` — read-only and not outliving the call.
    let handle = unsafe { &*handle };
    catch_unwind(AssertUnwindSafe(|| {
        // One snapshot for all four fields. This is the only entry point that reads
        // several at once, and it must not report a hotkey from one config and a login
        // setting from another because a set landed between them.
        let config = handle.config();
        let (hotkey, from_config) = config.hotkey();
        let agent_hotkey = config.agent_hotkey();
        BsStartup {
            hotkey_key_code: hotkey.key_code,
            hotkey_modifiers: hotkey.modifiers,
            hotkey_from_config: from_config,
            agent_hotkey_key_code: agent_hotkey.map_or(0, |h| h.key_code),
            agent_hotkey_modifiers: agent_hotkey.map_or(0, |h| h.modifiers),
            launch_at_login: config.launch_at_login,
            clips_enabled: config.clips.enabled,
            clips_images: config.clips.images,
            clips_ocr: config.clips.ocr,
        }
    }))
    .unwrap_or(defaults)
}

/// Every settable knob and every diagnostic, in the order the window draws them.
///
/// # Safety
///
/// `handle` must be NULL or a live pointer from [`bs_init`] that has not been shut down.
/// The returned [`BsSettingList`] must be passed to [`bs_free_settings`] exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_settings_list(handle: *const BsHandle) -> BsSettingList {
    if handle.is_null() {
        return BsSettingList {
            items: std::ptr::null_mut(),
            len: 0,
        };
    }
    // SAFETY: as `bs_query` — read-only and not outliving the call.
    let handle = unsafe { &*handle };
    catch_unwind(AssertUnwindSafe(|| handle.settings_list())).unwrap_or(BsSettingList {
        items: std::ptr::null_mut(),
        len: 0,
    })
}

/// Releases everything a [`bs_settings_list`] result owns.
///
/// # Safety
///
/// `list` must be exactly what [`bs_settings_list`] returned, not modified since, and
/// freed at most once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_free_settings(list: BsSettingList) {
    // SAFETY: delegated to this function's contract.
    let _ = catch_unwind(AssertUnwindSafe(move || unsafe { free_settings(list) }));
}

/// Records `key` as set to `value` and applies it.
///
/// Returns the reason it was refused, or an empty blob on success — so the window can say
/// *why* a chord or a host was rejected, which a boolean could not. The value is written
/// to the overrides file; **config.toml is never touched**.
///
/// `value` is pointer + length rather than a C string because a [`BS_SETTING_PATHS`] value
/// separates its members with NUL.
///
/// # Safety
///
/// `handle` must be NULL or a live pointer from [`bs_init`]. `key` must be NULL or a valid
/// NUL-terminated C string alive for the call, and `value` NULL or valid for `value_len`
/// bytes. The returned [`BsBlob`] must be passed to [`bs_free_blob`] exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_setting_set(
    handle: *mut BsHandle,
    key: *const c_char,
    value: *const u8,
    value_len: usize,
) -> BsBlob {
    // SAFETY: delegated to this function's contract, and done before `catch_unwind`
    // because a bad pointer is undefined behaviour rather than a panic.
    let key = unsafe { cstr_to_string(key) };
    // SAFETY: as above — NULL or valid for `value_len`.
    let value = String::from_utf8_lossy(unsafe { slice_or_empty(value, value_len) }).into_owned();
    if handle.is_null() {
        return leak_blob(b"no handle");
    }
    // SAFETY: as `bs_query` — the caller guarantees a live handle for the call.
    let handle = unsafe { &*handle };
    catch_unwind(AssertUnwindSafe(|| match key {
        Some(key) => handle.apply_setting(&key, &value),
        None => leak_blob(b"no key"),
    }))
    .unwrap_or_else(|_| leak_blob(b"the core panicked setting this"))
}

/// Forgets whatever the window set for `key`, so it falls back to config.toml and then to
/// the built-in default. Returns a refusal the same way [`bs_setting_set`] does.
///
/// # Safety
///
/// As [`bs_setting_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_setting_reset(handle: *mut BsHandle, key: *const c_char) -> BsBlob {
    // SAFETY: delegated to this function's contract.
    let key = unsafe { cstr_to_string(key) };
    if handle.is_null() {
        return leak_blob(b"no handle");
    }
    // SAFETY: as `bs_query`.
    let handle = unsafe { &*handle };
    catch_unwind(AssertUnwindSafe(|| match key {
        Some(key) => handle.reset_setting(&key),
        None => leak_blob(b"no key"),
    }))
    .unwrap_or_else(|_| leak_blob(b"the core panicked resetting this"))
}

/// Releases everything a [`bs_query`] result owns.
///
/// # Safety
///
/// `results` must be exactly what a [`bs_query`] call returned, not modified since,
/// and freed at most once. Passing a zeroed or hand-built struct is undefined.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_free_results(results: BsResults) {
    // SAFETY: delegated to this function's contract — the caller guarantees the struct
    // came from `bs_query` and is being freed once.
    let _ = catch_unwind(AssertUnwindSafe(move || unsafe { free_results(results) }));
}

/// Destroys the handle.
///
/// A rescan thread may still be walking; it holds `Arc` clones of everything it needs,
/// so it finishes harmlessly and drops the index afterwards.
///
/// # Safety
///
/// `handle` must be NULL or a live pointer from [`bs_init`], shut down at most once,
/// with no other call on it in flight or afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_shutdown(handle: *mut BsHandle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: `handle` came from `Box::into_raw` in `init`, and the contract above says
    // this is the only call that will reclaim it.
    let boxed = unsafe { Box::from_raw(handle) };
    let _ = catch_unwind(AssertUnwindSafe(move || drop(boxed)));
}

// ---------------------------------------------------------------------------
// Safe implementation
// ---------------------------------------------------------------------------

fn init(explicit: Option<String>) -> *mut BsHandle {
    let path = explicit.map(PathBuf::from).or_else(Config::default_path);
    let mut trouble = None;
    let config = match path.as_deref().map(Config::load) {
        Some(Ok(config)) => config,
        Some(Err(e)) => {
            trouble = Some(e.to_string());
            // stderr is the only channel a static library has. Launched from a
            // terminal it lands in the terminal; launched by `open` it lands in the
            // unified log, which is where a user debugging their config would look.
            eprintln!("blindspot: {e}; using defaults");
            Config::default()
        }
        // No `$HOME`, so there is no default path to read. Defaults are still a
        // working launcher.
        None => Config::default(),
    };

    // config.toml is the floor and is never written. Anything the settings window set
    // goes on top — the same arrangement `agent-model` already uses for the picked model.
    let file_config = Arc::new(config);
    let overrides = crate::settings::Overrides::load(crate::settings::Overrides::default_path());
    let mut config = (*file_config).clone();
    overrides.apply(&mut config);

    let index = Arc::new(Index::new());
    index.replace(apps::scan(&config.resolved_app_paths()));

    let store = match Store::default_path().and_then(|path| Store::open(&path)) {
        Ok(store) => Some(Arc::new(store)),
        Err(e) => {
            eprintln!("blindspot: {e}; frecency will not persist");
            None
        }
    };
    let visits = match store.as_ref().map(|s| s.load()) {
        Some(Ok(rows)) => rows,
        Some(Err(e)) => {
            eprintln!("blindspot: {e}; starting with no history");
            Vec::new()
        }
        None => Vec::new(),
    };
    let frecency = Frecency::load(config.frecency.half_life_days, visits);

    let config_agent = config
        .agent
        .enabled
        .then(|| crate::agent::session::Session::new(config.agent.clone()));

    let helpers=std::env::current_exe().ok().and_then(|path|path.parent()?.parent().map(|contents| {
        crate::semantic::search::Helpers {
            embedding:contents.join("Helpers/blindspot-semantic"),
            vectors:contents.join("Helpers/blindspot-vectors"),
        }
    }));
    let content_path = crate::store::data_dir().ok().map(|path|path.join("content.sqlite"));
    let content = crate::content_service::ContentService::with_helpers(content_path.clone(),helpers);
    config.content.embedding_host = config.agent.host.clone();
    content.configure(&config.content);
    let keep = config.clips.keep;
    let usage = spawn_usage_refresh(config.frecency.half_life_days);
    // Fetched now so the first panel show already has a list to paint.
    let recent = Arc::new(crate::recent::RecentFiles::default());
    recent.refresh();

    Box::into_raw(Box::new(BsHandle {
        config: Mutex::new(Arc::new(config)),
        file_config,
        overrides: Mutex::new(overrides),
        config_note: (path, trouble),
        reachable: Arc::new(Mutex::new(None)),
        index,
        ranker: Mutex::new(Ranker::new()),
        rescanning: Arc::new(AtomicBool::new(false)),
        usage,
        recent,
        last_scan: Mutex::new(Some(Instant::now())),
        frecency: Mutex::new(frecency),
        files: FileSearch::new(),
        content,
        content_path,
        content_stats: Arc::new(Mutex::new(ContentStats::default())),
        content_stats_running: Arc::new(AtomicBool::new(false)),
        resource_sample: Mutex::new(ResourceSample::default()),
        ports: crate::ports::PortSearch::new(),
        home: crate::files::home_dir(),
        clips: {
            let opened = open_clips();
            if let Some(clips) = &opened {
                clips.keep(keep);
            }
            opened.map(Arc::new)
        },
        clip_retention: crate::process_job::Latest::default(),
        store,
        agent: config_agent,
        shortcuts: crate::shortcuts::Library::open(crate::store::data_dir().ok().map(|directory| directory.join("shortcuts.toml"))),
    }))
}

/// Starts the first Spotlight usage fetch off the main thread and returns the store it will
/// fill. Launch does not wait for it: the first few queries rank on blindspot's own history
/// until it lands, about 84ms later.
fn spawn_usage_refresh(half_life_days: f64) -> Arc<crate::usage::UsageScores> {
    let usage = Arc::new(crate::usage::UsageScores::default());
    let target = Arc::clone(&usage);
    let _ = std::thread::Builder::new()
        .name("blindspot-usage".to_owned())
        .spawn(move || target.replace(crate::usage::fetch_scores(unix_now(), half_life_days)));
    usage
}

/// Clipboard history on disk, falling back to this session only. Losing history at quit
/// is a far better failure than refusing to start.
fn open_clips() -> Option<Clips> {
    match Clips::default_path().and_then(|path| Clips::open(&path)) {
        Ok(clips) => Some(clips),
        Err(e) => {
            eprintln!("blindspot: {e}; clipboard history will not persist");
            Clips::in_memory().ok()
        }
    }
}

/// Seconds since the Unix epoch, or 0 if the clock is before it. Frecency arithmetic
/// saturates on time going backwards, so a nonsense clock costs ranking quality and
/// nothing more.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Split out as a pure function so the window is unit-testable without a test that
/// actually sleeps for ten seconds.
fn should_rescan(last: Option<Instant>, now: Instant, interval: Duration) -> bool {
    match last {
        None => true,
        // Saturating, so a clock that jumped backwards reads as "just scanned" rather
        // than panicking on the subtraction.
        Some(last) => now.saturating_duration_since(last) >= interval,
    }
}

impl BsHandle {
    /// Both locks recover from poisoning rather than propagating: a poisoned lock here
    /// means an earlier call panicked, and the contents — scratch buffers and a score
    /// map — have no invariant a stale value could violate. Losing the launcher forever
    /// is much worse than one odd ranking.
    fn with_ranker<T>(&self, f: impl FnOnce(&mut Ranker) -> T) -> T {
        match self.ranker.lock() {
            Ok(mut guard) => f(&mut guard),
            Err(poisoned) => f(&mut poisoned.into_inner()),
        }
    }

    /// The config in force, as a snapshot. The lock is released before this returns, so
    /// nothing holds it across a query.
    fn config(&self) -> Arc<Config> {
        match self.config.lock() {
            Ok(guard) => Arc::clone(&guard),
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }

    fn with_overrides<T>(&self, f: impl FnOnce(&mut crate::settings::Overrides) -> T) -> T {
        match self.overrides.lock() {
            Ok(mut guard) => f(&mut guard),
            Err(poisoned) => f(&mut poisoned.into_inner()),
        }
    }

    /// Blindspot and its own helper processes, with CPU percentage computed between two samples.
    /// Sampled at most every two seconds because it enumerates processes; Ollama and other apps
    /// are excluded on purpose — their memory is not the launcher's.
    fn resource_line(&self) -> String {
        let mut sample = match self.resource_sample.lock() { Ok(guard) => guard, Err(poisoned) => poisoned.into_inner() };
        if sample.taken.is_some_and(|taken| taken.elapsed() < Duration::from_secs(2)) && !sample.line.is_empty() {
            return sample.line.clone();
        }
        let own = std::process::id();
        let now = Instant::now();
        let mut processes: Vec<(u32, String)> = crate::ffi::process_native::list(&AtomicBool::new(false)).into_iter()
            .filter(|process| process.parent_pid == own).map(|process| (process.pid, process.name)).collect();
        processes.sort_unstable();
        processes.insert(0, (own, "Blindspot".to_owned()));
        let elapsed = sample.taken.map(|taken| now.duration_since(taken).as_nanos() as f64);
        let mut cpu = std::collections::HashMap::new();
        let mut parts = Vec::new();
        let mut rows = Vec::new();
        for (pid, name) in processes {
            let Some((resident, total)) = crate::ffi::process_native::usage(pid) else { continue; };
            let percent = match (elapsed, sample.cpu.get(&pid)) {
                (Some(elapsed), Some(previous)) if elapsed > 0.0 => Some(total.saturating_sub(*previous) as f64 / elapsed * 100.0),
                _ => None,
            };
            cpu.insert(pid, total);
            let name = name.trim_start_matches("blindspot-").to_owned();
            let memory = human_bytes(resident);
            let load = percent.map_or_else(|| "CPU % on next refresh".to_owned(), |percent| format!("{percent:.1}% CPU"));
            parts.push(format!("{name} {memory} {load}"));
            rows.push((name, resident, percent));
        }
        sample.line = if parts.is_empty() { "unavailable".to_owned() } else { parts.join(" · ") };
        sample.taken = Some(now);
        sample.cpu = cpu;
        sample.rows = rows;
        sample.line.clone()
    }

    fn resource_rows(&self) -> Vec<(String, u64, Option<f64>)> {
        let _ = self.resource_line();
        match self.resource_sample.lock() {
            Ok(guard) => guard.rows.clone(),
            Err(poisoned) => poisoned.into_inner().rows.clone(),
        }
    }

    /// Every row the settings window draws: the schema, then what can only be measured.
    fn settings_list(&self) -> BsSettingList {
        let overrides = self.with_overrides(|o| o.clone());
        let rows = crate::settings::resolve(&self.file_config, &overrides);
        let mut items: Vec<BsSetting> = rows.iter().map(BsSetting::of).collect();
        items.extend(self.diagnostics(&overrides));
        leak_settings(items)
    }

    /// The Status page: what is indexed, what is on disk, what is answering.
    ///
    /// Read-only settings rather than a struct of their own — a diagnostic *is* a row with
    /// a value and no way to set it — so the window has one renderer and this module one
    /// free function.
    fn diagnostics(&self, overrides: &crate::settings::Overrides) -> Vec<BsSetting> {
        let config = self.config();
        let (kept, bytes) = self.clips.as_ref().map_or((0, 0), |clips|clips.stats());
        let (path, trouble) = &self.config_note;

        let file = match (path, trouble) {
            (Some(path), None) => short_path(path),
            (Some(path), Some(e)) => format!("{} — {e}", short_path(path)),
            (None, _) => "no $HOME, so defaults".to_owned(),
        };
        let set = match overrides.len() {
            0 => "nothing set here".to_owned(),
            1 => "1 value set here".to_owned(),
            n => format!("{n} values set here"),
        };
        let reach = match self.reachable.lock() {
            Ok(guard) => *guard,
            Err(poisoned) => *poisoned.into_inner(),
        };
        let agent = match (config.agent.enabled, reach) {
            (false, _) => "off — no socket is opened".to_owned(),
            (true, None) => format!("{} · not asked yet", config.agent.host),
            (true, Some(true)) => format!("{} · answering", config.agent.host),
            (true, Some(false)) => format!("{} · not answering", config.agent.host),
        };
        let resources = format!("{} · no OS CPU/RAM quota", self.resource_line());
        let stats = match self.content_stats.lock() { Ok(guard) => guard.clone(), Err(poisoned) => poisoned.into_inner().clone() };
        let stat_value = |value: Option<u64>| value.map_or_else(|| "Unavailable".to_owned(), human_bytes);
        let records = match (stats.documents, stats.embeddings) {
            (Some(documents), Some(embeddings)) => format!("{documents} documents · {embeddings} embeddings"),
            _ => "Unavailable".to_owned(),
        };
        let storage = if stats.sampled {
            format!("DB {} · WAL {} · SHM {} · vector catalog {}", stat_value(stats.database_bytes), stat_value(stats.wal_bytes), stat_value(stats.shm_bytes), stat_value(stats.vector_catalog_bytes))
        } else { "Sampling…".to_owned() };

        vec![
            BsSetting::reading("status.content", "Content index", "Open the Index tab for what is indexed, activity and resources.", &match stats.documents { Some(documents) => format!("{} · {documents} documents", self.content.headline()), None => self.content.headline() }),
            BsSetting::reading("status.content.records", "Content records", "Last complete read-only sample from the content database; counts can change while indexing.", &records),
            BsSetting::reading("status.content.storage", "Content storage", "On-disk SQLite files and the vector catalog, sampled in the background. WAL and SHM are separate from the database file.", &storage),
            BsSetting::reading(
                "status.apps",
                "Applications indexed",
                "Rescanned on every show, at most once every ten seconds.",
                &self.index.len().to_string(),
            ),
            BsSetting::reading(
                "status.clips",
                "Clipboard history",
                "Its own file, so clearing it cannot touch launch history.",
                &format!("{kept} kept · {}", human_bytes(bytes)),
            ),
            BsSetting::reading(
                "status.config",
                "config.toml",
                "Yours to edit. blindspot reads it and never writes it.",
                &file,
            ),
            BsSetting::reading(
                "status.overrides",
                "overrides.toml",
                "What this window has set. Deleting the file undoes all of it.",
                &set,
            ),
            BsSetting::reading(
                "status.agent",
                "Local model",
                "Loopback only, checked on every request.",
                &agent,
            ),
            BsSetting::reading(
                "status.resources",
                "App resources",
                "Blindspot and its own helpers (semantic model, vector search, PDF extraction): resident memory and CPU percentage between two refreshes. Ollama is a separate process. macOS offers no per-app CPU or RAM quota.",
                &resources,
            ),
        ]
    }

    /// Asks whether the agent's host answers, on a thread.
    ///
    /// A bare TCP connect rather than a request: "is Ollama up" is a question about the
    /// socket, and asking a model anything to find out would be both slower and a thing
    /// the user did not ask for. The host is loopback-checked before it is ever stored,
    /// so this cannot reach off the machine.
    fn probe_host(&self) {
        let host = self.config().agent.host.clone();
        let slot = Arc::clone(&self.reachable);
        let _ = std::thread::Builder::new()
            .name("blindspot-reach".to_owned())
            .spawn(move || {
                use std::net::{TcpStream,SocketAddr};
                let host=host.strip_prefix("localhost:").map_or_else(||host.clone(),|port|format!("127.0.0.1:{port}"));
                let answered=host.parse::<SocketAddr>().ok().filter(|address|address.ip().is_loopback())
                    .is_some_and(|address|TcpStream::connect_timeout(&address,Duration::from_millis(400)).is_ok());
                match slot.lock() {
                    Ok(mut guard) => *guard = Some(answered),
                    Err(poisoned) => *poisoned.into_inner() = Some(answered),
                }
            });
    }

    /// Records `key` as set and applies it, or says why it was refused.
    fn apply_setting(&self, key: &str, value: &str) -> BsBlob {
        if key.starts_with("content.") && (self.content.erasing() || self.content.compacting()) {
            return leak_blob(b"Wait for content erasure or compaction to finish before changing these settings");
        }
        let Some(def) = crate::settings::def(key) else {
            return leak_blob(format!("{key:?} is not a setting").as_bytes());
        };
        if let Err(why) = crate::settings::validate(def, value) {
            return leak_blob(why.as_bytes());
        }
        if let Err(why) = self.with_overrides(|o| o.set(key, value)) {
            return leak_blob(why.as_bytes());
        }
        self.recompute();
        leak_blob(b"")
    }

    /// Forgets what the window set for `key`, back to config.toml and then the built-in.
    fn reset_setting(&self, key: &str) -> BsBlob {
        if key.starts_with("content.") && (self.content.erasing() || self.content.compacting()) {
            return leak_blob(b"Wait for content erasure or compaction to finish before changing these settings");
        }
        if crate::settings::def(key).is_none() {
            return leak_blob(format!("{key:?} is not a setting").as_bytes());
        }
        if let Err(why) = self.with_overrides(|o| o.reset(key)) {
            return leak_blob(why.as_bytes());
        }
        self.recompute();
        leak_blob(b"")
    }

    /// Rebuilds the config in force from config.toml plus the overrides and swaps it in.
    ///
    /// Also re-derives the one piece of state that holds a *computed* copy rather than
    /// reading the config where it is used: the frecency decay curve, which is
    /// pre-multiplied into seconds. Everything else in the core reads the config per query
    /// or per request, so swapping the `Arc` is all they need.
    fn recompute(&self) {
        let previous = self.config();
        let mut config = (*self.file_config).clone();
        self.with_overrides(|o| o.apply(&mut config));

        config.content.embedding_host = config.agent.host.clone();
        self.content.configure(&config.content);
        let half_life = config.frecency.half_life_days;
        let agent = config.agent.clone();
        let keep = config.clips.keep;
        let folders_moved = config.app_paths != previous.app_paths;
        let swapped = Arc::new(config);
        match self.config.lock() {
            Ok(mut guard) => *guard = swapped,
            Err(poisoned) => *poisoned.into_inner() = swapped,
        }

        // The three places that hold something derived from the config rather than reading
        // it where they use it.
        self.with_frecency(|f| f.set_half_life(half_life));
        if let Some(session) = &self.agent {
            session.reconfigure(agent);
        }
        if keep!=previous.clips.keep && let Some(clips)=&self.clips {
            self.clip_retention.search(ClipRetention {clips:Arc::clone(clips),count:keep},retain_clips);
        }
        if folders_moved {
            // Forget when the last walk was, so the next panel show rescans immediately
            // instead of up to `RESCAN_INTERVAL` later. Changing where apps live and then
            // waiting ten seconds to see it would read as the setting not having worked.
            self.clear_last_scan();
        }
    }

    fn with_frecency<T>(&self, f: impl FnOnce(&mut Frecency) -> T) -> T {
        match self.frecency.lock() {
            Ok(mut guard) => f(&mut guard),
            Err(poisoned) => f(&mut poisoned.into_inner()),
        }
    }

    fn query(&self, query: &str, limit: usize) -> BsResults {
        if let Some(text) = crate::commands::content_request(query) {
            self.files.cancel(); self.ports.cancel();
            if let Some(agent) = &self.agent {agent.cancel();}
            return self.content_query(text,limit);
        }
        if query.starts_with([':', ';', '>']) || query.trim().chars().count()<2 {
            self.content.cancel_search();
        }
        if let Some(request) = crate::commands::settings_request(query) {
            self.files.cancel();
            self.ports.cancel();
            if let Some(agent) = &self.agent { agent.cancel(); }
            let rows = crate::commands::settings(request, limit).into_iter().map(|setting| {
                BsResult::navigation(setting.label, setting.key,
                    &format!("{} · {}", setting.section.name(), setting.help), BS_KIND_SETTING)
            }).collect();
            return leak_results(rows, false);
        }
        // Before completion, which would otherwise answer these exact words with their own help row.
        if query.trim() == ":system" {
            self.files.cancel();
            self.ports.cancel();
            self.content.cancel_search();
            return leak_results(crate::system::COMMANDS.iter().take(limit).map(BsResult::system).collect(), false);
        }
        // The shell answers these from EventKit; the core only keeps them away from other searches.
        if matches!(query.trim(), ":schedule" | ":today" | ":calendar") {
            self.files.cancel();
            self.ports.cancel();
            self.content.cancel_search();
            return leak_results(Vec::new(), false);
        }
        if let Some(commands) = crate::commands::builtins().complete(query) {
            self.files.cancel();
            self.ports.cancel();
            if let Some(agent) = &self.agent { agent.cancel(); }
            return leak_results(commands.into_iter().take(limit).map(|command| {
                BsResult::navigation(&command.invocation, &command.invocation, &command.help, BS_KIND_COMMAND)
            }).collect(), false);
        }
        if let Some(command) = crate::shortcuts::parse(query) {
            self.files.cancel();
            self.ports.cancel();
            self.content.cancel_search();
            if let Some(agent) = &self.agent { agent.cancel(); }
            return leak_results(self.shortcut_rows(&command, query, limit), false);
        }
        if let Some(filter) = query.strip_prefix(crate::shortcuts::SNIPPET_PREFIX) {
            self.files.cancel();
            self.ports.cancel();
            self.content.cancel_search();
            if let Some(agent) = &self.agent { agent.cancel(); }
            return leak_results(self.snippet_rows(filter.trim(), limit), false);
        }
        if !query.starts_with(crate::agent::PREFIX)
            && let Some(agent) = &self.agent
        {
            agent.cancel();
        }
        if query.starts_with(crate::agent::PREFIX)
            || query.starts_with(crate::clips::PREFIX)
            || query.starts_with(crate::ports::PREFIX)
            || query.trim().chars().count() < 2 {
            self.files.cancel();
        }
        if crate::ports::ConsoleQuery::parse(query).is_none() {
            self.ports.cancel();
        }
        // Clipboard history is its own mode, not mixed into app results: clipboard contents
        // appearing among launcher results would be noise, and a privacy leak on screen.
        if let Some(rest) = query.strip_prefix(crate::clips::PREFIX) {
            return self.clip_query(rest.trim_start(), limit);
        }
        if let Some(request) = query.strip_prefix(crate::agent::PREFIX) {
            return self.agent_query(request.trim_start(), limit);
        }
        // Colon commands are deterministic; unknown syntax offers the console vocabulary.
        if let Some(port) = crate::ports::ConsoleQuery::parse(query) {
            return self.port_query(port, limit);
        }
        if query.starts_with(crate::ports::PREFIX) {
            return leak_results(vec![BsResult::header("Use :ports, :processes, :localhost, :3000, or :name")]
                .into_iter().take(limit).collect(), false);
        }
        let snapshot = self.index.snapshot();
        let now = unix_now();
        if query.trim().is_empty() {
            return self.welcome(limit, &snapshot, now);
        }
        if let Some((keyword, rest)) = crate::shortcuts::split_link(query)
            && let Some(template) = self.shortcuts.link(keyword)
        {
            self.files.cancel();
            self.content.cancel_search();
            let (title, target) = if rest.is_empty() {
                (format!("{keyword} · type what to search for"), template.clone())
            } else {
                (format!("{keyword} · {rest}"), crate::shortcuts::expand(&template, rest))
            };
            return leak_results(vec![BsResult::navigation(&title, &target, &link_host(&template), BS_KIND_LINK)], false);
        }
        match crate::files::strip_prefix(query) {
            Some(text) => self.file_query(text, limit, &snapshot, now),
            None if query.trim().chars().count() >= 2 && answers(query).is_empty() => {
                self.file_query(query.trim(), limit, &snapshot, now)
            }
            None => {
                self.files.cancel();
                self.content.cancel_search();
                self.app_query(query, limit, &snapshot, now)
            }
        }
    }

    /// A `>` query: the local agent. Rows come from its state machine, which is doing its
    /// work on a thread — `pending` is true while the model or a command is still going, and
    /// the shell polls exactly as it does for file search.
    fn agent_query(&self, request: &str, limit: usize) -> BsResults {
        let Some(session) = &self.agent else {
            return leak_results(
                vec![BsResult::agent(
                    &crate::agent::session::Row {
                        kind: crate::agent::session::RowKind::Blocked,
                        name: "The agent is off".to_owned(),
                        detail: "Set agent.enabled = true in config.toml".to_owned(),
                    },
                    0,
                )],
                false,
            );
        };
        let (rows, pending) = session.rows(request);
        // The index is the row's own position, which is what `bs_agent_choose` takes: the
        // picker's rows are the only ones whose identity is "which one of these".
        let items = rows
            .iter()
            .take(limit)
            .enumerate()
            .map(|(at, row)| match row.kind {
                crate::agent::session::RowKind::Source {..} => BsResult::source(row),
                _ => BsResult::agent(row, at as u64),
            })
            .collect();
        leak_results(items, pending)
    }

    /// The empty query: what you are likely to want before typing anything — apps by use,
    /// then files opened recently, each section under a header row.
    ///
    /// Sized to show whole. `max_results` rows fit before the list scrolls and the shell
    /// draws two headers in one row's height, so the sections share `max_results - 1`
    /// rows. Apps take the larger half, since they are opened far more often than any one
    /// file, and either section gives its unused rows to the other.
    fn welcome(&self, limit: usize, snapshot: &[AppEntry], now: u64) -> BsResults {
        self.recent.refresh();
        let recent = self.recent.snapshot();
        let rows = self
            .config()
            .max_results
            .saturating_sub(1)
            .min(limit.saturating_sub(2));

        let usage = self.usage.snapshot();
        let mut suggested: Vec<(f64, &AppEntry)> = self.with_frecency(|frecency| {
            snapshot
                .iter()
                .map(|e| {
                    let spotlight = usage.get(&e.id).copied().unwrap_or(0.0);
                    (frecency.score(e.id, now).max(spotlight), e)
                })
                .filter(|(score, _)| *score > 0.0)
                .collect()
        });
        suggested.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));

        let apps = suggested.len().min(rows - recent.len().min(rows / 2));
        let files = recent.len().min(rows - apps);

        let mut items = Vec::with_capacity(apps + files + 2);
        if apps > 0 {
            items.push(BsResult::header("Suggested"));
            items.extend(
                suggested[..apps]
                    .iter()
                    .map(|(_, e)| BsResult::new(e, 0, BS_KIND_APP)),
            );
        }
        if files > 0 {
            items.push(BsResult::header("Recent files"));
            items.extend(
                recent[..files]
                    .iter()
                    .map(|e| BsResult::new(e, 0, BS_KIND_FILE)),
            );
        }
        leak_results(items, self.recent.is_refreshing())
    }

    /// Calculator/tool queries and one-character app searches retain the fast app path.
    fn app_query(&self, text: &str, limit: usize, snapshot: &[AppEntry], now: u64) -> BsResults {
        // A value is not a name, so a tool or the calculator is the most specific possible
        // reading and takes the top rows whenever it fires — and both decline everything
        // short of their exact shapes, a bare number and a plain word included.
        let mut items = answers(text);
        items.truncate(limit);
        let app_limit = limit - items.len();

        let usage = self.usage.snapshot();
        let apps = self.with_frecency(|frecency| {
            self.with_ranker(|ranker| {
                ranker.rank_with(text, snapshot, app_limit, |id| {
                    frecency.boost_with(id, now, usage.get(&id).copied().unwrap_or(0.0))
                })
            })
        });

        // `get` rather than indexing: a panic here would abort the process, which is too
        // high a price for an assumption about the ranker's indices.
        items.extend(apps.iter().filter_map(|r| {
            snapshot
                .get(r.index)
                .map(|e| BsResult::new(e, r.score, BS_KIND_APP))
        }));
        self.highlight(text, &mut items);
        leak_results(items, false)
    }

    /// A `?` query: apps and files in one list, ordered by [`crate::relevance::Key`].
    ///
    /// Candidates come from three places, merged and deduplicated by path: the app index,
    /// your home folder listed directly, and `mdfind`. The direct listing is what makes
    /// `?documents` work at all — Spotlight never returns `~/Documents` or `~/Downloads` —
    /// and it answers on the keystroke, while `mdfind` fills in the depths ~100ms later.
    fn file_query(&self, text: &str, limit: usize, snapshot: &[AppEntry], now: u64) -> BsResults {
        let parsed = match crate::query::FileQuery::parse(text) {
            Ok(parsed) => parsed,
            Err(message) => {
                self.files.cancel();
                self.content.cancel_search();
                return leak_results(vec![BsResult::header(message)].into_iter().take(limit).collect(), false);
            }
        };
        // Room is reserved up front so system rows never displace, and never force freeing, file rows.
        let full_limit = limit;
        let system_count = if parsed.filtered() { 0 } else { crate::system::matching(&parsed.name).len().min(3).min(limit) };
        let limit = limit - system_count;
        // A bare `?` is the Files browse mode: nothing to match yet, so the files you
        // opened most recently, rather than the alphabetical head of everything.
        if text.is_empty() {
            self.recent.refresh();
            let recent = self.recent.snapshot();
            let items = recent
                .iter()
                .take(limit)
                .map(|e| BsResult::new(e, 0, BS_KIND_FILE))
                .collect();
            return leak_results(items, self.recent.is_refreshing());
        }

        // Idempotent, so calling it on every keystroke and every poll needs no bookkeeping.
        self.files.search(text);
        let (hits, pending) = self.files.results(text);
        let text = parsed.name.as_str();
        let snapshot = if parsed.filtered() { &[] } else { snapshot };

        let home = self.home.as_deref();
        let (home_entries, home_pending) = if parsed.filtered() { (Vec::new(), false) }
            else { home.map(|path| self.files.home_entries(path)).unwrap_or_default() };
        let mut seen = std::collections::HashSet::new();
        let pool: Vec<AppEntry> = home_entries
            .into_iter()
            .chain(hits)
            .filter(|entry| seen.insert(entry.path.clone()))
            .collect();

        // (key, is_app, index into its own list, score). Indices rather than borrows, so
        // the sort owns nothing that points into either list.
        let usage = self.usage.snapshot();
        let mut ranked: Vec<(crate::relevance::Key, bool, usize, u32)> =
            self.with_frecency(|frecency| {
                self.with_ranker(|ranker| {
                    let boost =
                        |id| frecency.boost_with(id, now, usage.get(&id).copied().unwrap_or(0.0));
                    let apps = ranker.rank_with(text, snapshot, usize::MAX, boost);
                    let files = ranker.rank_with(text, &pool, usize::MAX, boost);
                    let score = |list: &[AppEntry], is_app: bool, r: &crate::matching::Ranked| {
                        let entry = list.get(r.index)?;
                        let candidate = crate::relevance::Candidate {
                            name: &entry.name,
                            path: &entry.path,
                            is_app,
                            fuzzy: r.score,
                            used: if frecency.score(entry.id, now) > 0.0 {
                                crate::relevance::Usage::Launched
                            } else if is_app {
                                if usage.get(&entry.id).copied().unwrap_or(0.0)
                                    >= crate::usage::RECENT_SCORE
                                {
                                    crate::relevance::Usage::Recent
                                } else {
                                    crate::relevance::Usage::Never
                                }
                            } else {
                                crate::relevance::recency(entry.last_used, now)
                            },
                        };
                        let key = crate::relevance::key(&candidate, text, home);
                        Some((key, is_app, r.index, r.score))
                    };
                    apps.iter()
                        .filter_map(|r| score(snapshot, true, r))
                        .chain(files.iter().filter_map(|r| score(&pool, false, r)))
                        .collect()
                })
            });

        ranked.sort_by_key(|r| std::cmp::Reverse(r.0));
        ranked.truncate(limit);

        let mut items: Vec<BsResult> = ranked
            .iter()
            .filter_map(|&(_, is_app, index, score)| {
                if is_app {
                    snapshot
                        .get(index)
                        .map(|e| BsResult::new(e, score, BS_KIND_APP))
                } else {
                    pool.get(index)
                        .map(|e| BsResult::new(e, score, BS_KIND_FILE))
                }
            })
            .collect();
        self.highlight(text, &mut items);
        let (content, content_pending) = if parsed.filtered() {
            self.content.cancel_search(); (None,false)
        } else {self.content.search(text)};
        if let Some(Ok(page)) = content {
            let mut shown: std::collections::HashSet<PathBuf> = ranked.iter().filter_map(|&(_,is_app,index,_)| {
                if is_app {snapshot.get(index)} else {pool.get(index)}
            }).map(|entry|entry.path.clone()).collect();
            for hit in page.hits {
                if items.len()>=limit {break;}
                if shown.insert(PathBuf::from(&hit.path)) {items.push(BsResult::content(&hit,page.limited));}
            }
        }
        let pending = pending || home_pending || content_pending;
        if items.is_empty() && !pending && parsed.filtered() {
            let message = if parsed.used.is_some() {
                "No matches. used: requires Spotlight last-opened dates."
            } else {
                "No files match these filters. Try a broader name or remove one filter."
            };
            items.push(BsResult::header(message));
        }
        if !parsed.filtered() {
            let at = items.iter().position(|item| item.kind != BS_KIND_APP).unwrap_or(items.len());
            let system: Vec<BsResult> = crate::system::matching(&parsed.name).into_iter().take(system_count).map(BsResult::system).collect();
            items.splice(at..at, system);
            // Only once searching settles, so the rows do not flash in and out while mdfind answers.
            if items.len() < 3 && !pending {
                let room = full_limit.saturating_sub(items.len());
                items.extend(self.fallback_rows(&parsed.name).into_iter().take(room));
            }
        }
        leak_results(items, pending)
    }

    fn content_query(&self, text:&str, limit:usize)->BsResults {
        if text.is_empty() {
            self.content.cancel_search();
            return leak_results(vec![BsResult::navigation("Content search settings","content.enabled",&self.content.status(),BS_KIND_SETTING)].into_iter().take(limit).collect(),false);
        }
        let parsed = match crate::query::FileQuery::parse(text) {
            Ok(parsed) => parsed,
            Err(why) => return leak_results(vec![BsResult::header(why)], false),
        };
        let filter = match content_filter(&parsed) {
            Ok(filter) => filter,
            Err(why) => return leak_results(vec![BsResult::header(why)], false),
        };
        if parsed.name.trim().chars().count() < 2 {
            self.content.cancel_search();
            return leak_results(vec![BsResult::header("Add words to search for, e.g. :content kind:pdf modified:month genetec")], false);
        }
        let (page,pending)=self.content.search_matches(&parsed.name, filter);
        let items=match page {
            Some(Ok(page))=> {
                if page.is_empty() && !pending {vec![BsResult::header("No indexed content matches — try fewer words")]}
                else {
                    page.iter().take(limit).map(BsResult::passage).collect()
                }
            },
            Some(Err(_))=>vec![BsResult::header("Content search unavailable — ordinary file search still works")],
            None if pending=>Vec::new(),
            None=>vec![BsResult::navigation("Content search settings","content.enabled",&self.content.status(),BS_KIND_SETTING)],
        };
        leak_results(items.into_iter().take(limit).collect(),pending)
    }

    fn shortcut_rows(&self, command: &crate::shortcuts::Command<'_>, query: &str, limit: usize) -> Vec<BsResult> {
        use crate::shortcuts::{Command, valid_keyword, valid_url};
        const KEYWORD: &str = "Keywords are 1–32 lowercase letters, digits, - or _";
        let usage = |text: &str| vec![BsResult::header(text)];
        let rows = match command {
            Command::SaveLink { url: "", .. } => usage("Type a web address after the keyword, e.g. :link gh https://github.com/search?q={query}"),
            Command::SaveLink { keyword, url } => {
                if !valid_keyword(keyword) {
                    usage(KEYWORD)
                } else if !valid_url(url) {
                    usage("Quick links must start with http:// or https://; put {query} where the search text goes")
                } else {
                    let verb = if self.shortcuts.link(keyword).is_some() { "Replace" } else { "Save" };
                    vec![BsResult::navigation(&format!("{verb} quick link “{keyword}”"), query,
                        &format!("Then type “{keyword} something” to open {url}"), BS_KIND_SHORTCUT)]
                }
            }
            Command::SaveSnippet { text, .. } if text.is_empty() => usage("Type the text after the keyword, e.g. :snippet sig Best regards,\\nSeif"),
            Command::SaveSnippet { keyword, text } => {
                if !valid_keyword(keyword) {
                    usage(KEYWORD)
                } else {
                    let verb = if self.shortcuts.snippets().iter().any(|(saved, _)| saved == keyword) { "Replace" } else { "Save" };
                    vec![BsResult::navigation(&format!("{verb} snippet “{keyword}”"), query,
                        &format!("Then type !{keyword} to paste: {}", one_line(text, 80)), BS_KIND_SHORTCUT)]
                }
            }
            Command::RemoveLink(keyword) => match self.shortcuts.link(keyword) {
                Some(url) => vec![BsResult::navigation(&format!("Remove quick link “{keyword}”"), query, &url, BS_KIND_SHORTCUT)],
                None => usage("No quick link with that keyword · :links lists them"),
            },
            Command::RemoveSnippet(keyword) => match self.shortcuts.snippets().into_iter().find(|(saved, _)| saved == keyword) {
                Some((_, text)) => vec![BsResult::navigation(&format!("Remove snippet “{keyword}”"), query, &one_line(&text, 80), BS_KIND_SHORTCUT)],
                None => usage("No snippet with that keyword · :snippets lists them"),
            },
            Command::ListLinks(filter) => {
                let links: Vec<_> = self.shortcuts.links().into_iter()
                    .filter(|(keyword, url)| filter.is_empty() || keyword.contains(filter) || url.contains(filter)).collect();
                if links.is_empty() {
                    usage("No quick links yet · :link gh https://github.com/search?q={query}")
                } else {
                    links.iter().map(|(keyword, url)| BsResult::navigation(keyword, url, &link_host(url), BS_KIND_LINK)).collect()
                }
            }
            Command::SavePrompt { text, .. } if text.is_empty() => usage("Type the instruction after the keyword, e.g. :prompt tldr Summarize this in one sentence"),
            Command::SavePrompt { keyword, text } => {
                if !valid_keyword(keyword) {
                    usage(KEYWORD)
                } else {
                    let verb = if self.shortcuts.prompts().iter().any(|(saved, _)| saved == keyword) { "Replace" } else { "Save" };
                    vec![BsResult::navigation(&format!("{verb} AI command “{keyword}”"), query,
                        &format!("Then select text anywhere and type ai {keyword}: {}", one_line(text, 80)), BS_KIND_SHORTCUT)]
                }
            }
            Command::RemovePrompt(keyword) => match self.shortcuts.prompts().into_iter().find(|(saved, _)| saved == keyword) {
                Some((_, text)) => vec![BsResult::navigation(&format!("Remove AI command “{keyword}”"), query, &one_line(&text, 80), BS_KIND_SHORTCUT)],
                None => usage("No saved AI command with that keyword · :prompts lists them"),
            },
            Command::ListPrompts(filter) => {
                let filter = filter.to_lowercase();
                crate::shortcuts::BUILT_IN_PROMPTS.iter()
                    .map(|(title, text)| ((*title).to_owned(), (*text).to_owned(), true))
                    .chain(self.shortcuts.prompts().into_iter().map(|(keyword, text)| (format!("ai {keyword}"), text, false)))
                    .filter(|(name, text, _)| filter.is_empty() || name.contains(&filter) || text.to_lowercase().contains(&filter))
                    .map(|(name, text, built_in)| BsResult::navigation(&name, &name,
                        &format!("{} · {}", if built_in { "Built-in AI command" } else { "Saved AI command" }, one_line(&text, 90)), BS_KIND_PROMPT))
                    .collect()
            }
            Command::ListSnippets(filter) => self.snippet_rows(filter, limit),
        };
        rows.into_iter().take(limit).collect()
    }

    /// Offered when a launcher search finds little: search inside documents, ask the documents or the
    /// local model, or open the configured web search. Each is an explicit next step, never automatic.
    fn fallback_rows(&self, text: &str) -> Vec<BsResult> {
        let text = text.trim();
        if text.chars().count() < 2 || text.len() > 512 {
            return Vec::new();
        }
        let mut rows = vec![BsResult::navigation(&format!("Search documents for “{text}”"), &format!(":content {text}"),
            "Fallback · text inside indexed files", BS_KIND_COMMAND)];
        if self.agent.is_some() {
            rows.push(BsResult::navigation(&format!("Ask your documents: “{text}”"), &format!(">docs {text}"),
                "Fallback · local answer with sources", BS_KIND_COMMAND));
            rows.push(BsResult::navigation(&format!("Ask local AI: “{text}”"), &format!(">{text}"), "Fallback · local model", BS_KIND_COMMAND));
        }
        let template = self.config().fallback_search.clone();
        if crate::shortcuts::valid_url(&template) && template.contains("{query}") {
            let host = link_host(&template);
            rows.push(BsResult::navigation(&format!("Search the web for “{text}”"), &crate::shortcuts::expand(&template, text),
                &format!("Fallback · {}", host.trim_start_matches("Quick link · ")), BS_KIND_LINK));
        }
        rows
    }

    fn snippet_rows(&self, filter: &str, limit: usize) -> Vec<BsResult> {
        let filter = filter.to_lowercase();
        let mut snippets: Vec<_> = self.shortcuts.snippets().into_iter().filter_map(|(keyword, text)| {
            let rank = if filter.is_empty() || keyword.starts_with(&filter) { 0 }
                else if keyword.contains(&filter) { 1 }
                else if text.to_lowercase().contains(&filter) { 2 }
                else { return None };
            Some((rank, keyword, text))
        }).collect();
        snippets.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        if snippets.is_empty() {
            return vec![BsResult::header(if filter.is_empty() {
                "No snippets yet · :snippet sig Best regards,\\nSeif · or ⌘K on clipboard text → Save as Snippet"
            } else {
                "No snippet matches · :snippets lists them all"
            })];
        }
        snippets.into_iter().take(limit)
            .map(|(_, keyword, text)| BsResult::navigation(&keyword, &text, &format!("Snippet · {}", one_line(&text, 100)), BS_KIND_SNIPPET))
            .collect()
    }

    /// Marks, on every row that was fuzzy-matched, which characters of its name the query
    /// found.
    ///
    /// A second pass rather than part of ranking: [`crate::matching::Ranker::highlights`]
    /// re-runs the match keeping the position matrix, and only the rows that survived the
    /// cut are ever drawn. Rows that were never matched against anything — a calculated
    /// value, an agent's command, a section title — are left alone.
    fn highlight(&self, text: &str, items: &mut [BsResult]) {
        if text.trim().is_empty() {
            return;
        }
        self.with_ranker(|ranker| {
            for item in items.iter_mut() {
                if !matches!(
                    item.kind,
                    BS_KIND_APP | BS_KIND_FILE | BS_KIND_CLIP_TEXT | BS_KIND_CLIP_IMAGE
                ) {
                    continue;
                }
                // SAFETY: `name` was leaked from a `&str`'s bytes by a `BsResult`
                // constructor moments ago and nothing has freed it, so the pair is valid
                // for this read.
                let name = unsafe { slice_or_empty(item.name, item.name_len) };
                // A bundle name that is not UTF-8 still launches; it simply draws with
                // nothing highlighted.
                let Ok(name) = std::str::from_utf8(name) else {
                    continue;
                };
                let found = ranker.highlights(text, name);
                if !found.is_empty() {
                    let (ptr, len) = leak_u32(&found);
                    item.highlights = ptr;
                    item.highlights_len = len;
                }
            }
        });
    }

    /// A `:` query: what is listening on a port.
    ///
    /// Idempotent like the file search it mirrors — safe to call on every keystroke and
    /// every poll — and `pending` stays true until `lsof` has answered, which is what makes
    /// the shell poll rather than block.
    fn port_query(&self, query: crate::ports::ConsoleQuery, limit: usize) -> BsResults {
        self.ports.search_query(query.clone());
        let (listeners, pending) = self.ports.results_query(&query);
        let listeners = match listeners {
            Ok(listeners) => listeners,
            Err(_) => {
                return leak_results(
                    vec![BsResult::header(
                        "Process inspection unavailable — retry or check permissions",
                    )]
                    .into_iter()
                    .take(limit)
                    .collect(),
                    false,
                );
            }
        };
        if listeners.is_empty() && !pending {
            return leak_results(
                vec![BsResult::header("No matching processes or listeners")]
                    .into_iter()
                    .take(limit)
                    .collect(),
                false,
            );
        }
        let items = listeners
            .iter()
            .take(limit)
            .map(BsResult::listener)
            .collect();
        leak_results(items, pending)
    }

    /// Ranked by recency, not frecency: clips are stored newest first and the ranker
    /// breaks ties by index, so a bare `;` lists the latest copies and a fuzzy tie
    /// favours the more recent one.
    fn clip_query(&self, text: &str, limit: usize) -> BsResults {
        let Some(clips) = &self.clips else {
            return BsResults::empty();
        };
        let mut items = clips.with_entries(|entries, info| {
            let ranked = self.with_ranker(|ranker| ranker.rank(text, entries, limit));
            ranked
                .iter()
                .filter_map(|r| {
                    let entry = entries.get(r.index)?;
                    let clip = info.get(&entry.id)?;
                    let kind = match clip.kind {
                        ClipKind::Text => BS_KIND_CLIP_TEXT,
                        ClipKind::Image => BS_KIND_CLIP_IMAGE,
                    };
                    Some(BsResult::clip(entry, r.score, kind, clip))
                })
                .collect::<Vec<_>>()
        });
        // Outside `with_entries`: that closure already holds the ranker, and taking it
        // again inside would deadlock.
        self.highlight(text, &mut items);
        leak_results(items, false)
    }

    fn record_activation(&self, result_id: u64) {
        let now = unix_now();
        // A reused clip goes back to the top of history and nowhere else. Recording it in
        // frecency would persist a content hash as if it were an app, for no benefit.
        if self.clips.as_ref().is_some_and(|c| c.touch(result_id, now)) {
            return;
        }
        let visit = self.with_frecency(|frecency| frecency.record(result_id, now));

        let Some(store) = self.store.clone() else {
            return;
        };
        // Persisted off-thread. A redb commit is an fsync, and this runs immediately
        // before Swift hands off to Launch Services — the in-memory score is already
        // updated, so ranking reflects the launch whether or not the write has landed.
        let _ = std::thread::Builder::new()
            .name("blindspot-frecency".to_owned())
            .spawn(move || {
                if let Err(e) = store.put(result_id, visit) {
                    eprintln!("blindspot: {e}");
                }
            });
    }

    fn last_scan_at(&self) -> Option<Instant> {
        match self.last_scan.lock() {
            Ok(guard) => *guard,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }

    /// Makes the next `reindex` walk rather than collapse into the rate limit.
    fn clear_last_scan(&self) {
        match self.last_scan.lock() {
            Ok(mut guard) => *guard = None,
            Err(poisoned) => *poisoned.into_inner() = None,
        }
    }

    fn set_last_scan(&self, at: Instant) {
        match self.last_scan.lock() {
            Ok(mut guard) => *guard = Some(at),
            Err(poisoned) => *poisoned.into_inner() = Some(at),
        }
    }

    fn reindex(&self) {
        let now = Instant::now();
        if !should_rescan(self.last_scan_at(), now, RESCAN_INTERVAL) {
            return;
        }

        // Compare-and-exchange rather than load-then-store: the panel kicks a rescan on
        // every show, and two shows in quick succession would otherwise both observe
        // `false` and start a filesystem walk each.
        if self
            .rescanning
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        // Only after winning the exchange, so a call that collapsed into an already
        // running walk does not push the window forward and starve the next real rescan.
        self.set_last_scan(now);

        let guard = RescanGuard(Arc::clone(&self.rescanning));
        let index = Arc::clone(&self.index);
        let usage = Arc::clone(&self.usage);
        // Snapshotted before the spawn, not read inside it: the walk outlives this call
        // and a set landing mid-walk must not change the paths under it.
        let config = self.config();
        let paths = config.resolved_app_paths();
        let half_life = config.frecency.half_life_days;

        // Dropping the `JoinHandle` detaches the thread, which is what we want: nobody
        // joins a rescan. If the spawn itself failed the closure was dropped, taking
        // the guard with it, so the flag is already clear and a later call can retry.
        let _ = std::thread::Builder::new()
            .name("blindspot-reindex".to_owned())
            .spawn(move || {
                let _guard = guard;
                index.replace(apps::scan(&paths));
                usage.replace(crate::usage::fetch_scores(unix_now(), half_life));
            });
    }
}

/// Tool rows if any tool recognises `text`, else the calculator's one row, else nothing.
/// Never both: `0xff` is a tool's, `0xff + 1` is arithmetic.
fn answers(text: &str) -> Vec<BsResult> {
    let tools = crate::tools::evaluate(text);
    if !tools.is_empty() {
        return tools.iter().map(BsResult::tool).collect();
    }
    crate::calc::evaluate(text)
        .map(|value| BsResult::calculated(&crate::calc::format(value)))
        .into_iter()
        .collect()
}

/// Clears the in-progress flag however the rescan ends, panic included, so one failed
/// walk cannot lock out every rescan for the life of the process.
struct RescanGuard(Arc<AtomicBool>);

impl Drop for RescanGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl BsResult {
    fn system(command: &crate::system::SystemCommand) -> Self {
        Self::navigation(command.title, command.id, command.detail, BS_KIND_SYSTEM)
    }

    /// A file a document answer drew on: a real file row, so ↩ opens it and ⌘K offers file actions.
    fn source(row: &crate::agent::session::Row) -> Self {
        let entry = AppEntry::new(row.name.clone(), PathBuf::from(&row.detail));
        let mut result=Self::new(&entry, 0, BS_KIND_FILE);
        if let crate::agent::session::RowKind::Source {page,line}=row.kind {result.page=page;result.line=line;}
        result.id=crate::index::fnv1a(&[b"blindspot:source:",row.detail.as_bytes(),row.name.as_bytes()]);
        result
    }

    /// A passage: the file it belongs to, the text that matched, and where to open it.
    fn passage(found: &crate::content::passage_search::Match) -> Self {
        let passage=&found.passage;
        let entry = AppEntry::new(passage.title.clone(), PathBuf::from(&passage.path));
        let mut result = Self::new(&entry, 0, BS_KIND_FILE);
        let text = one_line(&passage.text, 160);
        result.id ^= (found.chunk_id as u64).rotate_left(17);
        let excerpt = if passage.heading.is_empty() {
            format!("“{text}”")
        } else {
            format!("{} · “{text}”", one_line(&passage.heading, 60))
        };
        let reason=match (found.words,found.meaning) {
            (true,true)=>"Words + meaning",(false,true)=>"Meaning",_=>"Words",
        };
        let detail=format!("{reason} · {excerpt}");
        (result.detail, result.detail_len) = leak_bytes(detail.as_bytes());
        result.page = u32::try_from(passage.page).unwrap_or(0);
        result.line = u32::try_from(passage.line).unwrap_or(0);
        result
    }

    fn content(hit:&crate::content::Hit, limited:bool)->Self {
        let entry=AppEntry::new(hit.title.clone(),PathBuf::from(&hit.path));
        let mut result=Self::new(&entry,0,BS_KIND_FILE);
        let detail=match (&hit.snippet, hit.related) {
            (Some(snippet), true) => format!("Related by meaning · “{}”",one_line(snippet,160)),
            (None, true) => "Related by meaning · approximate, no exact word match".to_owned(),
            (Some(snippet), false) => format!("“{}”",one_line(snippet,160)),
            (None, false) if limited => "Content match · limited relevance; refine query".to_owned(),
            (None, false) => "Content match".to_owned(),
        };
        (result.detail,result.detail_len)=leak_bytes(detail.as_bytes());
        result
    }

    /// A calculator row. No path, because there is nothing on disk to open, and a score
    /// of `u32::MAX` so it can never be sorted under a text match.
    fn calculated(text: &str) -> Self {
        let (name, name_len) = leak_bytes(text.as_bytes());
        let (path, path_len) = leak_bytes(&[]);
        Self {
            id: 0,
            process_pid: 0,
            network_port: 0,
            name,
            name_len,
            path,
            path_len,
            score: u32::MAX,
            kind: BS_KIND_CALC,
            timestamp: 0,
            width: 0,
            height: 0,
            page: 0,
            line: 0,
            detail: std::ptr::null(),
            detail_len: 0,
            highlights: std::ptr::null(),
            highlights_len: 0,
        }
    }

    /// One agent row: a command, a refusal, or the prompt that starts it all.
    fn agent(row: &crate::agent::session::Row, index: u64) -> Self {
        use crate::agent::session::RowKind;
        let kind = match row.kind {
            RowKind::Prompt => BS_KIND_AGENT_PROMPT,
            RowKind::Header => BS_KIND_HEADER,
            RowKind::Step => BS_KIND_AGENT_STEP,
            RowKind::Blocked => BS_KIND_AGENT_BLOCKED,
            RowKind::Ok => BS_KIND_AGENT_OK,
            RowKind::Failed => BS_KIND_AGENT_FAILED,
            RowKind::Answer => BS_KIND_AGENT_ANSWER,
            RowKind::Model => BS_KIND_AGENT_MODEL,
            RowKind::Running => BS_KIND_AGENT_RUNNING,
            RowKind::Past => BS_KIND_AGENT_PAST,
            RowKind::Source {..} => BS_KIND_FILE,
        };
        let (detail, detail_len) = leak_bytes(row.detail.as_bytes());
        Self {
            kind,
            detail,
            detail_len,
            id: index,
            ..Self::calculated(&row.name)
        }
    }

    /// One listening process.
    fn listener(found: &crate::ports::Listener) -> Self {
        let (path, path_len) = leak_bytes(found.path.as_bytes());
        let (detail, detail_len) =
            leak_bytes(format!("PID {} · {} · {}", found.pid, found.protocol, found.address).as_bytes());
        Self {
            kind: BS_KIND_PORT,
            id: found.stable_id(),
            process_pid: found.pid,
            network_port: found.port(),
            timestamp: found.started,
            path,
            path_len,
            detail,
            detail_len,
            ..Self::calculated(&found.command)
        }
    }

    fn navigation(title: &str, target: &str, help: &str, kind: u8) -> Self {
        let (path, path_len) = leak_bytes(target.as_bytes());
        let (detail, detail_len) = leak_bytes(help.as_bytes());
        Self { kind, path, path_len, detail, detail_len,
            id: crate::index::fnv1a(&[b"blindspot:navigation:", &[kind], target.as_bytes()]),
            ..Self::calculated(title) }
    }

    /// A welcome-screen section title.
    fn header(title: &str) -> Self {
        Self {
            kind: BS_KIND_HEADER,
            score: 0,
            ..Self::calculated(title)
        }
    }

    /// A tool row: copied on Enter like a calculated one, with its form as the subtitle.
    fn tool(row: &crate::tools::ToolRow) -> Self {
        let (detail, detail_len) = leak_bytes(row.detail.as_bytes());
        Self {
            kind: BS_KIND_TOOL,
            timestamp: row.timestamp,
            detail,
            detail_len,
            ..Self::calculated(&row.value)
        }
    }

    fn clip(entry: &AppEntry, score: u32, kind: u8, info: &crate::clips::ClipInfo) -> Self {
        let (detail,detail_len)=leak_bytes(if info.pinned {b"Pinned"} else {b""});
        Self {
            detail,detail_len,
            timestamp: info.created,
            width: info.width,
            height: info.height,
            ..Self::new(entry, score, kind)
        }
    }

    fn new(entry: &AppEntry, score: u32, kind: u8) -> Self {
        let (name, name_len) = leak_bytes(entry.name.as_bytes());
        // A macOS path is bytes, not a `String`. Going through `as_bytes` rather than
        // `to_string_lossy` means a bundle with a non-UTF-8 name still launches.
        let (path, path_len) = leak_bytes(entry.path.as_os_str().as_bytes());
        Self {
            id: entry.id,
            process_pid: 0,
            network_port: 0,
            name,
            name_len,
            path,
            path_len,
            score,
            kind,
            timestamp: 0,
            width: 0,
            height: 0,
            page: 0,
            line: 0,
            detail: std::ptr::null(),
            detail_len: 0,
            highlights: std::ptr::null(),
            highlights_len: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Allocation helpers — every `leak_*` here has a matching `free_*` below
// ---------------------------------------------------------------------------

/// An owned message for Swift, empty for "nothing went wrong". Freed by `bs_free_blob`
/// like every other blob, so a refusal costs no new free function.
fn leak_blob(message: &[u8]) -> BsBlob {
    if message.is_empty() {
        return BsBlob {
            data: std::ptr::null(),
            len: 0,
        };
    }
    let (data, len) = leak_bytes(message);
    BsBlob { data, len }
}

fn leak_u32(values: &[u32]) -> (*const u32, usize) {
    let boxed: Box<[u32]> = Box::from(values);
    let len = boxed.len();
    (Box::into_raw(boxed).cast::<u32>(), len)
}

fn leak_bytes(bytes: &[u8]) -> (*const u8, usize) {
    let boxed: Box<[u8]> = Box::from(bytes);
    let len = boxed.len();
    (Box::into_raw(boxed).cast::<u8>(), len)
}

fn leak_results(items: Vec<BsResult>, pending: bool) -> BsResults {
    if items.is_empty() {
        return BsResults {
            pending,
            ..BsResults::empty()
        };
    }
    let boxed = items.into_boxed_slice();
    let len = boxed.len();
    BsResults {
        items: Box::into_raw(boxed).cast::<BsResult>(),
        len,
        pending,
    }
}

/// # Safety
///
/// `ptr` and `len` must be a pair returned by [`leak_bytes`] and not yet freed.
unsafe fn free_bytes(ptr: *const u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: `leak_bytes` produced this pointer from `Box<[u8]>::into_raw` with
    // exactly this length, so reconstituting the same box reclaims the same allocation.
    drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr.cast_mut(), len)) });
}

fn leak_settings(items: Vec<BsSetting>) -> BsSettingList {
    if items.is_empty() {
        return BsSettingList {
            items: std::ptr::null_mut(),
            len: 0,
        };
    }
    let boxed = items.into_boxed_slice();
    let len = boxed.len();
    BsSettingList {
        items: Box::into_raw(boxed).cast::<BsSetting>(),
        len,
    }
}

/// # Safety
///
/// `list` must be a value returned by [`leak_settings`], not yet freed.
unsafe fn free_settings(list: BsSettingList) {
    if list.items.is_null() {
        return;
    }
    // SAFETY: `leak_settings` produced this pointer from `Box<[BsSetting]>::into_raw`
    // with exactly this length.
    let items = unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(list.items, list.len)) };
    for item in &items {
        // SAFETY: every pair came from `leak_bytes` in `BsHandle::settings_list`, and the
        // boxed slice above owns them exclusively, so this is the only reclaim.
        unsafe {
            free_bytes(item.key, item.key_len);
            free_bytes(item.section, item.section_len);
            free_bytes(item.label, item.label_len);
            free_bytes(item.help, item.help_len);
            free_bytes(item.value, item.value_len);
            free_bytes(item.fallback, item.fallback_len);
        }
    }
}

/// # Safety
///
/// `ptr` and `len` must be a pair returned by [`leak_u32`] and not yet freed.
unsafe fn free_u32(ptr: *const u32, len: usize) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: `leak_u32` produced this pointer from `Box<[u32]>::into_raw` with exactly
    // this length, so reconstituting the same box reclaims the same allocation.
    drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr.cast_mut(), len)) });
}

/// # Safety
///
/// `results` must be a value returned by [`leak_results`], not yet freed.
unsafe fn free_results(results: BsResults) {
    if results.items.is_null() {
        return;
    }
    // SAFETY: `leak_results` produced this pointer from `Box<[BsResult]>::into_raw`
    // with exactly this length.
    let items = unsafe {
        Box::from_raw(std::ptr::slice_from_raw_parts_mut(
            results.items,
            results.len,
        ))
    };
    for item in &items {
        // SAFETY: every pair came from `leak_bytes` in a `BsResult` constructor, or is a
        // NULL `detail` that `free_bytes` skips, and the boxed slice above owns them
        // exclusively, so this is the only reclaim.
        unsafe {
            free_bytes(item.name, item.name_len);
            free_bytes(item.path, item.path_len);
            free_bytes(item.detail, item.detail_len);
            free_u32(item.highlights, item.highlights_len);
        }
    }
}

/// # Safety
///
/// `ptr` must be NULL, or valid for reads of `len` bytes for the lifetime `'a`.
unsafe fn slice_or_empty<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        // `from_raw_parts` is undefined for a NULL pointer even at length zero, so an
        // empty buffer from Swift must never reach it.
        return &[];
    }
    // SAFETY: non-NULL and, per the contract, valid for `len` bytes.
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

/// # Safety
///
/// `ptr` must be NULL or a valid NUL-terminated C string alive for this call.
unsafe fn cstr_to_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: delegated to this function's contract.
    let cstr = unsafe { CStr::from_ptr(ptr) };
    // Lossy rather than an error. Swift `String`s are already valid UTF-8, so this only
    // fires for a hand-built buffer, and a mangled query beats a dead one.
    Some(cstr.to_string_lossy().into_owned())
}

pub(crate) fn create_beneath(
    root: &std::path::Path,
    relative: &std::path::Path,
    directory: bool,
    cancel: &AtomicBool,
) -> Result<(), String> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::path::Component;
    fn open_at(fd: i32, name: &std::ffi::CStr, flags: i32) -> Result<OwnedFd, String> {
        // SAFETY: name is NUL terminated, fd is borrowed for the call; mode is supplied for O_CREAT.
        let raw = unsafe {
            libc::openat(
                fd,
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if raw < 0 {
            return Err("File unavailable, already exists, or contains a symlink".into());
        }
        // SAFETY: a successful openat returns a new descriptor owned by this function.
        Ok(unsafe { OwnedFd::from_raw_fd(raw) })
    }
    let mut fd = open_at(libc::AT_FDCWD, c"/", libc::O_RDONLY | libc::O_DIRECTORY)?;
    for component in root.components() {
        if let Component::Normal(name) = component {
            let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| "Invalid path")?;
            fd = open_at(fd.as_raw_fd(), &name, libc::O_RDONLY | libc::O_DIRECTORY)?;
        }
    }
    let components: Vec<_> = relative
        .components()
        .filter(|c| *c != Component::CurDir)
        .collect();
    if components.is_empty() {
        return Err("Name a new file or directory".into());
    }
    for (index, component) in components.iter().enumerate() {
        if cancel.load(Ordering::Acquire) {
            return Err("Cancelled".into());
        }
        let Component::Normal(name) = component else {
            return Err("Invalid path".into());
        };
        let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| "Invalid path")?;
        let last = index + 1 == components.len();
        if last && !directory {
            let _file = open_at(
                fd.as_raw_fd(),
                &name,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            )?;
        } else {
            if directory {
                // SAFETY: fd remains live, name is a single validated component, and mode is valid.
                let made = unsafe { libc::mkdirat(fd.as_raw_fd(), name.as_ptr(), 0o700) };
                if made != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
                {
                    return Err("Could not create directory".into());
                }
            }
            fd = open_at(fd.as_raw_fd(), &name, libc::O_RDONLY | libc::O_DIRECTORY)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    /// Builds a handle without touching the filesystem, so the tests are independent
    /// of what happens to be installed on the machine running them.
    fn handle(names: &[&str]) -> *mut BsHandle {
        let index = Index::new();
        index.replace(
            names
                .iter()
                .map(|n| {
                    AppEntry::new(
                        (*n).to_owned(),
                        PathBuf::from(format!("/Applications/{n}.app")),
                    )
                })
                .collect(),
        );
        Box::into_raw(Box::new(BsHandle {
            // `Overrides::default()` carries no path, so a test can never write to the
            // real state directory — the same reason `Session::model_path` is a field.
            config: Mutex::new(Arc::new(Config::default())),
            file_config: Arc::new(Config::default()),
            overrides: Mutex::new(crate::settings::Overrides::default()),
            config_note: (None, None),
            reachable: Arc::new(Mutex::new(None)),
            index: Arc::new(index),
            ranker: Mutex::new(Ranker::new()),
            rescanning: Arc::new(AtomicBool::new(false)),
            usage: Arc::new(crate::usage::UsageScores::default()),
            recent: Arc::new(crate::recent::RecentFiles::with(Vec::new())),
            last_scan: Mutex::new(None),
            frecency: Mutex::new(Frecency::new(14.0)),
            files: FileSearch::new(),
        content: crate::content_service::ContentService::new(None),
            content_path: None,
            content_stats: Arc::new(Mutex::new(ContentStats::default())),
            content_stats_running: Arc::new(AtomicBool::new(false)),
        resource_sample: Mutex::new(ResourceSample::default()),
            ports: crate::ports::PortSearch::new(),
            home: None,
            clips: Clips::in_memory().ok().map(Arc::new),
            clip_retention: crate::process_job::Latest::default(),
            store: None,
            agent: None,
            shortcuts: crate::shortcuts::Library::open(None),
        }))
    }

    /// Reads a refusal back the way Swift does, then frees it. Empty means it worked.
    fn refusal(blob: BsBlob) -> String {
        // SAFETY: `blob` came straight from a setting call, so the pair is either NULL
        // with a zero length or one live allocation of exactly that length.
        let text =
            String::from_utf8_lossy(unsafe { slice_or_empty(blob.data, blob.len) }).into_owned();
        // SAFETY: freed exactly once, and nothing reads it after.
        unsafe { bs_free_blob(blob) };
        text
    }

    #[test]
    fn content_settings_index_and_search_share_the_production_abi() {
        let directory=std::env::temp_dir().join(format!("blindspot-content-ffi-{}",std::process::id()));
        std::fs::create_dir(&directory).expect("fixture");
        let directory=directory.canonicalize().expect("canonical");
        let root=directory.join("root");std::fs::create_dir(&root).expect("root");
        let file=root.join("notes.txt");std::fs::write(&file,"zscontentfixture transactional migrations").expect("document");
        let h=handle(&["zscontentfixture"]);
        // SAFETY: this test exclusively owns the handle, before any content work is started.
        unsafe {(*h).content=crate::content_service::ContentService::new(Some(directory.join("content.sqlite")));}
        let set=|key:&str,value:&str| {
            let key=CString::new(key).expect("key");
            // SAFETY: the live handle and input buffers remain valid throughout the call.
            assert!(refusal(unsafe {bs_setting_set(h,key.as_ptr(),value.as_ptr(),value.len())}).is_empty());
        };
        set("content.roots",root.to_str().expect("root path"));
        set("content.enabled","true");
        // SAFETY: h remains live until the final shutdown below.
        unsafe {bs_content_pause(h,false,0);}
        let started=Instant::now();
        loop {
            // SAFETY: h is owned by this test; snapshot uses internal synchronization.
            let phase=unsafe {(*h).content.snapshot().phase};
            if phase!=crate::content_service::Phase::Indexing {assert_eq!(phase,crate::content_service::Phase::Ready);break;}
            assert!(started.elapsed()<Duration::from_secs(5));std::thread::sleep(Duration::from_millis(5));
        }
        let query=|text:&str| {
            let text=CString::new(text).expect("query");let started=Instant::now();
            loop {
                // SAFETY: h and the terminated text remain live; each returned list is freed once.
                let results=unsafe {bs_query(h,text.as_ptr(),20)};
                let mut found=Vec::new();
                if !results.items.is_null() {
                    // SAFETY: bs_query returned the pointer/length pair, still live before free_results.
                    for row in unsafe {std::slice::from_raw_parts(results.items,results.len)} {
                        // SAFETY: row paths are owned by the live result list and have these lengths.
                        let path=String::from_utf8_lossy(unsafe {slice_or_empty(row.path,row.path_len)}).into_owned();
                        found.push((row.kind,path));
                    }
                }
                let pending=results.pending;
                // SAFETY: exactly this returned result list is freed, once, after copying fields.
                unsafe {bs_free_results(results);}
                if !pending {break found;}
                assert!(started.elapsed()<Duration::from_secs(5));std::thread::sleep(Duration::from_millis(5));
            }
        };
        let content=query(":content zscontentfixture");
        assert_eq!(content.len(),1);assert_eq!(content[0].0,BS_KIND_FILE);
        assert_eq!(std::path::Path::new(&content[0].1),file);
        let combined=query("zscontentfixture");
        assert_eq!(combined.first().map(|row|row.0),Some(BS_KIND_APP));
        assert!(combined.iter().any(|row|row.0==BS_KIND_FILE && std::path::Path::new(&row.1)==file));
        for (suffix,relevant) in [("notes.txt",true),(".local/share/blindspot/content.sqlite-wal",false),("node_modules/cache.js",false)] {
            let path=CString::new(root.join(suffix).to_str().expect("event path")).expect("path");
            // SAFETY: the handle and path buffer outlive this call.
            assert_eq!(unsafe {bs_content_event_relevant(h,path.as_ptr())},relevant);
        }
        set("content.enabled","false");
        assert!(query(":content zscontentfixture").iter().all(|row|row.0!=BS_KIND_FILE));
        // SAFETY: this test owns the live handle and frees each returned blob through refusal.
        assert!(!refusal(unsafe {bs_content_erase(h,false)}).is_empty());
        assert_eq!(crate::content::ContentStore::open_reader(&directory.join("content.sqlite")).expect("reader").count().expect("preserved"),1);
        // SAFETY: confirmation is supplied only after the explicit refusal assertion, with indexing disabled.
        assert!(refusal(unsafe {bs_content_erase(h,true)}).is_empty());
        let started=Instant::now();
        loop {
            // SAFETY: this test retains h while the asynchronous worker completes.
            if !unsafe {(*h).content.erasing()} { break; }
            assert!(started.elapsed()<Duration::from_secs(5));std::thread::sleep(Duration::from_millis(5));
        }
        // SAFETY: h remains live until shutdown below.
        assert_eq!(unsafe {(*h).content.snapshot().phase},crate::content_service::Phase::Erased);
        assert_eq!(crate::content::ContentStore::open_reader(&directory.join("content.sqlite")).expect("reader").count().expect("erased"),0);
        assert!(file.exists());
        // SAFETY: h is exclusively owned by this test and is shut down exactly once.
        unsafe {bs_shutdown(h);}
        std::fs::remove_dir_all(directory).expect("cleanup");
    }

    /// Reads the settings list back the way Swift does — key, value, source — then frees it.
    fn settings_of(h: *mut BsHandle) -> Vec<(String, String, u8)> {
        // SAFETY: `h` is live and this call does not outlive it.
        let list = unsafe { bs_settings_list(h) };
        let rows = if list.items.is_null() {
            Vec::new()
        } else {
            // SAFETY: `bs_settings_list` returned this pointer and length together, so
            // they describe one live boxed slice.
            unsafe { std::slice::from_raw_parts(list.items, list.len) }
                .iter()
                .map(|item| {
                    // SAFETY: every pair in a live row is NULL-with-zero or valid for its
                    // stated length, which is what `slice_or_empty` requires.
                    let read = |ptr, len| unsafe {
                        String::from_utf8_lossy(slice_or_empty(ptr, len)).into_owned()
                    };
                    (
                        read(item.key, item.key_len),
                        read(item.value, item.value_len),
                        item.source,
                    )
                })
                .collect()
        };
        // SAFETY: freed exactly once, after the last read above.
        unsafe { bs_free_settings(list) };
        rows
    }

    fn value_of(h: *mut BsHandle, key: &str) -> Option<(String, u8)> {
        settings_of(h)
            .into_iter()
            .find(|(k, _, _)| k == key)
            .map(|(_, value, source)| (value, source))
    }

    #[test]
    fn setting_tags_match_the_c_constants() {
        // The Rust enums and the `BS_SETTING_*` constants are two lists Swift has to agree
        // with. A mismatch would draw the wrong control for a row, silently.
        use crate::settings::Kind;
        assert_eq!(Kind::Flag.tag(), BS_SETTING_FLAG);
        assert_eq!(Kind::Count { min: 0, max: 1 }.tag(), BS_SETTING_COUNT);
        assert_eq!(Kind::Number { min: 0.0, max: 1.0 }.tag(), BS_SETTING_NUMBER);
        assert_eq!(Kind::Text.tag(), BS_SETTING_TEXT);
        assert_eq!(Kind::Chord.tag(), BS_SETTING_CHORD);
        assert_eq!(Kind::Paths.tag(), BS_SETTING_PATHS);
        assert_eq!(Kind::Readonly.tag(), BS_SETTING_READONLY);

        use crate::settings::Source;
        assert_eq!(Source::Default as u8, BS_SOURCE_DEFAULT);
        assert_eq!(Source::ConfigFile as u8, BS_SOURCE_CONFIG);
        assert_eq!(Source::Override as u8, BS_SOURCE_OVERRIDE);
    }

    #[test]
    fn content_state_carries_the_index_overview() {
        let h = handle(&["Safari"]);
        // SAFETY: a live handle; the blob is copied before its single free.
        let blob = unsafe { bs_content_state(h) };
        // SAFETY: Rust allocated the blob with exactly this length and nothing has freed it yet.
        let bytes = unsafe { slice_or_empty(blob.data, blob.len) }.to_vec();
        // SAFETY: freed once, after the copy above.
        unsafe { bs_free_blob(blob) };
        let state: serde_json::Value = serde_json::from_slice(&bytes).expect("state JSON");
        let overview = &state["overview"];
        for key in ["state", "stage", "message", "pass", "roots", "semantic", "disk", "processes", "pacing", "busy", "sampled", "kinds", "folders", "attention", "recent"] {
            assert!(!overview[key].is_null(), "overview.{key} missing");
        }
        assert!(overview["processes"].as_array().is_some_and(|rows| rows.iter().any(|row| row["name"] == "Blindspot")));
        // Lets the shell's decoder be checked against what the core really emits.
        if let Some(path) = std::env::var_os("BLINDSPOT_OVERVIEW_OUT") {
            std::fs::write(path, &bytes).expect("overview fixture");
        }
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn every_setting_crosses_the_boundary_intact() {
        let h = handle(&["Safari"]);
        let rows = settings_of(h);
        // The schema, then the rows that can only be measured. A `status.` key is not in
        // the schema by design: nothing sets it, so there is nothing to validate or store.
        let (measured, settable): (Vec<_>, Vec<_>) = rows
            .iter()
            .partition(|(key, _, _)| key.starts_with("status."));
        assert_eq!(settable.len(), crate::settings::SCHEMA.len());
        assert!(!measured.is_empty(), "the Status page should have rows");
        for (key, _, source) in &settable {
            assert!(
                crate::settings::def(key).is_some(),
                "{key:?} came back but is not in the schema"
            );
            assert_eq!(*source, BS_SOURCE_DEFAULT, "{key:?} on a default handle");
        }
        // A path list arrives NUL-separated, which is the one byte a pathname cannot hold.
        let (paths, _) = value_of(h, "app_paths").expect("app_paths");
        assert!(paths.contains('\0'), "{paths:?}");
        assert_eq!(paths.split('\0').count(), Config::default().app_paths.len());
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn a_set_takes_effect_and_a_reset_undoes_it() {
        let h = handle(&["Safari"]);
        // SAFETY: a live handle and a NUL-terminated key alive for the call.
        let key = CString::new("max_results").expect("no interior NUL");
        assert_eq!(
            refusal(unsafe { bs_setting_set(h, key.as_ptr(), b"12".as_ptr(), 2) }),
            ""
        );
        assert_eq!(
            value_of(h, "max_results"),
            Some(("12".into(), BS_SOURCE_OVERRIDE))
        );
        // And the rest of the core sees it, not just the settings list.
        // SAFETY: a live handle.
        assert_eq!(unsafe { bs_max_results(h) }, 12);

        // SAFETY: as above.
        assert_eq!(refusal(unsafe { bs_setting_reset(h, key.as_ptr()) }), "");
        assert_eq!(
            value_of(h, "max_results"),
            Some((Config::default().max_results.to_string(), BS_SOURCE_DEFAULT))
        );
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn a_refused_set_says_why_and_changes_nothing() {
        let h = handle(&["Safari"]);
        let before = unsafe { bs_max_results(h) };

        let key = CString::new("max_results").expect("no interior NUL");
        // SAFETY: a live handle and a key alive for the call.
        let why = refusal(unsafe { bs_setting_set(h, key.as_ptr(), b"99".as_ptr(), 2) });
        assert!(why.contains("1"), "should name the range: {why:?}");
        // SAFETY: a live handle.
        assert_eq!(unsafe { bs_max_results(h) }, before);

        let bogus = CString::new("not.a.setting").expect("no interior NUL");
        // SAFETY: as above.
        let why = refusal(unsafe { bs_setting_set(h, bogus.as_ptr(), b"1".as_ptr(), 1) });
        assert!(why.contains("not a setting"), "{why:?}");

        // A chord goes through the same parser config.toml is read with.
        let hotkey = CString::new("hotkey").expect("no interior NUL");
        // SAFETY: as above.
        let why =
            refusal(unsafe { bs_setting_set(h, hotkey.as_ptr(), b"shift+space".as_ptr(), 11) });
        assert!(
            !why.is_empty(),
            "a chord with no real modifier should be refused"
        );
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn a_null_handle_never_crashes_a_setting_call() {
        let key = CString::new("max_results").expect("no interior NUL");
        // SAFETY: a NULL handle is explicitly allowed by every one of these.
        unsafe {
            let list = bs_settings_list(std::ptr::null());
            assert_eq!(list.len, 0);
            bs_free_settings(list);
            assert_eq!(
                refusal(bs_setting_set(
                    std::ptr::null_mut(),
                    key.as_ptr(),
                    b"1".as_ptr(),
                    1
                )),
                "no handle"
            );
            assert_eq!(
                refusal(bs_setting_reset(std::ptr::null_mut(), key.as_ptr())),
                "no handle"
            );
            assert_eq!(
                refusal(bs_setting_set(
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    b"".as_ptr(),
                    0
                )),
                "no handle"
            );
        }
    }

    /// Reads results back the way Swift does, then frees them.
    fn drain(results: BsResults) -> Vec<(String, String, u64)> {
        let out = if results.items.is_null() {
            Vec::new()
        } else {
            // SAFETY: `results` came straight from `bs_query`, so the pointer and
            // length describe one live boxed slice.
            let items = unsafe { std::slice::from_raw_parts(results.items, results.len) };
            items
                .iter()
                .map(|r| {
                    // SAFETY: same, for the byte buffers each result owns.
                    let name = unsafe { std::slice::from_raw_parts(r.name, r.name_len) };
                    let path = unsafe { std::slice::from_raw_parts(r.path, r.path_len) };
                    (
                        String::from_utf8_lossy(name).into_owned(),
                        String::from_utf8_lossy(path).into_owned(),
                        r.id,
                    )
                })
                .collect()
        };
        // SAFETY: exactly one free, of exactly what `bs_query` returned.
        unsafe { bs_free_results(results) };
        out
    }

    /// As `query`, but also reports whether a file search is still running.
    fn query_pending(h: *mut BsHandle, q: &str) -> bool {
        let c = CString::new(q).expect("test query has no interior NUL");
        // SAFETY: `h` is live for the duration of each test and `c` outlives the call.
        let results = unsafe { bs_query(h, c.as_ptr(), 8) };
        let pending = results.pending;
        // SAFETY: exactly one free, of exactly what `bs_query` returned.
        unsafe { bs_free_results(results) };
        pending
    }

    fn query(h: *mut BsHandle, q: &str, limit: usize) -> Vec<(String, String, u64)> {
        let c = CString::new(q).expect("test query has no interior NUL");
        // SAFETY: `h` is live for the duration of each test and `c` outlives the call.
        drain(unsafe { bs_query(h, c.as_ptr(), limit) })
    }

    #[test]
    fn system_fallback_and_ai_command_rows_come_from_the_core() {
        let h = handle(&["Safari"]);
        let rows = query(h, "sleep", 8);
        assert!(rows.iter().any(|row| row.0 == "Sleep" && row.1 == "sleep"), "{rows:?}");
        // Fallbacks wait for file search to settle, so poll until it has.
        for _ in 0..200 {
            if !query_pending(h, "zzqxw unmatched") { break; }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let rows = query(h, "zzqxw unmatched", 8);
        assert!(rows.iter().any(|row| row.0 == "Search documents for “zzqxw unmatched”" && row.1 == ":content zzqxw unmatched"), "{rows:?}");
        assert!(rows.iter().any(|row| row.1 == "https://duckduckgo.com/?q=zzqxw%20unmatched"), "{rows:?}");
        assert!(query(h, ":system", 20).len() >= 10);
        let resolve = |text: &str| {
            let text = CString::new(text).unwrap();
            // SAFETY: a live handle and a NUL-terminated string alive for the call.
            blob_text(unsafe { bs_prompt_resolve(h, text.as_ptr()) })
        };
        assert!(resolve("fix grammar").contains("grammar"));
        assert_eq!(resolve("mail"), "");
        let save = CString::new(":prompt tldr Summarize in one sentence.").unwrap();
        // SAFETY: as above.
        assert_eq!(blob_text(unsafe { bs_shortcut_apply(h, save.as_ptr()) }), "");
        assert_eq!(resolve("ai tldr"), "Summarize in one sentence.");
        assert_eq!(resolve("tldr"), "");
        assert!(query(h, ":prompts", 20).iter().any(|row| row.0 == "ai tldr"));
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn a_passage_row_carries_the_place_to_open_it() {
        let page = BsResult::passage(&crate::content::passage_search::Match { chunk_id:1,revision:1,words:true,meaning:false,passage:crate::content::Passage {
            document_id: 1,
            path: "/fixture/report.pdf".into(),
            title: "report.pdf".into(),
            ordinal: 3,
            page: 12,
            line: 0,
            heading: "Renewal".into(),
            text: "The deadline is October 31".into(),
            rank: -1.0,
        }});
        assert_eq!((page.page, page.line), (12, 0));
        // SAFETY: the row owns the bytes it just leaked, and they outlive this read.
        let detail = unsafe { std::slice::from_raw_parts(page.detail, page.detail_len) };
        let detail = String::from_utf8_lossy(detail);
        assert!(detail.contains("Renewal") && detail.contains("October 31"), "{detail}");

        let code = BsResult::passage(&crate::content::passage_search::Match { chunk_id:2,revision:1,words:true,meaning:false,passage:crate::content::Passage {
            document_id: 2,
            path: "/fixture/store.rs".into(),
            title: "store.rs".into(),
            ordinal: 0,
            page: 0,
            line: 340,
            heading: "fn rebuild_shard".into(),
            text: "let shard = publish();".into(),
            rank: -2.0,
        }});
        assert_eq!((code.page, code.line), (0, 340));
        assert_eq!(code.kind, BS_KIND_FILE, "a passage is still a file row");
    }

    fn blob_text(blob: BsBlob) -> String {
        // SAFETY: Rust allocated the blob with exactly this length; it is copied before its single free.
        let text = String::from_utf8_lossy(unsafe { slice_or_empty(blob.data, blob.len) }).into_owned();
        // SAFETY: freed once, after the copy above.
        unsafe { bs_free_blob(blob) };
        text
    }

    #[test]
    fn quick_links_and_snippets_save_open_and_refuse_unsafe_targets() {
        let h = handle(&["Safari"]);
        let save = ":link gh https://x.test/search?q={query}";
        let rows = query(h, save, 8);
        assert_eq!((rows[0].0.as_str(), rows[0].1.as_str()), ("Save quick link “gh”", save));
        let command = CString::new(save).unwrap();
        // SAFETY: a live handle and a NUL-terminated command alive for the call.
        assert_eq!(blob_text(unsafe { bs_shortcut_apply(h, command.as_ptr()) }), "");
        let opened = query(h, "gh hello world", 8);
        assert_eq!((opened[0].0.as_str(), opened[0].1.as_str()), ("gh · hello world", "https://x.test/search?q=hello%20world"));
        let refused = query(h, ":link bad file:///etc/passwd", 8);
        assert!(refused[0].0.contains("http") && refused[0].1.is_empty(), "{refused:?}");
        let bad = CString::new(":link bad file:///etc/passwd").unwrap();
        // SAFETY: as above.
        assert!(!blob_text(unsafe { bs_shortcut_apply(h, bad.as_ptr()) }).is_empty());
        let keyword = CString::new("sig").unwrap();
        let text = "Best regards,\nSeif";
        // SAFETY: a live handle, a NUL-terminated keyword and a buffer valid for its length.
        assert_eq!(blob_text(unsafe { bs_snippet_save(h, keyword.as_ptr(), text.as_ptr(), text.len()) }), "");
        let snippets = query(h, "!si", 8);
        assert_eq!((snippets[0].0.as_str(), snippets[0].1.as_str()), ("sig", text));
        let remove = CString::new(":unlink gh").unwrap();
        // SAFETY: as above.
        assert_eq!(blob_text(unsafe { bs_shortcut_apply(h, remove.as_ptr()) }), "");
        assert!(query(h, "gh hello world", 8).iter().all(|row| !row.1.starts_with("https://x.test")));
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn a_query_round_trips_through_free() {
        let h = handle(&["Safari", "Slack", "Terminal"]);
        let got = query(h, "sl", 8);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "Slack");
        assert_eq!(got[0].1, "/Applications/Slack.app");
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn the_id_matches_the_one_the_index_assigned() {
        let h = handle(&["Slack"]);
        let expected = AppEntry::new("Slack".into(), "/Applications/Slack.app".into()).id;
        assert_eq!(query(h, "slack", 8)[0].2, expected);
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn many_queries_free_cleanly() {
        let h = handle(&["Safari", "Slack", "Stocks", "System Settings", "Terminal"]);
        for _ in 0..100 {
            let _ = query(h, "s", 8);
            let _ = query(h, "", 8);
            let _ = query(h, "zzzz", 8);
        }
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn the_limit_is_honoured() {
        let h = handle(&["Safari", "Slack", "Stocks", "System Settings"]);
        assert_eq!(query(h, "s", 2).len(), 2);
        assert!(query(h, "s", 0).is_empty());
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn launching_an_app_promotes_it_for_later_queries() {
        // The M3 acceptance test, and CLAUDE.md's own example: a single character ties
        // every fuzzy score, so cold order is alphabetical and only frecency can break it.
        let h = handle(&["Safari", "Slack", "Stocks"]);
        let slack = AppEntry::new("Slack".into(), "/Applications/Slack.app".into()).id;

        assert_eq!(
            query(h, "s", 8)[0].0,
            "Safari",
            "cold, the tie breaks alphabetically"
        );

        // SAFETY: `h` is live and `slack` is one of its ids.
        unsafe { bs_activate(h, slack) };
        assert_eq!(
            query(h, "s", 8)[0].0,
            "Slack",
            "one launch should win the tie"
        );

        // A better textual match still beats a favourite.
        assert_eq!(query(h, "sto", 8)[0].0, "Stocks");

        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn activating_an_unknown_id_is_harmless() {
        let h = handle(&["Safari"]);
        // SAFETY: NULL and unknown ids are both part of the contract.
        unsafe { bs_activate(h, 0xDEAD_BEEF) };
        assert_eq!(query(h, "s", 8)[0].0, "Safari");
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    fn add_text_clip(h: *mut BsHandle, text: &str) {
        let clip = BsClip {
            kind: BS_KIND_CLIP_TEXT,
            content: text.as_ptr(),
            content_len: text.len(),
            thumbnail: std::ptr::null(),
            thumbnail_len: 0,
            text: std::ptr::null(),
            text_len: 0,
            width: 0,
            height: 0,
        };
        // SAFETY: `h` is live, and `clip` plus the buffer it points at outlive the call.
        unsafe { bs_clip_add(h, &clip) };
    }

    #[test]
    fn a_clip_round_trips_from_add_to_query_to_content() {
        let h = handle(&["Safari"]);
        add_text_clip(h, "kubectl get pods -A");

        let rows = query(h, ";kube", 8);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "kubectl get pods -A");
        let id = rows[0].2;

        // SAFETY: `h` is live; the blob is freed exactly once below.
        let blob = unsafe { bs_clip_content(h, id, BS_CLIP_FULL) };
        assert!(!blob.data.is_null());
        // SAFETY: the blob came straight from `bs_clip_content`.
        let bytes = unsafe { std::slice::from_raw_parts(blob.data, blob.len) };
        assert_eq!(bytes, b"kubectl get pods -A");
        // SAFETY: one free, of exactly what `bs_clip_content` returned.
        unsafe { bs_free_blob(blob) };

        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn a_bare_clip_prefix_lists_newest_first_and_never_apps() {
        let h = handle(&["Safari", "Slack"]);
        add_text_clip(h, "first copy");
        add_text_clip(h, "second copy");
        let names: Vec<String> = query(h, ";", 8).into_iter().map(|(n, _, _)| n).collect();
        assert_eq!(
            names,
            ["second copy", "first copy"],
            "apps must not leak in"
        );
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn reusing_a_clip_bumps_it_and_records_no_frecency() {
        let h = handle(&["Safari"]);
        add_text_clip(h, "older");
        add_text_clip(h, "newer");
        let older = query(h, ";older", 8)[0].2;

        // SAFETY: `h` is live.
        unsafe { bs_activate(h, older) };
        let names: Vec<String> = query(h, ";", 8).into_iter().map(|(n, _, _)| n).collect();
        assert_eq!(names, ["older", "newer"], "reuse moves it to the top");

        // SAFETY: `h` is live.
        let recorded = unsafe { (*h).with_frecency(|f| f.score(older, unix_now())) };
        assert_eq!(
            recorded, 0.0,
            "a content hash must never be recorded as an app"
        );
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn clip_entry_points_survive_null_and_nonsense() {
        let h = handle(&["Safari"]);
        // SAFETY: NULL and unknown ids are all part of the contracts.
        unsafe {
            bs_clip_add(std::ptr::null_mut(), std::ptr::null());
            bs_clip_add(h, std::ptr::null());
            let blob = bs_clip_content(h, 0xDEAD_BEEF, BS_CLIP_FULL);
            assert!(blob.data.is_null() && blob.len == 0);
            bs_free_blob(blob);
            let blob = bs_clip_content(std::ptr::null_mut(), 1, BS_CLIP_FULL);
            bs_free_blob(blob);
        }
        // An unknown kind is refused rather than stored as something it is not.
        let bogus = BsClip {
            kind: 99,
            content: b"x".as_ptr(),
            content_len: 1,
            thumbnail: std::ptr::null(),
            thumbnail_len: 0,
            text: std::ptr::null(),
            text_len: 0,
            width: 0,
            height: 0,
        };
        // SAFETY: `h` is live and `bogus` outlives the call.
        unsafe { bs_clip_add(h, &bogus) };
        assert!(query(h, ";", 8).is_empty());
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn trailing_whitespace_changes_nothing() {
        // What a pasted line leaves behind: the field turns its final newline into a space.
        let h = handle(&["Safari", "Slack", "Stocks"]);
        assert_eq!(query(h, "sa ", 8), query(h, "sa", 8));
        assert_eq!(query(h, "12 * 34 ", 8), query(h, "12 * 34", 8));
        assert_eq!(query(h, "@1757548800 ", 8), query(h, "@1757548800", 8));
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn an_expression_answers_above_the_apps() {
        let h = handle(&["Safari", "Slack"]);
        let rows = query(h, "12 * 34", 8);
        assert_eq!(rows[0].0, "408", "the answer takes the top row");
        assert_eq!(rows[0].1, "", "a calculator row has no path to open");

        // A plain name must never be hijacked by the calculator.
        let rows = query(h, "s", 8);
        assert!(rows.iter().all(|(name, _, _)| name != "408"));

        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    /// A welcome screen's rows as (kind, name), for a handle whose recent files are `files`.
    fn welcome(h: *mut BsHandle, files: &[&str]) -> (Vec<(u8, String)>, bool) {
        let recent = files
            .iter()
            .map(|n| AppEntry::new((*n).to_owned(), PathBuf::from(format!("/Users/seif/{n}"))))
            .collect();
        // SAFETY: `h` is live and nothing else touches it during the swap.
        unsafe { (*h).recent = Arc::new(crate::recent::RecentFiles::with(recent)) };
        let c = CString::new("").expect("no interior NUL");
        // SAFETY: `h` is live and `c` outlives the call.
        let results = unsafe { bs_query(h, c.as_ptr(), 50) };
        let rows = if results.items.is_null() {
            Vec::new()
        } else {
            // SAFETY: `results` came straight from `bs_query`, and each name buffer is
            // owned by it.
            let items = unsafe { std::slice::from_raw_parts(results.items, results.len) };
            items
                .iter()
                .map(|r| {
                    // SAFETY: same, for the name buffer each result owns.
                    let name = unsafe { std::slice::from_raw_parts(r.name, r.name_len) };
                    (r.kind, String::from_utf8_lossy(name).into_owned())
                })
                .collect()
        };
        let pending = results.pending;
        // SAFETY: exactly one free, of exactly what `bs_query` returned.
        unsafe { bs_free_results(results) };
        (rows, pending)
    }

    #[test]
    fn the_welcome_screen_is_used_apps_then_recent_files() {
        let h = handle(&["Safari", "Slack", "Stocks", "Xcode", "Zed"]);
        for (name, times) in [("Zed", 3), ("Slack", 1)] {
            let id = query(h, name, 8)[0].2;
            for _ in 0..times {
                // SAFETY: `h` is live.
                unsafe { bs_activate(h, id) };
            }
        }
        let (rows, pending) = welcome(h, &["resume.pdf", "Downloads"]);
        assert_eq!(
            rows,
            [
                (BS_KIND_HEADER, "Suggested".to_owned()),
                (BS_KIND_APP, "Zed".to_owned()),
                (BS_KIND_APP, "Slack".to_owned()),
                (BS_KIND_HEADER, "Recent files".to_owned()),
                (BS_KIND_FILE, "resume.pdf".to_owned()),
                (BS_KIND_FILE, "Downloads".to_owned()),
            ],
            "most-used first; never-used apps are not suggestions"
        );
        assert!(!pending, "a fresh cache starts no fetch");

        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn the_welcome_screen_fits_max_results_and_drops_empty_sections() {
        let names: Vec<String> = (0..20).map(|i| format!("App{i:02}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let h = handle(&refs);
        for name in &refs {
            let id = query(h, name, 8)[0].2;
            // SAFETY: `h` is live.
            unsafe { bs_activate(h, id) };
        }
        let files: Vec<String> = (0..20).map(|i| format!("file{i}")).collect();
        let file_refs: Vec<&str> = files.iter().map(String::as_str).collect();

        let (rows, _) = welcome(h, &file_refs);
        let count = |kind| rows.iter().filter(|(k, _)| *k == kind).count();
        // max_results is 8 by default: two headers in one row's height, then 4 + 3.
        assert_eq!((count(BS_KIND_APP), count(BS_KIND_FILE)), (4, 3));

        let (rows, _) = welcome(h, &["only.txt"]);
        let count = |kind| rows.iter().filter(|(k, _)| *k == kind).count();
        assert_eq!(
            (count(BS_KIND_APP), count(BS_KIND_FILE)),
            (6, 1),
            "unused rows move over"
        );

        let (rows, _) = welcome(h, &[]);
        assert!(
            !rows.iter().any(|(_, name)| name == "Recent files"),
            "no header over an empty section"
        );

        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn a_bare_prefix_browses_recent_files() {
        let h = handle(&["Safari"]);
        let (_, _) = welcome(h, &["a.txt", "b.txt"]);
        let rows = query(h, "?", 8);
        assert_eq!(
            rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
            ["a.txt", "b.txt"]
        );
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn tool_rows_carry_their_kind_detail_and_instant() {
        let h = handle(&["Safari"]);
        let c = CString::new("@1757548800").expect("no interior NUL");
        // SAFETY: `h` is live and `c` outlives the call.
        let results = unsafe { bs_query(h, c.as_ptr(), 8) };
        // SAFETY: `results` came straight from `bs_query`.
        let items = unsafe { std::slice::from_raw_parts(results.items, results.len) };
        let read = |ptr: *const u8, len: usize| {
            // SAFETY: each pair is a live buffer owned by `results`, or NULL with len 0.
            (!ptr.is_null())
                .then(|| unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec())
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default()
        };
        assert_eq!(items.len(), 2, "two forms, and nothing else matches");
        assert!(
            items
                .iter()
                .all(|r| r.kind == BS_KIND_TOOL && r.path_len == 0)
        );
        assert_eq!(
            read(items[0].name, items[0].name_len),
            "2025-09-11 00:00:00 UTC"
        );
        assert_eq!(items[0].timestamp, 1_757_548_800);
        assert_eq!(read(items[1].detail, items[1].detail_len), "ISO 8601");
        // SAFETY: exactly one free, of exactly what `bs_query` returned.
        unsafe { bs_free_results(results) };

        let rows = query(h, "12 * 34", 8);
        assert_eq!(
            rows.len(),
            1,
            "arithmetic is one calculator row, not tool rows"
        );
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn an_answer_costs_one_row_not_the_whole_list() {
        let h = handle(&["Safari", "Slack", "Stocks", "System Settings"]);
        let rows = query(h, "2+2", 3);
        assert_eq!(
            rows.len(),
            1,
            "nothing else matches '2+2', so just the answer"
        );
        assert_eq!(rows[0].0, "4");
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn ordinary_text_starts_file_search_and_commands_cancel_it() {
        let h = handle(&["Safari", "Slack"]);
        query(h, "saf", 8);

        // SAFETY: `h` is live.
        let files = unsafe { &(*h).files };
        let started = std::time::Instant::now();
        while files.results("saf").1 && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        query(h, ">help", 8);
        assert!(files.results("saf").0.is_empty());
        assert!(!files.results("saf").1);

        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn question_desktop_puts_the_folder_first_and_finds_what_spotlight_hides() {
        let home = std::env::temp_dir().join(format!("blindspot-fq-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        for dir in ["Desktop", "Documents", "Downloads", "blindspot"] {
            std::fs::create_dir_all(home.join(dir)).expect("mkdir");
        }
        let h = handle(&["Podman Desktop", "Safari"]);
        // SAFETY: `h` is live and nothing else touches it during the test.
        unsafe { (*h).home = Some(home.clone()) };

        // SAFETY: h remains live and this method only touches its synchronized cache.
        let files = unsafe { &(*h).files };
        let started = Instant::now();
        while files.home_entries(&home).1 && started.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(5));
        }

        // The first call returns before `mdfind` finishes, so this is the direct listing
        // plus the app index alone — deterministic, and the path that must answer instantly.
        let names = |q: &str| -> Vec<String> { query(h, q, 8).into_iter().map(|r| r.0).collect() };
        assert_eq!(names("?desktop")[..2], ["Desktop", "Podman Desktop"]);
        assert_eq!(
            names("?documents")[0],
            "Documents",
            "Spotlight never returns this one"
        );
        assert_eq!(names("?downloads")[0], "Downloads", "nor this one");
        assert_eq!(names("?blindspot")[0], "blindspot");
        assert_eq!(names("documents")[0], "Documents", "plain search finds files too");

        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn filtered_file_queries_do_not_include_unfiltered_app_candidates() {
        let h = handle(&["Synthetic-filter-bypass-pdf"]);
        let rows = query(h, "?kind:pdf", 8);
        assert!(rows.iter().all(|row| row.0 != "Synthetic-filter-bypass-pdf"));
        let invalid = query(h, "?size:18446744073709551615GB", 8);
        assert_eq!(invalid.first().map(|row| row.0.as_str()), Some("File size is too large"));
        assert!(!query_pending(h, "?size:18446744073709551615GB"));
        // SAFETY: exactly one shutdown, with no subsequent use of h.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn a_bare_prefix_returns_nothing_rather_than_the_app_index() {
        let h = handle(&["Safari", "Slack", "Stocks"]);
        assert!(query(h, "?", 8).is_empty());
        assert!(!query_pending(h, "?"));
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn a_prefixed_query_still_ranks_apps() {
        let h = handle(&["Safari", "Slack"]);
        // `?s` sits under the spawn floor and the handle has no home folder, so this sees
        // the app index alone. Both prefix-match equally, and the shorter name wins the
        // tie: it carries less unrelated text around the match.
        let names: Vec<String> = query(h, "?s", 8).into_iter().map(|(n, _, _)| n).collect();
        assert_eq!(
            names,
            ["Slack", "Safari"],
            "apps rank against the stripped query"
        );
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn a_prefixed_query_under_the_floor_does_not_leave_the_caller_polling() {
        let h = handle(&["Safari"]);
        assert!(!query_pending(h, "?a"), "the floor must not spawn or hang");
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn the_agent_prefix_says_so_when_the_agent_is_off() {
        // `handle()` builds a core with no session, which is what `enabled = false` produces.
        let h = handle(&["Safari"]);
        let c = CString::new("> in ~/dev make a folder").expect("no interior NUL");
        // SAFETY: `h` is live and `c` outlives the call.
        let results = unsafe { bs_query(h, c.as_ptr(), 8) };
        // SAFETY: `results` came straight from `bs_query`.
        let items = unsafe { std::slice::from_raw_parts(results.items, results.len) };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, BS_KIND_AGENT_BLOCKED);
        // SAFETY: the name buffer is owned by `results`.
        let name = unsafe { std::slice::from_raw_parts(items[0].name, items[0].name_len) };
        assert_eq!(String::from_utf8_lossy(name), "The agent is off");
        assert!(!results.pending, "nothing is running");
        // SAFETY: exactly one free.
        unsafe { bs_free_results(results) };

        // And the entry points are inert rather than fatal with the agent off.
        let request = CString::new("> anything").expect("no interior NUL");
        // SAFETY: `h` is live for all three calls.
        unsafe {
            bs_agent_submit(h, request.as_ptr());
            bs_agent_run(h);
            bs_agent_cancel(h);
            bs_shutdown(h);
        }
    }

    #[test]
    fn a_null_handle_is_survivable_on_every_entry_point() {
        let c = CString::new("slack").expect("no interior NUL");
        // SAFETY: NULL is explicitly part of every entry point's contract.
        unsafe {
            assert!(drain(bs_query(std::ptr::null_mut(), c.as_ptr(), 8)).is_empty());
            assert_eq!(bs_max_results(std::ptr::null()), 0);
            bs_activate(std::ptr::null_mut(), 1);
            bs_reindex(std::ptr::null_mut());
            bs_agent_submit(std::ptr::null_mut(), c.as_ptr());
            bs_agent_run(std::ptr::null_mut());
            bs_agent_cancel(std::ptr::null_mut());
            bs_shutdown(std::ptr::null_mut());
            bs_free_results(BsResults::empty());
        }
    }

    #[test]
    fn a_null_query_is_treated_as_an_empty_one() {
        let h = handle(&["Calendar", "Mail"]);
        // SAFETY: NULL is part of `bs_query`'s contract; `h` is live.
        let got = drain(unsafe { bs_query(h, std::ptr::null(), 8) });
        // The welcome screen, which with nothing used and nothing recent has no rows —
        // and in particular not the alphabetical head of the index.
        assert_eq!(got, query(h, "", 8));
        assert!(got.is_empty());
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn a_rescan_is_due_only_once_the_interval_has_elapsed() {
        let interval = Duration::from_secs(10);
        let now = Instant::now();

        assert!(should_rescan(None, now, interval), "never scanned");
        assert!(!should_rescan(Some(now), now, interval), "just scanned");
        assert!(!should_rescan(
            Some(now),
            now + Duration::from_secs(9),
            interval
        ));
        assert!(
            should_rescan(Some(now), now + interval, interval),
            "exactly due"
        );
        assert!(should_rescan(
            Some(now),
            now + Duration::from_secs(60),
            interval
        ));
    }

    #[test]
    fn a_backwards_clock_does_not_panic_or_force_a_rescan() {
        let interval = Duration::from_secs(10);
        let now = Instant::now();
        let future = now + Duration::from_secs(60);
        assert!(!should_rescan(Some(future), now, interval));
    }

    #[test]
    fn settings_default_to_the_working_hotkey_and_login_on() {
        // SAFETY: NULL is part of the contract.
        let s = unsafe { bs_startup(std::ptr::null()) };
        assert_eq!(
            (s.hotkey_key_code, s.hotkey_modifiers),
            (0x31, 0x100 | 0x200)
        );
        assert!(s.launch_at_login);
        let h = handle(&[]);
        // SAFETY: `h` is live.
        let s = unsafe { bs_startup(h) };
        assert!(s.hotkey_from_config, "the built-in default string parses");
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn a_second_hotkey_is_offered_only_when_it_is_configured() {
        // SAFETY: NULL is part of the contract.
        let s = unsafe { bs_startup(std::ptr::null()) };
        assert_eq!(s.agent_hotkey_key_code, 0, "none by default");

        let mut config = Config::default();
        assert_eq!(config.agent_hotkey(), None);
        config.agent_hotkey = "cmd+shift+space".to_owned();
        assert_eq!(
            config.agent_hotkey(),
            Some(crate::hotkey::Hotkey {
                key_code: 0x31,
                modifiers: crate::hotkey::CMD | crate::hotkey::SHIFT,
            })
        );
        // Nonsense is dropped rather than turned into some other chord.
        config.agent_hotkey = "cmd+nonsense".to_owned();
        assert_eq!(config.agent_hotkey(), None);
    }

    #[test]
    fn max_results_reports_the_config() {
        let h = handle(&[]);
        // SAFETY: `h` is live.
        assert_eq!(unsafe { bs_max_results(h) }, Config::default().max_results);
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }

    #[test]
    fn content_stats_is_explicitly_unavailable_without_a_database() {
        let stats = content_stats(None, &[]);
        assert!(!stats.sampled);
        assert!(stats.database_bytes.is_none());
        assert!(stats.documents.is_none());
        assert!(stats.vector_catalog_bytes.is_none());
    }

    #[test]
    fn content_stats_reads_records_and_extraction_outcomes_from_store() {
        let root = std::env::temp_dir().join(format!("blindspot-content-stats-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("fixture directory");
        let database = root.join("content.sqlite");
        let mut store = crate::content::ContentStore::open(&database).expect("content store");
        let scan = store.begin_scan(&root).expect("scan");
        store.put_batch(&scan, &[crate::content::Document {
            identity: "fixture-id", path: &root.join("report.pdf"), title: "report.pdf", body: "",
            modified_ns: 1, changed_ns: 1, bytes: 128, extraction: crate::content::Extraction::Oversized,
        }]).expect("record");
        drop(store);
        let stats = content_stats(Some(&database), &[]);
        assert!(stats.sampled);
        assert!(stats.database_bytes.is_some());
        assert_eq!(stats.documents, Some(1));
        assert_eq!(stats.extraction_counts, Some([0, 0, 1, 0]));
        assert_eq!(stats.vector_catalog_bytes, Some(0));
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn a_non_utf8_bundle_path_survives_the_round_trip() {
        use std::ffi::OsString;

        let index = Index::new();
        // SAFETY-adjacent: 0xff is not valid UTF-8, which is legal in a macOS path.
        let raw = OsString::from(unsafe {
            String::from_utf8_unchecked(b"/Applications/\xffx.app".to_vec())
        });
        index.replace(vec![AppEntry::new("Odd".into(), PathBuf::from(raw))]);
        let h = Box::into_raw(Box::new(BsHandle {
            // `Overrides::default()` carries no path, so a test can never write to the
            // real state directory — the same reason `Session::model_path` is a field.
            config: Mutex::new(Arc::new(Config::default())),
            file_config: Arc::new(Config::default()),
            overrides: Mutex::new(crate::settings::Overrides::default()),
            config_note: (None, None),
            reachable: Arc::new(Mutex::new(None)),
            index: Arc::new(index),
            ranker: Mutex::new(Ranker::new()),
            rescanning: Arc::new(AtomicBool::new(false)),
            usage: Arc::new(crate::usage::UsageScores::default()),
            recent: Arc::new(crate::recent::RecentFiles::with(Vec::new())),
            last_scan: Mutex::new(None),
            frecency: Mutex::new(Frecency::new(14.0)),
            files: FileSearch::new(),
        content: crate::content_service::ContentService::new(None),
            content_path: None,
            content_stats: Arc::new(Mutex::new(ContentStats::default())),
            content_stats_running: Arc::new(AtomicBool::new(false)),
        resource_sample: Mutex::new(ResourceSample::default()),
            ports: crate::ports::PortSearch::new(),
            home: None,
            clips: Clips::in_memory().ok().map(Arc::new),
            clip_retention: crate::process_job::Latest::default(),
            store: None,
            agent: None,
            shortcuts: crate::shortcuts::Library::open(None),
        }));

        let got = query(h, "odd", 8);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1.len(), "/Applications/\u{fffd}x.app".len());
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }
}
