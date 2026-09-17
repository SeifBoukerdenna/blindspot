use super::*;
use std::path::PathBuf;

pub struct Scope<'a> {
    pub roots: &'a [PathBuf],
    pub exclusions: &'a [PathBuf],
}

#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub chunk_id: i64,
    pub revision: i64,
    pub passage: Passage,
    pub meaning: bool,
    pub words: bool,
}

pub struct FileStatus {
    pub modified: i64,
    pub changed: i64,
    pub bytes: u64,
    pub extraction: u8,
    pub partial: bool,
    pub passages: u64,
    pub embedded: u64,
}

impl ContentStore {
    pub fn file_status(&self, path: &Path) -> Result<Option<FileStatus>> {
        let path = validated_path(path)?;
        self.passage_read(Arc::new(AtomicBool::new(false)), || {
            Ok(self.connection.query_row(
                "SELECT modified_ns,changed_ns,bytes,extraction,partial,
                 (SELECT count(*) FROM chunks c WHERE c.document_id=d.id),
                 (SELECT count(*) FROM chunks c WHERE c.document_id=d.id AND EXISTS(
                    SELECT 1 FROM chunk_embeddings e JOIN passage_models m ON m.model=e.model
                    WHERE e.chunk_id=c.id AND m.active=1 AND m.complete=1 AND e.dimensions=m.dimensions))
                 FROM documents d WHERE path=?1", [path], |r| Ok(FileStatus {
                    modified:r.get(0)?, changed:r.get(1)?, bytes:r.get::<_,i64>(2)?.max(0) as u64, extraction:r.get(3)?,
                    partial:r.get(4)?, passages:r.get::<_,i64>(5)?.max(0) as u64, embedded:r.get::<_,i64>(6)?.max(0) as u64,
                 })).optional()?)
        })
    }

    pub fn stored_passage(&self, id:i64,path:&Path,roots:&[PathBuf],exclusions:&[PathBuf],cancel:Arc<AtomicBool>)->Result<Option<String>> {
        let path=validated_path(path)?;
        let scope=scope_sql(roots,exclusions)?;
        self.passage_read(cancel,||{
            let text=self.connection.query_row(&format!("SELECT c.text FROM chunks c JOIN documents d ON d.id=c.document_id WHERE c.id=?1 AND d.path=?2 {scope}"),
                params![id,path],|row|row.get::<_,String>(0)).optional()?;
            Ok(text.filter(|text|text.len()<=8192))
        })
    }

    pub fn search_passages(
        &self, query: &str, filter: &SearchFilter, roots: &[PathBuf], exclusions: &[PathBuf],
        cancel: Arc<AtomicBool>,
    ) -> Result<Vec<Match>> {
        let strict = expression(query)?;
        if strict.is_empty() { return Ok(Vec::new()); }
        let scope = scope_sql(roots, exclusions)?;
        self.passage_read(cancel, || {
            let sql = format!("SELECT c.id,d.revision,c.document_id,d.path,d.title,c.ordinal,c.page,c.line,c.heading,c.text,chunks_fts.rank
                FROM chunks_fts JOIN chunks c ON c.id=chunks_fts.rowid JOIN documents d ON d.id=c.document_id
                WHERE chunks_fts MATCH ?1 {} {scope} ORDER BY chunks_fts.rank,c.id LIMIT 100", filter.sql());
            let mut statement = self.connection.prepare(&sql)?;
            let mut found: Vec<Match> = statement.query_map([&strict], read_match)?
                .collect::<std::result::Result<_,_>>()?;
            if found.len() < 3 && strict.contains(" AND ") {
                let terms = question_terms(query);
                let relaxed = if terms.is_empty() { strict.replace(" AND ", " OR ") }
                    else { terms.iter().map(|term| format!("\"{term}\"")).collect::<Vec<_>>().join(" OR ") };
                for item in statement.query_map([relaxed], read_match)? {
                    let item = item?;
                    if !found.iter().any(|old| old.chunk_id == item.chunk_id) { found.push(item); }
                }
            }
            if found.len() < 100 {
                let mut legacy=self.connection.prepare(&format!("SELECT -d.id,d.revision,d.id,d.path,d.title,0,0,0,'',substr(d.body,1,1800),content_fts.rank
                    FROM content_fts JOIN documents d ON d.id=content_fts.rowid WHERE content_fts MATCH ?1
                    AND NOT EXISTS(SELECT 1 FROM chunks c WHERE c.document_id=d.id) {} {scope}
                    ORDER BY content_fts.rank,d.id LIMIT ?2",filter.sql()))?;
                for item in legacy.query_map(params![strict,(100-found.len()) as i64],read_match)? { found.push(item?); }
            }
            found.retain(|item| validated_path(Path::new(&item.passage.path)).is_ok());
            found.truncate(100);
            Ok(found)
        })
    }

    pub(super) fn passage_read<T>(&self, cancel: Arc<AtomicBool>, read: impl FnOnce() -> Result<T>) -> Result<T> {
        if cancel.load(Ordering::Acquire) { return Err(Error::Cancelled); }
        let started = Instant::now();
        let trigger = Arc::clone(&cancel);
        self.connection.progress_handler(1000, Some(move || trigger.load(Ordering::Acquire) || started.elapsed() > Duration::from_millis(250)))?;
        let result = read();
        self.connection.progress_handler(0, None::<fn() -> bool>)?;
        if cancel.load(Ordering::Acquire) || started.elapsed() > Duration::from_millis(250) { return Err(Error::Cancelled); }
        result
    }
}

pub(super) fn read_match(row: &rusqlite::Row<'_>) -> rusqlite::Result<Match> {
    Ok(Match { chunk_id: row.get(0)?, revision: row.get(1)?, meaning: false, words: true,
        passage: Passage { document_id: row.get(2)?, path: row.get(3)?, title: row.get(4)?, ordinal: row.get(5)?,
            page: row.get(6)?, line: row.get(7)?, heading: row.get(8)?, text: row.get(9)?, rank: row.get(10)? } })
}

pub(super) fn scope_sql(roots: &[PathBuf], exclusions: &[PathBuf]) -> Result<String> {
    if roots.len() > 64 || exclusions.len() > 128 { return Err(Error::Invalid("Too many search folders")); }
    let predicate = |paths: &[PathBuf]| -> Result<String> {
        paths.iter().map(|path| {
            let path = validated_path(path)?.trim_end_matches('/').replace('\'', "''");
            Ok(format!("(d.path='{path}' OR substr(d.path,1,length('{path}')+1)='{path}/')"))
        }).collect::<Result<Vec<_>>>().map(|parts| parts.join(" OR "))
    };
    let included = predicate(roots)?;
    let excluded = predicate(exclusions)?;
    Ok(format!(" AND ({}){}", if included.is_empty() { "0" } else { &included },
        if excluded.is_empty() { String::new() } else { format!(" AND NOT ({excluded})") }))
}

pub fn fuse(lexical: Vec<Match>, semantic: Vec<Match>, per_document: usize) -> Vec<Match> {
    let ranked=retrieval::fuse_ranks(&lexical.iter().map(|item|item.chunk_id).collect::<Vec<_>>(),
        &semantic.iter().map(|item|item.chunk_id).collect::<Vec<_>>());
    let mut candidates:std::collections::BTreeMap<_,_>=semantic.into_iter().chain(lexical).map(|item|(item.chunk_id,item)).collect();
    let mut selected: Vec<Match> = Vec::new();
    for rank in ranked {
        let Some(mut item)=candidates.remove(&rank.id) else {continue;};
        let same: Vec<_> = selected.iter().filter(|old| old.passage.document_id==item.passage.document_id).collect();
        if same.len() >= per_document || same.iter().any(|old| retrieval::substantially_overlaps(&old.passage.text,&item.passage.text)) { continue; }
        item.passage.rank = -rank.score;
        item.words=rank.words;
        item.meaning=rank.meaning;
        selected.push(item);
        if selected.len() == 100 { break; }
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn passage_filters_apply_before_limits_and_scope_is_literal() {
        let mut store = ContentStore { connection: Connection::open_in_memory().unwrap() };
        store.migrate().unwrap();
        let scan = store.begin_scan(Path::new("/fixture")).unwrap();
        let paths: Vec<_> = (0..120).map(|n| format!("/fixture/private/{n}.md")).chain(std::iter::once("/fixture/kept/a.pdf".into())).collect();
        let docs: Vec<_> = paths.iter().map(|path| Document { identity: path, path: Path::new(path), title: path,
            body: "renewal deadline", modified_ns: 1, changed_ns: 1, bytes: 10, extraction: Extraction::Text }).collect();
        store.put_batch(&scan,&docs).unwrap();
        let cancel = || Arc::new(AtomicBool::new(false));
        let results = store.search_passages("renewal", &SearchFilter::default(), &["/fixture".into()], &["/fixture/private".into()],cancel()).unwrap();
        assert_eq!(results.len(),1);
        assert_eq!(store.stored_passage(results[0].chunk_id,Path::new(&results[0].passage.path),&["/fixture".into()],&[],cancel()).unwrap().as_deref(),Some("renewal deadline"));
        assert!(store.stored_passage(results[0].chunk_id,Path::new("/fixture/wrong.pdf"),&["/fixture".into()],&[],cancel()).unwrap().is_none());
        assert!(store.stored_passage(results[0].chunk_id,Path::new(&results[0].passage.path),&["/fixture".into()],&["/fixture/kept".into()],cancel()).unwrap().is_none());
        assert!(store.search_passages("renewal", &SearchFilter::default(), &["/fixture' OR 1=1 --".into()], &[],cancel()).unwrap().is_empty());
        assert!(matches!(store.search_passages("renewal", &SearchFilter::default(), &["/fixture".into()], &[],Arc::new(AtomicBool::new(true))),Err(Error::Cancelled)));
        let result = fuse(results.clone(),results,2);
        assert_eq!(result.len(),1);
        assert!(result[0].words && result[0].meaning);
        store.connection.execute("DELETE FROM chunks WHERE id=?1",[result[0].chunk_id]).unwrap();
        assert!(store.stored_passage(result[0].chunk_id,Path::new(&result[0].passage.path),&["/fixture".into()],&[],cancel()).unwrap().is_none());
    }
}
