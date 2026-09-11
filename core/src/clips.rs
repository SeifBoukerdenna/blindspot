//! Clipboard history: dedup, retention, ranking entries and persistence.
//!
//! Swift reads the pasteboard, because only AppKit can. Everything after that — deciding
//! what is a duplicate, what to evict, what to rank, what to write to disk — lives here,
//! per the CLAUDE.md split.
//!
//! Every clip is stored in redb, never as a loose file. Files under the data directory
//! would be one `mdfind` away from being surfaced by Spotlight, and by blindspot's own `?`
//! file search, which is exactly where clipboard contents must not turn up.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use redb::backends::InMemoryBackend;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

use crate::index::{AppEntry, fnv1a};
use crate::store::{StoreError, data_dir};

/// A query beginning with this searches clipboard history instead of the app index.
pub const PREFIX: char = ';';

/// Most clips kept. Oldest go first.
const MAX_CLIPS: usize = 200;

/// Most stored bytes across every clip, content and thumbnail together.
///
/// A count cap alone does not bound disk: 200 Retina screenshots at several megabytes
/// each is over a gigabyte. Eviction runs until *both* limits hold.
const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// Per-item ceilings. Larger items are skipped rather than truncated — pasting half a
/// log file back would be silently wrong, which is worse than not having it.
const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 25 * 1024 * 1024;

/// How much of a text clip is searchable. The ranker scores every character of every
/// candidate, and 200 megabyte-long haystacks per keystroke would blow the frame budget
/// for no gain — nobody searches for a word on line four thousand.
const SEARCH_CHARS: usize = 500;

/// Mixed into every clip id so that one can never equal the id of an app or a file,
/// both of which hash a path. Without it, copying a path string would make the clip and
/// the file share an id, and `bs_activate` could not tell them apart.
const DOMAIN: &[u8] = b"blindspot:clip:";

/// `id -> (kind, created, width, height, stored bytes, searchable name)`.
const META: TableDefinition<u64, (u8, u64, u32, u32, u64, &str)> = TableDefinition::new("meta");
const CONTENT: TableDefinition<u64, &[u8]> = TableDefinition::new("content");
const THUMB: TableDefinition<u64, &[u8]> = TableDefinition::new("thumb");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipKind {
    Text,
    Image,
}

impl ClipKind {
    fn to_byte(self) -> u8 {
        match self {
            Self::Text => 0,
            Self::Image => 1,
        }
    }

    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Text),
            1 => Some(Self::Image),
            _ => None,
        }
    }
}

/// Which bytes of a clip to fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// The UTF-8 text, or the full PNG.
    Full,
    /// A small PNG for the result row. Empty for text clips.
    Thumbnail,
}

/// A clip as it arrives from Swift, borrowing Swift's buffers for the length of the call.
pub struct NewClip<'a> {
    pub kind: ClipKind,
    pub content: &'a [u8],
    pub thumbnail: &'a [u8],
    /// For images, the text recognised in them, which becomes their name — so a screenshot
    /// of an error is found by `;error` and titled by its first line. Ignored for text
    /// clips, whose content already is their text.
    pub text: &'a [u8],
    pub width: u32,
    pub height: u32,
}

/// What a result row needs to know beyond the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipInfo {
    pub kind: ClipKind,
    /// Unix seconds, bumped whenever the clip is reused.
    pub created: u64,
    /// Pixel dimensions of an image clip, zero for text. Public so a result row can tell
    /// a full-screen screenshot (its size matches a display) from any other image.
    pub width: u32,
    pub height: u32,
    bytes: u64,
}

#[derive(Default)]
struct State {
    /// Most recent first. `Ranker` breaks ties by index, so this ordering is what makes
    /// a bare `;` return newest first and an equal fuzzy score favour the recent clip.
    entries: Vec<AppEntry>,
    info: HashMap<u64, ClipInfo>,
    total_bytes: u64,
}

impl State {
    fn position(&self, id: u64) -> Option<usize> {
        self.entries.iter().position(|e| e.id == id)
    }

    fn bump(&mut self, id: u64, now: u64) -> bool {
        let Some(at) = self.position(id) else {
            return false;
        };
        let entry = self.entries.remove(at);
        self.entries.insert(0, entry);
        if let Some(info) = self.info.get_mut(&id) {
            info.created = now;
        }
        true
    }

