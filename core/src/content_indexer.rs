//! Incremental local text indexing. Call only from a background indexing worker.

use crate::content::{
    self, ContentStore, Document, Extraction, ScanOutcome, MAX_BATCH, MAX_BODY_BYTES,
};
use crate::ffi::index_native::{Directory, Kind};
use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, UNIX_EPOCH};

const MAX_DEPTH: usize = 64;
const GENERATED: &[&str] = &[
    "Library",
    "node_modules",
    "target",
    "DerivedData",
    "__pycache__",
    "venv",
    "vendor",
    "Pods",
    "site-packages",
    "bower_components",
    "Carthage",
    "_deps",
];
const TEXT_EXTENSIONS: &[&str] = &[
    "txt", "md", "markdown", "rst", "csv", "tsv", "json", "toml", "yaml", "yml", "xml", "html",
    "css", "rs", "swift", "py", "js", "jsx", "ts", "tsx", "go", "java", "c", "h", "cpp", "hpp",
    "rb", "sh", "sql",
];
const DOCUMENT_EXTENSIONS: &[&str] = &["pdf", "docx", "doc", "rtf", "odt"];

#[derive(Debug, Clone)]
pub struct Policy {
    pub excluded_paths: Vec<PathBuf>,
    pub max_file_bytes: u64,
    pub max_entries: u64,
    pub batch_pause: Duration,
    pub documents: bool,
    pub max_document_bytes: u64,
    pub extractor: Option<PathBuf>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            excluded_paths: Vec::new(),
            max_file_bytes: 1_048_576,
            max_entries: 10_000_000,
            batch_pause: Duration::from_millis(10),
            documents: false,
            max_document_bytes: 32 * 1_048_576,
            extractor: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub visited: u64,
    pub indexed: u64,
    pub indexed_bytes: u64,
    pub unchanged: u64,
    pub skipped: u64,
    pub failed: u64,
    pub removed: u64,
    /// Written records the semantic stage would embed; zero lets a pass skip that stage.
    pub semantic_updates: u64,
    pub complete: bool,
}

struct Record {
    identity: String,
    path: PathBuf,
    title: String,
    body: String,
    modified_ns: i64,
    changed_ns: i64,
    bytes: u64,
    extraction: Extraction,
}

impl Record {
    fn document(&self) -> Document<'_> {
        Document {
            identity: &self.identity,
            path: &self.path,
            title: &self.title,
            body: &self.body,
            modified_ns: self.modified_ns,
            changed_ns: self.changed_ns,
            bytes: self.bytes,
            extraction: self.extraction,
        }
    }
}

pub fn scan_root(
    store: &mut ContentStore,
    root: &Path,
    policy: &Policy,
    cancel: &AtomicBool,
    mut report: impl FnMut(&Progress),
) -> content::Result<Progress> {
    if cancel.load(Ordering::Acquire) {
        return Err(content::Error::Cancelled);
    }
    if !root.is_absolute()
        || root
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(content::Error::Invalid(
            "Choose an absolute indexing directory",
        ));
    }
    let directory = Directory::open(root)?;
    if !directory.is_local()? {
        return Err(content::Error::Invalid(
            "Only local volumes support content indexing",
        ));
    }
    let device = directory.device()?;
    let scan = store.begin_scan(root)?;
    let mut progress = Progress::default();
    let mut complete = true;
    let mut batch = Vec::<Record>::with_capacity(MAX_BATCH);
    let mut checkpoint = 0;
    let context = Walk { scan: &scan, device, policy, cancel };
    if !walk(store, &context, directory, root.to_owned(), 0, &mut batch, &mut progress, &mut checkpoint, &mut report)? {
        complete = false;
    }
    if cancel.load(Ordering::Acquire) {
        return Err(content::Error::Cancelled);
    }
    flush(store, &scan, &mut batch, &mut progress)?;
    let outcome = if complete {
        ScanOutcome::Complete
    } else {
        ScanOutcome::Interrupted
    };
    progress.removed = store.finish_scan(&scan, outcome, cancel)? as u64;
    progress.complete = complete;
    report(&progress);
    Ok(progress)
}

