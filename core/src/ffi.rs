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

/// How often a rescan may actually run.
///
/// The panel asks for one on every show, and a walk of the configured paths costs about
/// 30ms of disk. Collapsing a burst of invocations into a single walk is free; the price
/// is that a newly installed app can take this long to surface. FSEvents is the real
/// answer to that and belongs at M4.
const RESCAN_INTERVAL: Duration = Duration::from_secs(10);

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
    /// Whose top-level folders `?` lists directly. Held rather than read from `$HOME` on
    /// each query so tests can point it somewhere controlled — otherwise their results
    /// depend on whatever is in the home folder of whoever runs them.
    home: Option<PathBuf>,
    /// `None` only if even an in-memory store could not be created. Clipboard history is
    /// then unavailable, and everything else carries on.
    clips: Option<Clips>,
    /// `None` if the store could not be opened. Frecency then works for the session and
    /// is forgotten at exit, which is a far better failure than refusing to launch.
    store: Option<Arc<Store>>,
    /// `None` when `agent.enabled` is false, which is the only state in which blindspot
    /// opens no socket at all.
    agent: Option<crate::agent::session::Session>,
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

impl BsSetting {
    /// A row that only reports. No source and no bounds, because nothing set it.
    fn reading(key: &str, label: &str, help: &str, value: &str) -> Self {
        let (key, key_len) = leak_bytes(key.as_bytes());
        let (section, section_len) =
            leak_bytes(crate::settings::Section::Status.name().as_bytes());
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
            session.submit(request.trim(), handle.home.as_deref());
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
        u32::try_from(handle.clips.as_ref().map_or(0, Clips::clear)).unwrap_or(u32::MAX)
    }))
    .unwrap_or(0)
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
        home: crate::files::home_dir(),
        clips: {
            let opened = open_clips();
            if let Some(clips) = &opened {
                clips.keep(keep);
            }
            opened
        },
        store,
        agent: config_agent,
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
        let (kept, bytes) = self.clips.as_ref().map_or((0, 0), Clips::stats);
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

        vec![
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
                use std::net::{TcpStream, ToSocketAddrs};
                let answered = host
                    .to_socket_addrs()
                    .ok()
                    .and_then(|mut found| found.next())
                    .is_some_and(|addr| {
                        TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok()
                    });
                match slot.lock() {
                    Ok(mut guard) => *guard = Some(answered),
                    Err(poisoned) => *poisoned.into_inner() = Some(answered),
                }
            });
    }

    /// Records `key` as set and applies it, or says why it was refused.
    fn apply_setting(&self, key: &str, value: &str) -> BsBlob {
        let Some(def) = crate::settings::def(key) else {
            return leak_blob(format!("{key:?} is not a setting").as_bytes());
        };
        if let Err(why) = crate::settings::validate(def, value) {
            return leak_blob(why.as_bytes());
        }
        self.with_overrides(|o| o.set(key, value));
        self.recompute();
        leak_blob(b"")
    }

    /// Forgets what the window set for `key`, back to config.toml and then the built-in.
    fn reset_setting(&self, key: &str) -> BsBlob {
        if crate::settings::def(key).is_none() {
            return leak_blob(format!("{key:?} is not a setting").as_bytes());
        }
        self.with_overrides(|o| o.reset(key));
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
        if let Some(clips) = &self.clips {
            clips.keep(keep);
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
        // Clipboard history is its own mode, not mixed into app results: clipboard contents
        // appearing among launcher results would be noise, and a privacy leak on screen.
        if let Some(rest) = query.strip_prefix(crate::clips::PREFIX) {
            return self.clip_query(rest.trim_start(), limit);
        }
        if let Some(request) = query.strip_prefix(crate::agent::PREFIX) {
            return self.agent_query(request.trim_start(), limit);
        }
        let snapshot = self.index.snapshot();
        let now = unix_now();
        if query.trim().is_empty() {
            return self.welcome(limit, &snapshot, now);
        }
        match crate::files::strip_prefix(query) {
            Some(text) => self.file_query(text, limit, &snapshot, now),
            None => self.app_query(query, limit, &snapshot, now),
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
            .map(|(at, row)| BsResult::agent(row, at as u64))
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

    /// A plain query: the calculator, then apps by fuzzy score and frecency — M3's ranking,
    /// unchanged. Files only ever appear behind `?`.
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

        let home = self.home.as_deref();
        let mut seen = std::collections::HashSet::new();
        let pool: Vec<AppEntry> = home
            .map(crate::files::home_entries)
            .unwrap_or_default()
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
        leak_results(items, pending)
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
    /// A calculator row. No path, because there is nothing on disk to open, and a score
    /// of `u32::MAX` so it can never be sorted under a text match.
    fn calculated(text: &str) -> Self {
        let (name, name_len) = leak_bytes(text.as_bytes());
        let (path, path_len) = leak_bytes(&[]);
        Self {
            id: 0,
            name,
            name_len,
            path,
            path_len,
            score: u32::MAX,
            kind: BS_KIND_CALC,
            timestamp: 0,
            width: 0,
            height: 0,
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
        Self {
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
            name,
            name_len,
            path,
            path_len,
            score,
            kind,
            timestamp: 0,
            width: 0,
            height: 0,
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
            home: None,
            clips: Clips::in_memory().ok(),
            store: None,
            agent: None,
        }))
    }

    /// Reads a refusal back the way Swift does, then frees it. Empty means it worked.
    fn refusal(blob: BsBlob) -> String {
        // SAFETY: `blob` came straight from a setting call, so the pair is either NULL
        // with a zero length or one live allocation of exactly that length.
        let text = String::from_utf8_lossy(unsafe { slice_or_empty(blob.data, blob.len) })
            .into_owned();
        // SAFETY: freed exactly once, and nothing reads it after.
        unsafe { bs_free_blob(blob) };
        text
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
        assert_eq!(
            Kind::Number { min: 0.0, max: 1.0 }.tag(),
            BS_SETTING_NUMBER
        );
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
    fn every_setting_crosses_the_boundary_intact() {
        let h = handle(&["Safari"]);
        let rows = settings_of(h);
        // The schema, then the rows that can only be measured. A `status.` key is not in
        // the schema by design: nothing sets it, so there is nothing to validate or store.
        let (measured, settable): (Vec<_>, Vec<_>) =
            rows.iter().partition(|(key, _, _)| key.starts_with("status."));
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
        assert_eq!(value_of(h, "max_results"), Some(("12".into(), BS_SOURCE_OVERRIDE)));
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
        let why = refusal(unsafe {
            bs_setting_set(h, hotkey.as_ptr(), b"shift+space".as_ptr(), 11)
        });
        assert!(!why.is_empty(), "a chord with no real modifier should be refused");
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
            assert_eq!(refusal(bs_setting_set(std::ptr::null_mut(), std::ptr::null(), b"".as_ptr(), 0)), "no handle");
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
    fn a_query_without_the_prefix_never_starts_a_file_search() {
        // The whole promise of the explicit-prefix decision: no `mdfind` behind your back.
        let h = handle(&["Safari", "Slack"]);
        assert!(!query_pending(h, "saf"), "a plain query is never pending");

        // SAFETY: `h` is live.
        let files = unsafe { &(*h).files };
        let (results, pending) = files.results("saf");
        assert!(
            results.is_empty() && !pending,
            "nothing was ever searched for"
        );

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

        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
        let _ = std::fs::remove_dir_all(&home);
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
            home: None,
            clips: Clips::in_memory().ok(),
            store: None,
            agent: None,
        }));

        let got = query(h, "odd", 8);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1.len(), "/Applications/\u{fffd}x.app".len());
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }
}