    fn insert_front(&mut self, entry: AppEntry, info: ClipInfo) {
        self.total_bytes += info.bytes;
        self.info.insert(entry.id, info);
        self.entries.insert(0, entry);
    }

    /// Drops the oldest clips until both limits hold, returning their ids so the caller
    /// can delete their rows. Never evicts the newest: per-item caps sit well under the
    /// byte budget, so one clip alone can never be over it.
    fn evict(&mut self) -> Vec<u64> {
        let mut gone = Vec::new();
        while self.entries.len() > 1
            && (self.entries.len() > MAX_CLIPS || self.total_bytes > MAX_TOTAL_BYTES)
        {
            let Some(entry) = self.entries.pop() else {
                break;
            };
            if let Some(info) = self.info.remove(&entry.id) {
                self.total_bytes = self.total_bytes.saturating_sub(info.bytes);
            }
            gone.push(entry.id);
        }
        gone
    }
}

pub struct Clips {
    db: Database,
    state: Mutex<State>,
}

impl Clips {
    /// `~/.local/share/blindspot/clips.redb` — its own file, so that clearing clipboard
    /// history is deleting one file and cannot touch launch history.
    pub fn default_path() -> Result<PathBuf, StoreError> {
        Ok(data_dir()?.join("clips.redb"))
    }