struct Walk<'a> {
    scan: &'a content::Scan,
    device: u64,
    policy: &'a Policy,
    cancel: &'a AtomicBool,
}

/// Depth-first traversal shared by full and event-driven scans. Returns whether every entry
/// below `start` was examined, which is what decides whether unseen records may be removed.
#[expect(clippy::too_many_arguments, reason = "the walk shares one batch, counter set and checkpoint with its caller")]
fn walk(
    store: &mut ContentStore,
    context: &Walk<'_>,
    start: Directory,
    start_path: PathBuf,
    depth: usize,
    batch: &mut Vec<Record>,
    progress: &mut Progress,
    checkpoint: &mut u64,
    report: &mut impl FnMut(&Progress),
) -> content::Result<bool> {
    let Walk { scan, device, policy, cancel } = *context;
    let mut complete = true;
    let mut stack = vec![(start, start_path)];
    while let Some((directory, path)) = stack.last_mut() {
        if cancel.load(Ordering::Acquire) {
            return Err(content::Error::Cancelled);
        }
        if progress.visited > *checkpoint && progress.visited.is_multiple_of(MAX_BATCH as u64) {
            *checkpoint = progress.visited;
            flush(store, scan, batch, progress)?;
            report(progress);
            pause(policy.batch_pause, cancel)?;
        }
        if progress.visited >= policy.max_entries {
            return Ok(false);
        }
        let entry = match directory.next_entry() {
            Ok(Some(entry)) => entry,
            Ok(None) => {
                stack.pop();
                continue;
            }
            Err(_) => {
                progress.failed += 1;
                complete = false;
                stack.pop();
                continue;
            }
        };
        progress.visited += 1;
        let child_path = path.join(&entry.name);
        if entry.dataless || entry.device != device || excluded(&child_path, &entry.name, policy) {
            progress.skipped += 1;
            continue;
        }
        match entry.kind {
            Kind::Directory => match directory.child(&entry.name) {
                Ok(child) if stack.len() + depth < MAX_DEPTH => stack.push((child, child_path)),
                Ok(_) => {
                    progress.skipped += 1;
                    complete = false;
                }
                Err(_) => {
                    progress.failed += 1;
                    complete = false;
                }
            },
            Kind::Unsupported => progress.skipped += 1,
            Kind::File => {
                if !supported(&child_path, policy.documents) {
                    progress.skipped += 1;
                    continue;
                }
                match load(store, scan, directory, &child_path, &entry.name, policy, cancel) {
                    Ok(Loaded::Record(record)) => batch.push(record),
                    Ok(Loaded::Unchanged) => progress.unchanged += 1,
                    Ok(Loaded::Skipped) => progress.skipped += 1,
                    Err(content::Error::Io(_)) => {
                        progress.failed += 1;
                        complete = false;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Ok(complete)
}

/// Re-reads only what filesystem events named instead of walking the whole root: a file is
/// reloaded, a directory is walked, and a path that no longer exists (or is now a symlink or
/// unreadable ancestor) loses its records. With a home-folder root, every saved file previously
/// triggered a walk of the entire home directory. Callers use it only after a complete full scan.
pub fn scan_paths(
    store: &mut ContentStore,
    root: &Path,
    paths: &[PathBuf],
    policy: &Policy,
    cancel: &AtomicBool,
    mut report: impl FnMut(&Progress),
) -> content::Result<Progress> {
    if cancel.load(Ordering::Acquire) {
        return Err(content::Error::Cancelled);
    }
    let valid = |path: &Path| path.is_absolute() && !path.components().any(|c| matches!(c, std::path::Component::ParentDir));
    if !valid(root) || paths.len() > 128 || paths.iter().any(|path| !valid(path) || !path.starts_with(root) || path == root) {
        return Err(content::Error::Invalid("Changed paths must lie inside their indexing root"));
    }
    let root_directory = Directory::open(root)?;
    if !root_directory.is_local()? {
        return Err(content::Error::Invalid("Only local volumes support content indexing"));
    }
    let device = root_directory.device()?;
    drop(root_directory);
    let mut sorted: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    sorted.sort();
    sorted.dedup();
    let targets: Vec<&Path> = sorted.iter().copied()
        .filter(|path| !sorted.iter().any(|other| other != path && path.starts_with(other))).collect();
    let scan = store.begin_scan(root)?;
    let context = Walk { scan: &scan, device, policy, cancel };
    let mut progress = Progress::default();
    let mut complete = true;
    let mut batch = Vec::<Record>::with_capacity(MAX_BATCH);
    let mut checkpoint = 0;
    for target in targets {
        if cancel.load(Ordering::Acquire) {
            return Err(content::Error::Cancelled);
        }
        let Ok(relative) = target.strip_prefix(root) else { continue; };
        let names: Vec<&std::ffi::OsStr> = relative.iter().collect();
        let Some((leaf, parents)) = names.split_last() else { continue; };
        // Ancestors are opened one component at a time with the same no-follow opens a full scan
        // uses, so an event cannot redirect indexing through a symlink.
        let mut prefix = root.to_path_buf();
        let mut parent = Some(Directory::open(root)?);
        let mut hidden = false;
        for name in parents {
            prefix.push(name);
            if excluded(&prefix, name, policy) {
                hidden = true;
                break;
            }
            parent = parent.and_then(|directory| directory.child(name).ok());
            if parent.is_none() {
                break;
            }
        }
        if hidden || excluded(target, leaf, policy) {
            continue;
        }
        progress.visited += 1;
        if let Some(parent) = &parent {
            match std::fs::symlink_metadata(target) {
                Ok(metadata) if metadata.is_dir() => {
                    if let Ok(child) = parent.child(leaf) {
                        if parents.len() + 1 >= MAX_DEPTH || metadata.dev() != device {
                            progress.skipped += 1;
                            continue;
                        }
                        if !walk(store, &context, child, target.to_path_buf(), parents.len() + 1, &mut batch, &mut progress, &mut checkpoint, &mut report)? {
                            complete = false;
                            continue;
                        }
                    }
                }
                Ok(metadata) if metadata.is_file() && metadata.dev() == device && supported(target, policy.documents) => {
                    match load(store, &scan, parent, target, leaf, policy, cancel) {
                        Ok(Loaded::Record(record)) => batch.push(record),
                        Ok(Loaded::Unchanged) => progress.unchanged += 1,
                        Ok(Loaded::Skipped) => progress.skipped += 1,
                        Err(content::Error::Io(error)) if error.kind() != std::io::ErrorKind::NotFound => {
                            progress.failed += 1;
                            complete = false;
                            continue;
                        }
                        Err(content::Error::Io(_)) => {}
                        Err(error) => return Err(error),
                    }
                }
                Ok(_) => progress.skipped += 1,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {
                    progress.failed += 1;
                    complete = false;
                    continue;
                }
            }
        }
        flush(store, &scan, &mut batch, &mut progress)?;
        progress.removed += store.remove_unseen_under(&scan, target, cancel)? as u64;
        report(&progress);
    }
    flush(store, &scan, &mut batch, &mut progress)?;
    progress.complete = complete;
    report(&progress);
    Ok(progress)
}

enum Loaded {
    Record(Record),
    Unchanged,
    Skipped,
}

fn load(
    store: &mut ContentStore,
    scan: &content::Scan,
    directory: &Directory,
    path: &Path,
    name: &std::ffi::OsStr,
    policy: &Policy,
    cancel: &AtomicBool,
) -> content::Result<Loaded> {
    let mut file = directory.file(name)?;
    let metadata = file.metadata()?;
    let is_document = policy.documents
        && path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                DOCUMENT_EXTENSIONS.contains(&value.to_ascii_lowercase().as_str())
            });
    let size_limit = if is_document {
        policy.max_document_bytes
    } else {
        policy.max_file_bytes
    };
    if (!is_document && metadata.len() > size_limit) || metadata.len() > i64::MAX as u64 {
        return Ok(Loaded::Skipped);
    }
    let Some(path_text) = path.to_str().filter(|s| s.len() <= 4096) else {
        return Ok(Loaded::Skipped);
    };
    let Some(title) = name.to_str().filter(|s| s.len() <= 1024) else {
        return Ok(Loaded::Skipped);
    };
    let identity = identity(&metadata)?;
    let modified_ns = modified_ns(&metadata);
    let changed_ns = changed_ns(&metadata);
    if store.mark_unchanged(
        scan,
        &identity,
        path,
        modified_ns,
        changed_ns,
        metadata.len(),
    )? {
        return Ok(Loaded::Unchanged);
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    if is_document {
        let kind = extension.as_deref().unwrap_or_default();
        if metadata.len() > policy.max_document_bytes {
            store.mark_extraction_failure(
                scan,
                &identity,
                path,
                title,
                modified_ns,
                changed_ns,
                metadata.len(),
                Extraction::Oversized,
            )?;
            return Ok(Loaded::Skipped);
        }
        let Some(extractor) = policy.extractor.as_ref() else {
            store.mark_extraction_failure(
                scan,
                &identity,
                path,
                title,
                modified_ns,
                changed_ns,
                metadata.len(),
                Extraction::Unreadable,
            )?;
            return Ok(Loaded::Skipped);
        };
        match extract_document(&file, extractor, kind, MAX_BODY_BYTES, cancel)? {
            Ok((status, text)) => {
                if status == Extraction::Extracted {
                    let after = file.metadata()?;
                    if metadata.len() != after.len()
                        || modified_ns != self::modified_ns(&after)
                        || metadata.ctime() != after.ctime()
                        || metadata.ctime_nsec() != after.ctime_nsec()
                    {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "File changed during indexing",
                        )
                        .into());
                    }
                    return Ok(Loaded::Record(Record {
                        identity,
                        path: path_text.into(),
                        title: title.into(),
                        body: text.unwrap_or_default(),
                        modified_ns,
                        changed_ns,
                        bytes: metadata.len(),
                        extraction: status,
                    }));
                }
                store.mark_extraction_failure(
                    scan,
                    &identity,
                    path,
                    title,
                    modified_ns,
                    changed_ns,
                    metadata.len(),
                    status,
                )?;
                return Ok(Loaded::Skipped);
            }
            Err(status) => {
                store.mark_extraction_failure(
                    scan,
                    &identity,
                    path,
                    title,
                    modified_ns,
                    changed_ns,
                    metadata.len(),
                    status,
                )?;
                return Ok(Loaded::Skipped);
            }
        }
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_BODY_BYTES as u64)
        .read_to_end(&mut bytes)?;
    let end = match std::str::from_utf8(&bytes) {
        Ok(_) => bytes.len(),
        Err(error) if error.error_len().is_none() => error.valid_up_to(),
        Err(_) => return Ok(Loaded::Skipped),
    };
    bytes.truncate(end);
    if bytes.contains(&0) {
        return Ok(Loaded::Skipped);
    }
    let after = file.metadata()?;
    if metadata.len() != after.len()
        || modified_ns != self::modified_ns(&after)
        || metadata.ctime() != after.ctime()
        || metadata.ctime_nsec() != after.ctime_nsec()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "File changed during indexing",
        )
        .into());
    }
    let body = String::from_utf8(bytes).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "Unsupported text encoding")
    })?;
    Ok(Loaded::Record(Record {
        identity,
        path: path_text.into(),
        title: title.into(),
        body,
        modified_ns,
        changed_ns,
        bytes: metadata.len(),
        extraction: Extraction::Text,
    }))
}

