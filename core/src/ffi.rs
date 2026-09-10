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
    config: Config,
    /// `Arc` because a background rescan thread outlives the call that started it and
    /// may still be walking when `bs_shutdown` drops the handle.
    index: Arc<Index>,
    /// Long-lived: the ranker owns the matcher's scratch matrix and the parsed pattern,
    /// and rebuilding those per keystroke is the allocation this design exists to avoid.
    /// A mutex rather than a `RefCell` because nothing in a C ABI stops Swift from
    /// calling on a second thread; the lock is uncontended in practice.
    ranker: Mutex<Ranker>,
    rescanning: Arc<AtomicBool>,
    /// When the last rescan started. Seeded at init, which has already scanned.
    last_scan: Mutex<Option<Instant>>,
    frecency: Mutex<Frecency>,
    files: FileSearch,
    /// `None` if the store could not be opened. Frecency then works for the session and
    /// is forgotten at exit, which is a far better failure than refusing to launch.
    store: Option<Arc<Store>>,
}

/// A [`BsResult`] that is an application bundle.
pub const BS_KIND_APP: u8 = 0;
/// A [`BsResult`] that is a file found through Spotlight.
pub const BS_KIND_FILE: u8 = 1;

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
    /// [`BS_KIND_APP`] or [`BS_KIND_FILE`]. Swift needs it to choose between launching
    /// an application and opening a document in whatever owns it.
    pub kind: u8,
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
/// A NULL handle or a NULL query yields an empty result set rather than a crash. A
/// `limit` of zero yields no results; ask [`bs_max_results`] for the configured cap.
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
    catch_unwind(AssertUnwindSafe(|| handle.config.max_results)).unwrap_or(0)
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
    let config = match path.as_deref().map(Config::load) {
        Some(Ok(config)) => config,
        Some(Err(e)) => {
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

    Box::into_raw(Box::new(BsHandle {
        config,
        index,
        ranker: Mutex::new(Ranker::new()),
        rescanning: Arc::new(AtomicBool::new(false)),
        last_scan: Mutex::new(Some(Instant::now())),
        frecency: Mutex::new(frecency),
        files: FileSearch::new(),
        store,
    }))
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

    fn with_frecency<T>(&self, f: impl FnOnce(&mut Frecency) -> T) -> T {
        match self.frecency.lock() {
            Ok(mut guard) => f(&mut guard),
            Err(poisoned) => f(&mut poisoned.into_inner()),
        }
    }

    fn query(&self, query: &str, limit: usize) -> BsResults {
        let snapshot = self.index.snapshot();
        let now = unix_now();

        let file_query = crate::files::strip_prefix(query);
        let text = file_query.unwrap_or(query);

        // A bare `?` has nothing to match yet. Falling through would rank the empty
        // string against the app index and return its alphabetical head — exactly the
        // noise the empty state exists to suppress.
        if file_query.is_some() && text.is_empty() {
            return BsResults::empty();
        }

        // Kicked and read in one breath: `search` is idempotent, so calling it on every
        // keystroke and every poll needs no bookkeeping here.
        let (hits, pending) = match file_query {
            Some(text) => {
                self.files.search(text);
                self.files.results(text)
            }
            None => (Vec::new(), false),
        };

        let (apps, files) = self.with_frecency(|frecency| {
            self.with_ranker(|ranker| {
                let apps = ranker.rank_with(text, &snapshot, limit, |id| frecency.boost(id, now));
                // Apps first, files fill what is left. `mdfind` returns paths in no
                // useful order, so ranking their basenames is real value for free — and
                // it picks up frecency through the same boost.
                let remaining = limit.saturating_sub(apps.len());
                let files = if remaining > 0 && !hits.is_empty() {
                    ranker.rank_with(text, &hits, remaining, |id| frecency.boost(id, now))
                } else {
                    Vec::new()
                };
                (apps, files)
            })
        });

        // `get` rather than indexing: a panic here would abort the process, which is too
        // high a price for an assumption about the ranker's indices.
        let mut items: Vec<BsResult> = apps
            .iter()
            .filter_map(|r| {
                snapshot
                    .get(r.index)
                    .map(|e| BsResult::new(e, r.score, BS_KIND_APP))
            })
            .collect();
        items.extend(files.iter().filter_map(|r| {
            hits.get(r.index)
                .map(|e| BsResult::new(e, r.score, BS_KIND_FILE))
        }));

        leak_results(items, pending)
    }

    fn record_activation(&self, result_id: u64) {
        let now = unix_now();
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
        let paths = self.config.resolved_app_paths();

        // Dropping the `JoinHandle` detaches the thread, which is what we want: nobody
        // joins a rescan. If the spawn itself failed the closure was dropped, taking
        // the guard with it, so the flag is already clear and a later call can retry.
        let _ = std::thread::Builder::new()
            .name("blindspot-reindex".to_owned())
            .spawn(move || {
                let _guard = guard;
                index.replace(apps::scan(&paths));
            });
    }
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
        }
    }
}

// ---------------------------------------------------------------------------
// Allocation helpers — every `leak_*` here has a matching `free_*` below
// ---------------------------------------------------------------------------

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
        // SAFETY: both pairs came from `leak_bytes` in `BsResult::new`, and the boxed
        // slice above owns them exclusively, so this is the only reclaim.
        unsafe {
            free_bytes(item.name, item.name_len);
            free_bytes(item.path, item.path_len);
        }
    }
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
            config: Config::default(),
            index: Arc::new(index),
            ranker: Mutex::new(Ranker::new()),
            rescanning: Arc::new(AtomicBool::new(false)),
            last_scan: Mutex::new(None),
            frecency: Mutex::new(Frecency::new(14.0)),
            files: FileSearch::new(),
            store: None,
        }))
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
        // `?s` sits under the spawn floor, so this proves apps rank against the stripped
        // query without the test depending on the filesystem or spawning anything.
        let names: Vec<String> = query(h, "?s", 8).into_iter().map(|(n, _, _)| n).collect();
        assert_eq!(
            names,
            ["Safari", "Slack"],
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
    fn a_null_handle_is_survivable_on_every_entry_point() {
        let c = CString::new("slack").expect("no interior NUL");
        // SAFETY: NULL is explicitly part of every entry point's contract.
        unsafe {
            assert!(drain(bs_query(std::ptr::null_mut(), c.as_ptr(), 8)).is_empty());
            assert_eq!(bs_max_results(std::ptr::null()), 0);
            bs_activate(std::ptr::null_mut(), 1);
            bs_reindex(std::ptr::null_mut());
            bs_shutdown(std::ptr::null_mut());
            bs_free_results(BsResults::empty());
        }
    }

    #[test]
    fn a_null_query_is_treated_as_an_empty_one() {
        let h = handle(&["Calendar", "Mail"]);
        // SAFETY: NULL is part of `bs_query`'s contract; `h` is live.
        let got = drain(unsafe { bs_query(h, std::ptr::null(), 8) });
        assert_eq!(got.len(), 2, "empty query returns the head of the index");
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
            config: Config::default(),
            index: Arc::new(index),
            ranker: Mutex::new(Ranker::new()),
            rescanning: Arc::new(AtomicBool::new(false)),
            last_scan: Mutex::new(None),
            frecency: Mutex::new(Frecency::new(14.0)),
            files: FileSearch::new(),
            store: None,
        }));

        let got = query(h, "odd", 8);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1.len(), "/Applications/\u{fffd}x.app".len());
        // SAFETY: one shutdown, no calls after it.
        unsafe { bs_shutdown(h) };
    }
}