    /// Opens and rehydrates, creating the file and directory if needed.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(StoreError::Io)?;
        }
        let db = Database::create(path).map_err(|e| StoreError::Open(Box::new(e)))?;
        Self::from_db(db)
    }

    /// History for this session only. Used when the file cannot be opened — losing
    /// history at quit beats losing the launcher — and by tests, which then run against
    /// real redb rather than a stand-in.
    pub fn in_memory() -> Result<Self, StoreError> {
        let db = Database::builder()
            .create_with_backend(InMemoryBackend::new())
            .map_err(|e| StoreError::Open(Box::new(e)))?;
        Self::from_db(db)
    }

    fn from_db(db: Database) -> Result<Self, StoreError> {
        let mut rows = Vec::new();
        {
            let read = db.begin_read()?;
            match read.open_table(META) {
                Ok(table) => {
                    for row in table.iter()? {
                        let (id, value) = row?;
                        let (kind, created, width, height, bytes, name) = value.value();
                        if let Some(kind) = ClipKind::from_byte(kind) {
                            let info = ClipInfo {
                                kind,
                                created,
                                width,
                                height,
                                bytes,
                            };
                            let name = if kind == ClipKind::Image && is_legacy_image_name(name) {
                                "Image".to_owned()
                            } else {
                                name.to_owned()
                            };
                            rows.push((id.value(), name, info));
                        }
                    }
                }
                Err(redb::TableError::TableDoesNotExist(_)) => {}
                Err(e) => return Err(e.into()),
            }
        }
        // Newest first, matching the order `add` maintains.
        rows.sort_by_key(|row| std::cmp::Reverse(row.2.created));

        let mut state = State::default();
        for (id, name, info) in rows.into_iter().rev() {
            state.insert_front(entry(id, name), info);
        }
        Ok(Self {
            db,
            state: Mutex::new(state),
        })
    }

    /// Records a clip, returning its id, or `None` if it was rejected.
    ///
    /// Content already present is bumped to the top rather than stored twice: the id is
    /// a hash of the bytes, so identical content always lands on the same id.
    pub fn add(&self, clip: NewClip<'_>, now: u64) -> Option<u64> {
        let name = searchable_name(&clip)?;
        let id = fnv1a(&[DOMAIN, &[clip.kind.to_byte()], clip.content]);

        if self.touch(id, now) {
            return Some(id);
        }

        let info = ClipInfo {
            kind: clip.kind,
            created: now,
            width: clip.width,
            height: clip.height,
            bytes: (clip.content.len() + clip.thumbnail.len()) as u64,
        };

        // Written before the clip is published in memory, so a query can never return
        // an id whose content is not on disk yet. And written *without* the state lock,
        // so a keystroke never waits on an fsync behind a 25 MB screenshot.
        if let Err(e) = self.write(id, &name, &info, clip.content, clip.thumbnail) {
            eprintln!("blindspot: could not save clip: {e}");
            return None;
        }

        let evicted = {
            let mut state = lock(&self.state);
            state.insert_front(entry(id, name), info);
            state.evict()
        };
        if let Err(e) = self.delete(&evicted) {
            eprintln!("blindspot: could not evict old clips: {e}");
        }
        Some(id)
    }

    /// Moves a clip to the top, as when it is pasted again. False if it is not a clip —
    /// which is how `bs_activate` tells clip ids from app and file ids.
    pub fn touch(&self, id: u64, now: u64) -> bool {
        let bumped = {
            let mut state = lock(&self.state);
            state
                .bump(id, now)
                .then(|| state.info.get(&id).copied())
                .flatten()
        };
        let Some(info) = bumped else {
            return false;
        };
        if let Err(e) = self.write_meta(id, &info) {
            eprintln!("blindspot: could not update clip: {e}");
        }
        true
    }

    pub fn content(&self, id: u64, part: Part) -> Option<Vec<u8>> {
        let read = self.db.begin_read().ok()?;
        let table = read
            .open_table(match part {
                Part::Full => CONTENT,
                Part::Thumbnail => THUMB,
            })
            .ok()?;
        Some(table.get(id).ok()??.value().to_vec())
    }

    /// Runs `f` over the clips, newest first, plus their per-clip details.
    ///
    /// Borrowed rather than cloned: this is on the keystroke path for every `;` query,
    /// and copying two hundred searchable names per key would be pure waste.
    pub fn with_entries<T>(&self, f: impl FnOnce(&[AppEntry], &HashMap<u64, ClipInfo>) -> T) -> T {
        let state = lock(&self.state);
        f(&state.entries, &state.info)
    }

    fn write(
        &self,
        id: u64,
        name: &str,
        info: &ClipInfo,
        content: &[u8],
        thumbnail: &[u8],
    ) -> Result<(), StoreError> {
        let write = self.db.begin_write()?;
        {
            write.open_table(CONTENT)?.insert(id, content)?;
            write.open_table(THUMB)?.insert(id, thumbnail)?;
            write.open_table(META)?.insert(id, meta_row(info, name))?;
        }
        write.commit()?;
        Ok(())
    }

    fn write_meta(&self, id: u64, info: &ClipInfo) -> Result<(), StoreError> {
        let name = {
            let state = lock(&self.state);
            state
                .position(id)
                .and_then(|at| state.entries.get(at))
                .map(|e| e.name.clone())
        };
        let Some(name) = name else { return Ok(()) };
        let write = self.db.begin_write()?;
        write.open_table(META)?.insert(id, meta_row(info, &name))?;
        write.commit()?;
        Ok(())
    }

    fn delete(&self, ids: &[u64]) -> Result<(), StoreError> {
        if ids.is_empty() {
            return Ok(());
        }
        let write = self.db.begin_write()?;
        {
            let mut meta = write.open_table(META)?;
            let mut content = write.open_table(CONTENT)?;
            let mut thumb = write.open_table(THUMB)?;
            for id in ids {
                meta.remove(*id)?;
                content.remove(*id)?;
                thumb.remove(*id)?;
            }
        }
        write.commit()?;
        Ok(())
    }
}

fn meta_row<'a>(info: &ClipInfo, name: &'a str) -> (u8, u64, u32, u32, u64, &'a str) {
    (
        info.kind.to_byte(),
        info.created,
        info.width,
        info.height,
        info.bytes,
        name,
    )
}

/// A clip dressed as an `AppEntry` so the existing ranker scores it unchanged. The path
/// is empty — there is nothing on disk to open — and the id is the content hash rather
/// than `AppEntry::new`'s path hash.
fn entry(id: u64, name: String) -> AppEntry {
    AppEntry {
        id,
        name,
        path: PathBuf::new(),
        last_used: None,
    }
}

/// The text a clip is ranked by and shown as, or `None` to reject the clip.
fn searchable_name(clip: &NewClip<'_>) -> Option<String> {
    match clip.kind {
        ClipKind::Text => {
            if clip.content.len() > MAX_TEXT_BYTES {
                return None;
            }
            flatten(std::str::from_utf8(clip.content).ok()?)
        }
        ClipKind::Image => {
            if clip.content.is_empty() || clip.content.len() > MAX_IMAGE_BYTES {
                return None;
            }
            // The recognised text when there is any. "Image" otherwise — a placeholder
            // Swift recognises and replaces with "Screenshot" or "Image" at render time,
            // since only it knows the attached displays.
            let text = std::str::from_utf8(clip.text).unwrap_or_default();
            Some(flatten(text).unwrap_or_else(|| "Image".to_owned()))
        }
    }
}

