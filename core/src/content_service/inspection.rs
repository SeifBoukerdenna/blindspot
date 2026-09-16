use super::*;
use crate::ffi::index_native::Directory;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    pub title: &'static str,
    pub detail: String,
    pub next: &'static str,
    pub setting: &'static str,
    pub root: Option<String>,
    pub passages: Option<u64>,
    pub embedded: Option<u64>,
}

fn report(title: &'static str, detail: impl Into<String>, next: &'static str, setting: &'static str) -> Inspection {
    Inspection { title, detail: detail.into(), next, setting, root: None, passages: None, embedded: None }
}

impl ContentService {
    pub fn inspect_file(&self, path: &Path) -> Inspection {
        let (config, epoch) = {
            let control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
            (control.config.clone(), control.epoch)
        };
        let snapshot = self.snapshot();
        let result = inspect(path, &config, &snapshot, self.path.as_deref());
        if self.control.lock().unwrap_or_else(PoisonError::into_inner).epoch != epoch {
            return report("Settings changed", "The indexing configuration changed during this check.", "Check this file again.", "");
        }
        result
    }
}

fn inspect(path: &Path, config: &Content, snapshot: &Snapshot, database: Option<&Path>) -> Inspection {
    if !path.is_absolute() || path.as_os_str().len() > 4096
        || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return report("Invalid path", "Choose a file using the file picker.", "Choose another file.", "");
    }
    if !config.enabled {
        return report("Content search is off", "The stored index is retained, but content search is disabled.", "Enable content indexing.", "content.enabled");
    }
    if matches!(snapshot.phase, Phase::Erasing | Phase::Compacting) {
        return report("Index maintenance in progress", "The index is being maintained.", "Wait for maintenance to finish, then check again.", "");
    }
    let mut roots: Vec<_> = config.expanded_roots().into_iter().map(|root| {
        let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
        (root, canonical)
    }).collect();
    roots.sort_by_key(|(_, root)| root.components().count());
    let Some((root, path)) = roots.iter().find_map(|(spelled, root)| {
        path.strip_prefix(root).or_else(|_| path.strip_prefix(spelled)).ok()
            .map(|relative| (root, root.join(relative)))
    }) else {
        return report("Outside your indexed folders", "This file is not under a selected content or code folder. Filename search is separate from content search.", "Add its folder in Content settings if you want its text indexed.", "content.roots");
    };
    let policy = Policy { excluded_paths: config.expanded_exclusions().into_iter()
        .map(|p| p.canonicalize().unwrap_or(p)).collect(), ..Policy::default() };
    if !within_scope(&path, std::slice::from_ref(root), &policy) {
        return report("Excluded from indexing", "An exclusion or built-in privacy rule skips this file. Hidden files, generated folders and known secret files are not indexed.", "Review exclusions. Built-in privacy exclusions cannot be overridden here.", "content.excluded_paths");
    }
    let relative = path.strip_prefix(root).unwrap();
    let names: Vec<_> = relative.iter().collect();
    if names.len() > 64 {
        return report("Folder depth limit reached", "This file is deeper than the indexer's 64-level traversal limit.", "Choose a closer parent as an indexing root.", "content.roots");
    }
    let Some((leaf, parents)) = names.split_last() else {
        return report("Choose a file", "The selected path is an indexing root, not a document.", "Choose an individual file.", "");
    };
    let outcome = (|| -> std::io::Result<std::fs::Metadata> {
        let mut directory = Directory::open(root)?;
        if !directory.is_local()? { return Err(std::io::Error::other("nonlocal")); }
        let device = directory.device()?;
        for name in parents {
            directory = directory.child(name)?;
            if directory.device()? != device || !directory.is_local()? {
                return Err(std::io::Error::other("volume boundary"));
            }
        }
        let file = directory.file(leaf)?;
        let metadata = file.metadata()?;
        if metadata.dev() != device { return Err(std::io::Error::other("volume boundary")); }
        Ok(metadata)
    })();
    let metadata = match outcome {
        Ok(metadata) => metadata,
        Err(_) => return report("File unavailable to the indexer", "The file may be missing, unreadable, cloud-only, on a nonlocal volume, or behind a symbolic link. Blindspot does not download it or follow links to inspect its contents.", "Download it locally if needed, check folder access, and choose the original file.", "content.roots"),
    };
    if !content_indexer::supported(&path, config.documents) {
        return if content_indexer::supported(&path, true) {
            report("Document extraction is off", "This file type needs PDF and Office document indexing.", "Enable Index PDF and Office documents, then rescan its folder.", "content.documents")
        } else {
            report("Unsupported file type", "Background content indexing does not support this extension. Filename search may still find it.", "Export a local copy as a supported text, PDF or Office document.", "")
        };
    }
    let document = !content_indexer::supported(&path, false);
    let limit = if document {config.max_document_mb} else {config.max_file_mb}.saturating_mul(1_048_576);
    if metadata.len() > limit {
        return report("File exceeds the size limit", format!("This file is {} bytes; its configured indexing limit is {} bytes.", metadata.len(), limit), "Raise the appropriate file-size limit or index a smaller copy.", if document {"content.max_document_mb"} else {"content.max_file_mb"});
    }
    let mut result = match database.filter(|p| p.exists()) {
        None => report("Not indexed yet", "There is no content index to inspect yet.", "Let indexing start, then check again.", "content.enabled"),
        Some(database) => match ContentStore::open_reader(database).and_then(|store| store.file_status(&path)) {
            Err(_) => report("Index could not be read", "The read-only index check did not finish. This does not establish that the index is damaged.", "Wait for current work to finish and check again.", ""),
            Ok(None) => report("No indexed record", "This file is in scope, but no stored record exists. A scan may not have reached it, or a read/encoding check may have skipped it; the index has no recorded reason.", "Rescan its folder. For text files, check that the source is valid UTF-8 text.", ""),
            Ok(Some(status)) => {
                let modified = metadata.mtime().saturating_mul(1_000_000_000).saturating_add(metadata.mtime_nsec());
                let changed = metadata.ctime().saturating_mul(1_000_000_000).saturating_add(metadata.ctime_nsec());
                let mut result = if status.modified != modified || status.changed != changed || status.bytes != metadata.len() {
                    report("Indexed copy is out of date", "The file's current metadata differs from its last indexed version.", "Rescan its folder to refresh the stored passages.", "")
                } else { match status.extraction {
                    2 => report("No extractable text", "The last extraction found no text. A scanned PDF may need OCR; some documents are genuinely empty.", "For a scanned PDF, enable OCR, then retry failed items.", "content.ocr"),
                    3 => report("Password-protected document", "The last extraction reported a locked document.", "Save an unlocked local copy, then rescan its folder.", ""),
                    4 => report("Previously over the size limit", "The last extraction recorded an oversized document.", "Retry failed items after adjusting the size limit.", "content.max_document_mb"),
                    5 => report("Extraction did not succeed", "The last extraction was unreadable or unavailable. Any retained older passages may be out of date.", "Check that the document opens locally, then retry failed items.", ""),
                    _ if status.partial => report("Partially indexed", "Only part of this file was extracted. Text beyond a byte, passage, page or OCR limit may be absent.", "Review extraction limits; search a phrase in the indexed portion or split the source.", "content.ocr_pages"),
                    _ if status.passages == 0 => report("No stored passages", "The record exists but has no searchable passage text.", "Check the source contains text, then rescan its folder.", ""),
                    _ => report("Indexed for word search", "The index contains passages from this file. This is not a ranking or FTS integrity test; a specific query may still miss it.", "Try a distinctive phrase without kind/date filters in Documents search.", ""),
                }};
                let code_roots = config.expanded_code_roots().into_iter().filter_map(|p|p.canonicalize().ok()).collect::<Vec<_>>();
                if status.passages > 0 {
                    result.detail.push_str(if !config.semantic { " Search by meaning is turned off." }
                        else if !passage_engine::eligible_path(&path, &code_roots) { " This file is word-only under the current semantic rules; code needs a Code folder, and structured data stays word-only." }
                        else if status.embedded < status.passages { " Active-model embedding coverage is incomplete. Word search can still work; check model health and indexing progress." }
                        else { " Every stored passage has an active-generation embedding; model/cache availability is checked separately." });
                }
                result.passages = Some(status.passages);
                result.embedded = Some(status.embedded);
                result
            }
        },
    };
    result.root = root.to_str().map(str::to_owned);
    if snapshot.phase == Phase::Paused {
        result.detail.push_str(&format!(" Indexing is paused: {}.", snapshot.pause_reason.label()));
    } else if snapshot.progress.budget_exhausted {
        result.detail.push_str(" The last pass reached the index storage budget.");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!("blindspot-inspection-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir_all(path.join("root")).unwrap();
            Self(path.canonicalize().unwrap())
        }
        fn config(&self) -> Content { Content { roots: vec![self.0.join("root").to_string_lossy().into()], semantic:true, ..Content::default() } }
        fn check(&self, path: &Path, config: &Content) -> Inspection { inspect(path, config, &Snapshot::default(), Some(&self.0.join("index.sqlite"))) }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

    #[test]
    fn inspection_scope_types_and_limits_do_not_create_an_index() {
        let f = Fixture::new(); let mut config = f.config(); let root = f.0.join("root");
        let file = root.join("note.md"); std::fs::write(&file, "bicycle repairs").unwrap();
        assert_eq!(f.check(&file, &config).title, "Not indexed yet");
        assert_eq!(f.check(&f.0.join("outside.md"), &config).title, "Outside your indexed folders");
        assert_eq!(f.check(&root.join(".hidden.md"), &config).title, "Excluded from indexing");
        config.excluded_paths = vec![file.to_string_lossy().into()];
        assert_eq!(f.check(&file, &config).title, "Excluded from indexing"); config.excluded_paths.clear();
        let pdf = root.join("guide.pdf"); std::fs::write(&pdf, "fixture").unwrap(); config.documents=false;
        assert_eq!(f.check(&pdf, &config).title, "Document extraction is off");
        let other = root.join("image.png"); std::fs::write(&other, "fixture").unwrap();
        assert_eq!(f.check(&other, &config).title, "Unsupported file type");
        config.max_file_mb=0; assert_eq!(f.check(&file, &config).title, "File exceeds the size limit");
        config.enabled=false; assert_eq!(f.check(&file, &config).title, "Content search is off");
        assert!(!f.0.join("index.sqlite").exists());
    }

    #[test]
    fn inspection_reports_freshness_partial_and_active_embeddings() {
        let f = Fixture::new(); let config=f.config(); let root=f.0.join("root");
        let file=root.join("notes.md"); std::fs::write(&file,"Repair bicycle brakes before the mountain route.").unwrap();
        let mut store=ContentStore::open(&f.0.join("index.sqlite")).unwrap();
        content_indexer::scan_root(&mut store,&root,&Policy::default(),&AtomicBool::new(false), |_|{}).unwrap();
        let report=f.check(&file,&config); assert_eq!(report.title,"Indexed for word search");
        assert_eq!(report.passages,Some(1)); assert_eq!(report.embedded,Some(0)); assert!(report.detail.contains("incomplete"));
        let hits=store.search_passages("bicycle",&crate::content::SearchFilter::default(),std::slice::from_ref(&root),&[],Arc::new(AtomicBool::new(false))).unwrap();
        store.begin_passage_model("fixture",2).unwrap(); store.put_chunk_embedding(hits[0].chunk_id,"fixture",&[1.0,0.0]).unwrap();
        assert_eq!(f.check(&file,&config).embedded,Some(0));
        store.activate_passage_model("fixture",&AtomicBool::new(false)).unwrap();
        assert_eq!(f.check(&file,&config).embedded,Some(1));
        store.set_extraction_metadata(hits[0].passage.document_id,1,true,&[]).unwrap();
        assert_eq!(f.check(&file,&config).title,"Partially indexed");
        std::fs::write(&file,"Revised route with new content").unwrap();
        assert_eq!(f.check(&file,&config).title,"Indexed copy is out of date");
        assert_eq!(store.count().unwrap(),1);
    }

    #[test]
    fn inspection_rejects_symlinks_missing_files_and_traversal() {
        let f=Fixture::new(); let config=f.config(); let root=f.0.join("root");
        let outside=f.0.join("private.txt"); std::fs::write(&outside,"private").unwrap();
        std::os::unix::fs::symlink(&outside,root.join("link.txt")).unwrap();
        std::os::unix::fs::symlink(&f.0,root.join("linked")).unwrap();
        for file in [root.join("link.txt"),root.join("linked/private.txt"),root.join("missing.txt")] {
            assert_eq!(f.check(&file,&config).title,"File unavailable to the indexer");
        }
        assert_eq!(f.check(&root.join("../private.txt"),&config).title,"Invalid path");
    }

    #[test]
    fn inspection_explains_recorded_extraction_failures() {
        use crate::content::Extraction;
        let f=Fixture::new(); let config=f.config(); let root=f.0.join("root");
        let mut store=ContentStore::open(&f.0.join("index.sqlite")).unwrap();
        let scan=store.begin_scan(&root).unwrap();
        for (index, (status, expected)) in [
            (Extraction::NoText,"No extractable text"),
            (Extraction::Locked,"Password-protected document"),
            (Extraction::Oversized,"Previously over the size limit"),
            (Extraction::Unreadable,"Extraction did not succeed"),
        ].into_iter().enumerate() {
            let path=root.join(format!("fixture{index}.pdf")); std::fs::write(&path,"fixture").unwrap();
            let metadata=std::fs::metadata(&path).unwrap();
            let modified=metadata.mtime()*1_000_000_000+metadata.mtime_nsec();
            let changed=metadata.ctime()*1_000_000_000+metadata.ctime_nsec();
            store.mark_extraction_failure(&scan,&format!("fixture{index}"),&path,"fixture",modified,changed,metadata.len(),status).unwrap();
            assert_eq!(f.check(&path,&config).title,expected);
        }
        assert_eq!(store.count().unwrap(),4);
    }
}