#[derive(serde::Deserialize)]
struct HelperResponse {
    status: String,
    text: Option<String>,
}

fn extract_document(
    file: &std::fs::File,
    extractor: &Path,
    kind: &str,
    max_bytes: usize,
    cancel: &AtomicBool,
) -> content::Result<std::result::Result<(Extraction, Option<String>), Extraction>> {
    if !extractor.is_absolute() || !extractor.is_file() {
        return Ok(Err(Extraction::Unreadable));
    }
    let input = file.try_clone()?;
    let (mut output, peer) = UnixStream::pair().map_err(content::Error::Io)?;
    output
        .set_read_timeout(Some(Duration::from_millis(25)))
        .map_err(content::Error::Io)?;
    let mut child = Command::new(extractor)
        .args(["--index", kind, &max_bytes.to_string(), "100"])
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(OwnedFd::from(peer)))
        .stderr(Stdio::null())
        .spawn()
        .map_err(content::Error::Io)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut exited = None;
    let mut eof = false;
    loop {
        if cancel.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(content::Error::Cancelled);
        }
        if exited.is_none() {
            match child.try_wait() {
                Ok(status) => exited = status,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(content::Error::Io(error));
                }
            }
        }
        if !eof {
            match output.read(&mut buffer) {
                Ok(0) => eof = true,
                Ok(count) => {
                    bytes.extend_from_slice(&buffer[..count]);
                    if bytes.len() > 262_144 {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Ok(Err(Extraction::Unreadable));
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    if exited.is_some() {
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(content::Error::Io(error));
                }
            }
        }
        if exited.is_some() && eof {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Err(Extraction::Unreadable));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
    if !exited.is_some_and(|status| status.success()) {
        return Ok(Err(Extraction::Unreadable));
    }
    let response: HelperResponse = match serde_json::from_slice(&bytes) {
        Ok(response) => response,
        Err(_) => return Ok(Err(Extraction::Unreadable)),
    };
    let status = match response.status.as_str() {
        "text"
            if response
                .text
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty()) =>
        {
            Extraction::Extracted
        }
        "locked" => Extraction::Locked,
        "empty" => Extraction::NoText,
        _ => Extraction::Unreadable,
    };
    if response
        .text
        .as_ref()
        .is_some_and(|text| text.len() > MAX_BODY_BYTES)
    {
        return Ok(Err(Extraction::Unreadable));
    }
    Ok(Ok((status, response.text)))
}