/// Whitespace runs collapse to one space, so a multi-line clip reads as a single line in
/// its row and a newline never splits a word being searched for. `None` if nothing is left.
fn flatten(text: &str) -> Option<String> {
    let flat: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(SEARCH_CHARS)
        .collect();
    (!flat.is_empty()).then_some(flat)
}

/// Names written before images carried recognised text, like "Image 3024 × 1964". Those
/// rows are renamed on load rather than left showing their pixel dimensions as a title;
/// their content was never recognised, so there is no text to put in its place.
fn is_legacy_image_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("Image ") else {
        return false;
    };
    let mut parts = rest.split(" × ");
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    matches!((parts.next(), parts.next(), parts.next()), (Some(w), Some(h), None) if digits(w) && digits(h))
}

fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> NewClip<'_> {
        NewClip {
            kind: ClipKind::Text,
            content: s.as_bytes(),
            thumbnail: &[],
            text: &[],
            width: 0,
            height: 0,
        }
    }

    fn names(clips: &Clips) -> Vec<String> {
        clips.with_entries(|entries, _| entries.iter().map(|e| e.name.clone()).collect())
    }

    fn fresh() -> Clips {
        Clips::in_memory().expect("in-memory store opens")
    }

    #[test]
    fn clips_are_kept_newest_first() {
        let clips = fresh();
        clips.add(text("first"), 1);
        clips.add(text("second"), 2);
        assert_eq!(names(&clips), ["second", "first"]);
    }

    #[test]
    fn copying_the_same_thing_again_bumps_rather_than_duplicates() {
        let clips = fresh();
        let a = clips.add(text("alpha"), 1);
        clips.add(text("beta"), 2);
        let again = clips.add(text("alpha"), 3);
        assert_eq!(a, again, "identical content, identical id");
        assert_eq!(names(&clips), ["alpha", "beta"]);
    }

    #[test]
    fn touching_reorders_and_reports_whether_it_was_a_clip() {
        let clips = fresh();
        let a = clips.add(text("alpha"), 1).expect("stored");
        clips.add(text("beta"), 2);
        assert!(clips.touch(a, 3));
        assert_eq!(names(&clips), ["alpha", "beta"]);
        assert!(
            !clips.touch(0xDEAD_BEEF, 4),
            "not a clip, so frecency gets it"
        );
    }

    #[test]
    fn content_round_trips_for_both_parts() {
        let clips = fresh();
        let id = clips.add(text("hello world"), 1).expect("stored");
        assert_eq!(
            clips.content(id, Part::Full).as_deref(),
            Some(&b"hello world"[..])
        );
        assert_eq!(clips.content(id, Part::Thumbnail).as_deref(), Some(&[][..]));

        let png = [0x89u8, b'P', b'N', b'G', 1, 2, 3];
        let image = NewClip {
            kind: ClipKind::Image,
            content: &png,
            thumbnail: &[9, 9],
            text: b"error[E0599]: no method named\n  --> src/apps.rs:387",
            width: 1920,
            height: 1080,
        };
        let id = clips.add(image, 2).expect("stored");
        assert_eq!(clips.content(id, Part::Full).as_deref(), Some(&png[..]));
        assert_eq!(
            clips.content(id, Part::Thumbnail).as_deref(),
            Some(&[9u8, 9][..])
        );
        assert_eq!(
            names(&clips)[0],
            "error[E0599]: no method named --> src/apps.rs:387",
            "an image is named by the text found in it, flattened to one line"
        );
    }

    #[test]
    fn whitespace_collapses_and_blank_text_is_rejected() {
        let clips = fresh();
        clips.add(text("  line one\n\n\tline two  "), 1);
        assert_eq!(names(&clips), ["line one line two"]);
        assert!(clips.add(text("   \n\t "), 2).is_none());
        assert!(clips.add(text(""), 3).is_none());
    }

    #[test]
    fn oversized_items_are_skipped_not_truncated() {
        let clips = fresh();
        let big = "x".repeat(MAX_TEXT_BYTES + 1);
        assert!(clips.add(text(&big), 1).is_none());
        let huge = vec![0u8; MAX_IMAGE_BYTES + 1];
        let image = NewClip {
            kind: ClipKind::Image,
            content: &huge,
            thumbnail: &[],
            text: &[],
            width: 1,
            height: 1,
        };
        assert!(clips.add(image, 2).is_none());
        assert!(names(&clips).is_empty());
    }

    #[test]
    fn invalid_utf8_text_is_rejected() {
        let clips = fresh();
        let bad = NewClip {
            kind: ClipKind::Text,
            content: &[0xFF, 0xFE],
            thumbnail: &[],
            text: &[],
            width: 0,
            height: 0,
        };
        assert!(clips.add(bad, 1).is_none());
    }

    #[test]
    fn the_count_cap_evicts_the_oldest_and_deletes_its_rows() {
        let clips = fresh();
        let first = clips.add(text("clip 0"), 0).expect("stored");
        for n in 1..=MAX_CLIPS {
            clips.add(text(&format!("clip {n}")), n as u64);
        }
        assert_eq!(names(&clips).len(), MAX_CLIPS);
        assert!(
            !names(&clips).contains(&"clip 0".to_owned()),
            "oldest evicted"
        );
        assert!(
            clips.content(first, Part::Full).is_none(),
            "its content row is gone too"
        );
    }

    #[test]
    fn the_byte_budget_evicts_even_under_the_count_cap() {
        let clips = fresh();
        // Eleven images near the per-item cap overflow 256 MB long before 200 clips.
        let chunk = vec![1u8; 24 * 1024 * 1024];
        let mut ids = Vec::new();
        for n in 0..11u8 {
            let mut data = chunk.clone();
            data[0] = n; // distinct content, distinct ids
            let image = NewClip {
                kind: ClipKind::Image,
                content: &data,
                thumbnail: &[],
                text: &[],
                width: 1,
                height: 1,
            };
            ids.push(clips.add(image, u64::from(n)).expect("stored"));
        }
        let kept = names(&clips).len();
        assert!(
            kept < 11,
            "the byte budget must have evicted something, kept {kept}"
        );
        assert!(
            clips.content(ids[0], Part::Full).is_none(),
            "oldest image deleted"
        );
        let total = clips.with_entries(|_, info| info.values().map(|i| i.bytes).sum::<u64>());
        assert!(total <= MAX_TOTAL_BYTES);
    }

    #[test]
    fn history_survives_a_reopen_in_order() {
        let dir = std::env::temp_dir().join(format!("blindspot-clips-{}", std::process::id()));
        let path = dir.join("clips.redb");
        let _ = std::fs::remove_dir_all(&dir);
        {
            let clips = Clips::open(&path).expect("opens");
            clips.add(text("older"), 1);
            clips.add(text("newer"), 2);
        }
        let clips = Clips::open(&path).expect("reopens");
        assert_eq!(names(&clips), ["newer", "older"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_image_with_no_text_is_still_named() {
        let clips = fresh();
        let image = NewClip {
            kind: ClipKind::Image,
            content: &[1, 2, 3],
            thumbnail: &[],
            text: b"   ",
            width: 10,
            height: 10,
        };
        clips.add(image, 1).expect("stored");
        assert_eq!(names(&clips), ["Image"]);
    }

    #[test]
    fn only_old_dimension_names_are_migrated() {
        assert!(is_legacy_image_name("Image 3024 × 1964"));
        assert!(!is_legacy_image_name("Image"));
        assert!(!is_legacy_image_name("Image 3024 × 1964 × 2"));
        assert!(
            !is_legacy_image_name("Image of a cat"),
            "real recognised text is kept"
        );
        assert!(!is_legacy_image_name("Imagex 1 × 2"));
    }

    #[test]
    fn a_clip_id_can_never_collide_with_a_path_id() {
        // Copying a path as text must not produce the same id as that file.
        let clips = fresh();
        let path = "/Applications/Safari.app";
        let clip_id = clips.add(text(path), 1).expect("stored");
        let file_id = AppEntry::new("Safari".into(), PathBuf::from(path)).id;
        assert_ne!(clip_id, file_id);
    }
}