fn flush(
    store: &mut ContentStore,
    scan: &content::Scan,
    batch: &mut Vec<Record>,
    progress: &mut Progress,
) -> content::Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    store.put_batch(
        scan,
        &batch.iter().map(Record::document).collect::<Vec<_>>(),
    )?;
    progress.indexed += batch.len() as u64;
    progress.semantic_updates += batch.iter().filter(|record| content::semantic_eligible(&record.path, record.extraction)).count() as u64;
    progress.indexed_bytes += batch.iter().map(|record| record.bytes).sum::<u64>();
    batch.clear();
    Ok(())
}

fn pause(duration: Duration, cancel: &AtomicBool) -> content::Result<()> {
    let started = std::time::Instant::now();
    while started.elapsed() < duration {
        if cancel.load(Ordering::Acquire) {
            return Err(content::Error::Cancelled);
        }
        std::thread::sleep(
            duration
                .saturating_sub(started.elapsed())
                .min(Duration::from_millis(10)),
        );
    }
    Ok(())
}

fn identity(metadata: &std::fs::Metadata) -> std::io::Result<String> {
    let created = metadata
        .created()?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "Unsupported file identity")
        })?;
    Ok(format!(
        "{}:{}:{}",
        metadata.dev(),
        metadata.ino(),
        created.as_nanos()
    ))
}

fn modified_ns(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .mtime()
        .saturating_mul(1_000_000_000)
        .saturating_add(metadata.mtime_nsec())
}

fn changed_ns(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .ctime()
        .saturating_mul(1_000_000_000)
        .saturating_add(metadata.ctime_nsec())
}

pub(crate) fn excluded(path: &Path, name: &std::ffi::OsStr, policy: &Policy) -> bool {
    name.to_str().is_none_or(|name| {
        name.starts_with('.')
            || GENERATED.contains(&name)
            || matches!(
                name,
                "credentials.json" | "secrets.json" | "secrets.yaml" | "secrets.yml"
            )
            // Go's module cache: 15,921 of 41,155 documents on the measured home index.
            || (name == "mod" && path.parent().and_then(Path::file_name).is_some_and(|parent| parent == "pkg"))
    }) || policy
        .excluded_paths
        .iter()
        .any(|excluded| path.starts_with(excluded))
        || path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| {
                matches!(
                    ext,
                    "app" | "bundle" | "framework" | "photoslibrary" | "keychain-db"
                )
            })
}

fn supported(path: &Path, documents: bool) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            let ext = ext.to_ascii_lowercase();
            TEXT_EXTENSIONS.contains(&ext.as_str())
                || (documents && DOCUMENT_EXTENSIONS.contains(&ext.as_str()))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;


    #[test]
    fn event_scans_reload_changed_paths_remove_vanished_ones_and_leave_the_rest() {
        let base = std::env::temp_dir().join(format!("blindspot-event-scan-{}", std::process::id()));
        std::fs::create_dir(&base).unwrap();
        let base = base.canonicalize().unwrap();
        let root = base.join("root");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join("other")).unwrap();
        std::fs::write(root.join("gone.txt"), "vanishing walrus").unwrap();
        std::fs::write(root.join("sub/kept.txt"), "migrating heron").unwrap();
        std::fs::write(root.join("other/untouched.txt"), "patient tortoise").unwrap();
        let mut store = ContentStore::open(&base.join("content.sqlite")).unwrap();
        let cancel = AtomicBool::new(false);
        let policy = Policy::default();
        scan_root(&mut store, &root, &policy, &cancel, |_| {}).unwrap();
        assert_eq!(store.count().unwrap(), 3);
        std::fs::remove_file(root.join("gone.txt")).unwrap();
        std::fs::rename(root.join("sub"), root.join("moved")).unwrap();
        std::fs::write(root.join("moved/new.txt"), "arriving puffin").unwrap();
        std::fs::create_dir(root.join(".hidden")).unwrap();
        std::fs::write(root.join(".hidden/secret.txt"), "hidden lynx").unwrap();
        let changed = [root.join("gone.txt"), root.join("sub"), root.join("moved"), root.join(".hidden/secret.txt")];
        let progress = scan_paths(&mut store, &root, &changed, &policy, &cancel, |_| {}).unwrap();
        assert!(progress.complete);
        let paths = |word: &str| store.search(word, 10, Arc::new(AtomicBool::new(false))).unwrap().hits.into_iter().map(|hit| hit.path).collect::<Vec<_>>();
        assert!(paths("walrus").is_empty());
        assert_eq!(paths("heron"), [root.join("moved/kept.txt").to_string_lossy()]);
        assert_eq!(paths("puffin"), [root.join("moved/new.txt").to_string_lossy()]);
        assert_eq!(paths("tortoise"), [root.join("other/untouched.txt").to_string_lossy()]);
        assert!(paths("lynx").is_empty());
        assert_eq!(store.count().unwrap(), 3);
        assert!(scan_paths(&mut store, &root, &[base.join("outside.txt")], &policy, &cancel, |_| {}).is_err());
        drop(store);
        std::fs::remove_dir_all(base).unwrap();
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "blindspot-indexer-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).expect("fixture");
            let path = path.canonicalize().expect("canonical");
            std::fs::create_dir(path.join("root")).expect("root");
            Self(path)
        }
        fn root(&self) -> PathBuf {
            self.0.join("root")
        }
        fn store(&self) -> ContentStore {
            ContentStore::open(&self.0.join("content.sqlite")).expect("store")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn scan(store: &mut ContentStore, root: &Path, policy: &Policy) -> Progress {
        scan_root(store, root, policy, &AtomicBool::new(false), |_| {}).expect("scan")
    }
    fn find(store: &ContentStore, text: &str) -> Vec<content::Hit> {
        store
            .search(text, 50, Arc::new(AtomicBool::new(false)))
            .expect("search")
            .hits
    }

    #[test]
    fn real_files_reconcile_content_rename_move_delete_and_exclusions() {
        let fixture = Fixture::new();
        let root = fixture.root();
        let mut store = fixture.store();
        let policy = Policy {
            batch_pause: Duration::ZERO,
            ..Policy::default()
        };
        std::fs::write(root.join("notes.md"), "database migrations").expect("notes");
        std::fs::write(root.join(".env"), "private credential").expect("hidden");
        std::fs::create_dir(root.join("node_modules")).expect("generated");
        std::fs::write(root.join("node_modules/private.txt"), "private credential")
            .expect("generated file");
        std::os::unix::fs::symlink(&root, root.join("loop")).expect("loop");
        assert_eq!(scan(&mut store, &root, &policy).indexed, 1);
        let before = find(&store, "migrations")[0].id;
        assert!(find(&store, "private").is_empty());
        assert_eq!(scan(&mut store, &root, &policy).unchanged, 1);
        std::fs::create_dir(root.join("project")).expect("folder");
        std::fs::rename(root.join("notes.md"), root.join("project/renamed.md")).expect("move");
        assert!(scan(&mut store, &root, &policy).complete);
        assert_eq!(find(&store, "migrations")[0].id, before);
        std::fs::write(root.join("project/renamed.md"), "updated schema").expect("edit");
        scan(&mut store, &root, &policy);
        assert!(find(&store, "migrations").is_empty());
        assert_eq!(find(&store, "schema").len(), 1);
        std::fs::remove_file(root.join("project/renamed.md")).expect("delete");
        assert_eq!(scan(&mut store, &root, &policy).removed, 1);
        assert!(find(&store, "schema").is_empty());
        store.check_integrity().expect("integrity");
    }

    #[test]
    fn interrupted_scan_preserves_records_and_oversized_binary_files_are_skipped() {
        let fixture = Fixture::new();
        let root = fixture.root();
        let mut store = fixture.store();
        let mut policy = Policy {
            batch_pause: Duration::ZERO,
            max_file_bytes: 100,
            ..Policy::default()
        };
        std::fs::write(root.join("notes.txt"), "retained text").expect("notes");
        std::fs::write(root.join("large.txt"), vec![b'a'; 101]).expect("large");
        std::fs::write(root.join("binary.txt"), [0, 255, 0]).expect("binary");
        scan(&mut store, &root, &policy);
        assert_eq!(store.count().expect("count"), 1);
        std::fs::remove_file(root.join("notes.txt")).expect("remove");
        policy.max_entries = 0;
        assert!(!scan(&mut store, &root, &policy).complete);
        assert_eq!(store.count().expect("preserved"), 1);
        assert!(matches!(
            scan_root(&mut store, &root, &policy, &AtomicBool::new(true), |_| {}),
            Err(content::Error::Cancelled)
        ));
        policy.max_entries = 100;
        assert_eq!(scan(&mut store, &root, &policy).removed, 1);
    }

    #[test]
    fn cancellation_after_a_batch_keeps_committed_work_and_next_scan_resumes() {
        let fixture = Fixture::new();
        let root = fixture.root();
        let mut store = fixture.store();
        for i in 0..300 {
            std::fs::write(root.join(format!("note{i}.txt")), "searchable text").expect("file");
        }
        let policy = Policy {
            batch_pause: Duration::from_millis(1),
            ..Policy::default()
        };
        let cancel = AtomicBool::new(false);
        let interrupted = scan_root(&mut store, &root, &policy, &cancel, |progress| {
            if progress.visited >= MAX_BATCH as u64 {
                cancel.store(true, Ordering::Release);
            }
        });
        assert!(matches!(interrupted, Err(content::Error::Cancelled)));
        assert_eq!(store.count().expect("committed batch"), MAX_BATCH as u64);
        let resumed = scan(&mut store, &root, &policy);
        assert!(resumed.complete);
        assert_eq!(resumed.unchanged, MAX_BATCH as u64);
        assert_eq!(resumed.indexed, 300 - MAX_BATCH as u64);
        assert_eq!(store.count().expect("complete"), 300);
    }

    #[test]
    fn hard_links_do_not_duplicate_and_new_exclusions_remove_indexed_text() {
        let fixture = Fixture::new();
        let root = fixture.root();
        let mut store = fixture.store();
        std::fs::create_dir(root.join("project")).expect("folder");
        std::fs::write(root.join("project/a.txt"), "private fixture").expect("file");
        std::fs::hard_link(root.join("project/a.txt"), root.join("project/b.txt"))
            .expect("hard link");
        let mut policy = Policy {
            batch_pause: Duration::ZERO,
            ..Policy::default()
        };
        scan(&mut store, &root, &policy);
        assert_eq!(store.count().expect("deduplicated"), 1);
        assert_eq!(
            find(&store, "private")[0].path,
            root.join("project/a.txt").to_str().expect("path")
        );
        scan(&mut store, &root, &policy);
        assert_eq!(
            find(&store, "private")[0].path,
            root.join("project/a.txt").to_str().expect("stable path")
        );
        policy.excluded_paths.push(root.join("project"));
        assert_eq!(scan(&mut store, &root, &policy).removed, 1);
        assert!(find(&store, "private").is_empty());
    }

    #[test]
    fn content_changes_with_preserved_size_and_mtime_are_reindexed() {
        let fixture = Fixture::new();
        let root = fixture.root();
        let mut store = fixture.store();
        let path = root.join("notes.txt");
        std::fs::write(&path, "first text").expect("original");
        let modified = std::fs::metadata(&path)
            .expect("metadata")
            .modified()
            .expect("mtime");
        let policy = Policy {
            batch_pause: Duration::ZERO,
            ..Policy::default()
        };
        scan(&mut store, &root, &policy);
        std::fs::write(&path, "other text").expect("replacement");
        std::fs::File::open(&path)
            .expect("file")
            .set_modified(modified)
            .expect("restore mtime");
        let progress = scan(&mut store, &root, &policy);
        assert_eq!(progress.indexed, 1);
        assert_eq!(progress.unchanged, 0);
        assert!(find(&store, "first").is_empty());
        assert_eq!(find(&store, "other").len(), 1);
    }

    #[test]
    #[ignore = "requires the native PDF extraction helper"]
    fn native_pdf_is_indexed_and_searchable() {
        let fixture = Fixture::new();
        let root = fixture.root();
        let mut store = fixture.store();
        std::fs::write(root.join("genetec.pdf"), minimal_pdf()).expect("pdf");
        let extractor =
            std::env::var_os("BLINDSPOT_EXTRACT_WORKER").expect("BLINDSPOT_EXTRACT_WORKER");
        let policy = Policy {
            documents: true,
            extractor: Some(extractor.into()),
            batch_pause: Duration::ZERO,
            ..Policy::default()
        };
        let progress = scan(&mut store, &root, &policy);
        assert_eq!(progress.indexed, 1);
        assert_eq!(find(&store, "Genetec migration").len(), 1);
    }

    #[test]
    fn bounded_helper_capture_does_not_wait_for_inherited_stdout() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let input = fixture.root().join("input.pdf");
        let helper = fixture.root().join("helper.sh");
        std::fs::write(&input, b"fixture").expect("input");
        std::fs::write(
            &helper,
            "#!/bin/sh\nprintf '%s' '{\"status\":\"text\",\"text\":\"'\n/usr/bin/head -c 65536 /dev/zero | /usr/bin/tr '\\000' a\nprintf '%s\\n' '\"}'\n(sleep 10) &\n",
        )
        .expect("helper");
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))
            .expect("permissions");
        let file = std::fs::File::open(&input).expect("input file");
        let started = std::time::Instant::now();
        let result = extract_document(
            &file,
            &helper,
            "pdf",
            MAX_BODY_BYTES,
            &AtomicBool::new(false),
        )
        .expect("capture")
        .expect("status");
        assert_eq!(result.0, Extraction::Extracted);
        // Far below the grandchild's 10 s sleep, which is what would hold stdout open, yet far
        // above process start-up on a loaded CI runner. 500 ms against a 2 s sleep failed there.
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn helper_cancellation_reaps_child_promptly() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let input = fixture.root().join("input.pdf");
        let helper = fixture.root().join("sleep.sh");
        std::fs::write(&input, b"fixture").expect("input");
        std::fs::write(&helper, "#!/bin/sh\nsleep 2\n").expect("helper");
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))
            .expect("permissions");
        let file = std::fs::File::open(&input).expect("input file");
        let cancel = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancel);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            trigger.store(true, Ordering::Release);
        });
        let started = std::time::Instant::now();
        assert!(matches!(
            extract_document(&file, &helper, "pdf", MAX_BODY_BYTES, &cancel),
            Err(content::Error::Cancelled)
        ));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    fn minimal_pdf() -> Vec<u8> {
        let objects = [
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>\nendobj\n",
            "4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
            "5 0 obj\n<< /Length 59 >>\nstream\nBT /F1 18 Tf 72 720 Td (Genetec migration checklist) Tj ET\nendstream\nendobj\n",
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![0usize];
        for object in objects {
            offsets.push(pdf.len());
            pdf.extend_from_slice(object.as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len()).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets.iter().skip(1) {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                offsets.len()
            )
            .as_bytes(),
        );
        pdf
    }
}
