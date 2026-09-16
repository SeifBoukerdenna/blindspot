//! Durable, bounded content retrieval. Owned by background workers, never the UI thread.

pub mod vectors;
pub mod passage_search;
pub mod passage_vectors;

use blindspot_retrieval as retrieval;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::path::{Component, Path};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

const APPLICATION_ID: i64 = 0x42534349;
const SCHEMA_VERSION: i64 = 10;
pub const MAX_BODY_BYTES: usize = 65_536;
pub const MAX_PASSAGE_BYTES: usize = 2 * 1024 * 1024;

/// Constraints from `kind:`, `size:` and `modified:` on a content query. Applied inside the ranked
/// candidate set, so a filter narrows the results instead of emptying the first page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchFilter {
    pub extensions: Vec<String>,
    pub min_bytes: Option<u64>,
    pub max_bytes: Option<u64>,
    pub modified_after_ns: Option<i64>,
    pub modified_before_ns: Option<i64>,
}

impl SearchFilter {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// SQL over `d` built only from validated literals: extensions must be plain alphanumerics
    /// (anything else matches nothing rather than widening the query) and the rest are integers.
    fn sql(&self) -> String {
        let mut clauses = Vec::new();
        if !self.extensions.is_empty() {
            let valid: Vec<_> = self
                .extensions
                .iter()
                .filter(|extension| {
                    (1..=16).contains(&extension.len())
                        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
                })
                .map(|extension| {
                    format!("lower(d.path) GLOB '*.{}'", extension.to_ascii_lowercase())
                })
                .collect();
            clauses.push(if valid.len() == self.extensions.len() {
                format!("({})", valid.join(" OR "))
            } else {
                "0".to_owned()
            });
        }
        if let Some(min) = self.min_bytes {
            clauses.push(format!("d.bytes>={}", min.min(i64::MAX as u64)));
        }
        if let Some(max) = self.max_bytes {
            clauses.push(format!("d.bytes<={}", max.min(i64::MAX as u64)));
        }
        if let Some(after) = self.modified_after_ns {
            clauses.push(format!("d.modified_ns>={after}"));
        }
        if let Some(before) = self.modified_before_ns {
            clauses.push(format!("d.modified_ns<{before}"));
        }
        clauses
            .iter()
            .map(|clause| format!(" AND {clause}"))
            .collect()
    }
}

/// Which excerpt a result shows: around the matched words, or the start of a document found by
/// meaning (which has no matched words to centre on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Snippet {
    Matched,
    Opening,
}

const STOPWORDS: &[&str] = &[
    "about",
    "after",
    "all",
    "and",
    "any",
    "are",
    "been",
    "but",
    "can",
    "could",
    "did",
    "does",
    "doc",
    "docs",
    "document",
    "documents",
    "file",
    "files",
    "find",
    "for",
    "from",
    "had",
    "has",
    "have",
    "how",
    "into",
    "its",
    "mention",
    "mentioned",
    "note",
    "notes",
    "not",
    "our",
    "said",
    "say",
    "says",
    "should",
    "show",
    "that",
    "the",
    "their",
    "them",
    "there",
    "they",
    "this",
    "those",
    "was",
    "were",
    "what",
    "when",
    "where",
    "which",
    "who",
    "why",
    "will",
    "with",
    "would",
    "you",
    "your",
];

/// The distinctive words of a natural-language question, for an any-word search: its function
/// words would otherwise make an all-words match impossible.
pub fn question_terms(question: &str) -> Vec<String> {
    let mut terms = Vec::new();
    for word in question
        .split(|character: char| !character.is_alphanumeric())
        .map(str::to_lowercase)
    {
        if word.chars().count() >= 3
            && word.len() <= 64
            && !STOPWORDS.contains(&word.as_str())
            && !terms.contains(&word)
        {
            terms.push(word);
            if terms.len() == 12 {
                break;
            }
        }
    }
    terms
}

/// What compaction reclaimed, measured from the database files before and after.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CompactReport {
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub removed_vectors: u64,
}

fn database_bytes(path: &Path) -> u64 {
    ["", "-wal", "-shm"]
        .iter()
        .filter_map(|suffix| std::fs::metadata(format!("{}{suffix}", path.display())).ok())
        .map(|metadata| metadata.len())
        .sum()
}

fn available_bytes(directory: &Path) -> Option<u64> {
    let path = std::ffi::CString::new(directory.as_os_str().as_encoded_bytes()).ok()?;
    // SAFETY: an all-zero statvfs is valid output storage that statvfs overwrites on success.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: the path is NUL-terminated and `stats` is live, writable storage for the call.
    if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    Some(u64::from(stats.f_bavail).saturating_mul(stats.f_frsize))
}

fn tidy_excerpt(text: &str) -> String {
    let words: Vec<_> = text.split_whitespace().collect();
    let mut excerpt = words.join(" ");
    if excerpt.len() > 180 {
        let end = excerpt.floor_char_boundary(180);
        excerpt.truncate(end);
        excerpt.push('…');
    }
    excerpt
}

/// Semantic vectors only help for prose-like sources: on a 41k-document home index the local
/// model ranked code and structured data no better than noise, so those stay lexical-only.
/// Extracted documents are eligible only when extraction produced text.
pub(crate) const SEMANTIC_ELIGIBLE: &str = "(d.extraction=1 OR (d.extraction=0 AND (lower(d.path) GLOB '*.txt' OR lower(d.path) GLOB '*.md'
 OR lower(d.path) GLOB '*.markdown' OR lower(d.path) GLOB '*.rst' OR lower(d.path) GLOB '*.html')))";

/// Rust mirror of [`SEMANTIC_ELIGIBLE`], used to decide whether a pass wrote anything the
/// semantic stage would embed.
pub fn semantic_eligible(path: &Path, extraction: Extraction) -> bool {
    match extraction {
        Extraction::Extracted => true,
        Extraction::Text => path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "txt" | "md" | "markdown" | "rst" | "html"
                )
            }),
        _ => false,
    }
}

/// How a document's body was obtained. Stored so unreadable documents are counted once and not
/// re-extracted on every pass; only a changed file is retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Extraction {
    #[default]
    Text = 0,
    Extracted = 1,
    NoText = 2,
    Locked = 3,
    Oversized = 4,
    Unreadable = 5,
}
pub const MAX_BATCH: usize = 256;
const MAX_RESULTS: usize = 100;
pub const MAX_CANDIDATES: usize = 10_000;

const SCHEMA_V1: &str = r#"
CREATE TABLE scopes(root TEXT PRIMARY KEY, generation INTEGER NOT NULL) WITHOUT ROWID;
CREATE TABLE documents(
 id INTEGER PRIMARY KEY AUTOINCREMENT, identity TEXT NOT NULL UNIQUE, root TEXT NOT NULL,
 path TEXT NOT NULL UNIQUE, title TEXT NOT NULL, body TEXT NOT NULL,
 modified_ns INTEGER NOT NULL, bytes INTEGER NOT NULL CHECK(bytes>=0),
 seen INTEGER NOT NULL, revision INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX documents_scan ON documents(root,seen,id);
CREATE VIRTUAL TABLE content_fts USING fts5(title,body,content='documents',content_rowid='id',tokenize='unicode61');
INSERT INTO content_fts(content_fts,rank) VALUES('rank','bm25(4.0,1.0)');
CREATE TRIGGER documents_insert AFTER INSERT ON documents BEGIN
 INSERT INTO content_fts(rowid,title,body) VALUES(new.id,new.title,new.body);
END;
CREATE TRIGGER documents_delete AFTER DELETE ON documents BEGIN
 INSERT INTO content_fts(content_fts,rowid,title,body) VALUES('delete',old.id,old.title,old.body);
END;
CREATE TRIGGER documents_update AFTER UPDATE OF title,body ON documents BEGIN
 INSERT INTO content_fts(content_fts,rowid,title,body) VALUES('delete',old.id,old.title,old.body);
 INSERT INTO content_fts(rowid,title,body) VALUES(new.id,new.title,new.body);
END;
"#;

const SCHEMA_V2: &str = r#"
CREATE TABLE embeddings(
 document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
 revision INTEGER NOT NULL, model TEXT NOT NULL, dimensions INTEGER NOT NULL,
 vector BLOB NOT NULL CHECK(length(vector)=dimensions*4)
);
"#;

/// Passages, and the search over them. External content again, so the text is stored once in
/// `chunks`; the triggers keep the index in step exactly as `content_fts` does for documents.
/// `symbols` carries identifiers split into words, which is why it is its own column: a code
/// search for "search filtered" must not be outranked by prose that happens to use both words.
const SCHEMA_V8: &str = r#"
CREATE TABLE IF NOT EXISTS chunks(
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
 ordinal INTEGER NOT NULL CHECK(ordinal>=0),
 page INTEGER NOT NULL DEFAULT 0 CHECK(page>=0),
 line INTEGER NOT NULL DEFAULT 0 CHECK(line>=0),
 heading TEXT NOT NULL, text TEXT NOT NULL, symbols TEXT NOT NULL,
 UNIQUE(document_id,ordinal)
);
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(heading,text,symbols,content='chunks',content_rowid='id',tokenize='unicode61');
INSERT INTO chunks_fts(chunks_fts,rank) VALUES('rank','bm25(2.0,1.0,1.0)');
CREATE TRIGGER IF NOT EXISTS chunks_insert AFTER INSERT ON chunks BEGIN
 INSERT INTO chunks_fts(rowid,heading,text,symbols) VALUES(new.id,new.heading,new.text,new.symbols);
END;
CREATE TRIGGER IF NOT EXISTS chunks_delete AFTER DELETE ON chunks BEGIN
 INSERT INTO chunks_fts(chunks_fts,rowid,heading,text,symbols) VALUES('delete',old.id,old.heading,old.text,old.symbols);
END;
CREATE TRIGGER IF NOT EXISTS chunks_update AFTER UPDATE OF heading,text,symbols ON chunks BEGIN
 INSERT INTO chunks_fts(chunks_fts,rowid,heading,text,symbols) VALUES('delete',old.id,old.heading,old.text,old.symbols);
 INSERT INTO chunks_fts(rowid,heading,text,symbols) VALUES(new.id,new.heading,new.text,new.symbols);
END;
"#;

/// One vector per passage, beside the whole-document vectors rather than replacing them, so the
/// existing semantic search keeps serving while this set fills in.
///
/// No revision column, unlike `embeddings`: [`write_chunks`] deletes and re-inserts a document's
/// chunks on every rewrite, and `chunks.id` is AUTOINCREMENT, so a changed file's old vectors are
/// cascaded away and their ids are never handed to a new chunk. Staleness cannot outlive the chunk.
const SCHEMA_V9: &str = r#"
CREATE TABLE IF NOT EXISTS chunk_embeddings(
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 chunk_id INTEGER NOT NULL UNIQUE REFERENCES chunks(id) ON DELETE CASCADE,
 model TEXT NOT NULL CHECK(length(CAST(model AS BLOB)) BETWEEN 1 AND 128),
 dimensions INTEGER NOT NULL CHECK(dimensions BETWEEN 1 AND 2048),
 vector BLOB NOT NULL CHECK(length(vector)=dimensions*4)
);
"#;

/// Which passages earn a vector. Wider than [`SEMANTIC_ELIGIBLE`] on purpose: a whole-file vector
/// built from a source file's opening lines ranked as noise, but a passage is one declaration with
/// its symbols, which is the case chunking was built for. Machine data stays lexical-only — a
/// vector over a JSON fragment or a CSV row describes its shape, not its meaning.
pub(crate) const CHUNK_ELIGIBLE: &str = "((d.extraction=1 AND lower(d.path) NOT GLOB '*.xlsx') OR (d.extraction=0 AND lower(d.path) NOT GLOB '*.json'
 AND lower(d.path) NOT GLOB '*.csv' AND lower(d.path) NOT GLOB '*.tsv' AND lower(d.path) NOT GLOB '*.xml'
 AND lower(d.path) NOT GLOB '*.yaml' AND lower(d.path) NOT GLOB '*.yml' AND lower(d.path) NOT GLOB '*.toml'))";

/// Rust mirror of [`CHUNK_ELIGIBLE`].
pub fn chunk_eligible(path: &Path, extraction: Extraction) -> bool {
    match extraction {
        Extraction::Extracted => !path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("xlsx")),
        Extraction::Text => !path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "json" | "csv" | "tsv" | "xml" | "yaml" | "yml" | "toml"
                )
            }),
        _ => false,
    }
}

/// A passage of a file: what search returns and what "open at" uses.
#[derive(Debug, Clone, PartialEq)]
pub struct Passage {
    pub document_id: i64,
    pub path: String,
    pub title: String,
    pub ordinal: i64,
    /// Page for an extracted document, else 0.
    pub page: i64,
    /// Line for text and code, else 0.
    pub line: i64,
    pub heading: String,
    pub text: String,
    pub rank: f64,
}

#[derive(Debug)]
pub enum Error {
    Database(rusqlite::Error),
    Io(std::io::Error),
    Invalid(&'static str),
    UnsupportedSchema(i64),
    Cancelled,
}
impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(_) => f.write_str("Content database unavailable"),
            Self::Io(_) => f.write_str("Content database file unavailable"),
            Self::Invalid(reason) => f.write_str(reason),
            Self::UnsupportedSchema(version) => {
                write!(f, "Unsupported content database version {version}")
            }
            Self::Cancelled => f.write_str("Content search cancelled or timed out"),
        }
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone)]
pub struct Scan {
    root: String,
    generation: i64,
}

pub struct Document<'a> {
    pub identity: &'a str,
    pub path: &'a Path,
    pub title: &'a str,
    pub body: &'a str,
    pub modified_ns: i64,
    pub changed_ns: i64,
    pub bytes: u64,
    pub extraction: Extraction,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub id: i64,
    pub identity: String,
    pub path: String,
    pub title: String,
    pub rank: f64,
    pub revision: i64,
    /// Found only by semantic similarity, without an exact query-term match.
    pub related: bool,
    /// A short excerpt showing why the document matched, when one was computed.
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchPage {
    pub hits: Vec<Hit>,
    /// A common query term requires index-ID ordering instead of global BM25.
    /// Callers must label this degraded ranking and offer query refinement.
    pub limited: bool,
}

#[derive(Debug, Clone)]
pub struct EmbeddingDocument {
    pub id: i64,
    pub revision: i64,
    pub path: String,
    pub text: String,
}

#[derive(Debug, Default)]
pub struct EmbeddingPage {
    pub after: i64,
    pub scanned: usize,
    pub pending: Vec<EmbeddingDocument>,
    /// Documents deliberately kept out of semantic retrieval (code, data, failed extraction).
    pub lexical_only: usize,
}

/// A passage awaiting a vector. No revision: a rewritten document's chunks are deleted and
/// re-inserted with fresh ids, so a vector cannot outlive the text it describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingChunk {
    pub id: i64,
    pub document_id: i64,
    pub path: String,
    /// The heading breadcrumb and the passage, which is what reaches the model.
    pub text: String,
}

#[derive(Debug, Default)]
pub struct ChunkEmbeddingPage {
    pub after: i64,
    pub scanned: usize,
    pub pending: Vec<PendingChunk>,
    /// Passages deliberately kept lexical-only: machine data, and failed extraction.
    pub lexical_only: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanOutcome {
    Complete,
    Interrupted,
}

pub struct ContentStore {
    connection: Connection,
}

impl ContentStore {
    pub fn storage_bytes(&self) -> Result<u64> {
        let path:String=self.connection.query_row("SELECT file FROM pragma_database_list WHERE name='main'",[],|row|row.get(0))?;
        if path.is_empty() { return Ok(0); }
        let path=Path::new(&path);
        let mut total=database_bytes(path);
        if let Some(parent)=path.parent() {
            let directory=parent.join("content-vectors");
            match std::fs::symlink_metadata(&directory) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink()=>{
                    for entry in std::fs::read_dir(directory)?.take(10_001) {
                        let entry=entry?;
                        let metadata=std::fs::symlink_metadata(entry.path())?;
                        if metadata.is_file() { total=total.saturating_add(metadata.len()); }
                    }
                },
                Err(error) if error.kind()==std::io::ErrorKind::NotFound=>{},
                _=>return Err(Error::Invalid("Vector storage unavailable")),
            }
        }
        Ok(total)
    }

    pub fn open(path: &Path) -> Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        if let Some(parent) = path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        let path = database_path(path)?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        let mut store = Self { connection };
        store.migrate()?;
        store.connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA secure_delete=ON; PRAGMA cache_size=-8192; PRAGMA wal_autocheckpoint=1000;")?;
        store.connection.execute(
            "INSERT INTO content_fts(content_fts,rank) VALUES('secure-delete',1)",
            [],
        )?;
        store.connection.execute(
            "INSERT INTO chunks_fts(chunks_fts,rank) VALUES('secure-delete',1)",
            [],
        )?;
        store.connection.busy_timeout(Duration::from_millis(100))?;
        Ok(store)
    }

    pub fn open_reader(path: &Path) -> Result<Self> {
        let connection = Connection::open_with_flags(
            database_path(path)?,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let application: i64 = connection.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        if version != SCHEMA_VERSION || application != APPLICATION_ID {
            return Err(Error::UnsupportedSchema(version));
        }
        connection.execute_batch("PRAGMA query_only=ON; PRAGMA cache_size=-4096;")?;
        connection.busy_timeout(Duration::from_millis(50))?;
        Ok(Self { connection })
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i64 = self
            .connection
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let application: i64 = self
            .connection
            .query_row("PRAGMA application_id", [], |r| r.get(0))?;
        if !(0..=SCHEMA_VERSION).contains(&version) {
            return Err(Error::UnsupportedSchema(version));
        }
        if application != APPLICATION_ID {
            let tables: i64 = self.connection.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
                [],
                |r| r.get(0),
            )?;
            if application != 0 || version != 0 || tables != 0 {
                return Err(Error::Invalid("This is not a Blindspot content database"));
            }
        }
        let transaction = self.connection.transaction()?;
        if version == 0 {
            transaction.execute_batch(SCHEMA_V1)?;
        }
        if version < 2 {
            transaction.execute_batch(SCHEMA_V2)?;
        }
        if version < 3 {
            transaction.execute_batch(
                "ALTER TABLE documents ADD COLUMN changed_ns INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        if version < 4 {
            transaction.execute_batch("CREATE TABLE scan_clock(id INTEGER PRIMARY KEY CHECK(id=1), generation INTEGER NOT NULL);
                INSERT INTO scan_clock SELECT 1,coalesce(max(generation),0) FROM scopes;")?;
        }
        if version < 5 {
            transaction.execute_batch("ALTER TABLE embeddings RENAME TO embeddings_v4;
                CREATE TABLE embeddings(
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    document_id INTEGER NOT NULL UNIQUE REFERENCES documents(id) ON DELETE CASCADE,
                    revision INTEGER NOT NULL, model TEXT NOT NULL, dimensions INTEGER NOT NULL,
                    vector BLOB NOT NULL CHECK(length(vector)=dimensions*4)
                );
                INSERT INTO embeddings(document_id,revision,model,dimensions,vector)
                    SELECT document_id,revision,model,dimensions,vector FROM embeddings_v4 ORDER BY document_id;
                DROP TABLE embeddings_v4;")?;
        }
        if version < 6 {
            transaction.execute_batch("CREATE TABLE vector_shards(
                after_key INTEGER PRIMARY KEY CHECK(after_key>=0 AND after_key%65536=0 AND after_key<=9223372036854710271),
                through_key INTEGER NOT NULL CHECK(through_key>after_key AND through_key-after_key<=65536),
                model TEXT NOT NULL CHECK(length(CAST(model AS BLOB)) BETWEEN 1 AND 128),
                dimensions INTEGER NOT NULL CHECK(dimensions BETWEEN 1 AND 2048),
                token TEXT NOT NULL UNIQUE CHECK(length(token)=32 AND token NOT GLOB '*[^0-9a-f]*'),
                checksum TEXT NOT NULL CHECK(length(checksum)=64 AND checksum NOT GLOB '*[^0-9a-f]*'),
                count INTEGER NOT NULL CHECK(count BETWEEN 1 AND 65536),
                bytes INTEGER NOT NULL CHECK(bytes BETWEEN 64 AND 402653184)
            );")?;
        }
        if version < 7 {
            // Keep whole-document vectors and their published shards compatible with the
            // existing semantic index. Passage vectors need a separate migration once all
            // readers and writers support them without invalidating this cache.
            // Checked rather than assumed: a database restored from a partial upgrade may already
            // carry the column, and re-adding it would make the whole migration fail.
            let existing: Option<(String, i64, Option<String>)> = transaction.query_row(
                "SELECT type,\"notnull\",dflt_value FROM pragma_table_info('documents') WHERE name='extraction'", [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
            if let Some((kind, not_null, default)) = existing {
                if kind != "INTEGER" || not_null != 1 || default.as_deref() != Some("0") {
                    return Err(Error::Invalid(
                        "Incompatible content database extraction column",
                    ));
                }
            } else {
                transaction.execute_batch("ALTER TABLE documents ADD COLUMN extraction INTEGER NOT NULL DEFAULT 0 CHECK(extraction BETWEEN 0 AND 5);")?;
            }
        }
        transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
        if version < 8 {
            // Additive: documents, their bodies and the published vector shards are untouched, so
            // search keeps working on the old rows while chunks fill in file by file.
            transaction.execute_batch(SCHEMA_V8)?;
        }
        if version < 9 {
            // Additive for the same reason: whole-document vectors and their shards keep serving
            // semantic search while passage vectors are embedded file by file.
            transaction.execute_batch(SCHEMA_V9)?;
        }
        if version < 10 {
            let present: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('documents') WHERE name='passage_version')",
                [], |row| row.get(0))?;
            if !present {
                transaction.execute_batch("ALTER TABLE documents ADD COLUMN passage_version INTEGER NOT NULL DEFAULT 0;
                    ALTER TABLE documents ADD COLUMN partial INTEGER NOT NULL DEFAULT 0;")?;
            }
            let generations: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='passage_models')", [], |row| row.get(0))?;
            if !generations {
                transaction.execute_batch("ALTER TABLE chunk_embeddings RENAME TO chunk_embeddings_v9;
                    CREATE TABLE chunk_embeddings(
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        chunk_id INTEGER NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
                        model TEXT NOT NULL CHECK(length(CAST(model AS BLOB)) BETWEEN 1 AND 128),
                        dimensions INTEGER NOT NULL CHECK(dimensions BETWEEN 1 AND 2048),
                        vector BLOB NOT NULL CHECK(length(vector)=dimensions*4 OR length(vector)=dimensions),
                        scale REAL NOT NULL DEFAULT 0,
                        UNIQUE(chunk_id,model,dimensions));
                    INSERT INTO chunk_embeddings(id,chunk_id,model,dimensions,vector) SELECT * FROM chunk_embeddings_v9;
                    UPDATE sqlite_sequence SET seq=max(seq,coalesce((SELECT seq FROM sqlite_sequence WHERE name='chunk_embeddings_v9'),0)) WHERE name='chunk_embeddings';
                    DROP TABLE chunk_embeddings_v9;
                    CREATE TABLE passage_models(model TEXT PRIMARY KEY, dimensions INTEGER NOT NULL,
                        active INTEGER NOT NULL DEFAULT 0, complete INTEGER NOT NULL DEFAULT 0);
                    CREATE UNIQUE INDEX passage_active_model ON passage_models(active) WHERE active=1;
                    CREATE TABLE passage_shards(model TEXT NOT NULL, after_key INTEGER NOT NULL,
                        through_key INTEGER NOT NULL, dimensions INTEGER NOT NULL, token TEXT NOT NULL UNIQUE,
                        checksum TEXT NOT NULL, count INTEGER NOT NULL, bytes INTEGER NOT NULL,
                        PRIMARY KEY(model,after_key));")?;
            }
        }
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn begin_scan(&mut self, root: &Path) -> Result<Scan> {
        let root = validated_path(root)?;
        let transaction = self.connection.transaction()?;
        let generation: i64 = transaction.query_row(
            "UPDATE scan_clock SET generation=generation+1 WHERE id=1 RETURNING generation",
            [],
            |r| r.get(0),
        )?;
        transaction.execute("INSERT INTO scopes(root,generation) VALUES(?1,?2) ON CONFLICT(root) DO UPDATE SET generation=excluded.generation",
            params![root,generation])?;
        transaction.commit()?;
        Ok(Scan {
            root: root.into(),
            generation,
        })
    }

    pub fn put_batch(
        &mut self,
        scan: &Scan,
        documents: &[Document<'_>],
    ) -> Result<Vec<(i64, i64)>> {
        if documents.len() > MAX_BATCH {
            return Err(Error::Invalid("Content batch is too large"));
        }
        for document in documents {
            validate_document(scan, document)?;
        }
        let transaction = self.connection.transaction()?;
        check_scan(&transaction, scan)?;
        let mut identities = Vec::with_capacity(documents.len());
        for document in documents {
            let path = validated_path(document.path)?;
            let representative: Option<(i64,i64)> = transaction.query_row(
                "SELECT id,revision FROM documents WHERE identity=?1 AND root=?2 AND seen=?3 AND path<?4",
                params![document.identity,scan.root,scan.generation,path], |row|Ok((row.get(0)?,row.get(1)?))).optional()?;
            if let Some(representative) = representative {
                identities.push(representative);
                continue;
            }
            transaction.execute(
                "DELETE FROM documents WHERE path=?1 AND identity<>?2",
                params![path, document.identity],
            )?;
            let identity = transaction.query_row(
                "INSERT INTO documents(identity,root,path,title,body,modified_ns,bytes,seen,changed_ns,extraction) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                 ON CONFLICT(identity) DO UPDATE SET root=excluded.root,path=excluded.path,title=excluded.title,body=excluded.body,
                 modified_ns=excluded.modified_ns,bytes=excluded.bytes,seen=excluded.seen,changed_ns=excluded.changed_ns,extraction=excluded.extraction,revision=documents.revision+1 RETURNING id,revision",
                params![document.identity,scan.root,path,document.title,&document.body[..document.body.floor_char_boundary(MAX_BODY_BYTES.min(document.body.len()))],document.modified_ns,document.bytes as i64,scan.generation,document.changed_ns,document.extraction as i64],
                |r| Ok((r.get(0)?,r.get(1)?)))?;
            transaction.execute("DELETE FROM embeddings WHERE document_id=?1", [identity.0])?;
            write_chunks(&transaction, identity.0, document)?;
            identities.push(identity);
        }
        transaction.commit()?;
        Ok(identities)
    }

    pub fn mark_unchanged(
        &mut self,
        scan: &Scan,
        identity: &str,
        path: &Path,
        modified_ns: i64,
        changed_ns: i64,
        bytes: u64,
    ) -> Result<bool> {
        self.mark_unchanged_version(scan,identity,path,modified_ns,changed_ns,bytes,1)
    }

    #[expect(clippy::too_many_arguments, reason="matches indexed file metadata and extraction version")]
    pub fn mark_unchanged_version(&mut self,scan:&Scan,identity:&str,path:&Path,modified_ns:i64,changed_ns:i64,bytes:u64,version:i64)->Result<bool> {
        let path = validated_path(path)?;
        if bytes > i64::MAX as u64 {
            return Err(Error::Invalid("Invalid file size"));
        }
        let transaction = self.connection.transaction()?;
        check_scan(&transaction, scan)?;
        let changed = transaction.execute("UPDATE documents SET seen=?1 WHERE identity=?2 AND path=?3 AND root=?4 AND modified_ns=?5 AND bytes=?6 AND changed_ns=?7 AND extraction IN (0,1) AND passage_version=?8",
            params![scan.generation,identity,path,scan.root,modified_ns,bytes as i64,changed_ns,version])?;
        transaction.commit()?;
        Ok(changed != 0)
    }

    pub fn set_extraction_metadata(&mut self,id:i64,version:i64,partial:bool,ocr_pages:&[u32])->Result<()> {
        let transaction=self.connection.transaction()?;
        transaction.execute("UPDATE documents SET passage_version=?2,partial=max(partial,?3) WHERE id=?1",params![id,version,partial])?;
        for page in ocr_pages.iter().take(100) {
            transaction.execute("UPDATE chunks SET heading='Read by OCR · '||heading WHERE document_id=?1 AND page=?2",params![id,page])?;
        }
        transaction.commit()?;
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors the identity/metadata fields a scan records for every file"
    )]
    pub fn mark_extraction_failure(
        &mut self,
        scan: &Scan,
        identity: &str,
        path: &Path,
        title: &str,
        modified_ns: i64,
        changed_ns: i64,
        bytes: u64,
        extraction: Extraction,
    ) -> Result<()> {
        let path = validated_path(path)?;
        if identity.is_empty()
            || identity.len() > 128
            || identity.contains('\0')
            || title.len() > 1024
            || bytes > i64::MAX as u64
        {
            return Err(Error::Invalid("Invalid extraction failure metadata"));
        }
        let transaction = self.connection.transaction()?;
        check_scan(&transaction, scan)?;
        transaction.execute(
            "DELETE FROM documents WHERE path=?1 AND identity<>?2",
            params![path, identity],
        )?;
        transaction.execute(
            "INSERT INTO documents(identity,root,path,title,body,modified_ns,bytes,seen,changed_ns,extraction)
             VALUES(?1,?2,?3,?4,'',?5,?6,?7,?8,?9)
             ON CONFLICT(identity) DO UPDATE SET root=excluded.root,path=excluded.path,title=excluded.title,
             modified_ns=excluded.modified_ns,bytes=excluded.bytes,seen=excluded.seen,changed_ns=excluded.changed_ns,
             extraction=excluded.extraction,body=documents.body,revision=documents.revision+1",
            params![
                identity,
                scan.root,
                path,
                title,
                modified_ns,
                bytes as i64,
                scan.generation,
                changed_ns,
                extraction as i64,
            ],
        )?;
        transaction.execute(
            "DELETE FROM embeddings WHERE document_id=(SELECT id FROM documents WHERE identity=?1)",
            [identity],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Removes records at or below `path` that this scan did not see. Only for event-driven partial
    /// scans: records elsewhere in the root keep their older generation until the next full pass.
    /// Uses a range on the unique path index rather than a prefix pattern over the whole root.
    pub fn remove_unseen_under(
        &mut self,
        scan: &Scan,
        path: &Path,
        cancel: &AtomicBool,
    ) -> Result<usize> {
        let path = validated_path(path)?;
        if !Path::new(path).starts_with(&scan.root) || path == scan.root {
            return Err(Error::Invalid("Path is outside its indexing root"));
        }
        let (first, after) = (format!("{path}/"), format!("{path}0"));
        let mut total = 0;
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(Error::Cancelled);
            }
            let transaction = self.connection.transaction()?;
            check_scan(&transaction, scan)?;
            let removed = transaction.execute(
                "DELETE FROM documents WHERE id IN (SELECT id FROM documents WHERE (path=?1 OR (path>=?2 AND path<?3))
                 AND root=?4 AND seen<>?5 LIMIT ?6)",
                params![path, first, after, scan.root, scan.generation, MAX_BATCH as i64])?;
            transaction.commit()?;
            total += removed;
            if removed < MAX_BATCH {
                return Ok(total);
            }
        }
    }

    pub fn finish_scan(
        &mut self,
        scan: &Scan,
        outcome: ScanOutcome,
        cancel: &AtomicBool,
    ) -> Result<usize> {
        if outcome == ScanOutcome::Interrupted {
            return Ok(0);
        }
        let mut total = 0;
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(Error::Cancelled);
            }
            let transaction = self.connection.transaction()?;
            check_scan(&transaction, scan)?;
            let removed = transaction.execute(
                "DELETE FROM documents WHERE id IN (SELECT id FROM documents WHERE root=?1 AND seen<>?2 LIMIT ?3)",
                params![scan.root,scan.generation,MAX_BATCH as i64])?;
            transaction.commit()?;
            total += removed;
            if removed < MAX_BATCH {
                return Ok(total);
            }
        }
    }

    pub fn retain_roots(
        &mut self,
        roots: &[std::path::PathBuf],
        cancel: &AtomicBool,
    ) -> Result<u64> {
        if roots.len() > 32 {
            return Err(Error::Invalid("Too many indexing roots"));
        }
        for root in roots {
            validated_path(root)?;
        }
        let mut after = String::new();
        let mut total = 0;
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(Error::Cancelled);
            }
            let root: Option<String> = self
                .connection
                .query_row(
                    "SELECT substr(root,1,4097) FROM scopes WHERE root>?1 ORDER BY root LIMIT 1",
                    [&after],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(root) = root else {
                return Ok(total);
            };
            validated_path(Path::new(&root))?;
            after = root.clone();
            if roots.iter().any(|retained| retained == Path::new(&root)) {
                continue;
            }
            loop {
                if cancel.load(Ordering::Acquire) {
                    return Err(Error::Cancelled);
                }
                let transaction = self.connection.transaction()?;
                let removed = transaction.execute(
                    "DELETE FROM documents WHERE id IN (SELECT id FROM documents WHERE root=?1 LIMIT ?2)",
                    params![root,MAX_BATCH as i64])?;
                if removed < MAX_BATCH {
                    transaction.execute("DELETE FROM scopes WHERE root=?1", [&root])?;
                }
                transaction.commit()?;
                total += removed as u64;
                if removed < MAX_BATCH {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    pub fn erase(&mut self, cancel: Arc<AtomicBool>, mut report: impl FnMut(u64)) -> Result<()> {
        let cancelled = Arc::clone(&cancel);
        self.connection
            .progress_handler(1000, Some(move || cancelled.load(Ordering::Acquire)))?;
        let result = (|| -> Result<()> {
            if cancel.load(Ordering::Acquire) {
                return Err(Error::Cancelled);
            }
            self.connection
                .execute("UPDATE scopes SET generation=0", [])?;
            let mut total = 0;
            loop {
                if cancel.load(Ordering::Acquire) {
                    return Err(Error::Cancelled);
                }
                let transaction = self.connection.transaction()?;
                let removed = transaction.execute(
                    "DELETE FROM documents WHERE id IN (SELECT id FROM documents LIMIT ?1)",
                    [MAX_BATCH as i64],
                )?;
                transaction.commit()?;
                total += removed as u64;
                report(total);
                if removed < MAX_BATCH {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if cancel.load(Ordering::Acquire) {
                return Err(Error::Cancelled);
            }
            let transaction = self.connection.transaction()?;
            transaction.execute(
                "INSERT INTO chunks_fts(chunks_fts) VALUES('delete-all')",
                [],
            )?;
            transaction.execute("DELETE FROM embeddings", [])?;
            transaction.execute("DELETE FROM chunk_embeddings", [])?;
            transaction.execute("DELETE FROM passage_shards", [])?;
            transaction.execute("DELETE FROM passage_models", [])?;
            transaction.execute("DELETE FROM vector_shards", [])?;
            transaction.execute("DELETE FROM scopes", [])?;
            transaction.commit()?;
            let busy: i64 =
                self.connection
                    .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
            if busy != 0 {
                return Err(Error::Invalid(
                    "Journal cleanup is waiting for a reader; retry erasing the index",
                ));
            }
            Ok(())
        })();
        self.connection.progress_handler(0, None::<fn() -> bool>)?;
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        result
    }

    /// Documents matching any of `terms`, best first, for answering a question from passages.
    pub fn search_any(
        &self,
        terms: &[String],
        limit: usize,
        cancel: Arc<AtomicBool>,
    ) -> Result<Vec<Hit>> {
        let expression = terms
            .iter()
            .filter(|term| {
                !term.is_empty() && term.len() <= 64 && term.chars().all(char::is_alphanumeric)
            })
            .map(|term| format!("\"{term}\""))
            .collect::<Vec<_>>()
            .join(" OR ");
        if expression.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        let started = Instant::now();
        let cancelled = Arc::clone(&cancel);
        self.connection.progress_handler(
            1000,
            Some(move || {
                cancelled.load(Ordering::Acquire) || started.elapsed() > Duration::from_millis(500)
            }),
        )?;
        let result = (|| -> Result<Vec<Hit>> {
            let mut statement = self.connection.prepare(
                "SELECT d.id,d.identity,d.path,d.title,content_fts.rank,d.revision FROM content_fts
                 JOIN documents d ON d.id=content_fts.rowid WHERE content_fts MATCH ?1
                 AND length(CAST(d.path AS BLOB))<=4096 AND length(CAST(d.title AS BLOB))<=1024
                 ORDER BY content_fts.rank LIMIT ?2",
            )?;
            let rows =
                statement.query_map(params![expression, limit.min(MAX_RESULTS) as i64], |row| {
                    Ok(Hit {
                        id: row.get(0)?,
                        identity: row.get(1)?,
                        path: row.get(2)?,
                        title: row.get(3)?,
                        rank: row.get(4)?,
                        revision: row.get(5)?,
                        related: false,
                        snippet: None,
                    })
                })?;
            let mut hits = Vec::new();
            for hit in rows {
                let hit = hit?;
                if validated_path(Path::new(&hit.path)).is_ok() {
                    hits.push(hit);
                }
            }
            Ok(hits)
        })();
        self.connection.progress_handler(0, None::<fn() -> bool>)?;
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        result
    }

    /// The window of a document's stored text that mentions the most `terms`, at most `budget`
    /// bytes and whitespace-normalized; the opening when no term appears.
    /// The passages matching `query`, best first, at most one per document so a single long file
    /// cannot fill the list. Used for content results, and for the benchmark.
    pub fn passages(
        &self,
        query: &str,
        limit: usize,
        cancel: Arc<AtomicBool>,
    ) -> Result<Vec<Passage>> {
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        let expression = expression(query)?;
        if expression.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let started = Instant::now();
        let cancelled = Arc::clone(&cancel);
        self.connection.progress_handler(
            1000,
            Some(move || {
                cancelled.load(Ordering::Acquire) || started.elapsed() > Duration::from_millis(250)
            }),
        )?;
        let result = (|| -> Result<Vec<Passage>> {
            let mut found = self.passage_page(&expression, limit)?;
            if found.len() < 3 && expression.contains(" AND ") {
                let seen: Vec<i64> = found.iter().map(|passage| passage.document_id).collect();
                for passage in self.passage_page(&expression.replace(" AND ", " OR "), limit)? {
                    if found.len() >= limit {
                        break;
                    }
                    if !seen.contains(&passage.document_id) {
                        found.push(passage);
                    }
                }
            }
            Ok(found)
        })();
        self.connection.progress_handler(0, None::<fn() -> bool>)?;
        result
    }

    fn passage_page(&self, expression: &str, limit: usize) -> Result<Vec<Passage>> {
        let mut statement = self.connection.prepare(
            "WITH matched AS MATERIALIZED (
               SELECT rowid,rank FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY rank LIMIT ?3
             ), ranked AS (
               SELECT c.id AS chunk_id, m.rank AS rank,
                      ROW_NUMBER() OVER (PARTITION BY c.document_id ORDER BY m.rank, c.id) AS place
               FROM matched m JOIN chunks c ON c.id=m.rowid
             )
             SELECT c.document_id,d.path,d.title,c.ordinal,c.page,c.line,c.heading,c.text,r.rank
             FROM ranked r JOIN chunks c ON c.id=r.chunk_id JOIN documents d ON d.id=c.document_id
             WHERE r.place=1 ORDER BY r.rank, c.document_id LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![
                expression,
                limit.min(MAX_RESULTS) as i64,
                (MAX_CANDIDATES + 1) as i64
            ],
            |row| {
                Ok(Passage {
                    document_id: row.get(0)?,
                    path: row.get(1)?,
                    title: row.get(2)?,
                    ordinal: row.get(3)?,
                    page: row.get(4)?,
                    line: row.get(5)?,
                    heading: row.get(6)?,
                    text: row.get(7)?,
                    rank: row.get(8)?,
                })
            },
        )?;
        let mut found = Vec::new();
        for row in rows {
            let passage = row?;
            if passage.path.len() <= 4096 && validated_path(Path::new(&passage.path)).is_ok() {
                found.push(passage);
            }
        }
        Ok(found)
    }

    pub fn passage(&self, id: i64, terms: &[String], budget: usize) -> Result<Option<String>> {
        let body: Option<String> = self
            .connection
            .query_row("SELECT body FROM documents WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(body) = body else { return Ok(None) };
        let text = body.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() || budget == 0 {
            return Ok(None);
        }
        let window = budget.min(text.len());
        let step = (window / 2).max(1);
        let (mut best_score, mut best_start, mut start) = (0usize, 0usize, 0usize);
        loop {
            let begin = text.floor_char_boundary(start);
            let end = text.floor_char_boundary((begin + window).min(text.len()));
            let slice = text.get(begin..end).unwrap_or_default().to_lowercase();
            let score: usize = terms
                .iter()
                .map(|term| {
                    if slice.contains(term.as_str()) {
                        100 + slice.matches(term.as_str()).count()
                    } else {
                        0
                    }
                })
                .sum();
            if score > best_score {
                (best_score, best_start) = (score, begin);
            }
            if end >= text.len() || end <= begin {
                break;
            }
            start = begin + step;
        }
        let end = text.floor_char_boundary((best_start + window).min(text.len()));
        Ok(text.get(best_start..end).map(str::to_owned))
    }

    /// Removes vectors of retired models, merges the word index and rewrites the database file so
    /// deleted pages go back to the disk. Runs only when the user asks.
    pub fn compact(
        &mut self,
        database: &Path,
        model_identifier: &str,
        cancel: Arc<AtomicBool>,
        mut report: impl FnMut(&'static str),
    ) -> Result<CompactReport> {
        let database = database_path(database)?;
        let before = database_bytes(&database);
        let directory = database
            .parent()
            .ok_or(Error::Invalid("Missing database directory"))?;
        // VACUUM writes a complete copy before it replaces the original.
        if available_bytes(directory)
            .is_some_and(|free| free < before.saturating_add(64 * 1_048_576))
        {
            return Err(Error::Invalid(
                "Not enough free disk space to compact the index",
            ));
        }
        let current = format!("[[]\"{model_identifier}\",*");
        let cancelled = Arc::clone(&cancel);
        self.connection
            .progress_handler(1000, Some(move || cancelled.load(Ordering::Acquire)))?;
        let result = (|| -> Result<u64> {
            report("Removing unused vectors");
            let mut removed = 0u64;
            loop {
                if cancel.load(Ordering::Acquire) {
                    return Err(Error::Cancelled);
                }
                let count = self.connection.execute(
                    "DELETE FROM embeddings WHERE id IN (SELECT id FROM embeddings WHERE model NOT GLOB ?1 LIMIT 1024)", [&current])?;
                removed += count as u64;
                if count < 1024 {
                    break;
                }
            }
            self.connection.execute(
                "DELETE FROM vector_shards WHERE model NOT GLOB ?1",
                [&current],
            )?;
            report("Merging the word index");
            self.connection.execute(
                "INSERT INTO content_fts(content_fts) VALUES('optimize')",
                [],
            )?;
            report("Rewriting the database");
            let mut attempts = 0;
            loop {
                match self.connection.execute_batch("VACUUM") {
                    Ok(()) => break,
                    Err(rusqlite::Error::SqliteFailure(error, _))
                        if error.code == rusqlite::ErrorCode::DatabaseBusy && attempts < 10 =>
                    {
                        attempts += 1;
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            let _: i64 =
                self.connection
                    .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
            Ok(removed)
        })();
        self.connection.progress_handler(0, None::<fn() -> bool>)?;
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        let removed = result?;
        Ok(CompactReport {
            before_bytes: before,
            after_bytes: database_bytes(&database),
            removed_vectors: removed,
        })
    }

    /// Adds a short excerpt to the first `count` hits so a result shows why it matched.
    pub fn annotate(
        &self,
        query: &str,
        hits: &mut [Hit],
        count: usize,
        mode: Snippet,
        cancel: Arc<AtomicBool>,
    ) -> Result<()> {
        let expression = expression(query)?;
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        let started = Instant::now();
        let cancelled = Arc::clone(&cancel);
        self.connection.progress_handler(
            1000,
            Some(move || {
                cancelled.load(Ordering::Acquire) || started.elapsed() > Duration::from_millis(250)
            }),
        )?;
        let result = (|| -> Result<()> {
            let mut matched = self.connection.prepare(
                "SELECT snippet(content_fts,1,'','','…',16) FROM content_fts WHERE content_fts MATCH ?1 AND rowid=?2")?;
            let mut opening = self
                .connection
                .prepare("SELECT substr(body,1,600) FROM documents WHERE id=?1")?;
            for hit in hits.iter_mut().take(count) {
                let text: Option<String> = if mode == Snippet::Opening || expression.is_empty() {
                    opening.query_row([hit.id], |row| row.get(0)).optional()?
                } else {
                    matched
                        .query_row(params![expression, hit.id], |row| row.get(0))
                        .optional()?
                };
                hit.snippet = text
                    .map(|text| tidy_excerpt(&text))
                    .filter(|text| !text.is_empty());
            }
            Ok(())
        })();
        self.connection.progress_handler(0, None::<fn() -> bool>)?;
        result
    }

    /// Drops hits that fail `filter`; used for semantic hits, which are found outside the SQL filter.
    pub fn keep_matching(&self, hits: &mut Vec<Hit>, filter: &SearchFilter) -> Result<()> {
        if filter.is_empty() {
            return Ok(());
        }
        let mut statement = self.connection.prepare(&format!(
            "SELECT EXISTS(SELECT 1 FROM documents d WHERE d.id=?1{})",
            filter.sql()
        ))?;
        let mut kept = Vec::with_capacity(hits.len());
        for hit in hits.drain(..) {
            if statement.query_row([hit.id], |row| row.get::<_, bool>(0))? {
                kept.push(hit);
            }
        }
        *hits = kept;
        Ok(())
    }

    pub fn search(&self, query: &str, limit: usize, cancel: Arc<AtomicBool>) -> Result<SearchPage> {
        self.search_filtered(query, &SearchFilter::default(), limit, cancel)
    }

    pub fn search_filtered(
        &self,
        query: &str,
        filter: &SearchFilter,
        limit: usize,
        cancel: Arc<AtomicBool>,
    ) -> Result<SearchPage> {
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        let expression = expression(query)?;
        let filter_sql = filter.sql();
        if expression.is_empty() || limit == 0 {
            return Ok(SearchPage::default());
        }
        let started = Instant::now();
        let cancelled = Arc::clone(&cancel);
        self.connection.progress_handler(
            1000,
            Some(move || {
                cancelled.load(Ordering::Acquire) || started.elapsed() > Duration::from_millis(250)
            }),
        )?;
        let result = (|| -> Result<SearchPage> {
            let mut broad = false;
            for term in expression.split(" AND ") {
                let matches: i64 = self.connection.query_row(
                    "SELECT count(*) FROM (SELECT rowid FROM content_fts WHERE content_fts MATCH ?1 LIMIT ?2)",
                    params![term,(MAX_CANDIDATES+1) as i64], |row|row.get(0))?;
                if matches > MAX_CANDIDATES as i64 {
                    broad = true;
                    break;
                }
            }
            let sql = if broad {
                format!("WITH candidates AS MATERIALIZED (
                   SELECT rowid FROM content_fts WHERE content_fts MATCH ?1 ORDER BY rowid DESC LIMIT ?3
                 ) SELECT d.id,d.identity,d.path,d.title,0.0,d.revision,1
                 FROM candidates JOIN documents d ON d.id=candidates.rowid
                 WHERE length(CAST(d.identity AS BLOB))<=128
                 AND length(CAST(d.path AS BLOB))<=4096 AND length(CAST(d.title AS BLOB))<=1024{filter_sql}
                 ORDER BY d.id DESC LIMIT ?2")
            } else {
                format!("WITH candidates AS MATERIALIZED (
                   SELECT rowid,rank FROM content_fts WHERE content_fts MATCH ?1 ORDER BY rowid DESC LIMIT ?3
                 ), ranked AS MATERIALIZED (
                   SELECT c.rowid AS rowid,c.rank AS rank FROM candidates c JOIN documents d ON d.id=c.rowid
                   WHERE 1{filter_sql} ORDER BY c.rank,c.rowid DESC LIMIT ?2
                 ) SELECT d.id,d.identity,d.path,d.title,ranked.rank,d.revision,(SELECT count(*) FROM candidates)>=?3
                 FROM ranked JOIN documents d ON d.id=ranked.rowid
                 WHERE length(CAST(d.identity AS BLOB))<=128
                 AND length(CAST(d.path AS BLOB))<=4096 AND length(CAST(d.title AS BLOB))<=1024
                 ORDER BY ranked.rank,d.id")
            };
            let mut statement = self.connection.prepare(&sql)?;
            let rows = statement.query_map(
                params![
                    expression,
                    limit.min(MAX_RESULTS) as i64,
                    (MAX_CANDIDATES + 1) as i64
                ],
                |row| {
                    Ok((
                        Hit {
                            id: row.get(0)?,
                            identity: row.get(1)?,
                            path: row.get(2)?,
                            title: row.get(3)?,
                            rank: row.get(4)?,
                            revision: row.get(5)?,
                            related: false,
                            snippet: None,
                        },
                        row.get::<_, bool>(6)?,
                    ))
                },
            )?;
            let mut found = Vec::new();
            let mut limited = broad;
            for row in rows {
                let (row, truncated) = row?;
                limited |= truncated;
                if row.path.len() <= 4096
                    && validated_path(Path::new(&row.path)).is_ok()
                    && row.title.len() <= 1024
                    && row.identity.len() <= 128
                    && row.rank.is_finite()
                {
                    found.push(row);
                }
            }
            Ok(SearchPage {
                hits: found,
                limited,
            })
        })();
        self.connection.progress_handler(0, None::<fn() -> bool>)?;
        if cancel.load(Ordering::Acquire) || started.elapsed() > Duration::from_millis(250) {
            return Err(Error::Cancelled);
        }
        result
    }

    pub fn embedding_page(
        &self,
        after: i64,
        model: &str,
        dimensions: usize,
        cancel: Arc<AtomicBool>,
    ) -> Result<EmbeddingPage> {
        if after < 0 || model.is_empty() || model.len() > 128 || !(1..=2048).contains(&dimensions) {
            return Err(Error::Invalid("Invalid embedding cursor or model"));
        }
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        let started = Instant::now();
        let cancelled = Arc::clone(&cancel);
        self.connection.progress_handler(
            1000,
            Some(move || {
                cancelled.load(Ordering::Acquire) || started.elapsed() > Duration::from_millis(250)
            }),
        )?;
        let result = (|| -> Result<EmbeddingPage> {
            let mut statement = self.connection.prepare(&format!(
                "SELECT d.id,d.revision,substr(d.path,1,4097),
                 CASE WHEN NOT {SEMANTIC_ELIGIBLE} THEN NULL
                 WHEN e.revision=d.revision AND e.model=?2 AND e.dimensions=?3 AND length(e.vector)=?3*4
                 THEN NULL ELSE substr(d.title,1,1025)||char(10)||substr(d.body,1,4096) END,{SEMANTIC_ELIGIBLE}
                 FROM documents d LEFT JOIN embeddings e ON e.document_id=d.id
                 WHERE d.id>?1 ORDER BY d.id LIMIT 128"))?;
            let rows = statement.query_map(params![after, model, dimensions as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, bool>(4)?,
                ))
            })?;
            let mut page = EmbeddingPage {
                after,
                ..EmbeddingPage::default()
            };
            for row in rows {
                if cancel.load(Ordering::Acquire) {
                    return Err(Error::Cancelled);
                }
                let (id, revision, path, text, eligible) = row?;
                if id <= page.after || revision <= 0 {
                    return Err(Error::Invalid("Invalid embedding document identity"));
                }
                validated_path(Path::new(&path))?;
                page.after = id;
                page.scanned += 1;
                if !eligible {
                    page.lexical_only += 1;
                }
                if let Some(mut text) = text {
                    let mut end = text.len().min(4096);
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    text.truncate(end);
                    page.pending.push(EmbeddingDocument {
                        id,
                        revision,
                        path,
                        text,
                    });
                }
            }
            Ok(page)
        })();
        self.connection.progress_handler(0, None::<fn() -> bool>)?;
        if cancel.load(Ordering::Acquire) || started.elapsed() > Duration::from_millis(250) {
            return Err(Error::Cancelled);
        }
        result
    }

    pub fn put_embedding(
        &mut self,
        id: i64,
        revision: i64,
        model: &str,
        vector: &[f32],
    ) -> Result<bool> {
        if model.is_empty()
            || model.len() > 128
            || vector.is_empty()
            || vector.len() > 2048
            || vector.iter().any(|v| !v.is_finite())
        {
            return Err(Error::Invalid("Invalid embedding metadata"));
        }
        let norm = vector
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum::<f64>()
            .sqrt();
        if norm <= 0.0 || !norm.is_finite() {
            return Err(Error::Invalid("Invalid embedding norm"));
        }
        let bytes: Vec<u8> = vector
            .iter()
            .flat_map(|value| ((f64::from(*value) / norm) as f32).to_le_bytes())
            .collect();
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM documents WHERE id=?1 AND revision=?2)",
            params![id, revision],
            |row| row.get(0),
        )?;
        if !current {
            return Ok(false);
        }
        transaction.execute("DELETE FROM embeddings WHERE document_id=?1", [id])?;
        transaction.execute("INSERT INTO embeddings(document_id,revision,model,dimensions,vector) VALUES(?1,?2,?3,?4,?5)",
            params![id,revision,model,vector.len() as i64,bytes])?;
        transaction.commit()?;
        Ok(true)
    }

    /// Passages still needing a vector for this model, paged by chunk id.
    ///
    /// Shaped like [`ContentStore::embedding_page`]: the text comes back only when no current
    /// vector exists, so a pass that is interrupted and resumed re-embeds nothing it already did.
    pub fn chunk_embedding_page(
        &self,
        after: i64,
        model: &str,
        dimensions: usize,
        cancel: Arc<AtomicBool>,
    ) -> Result<ChunkEmbeddingPage> {
        if after < 0 || model.is_empty() || model.len() > 128 || !(1..=2048).contains(&dimensions) {
            return Err(Error::Invalid("Invalid embedding cursor or model"));
        }
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        let started = Instant::now();
        let cancelled = Arc::clone(&cancel);
        self.connection.progress_handler(
            1000,
            Some(move || {
                cancelled.load(Ordering::Acquire) || started.elapsed() > Duration::from_millis(250)
            }),
        )?;
        let result = (|| -> Result<ChunkEmbeddingPage> {
            let mut statement = self.connection.prepare(&format!(
                "SELECT c.id,c.document_id,substr(d.path,1,4097),
                 CASE WHEN NOT {CHUNK_ELIGIBLE} THEN NULL
                 WHEN ce.model=?2 AND ce.dimensions=?3 AND (length(ce.vector)=?3*4 OR length(ce.vector)=?3) THEN NULL
                 ELSE substr(c.heading,1,256)||char(10)||substr(c.text,1,4096) END,{CHUNK_ELIGIBLE}
                 FROM chunks c JOIN documents d ON d.id=c.document_id
                 LEFT JOIN chunk_embeddings ce ON ce.chunk_id=c.id AND ce.model=?2 AND ce.dimensions=?3
                 WHERE c.id>?1 ORDER BY c.id LIMIT 128"
            ))?;
            let rows = statement.query_map(params![after, model, dimensions as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, bool>(4)?,
                ))
            })?;
            let mut page = ChunkEmbeddingPage {
                after,
                ..ChunkEmbeddingPage::default()
            };
            for row in rows {
                if cancel.load(Ordering::Acquire) {
                    return Err(Error::Cancelled);
                }
                let (id, document_id, path, text, eligible) = row?;
                if id <= page.after || document_id <= 0 {
                    return Err(Error::Invalid("Invalid chunk identity"));
                }
                validated_path(Path::new(&path))?;
                page.after = id;
                page.scanned += 1;
                if !eligible {
                    page.lexical_only += 1;
                }
                if let Some(mut text) = text {
                    let mut end = text.len().min(4096);
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    text.truncate(end);
                    page.pending.push(PendingChunk {
                        id,
                        document_id,
                        path,
                        text,
                    });
                }
            }
            Ok(page)
        })();
        self.connection.progress_handler(0, None::<fn() -> bool>)?;
        if cancel.load(Ordering::Acquire) || started.elapsed() > Duration::from_millis(250) {
            return Err(Error::Cancelled);
        }
        result
    }

    /// Stores one passage vector, normalized as [`ContentStore::put_embedding`] does so readers can
    /// assert unit norm. `false` means the chunk was rewritten while the model was working, which
    /// is not an error: the replacement chunk is picked up by the next page.
    pub fn put_chunk_embedding(
        &mut self,
        chunk_id: i64,
        model: &str,
        vector: &[f32],
    ) -> Result<bool> {
        if model.is_empty()
            || model.len() > 128
            || vector.is_empty()
            || vector.len() > 2048
            || vector.iter().any(|v| !v.is_finite())
        {
            return Err(Error::Invalid("Invalid embedding metadata"));
        }
        let norm = vector
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum::<f64>()
            .sqrt();
        if norm <= 0.0 || !norm.is_finite() {
            return Err(Error::Invalid("Invalid embedding norm"));
        }
        let normalized:Vec<f32>=vector.iter().map(|value|(f64::from(*value)/norm) as f32).collect();
        let (bytes,scale)=retrieval::quantize(&normalized).ok_or(Error::Invalid("Invalid compressed vector"))?;
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let present: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM chunks WHERE id=?1)",
            [chunk_id],
            |row| row.get(0),
        )?;
        if !present {
            return Ok(false);
        }
        transaction.execute("DELETE FROM chunk_embeddings WHERE chunk_id=?1 AND model=?2 AND dimensions=?3", params![chunk_id,model,vector.len() as i64])?;
        transaction.execute(
            "INSERT INTO chunk_embeddings(chunk_id,model,dimensions,vector,scale) VALUES(?1,?2,?3,?4,?5)",
            params![chunk_id, model, vector.len() as i64, bytes,scale],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn count(&self) -> Result<u64> {
        let count: i64 = self
            .connection
            .query_row("SELECT count(*) FROM documents", [], |r| r.get(0))?;
        u64::try_from(count).map_err(|_| Error::Invalid("Invalid content record count"))
    }

    pub fn check_integrity(&self) -> Result<()> {
        let status: String = self
            .connection
            .query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if status != "ok" {
            return Err(Error::Invalid("Content database integrity check failed"));
        }
        self.connection.execute(
            "INSERT INTO content_fts(content_fts,rank) VALUES('integrity-check',1)",
            [],
        )?;
        Ok(())
    }
}

fn database_path(path: &Path) -> Result<std::path::PathBuf> {
    let parent = path
        .parent()
        .ok_or(Error::Invalid("Missing database directory"))?;
    let name = path
        .file_name()
        .ok_or(Error::Invalid("Missing database filename"))?;
    Ok(parent.canonicalize()?.join(name))
}

fn check_scan(connection: &Connection, scan: &Scan) -> Result<()> {
    let generation: Option<i64> = connection
        .query_row(
            "SELECT generation FROM scopes WHERE root=?1",
            [&scan.root],
            |r| r.get(0),
        )
        .optional()?;
    if generation != Some(scan.generation) {
        return Err(Error::Invalid("Index scan was superseded"));
    }
    Ok(())
}

fn validated_path(path: &Path) -> Result<&str> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(Error::Invalid(
            "Index paths must be absolute and contain no traversal",
        ));
    }
    path.to_str()
        .filter(|s| s.len() <= 4096 && !s.contains('\0'))
        .ok_or(Error::Invalid("Invalid index path"))
}

/// Replaces a document's passages. Same transaction as the document itself, so the two can never
/// disagree about what a file says.
fn write_chunks(
    transaction: &rusqlite::Transaction<'_>,
    document_id: i64,
    document: &Document<'_>,
) -> Result<()> {
    transaction.execute("DELETE FROM chunks WHERE document_id=?1", [document_id])?;
    let chunked = retrieval::chunks(
        chunk_kind(document),
        document.body,
        &retrieval::Limits::default(),
    );
    transaction.execute("UPDATE documents SET passage_version=1,partial=?2 WHERE id=?1",
        params![document_id, chunked.truncated || document.body.len() >= MAX_PASSAGE_BYTES])?;
    let mut insert = transaction.prepare(
        "INSERT INTO chunks(document_id,ordinal,page,line,heading,text,symbols) VALUES(?1,?2,?3,?4,?5,?6,?7)",
    )?;
    for chunk in &chunked.chunks {
        let (page, line) = match chunk.location {
            retrieval::Location::Page(page) => (i64::from(page), 0),
            retrieval::Location::Line(line) => (0, if document.extraction==Extraction::Extracted {0} else {i64::from(line)}),
        };
        insert.execute(params![
            document_id,
            i64::from(chunk.ordinal),
            page,
            line,
            chunk.heading,
            chunk.text,
            chunk.symbols
        ])?;
    }
    Ok(())
}

/// Extracted documents arrive as pages; source files are split at declarations; everything else
/// reads as prose.
fn chunk_kind(document: &Document<'_>) -> retrieval::Kind {
    if document.extraction==Extraction::Extracted && document.path.extension().is_some_and(|extension|extension.eq_ignore_ascii_case("xlsx")) {
        return retrieval::Kind::Sections;
    }
    if document.extraction == Extraction::Extracted
        && document.path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("pdf") || extension.eq_ignore_ascii_case("pptx")) {
        return retrieval::Kind::Paged;
    }
    let extension = document
        .path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    const CODE: &[&str] = &[
        "rs", "swift", "py", "js", "jsx", "ts", "tsx", "go", "java", "c", "h", "cpp", "hpp", "rb",
        "sh", "sql", "css",
    ];
    if CODE.contains(&extension.as_str()) {
        retrieval::Kind::Code
    } else {
        retrieval::Kind::Prose
    }
}

fn validate_document(scan: &Scan, document: &Document<'_>) -> Result<()> {
    validated_path(document.path)?;
    if !document.path.starts_with(&scan.root) || document.path == Path::new(&scan.root) {
        return Err(Error::Invalid("Document is outside its indexing root"));
    }
    if document.identity.is_empty()
        || document.identity.len() > 128
        || document.identity.contains('\0')
        || document.title.len() > 1024
        || document.body.len() > MAX_PASSAGE_BYTES
        || document.bytes > i64::MAX as u64
    {
        return Err(Error::Invalid("Document exceeds index limits"));
    }
    Ok(())
}

fn expression(query: &str) -> Result<String> {
    if query.len() > 4096 {
        return Err(Error::Invalid("Content query is too long"));
    }
    let mut tokens = Vec::new();
    for token in query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
    {
        if tokens.len() == 32 {
            return Err(Error::Invalid("Content query has too many terms"));
        }
        if token.len() > 256 {
            return Err(Error::Invalid("Content query term is too long"));
        }
        tokens.push(format!("\"{token}\""));
    }
    Ok(tokens.join(" AND "))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            Self(
                std::env::temp_dir()
                    .join(format!(
                        "blindspot-content-{}-{}",
                        std::process::id(),
                        NEXT.fetch_add(1, Ordering::Relaxed)
                    ))
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
    fn doc<'a>(identity: &'a str, path: &'a str, body: &'a str) -> Document<'a> {
        Document {
            identity,
            path: Path::new(path),
            title: path.rsplit('/').next().expect("name"),
            body,
            modified_ns: 1,
            changed_ns: 1,
            bytes: body.len() as u64,
            extraction: Extraction::Text,
        }
    }
    fn search(store: &ContentStore, query: &str) -> Vec<Hit> {
        store
            .search(query, 50, Arc::new(AtomicBool::new(false)))
            .expect("search")
            .hits
    }

    #[test]
    fn lexical_content_survives_reopen_and_rename_preserves_identity() {
        let fixture = Fixture::new();
        let id;
        {
            let mut store = fixture.open();
            let scan = store.begin_scan(Path::new("/fixture")).expect("scan");
            id = store
                .put_batch(
                    &scan,
                    &[doc(
                        "device:inode:birth",
                        "/fixture/notes.md",
                        "database migrations with transactional rollback",
                    )],
                )
                .expect("put")[0]
                .0;
            assert_eq!(search(&store, "database migrations")[0].id, id);
        }
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).expect("scan");
        let renamed = store
            .put_batch(
                &scan,
                &[doc(
                    "device:inode:birth",
                    "/fixture/renamed.md",
                    "database migrations with transactional rollback",
                )],
            )
            .expect("rename");
        assert_eq!(renamed[0].0, id);
        assert_eq!(store.count().expect("count"), 1);
        assert_eq!(search(&store, "migrations")[0].path, "/fixture/renamed.md");
        store.check_integrity().expect("integrity");
    }

    #[test]
    fn interrupted_scan_keeps_records_and_completed_scan_removes_deleted_content() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let first = store.begin_scan(Path::new("/fixture")).expect("scan");
        store
            .put_batch(
                &first,
                &[
                    doc("a", "/fixture/a.txt", "alpha"),
                    doc("b", "/fixture/b.txt", "beta"),
                ],
            )
            .expect("put");
        let next = store.begin_scan(Path::new("/fixture")).expect("scan");
        assert!(
            store
                .mark_unchanged(&next, "a", Path::new("/fixture/a.txt"), 1, 1, 5)
                .expect("fresh")
        );
        assert_eq!(
            store
                .finish_scan(&next, ScanOutcome::Interrupted, &AtomicBool::new(false))
                .expect("interrupt"),
            0
        );
        assert_eq!(store.count().expect("count"), 2);
        assert!(
            store
                .put_batch(&first, &[doc("x", "/fixture/x.txt", "stale")])
                .is_err()
        );
        assert_eq!(
            store
                .finish_scan(&next, ScanOutcome::Complete, &AtomicBool::new(false))
                .expect("complete"),
            1
        );
        assert!(search(&store, "beta").is_empty());
        store.check_integrity().expect("integrity");
    }

    #[test]
    fn replacing_a_path_does_not_duplicate_and_stale_embeddings_are_rejected() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).expect("scan");
        let old = store
            .put_batch(&scan, &[doc("old", "/fixture/a.txt", "old content")])
            .expect("put")[0];
        assert!(
            store
                .put_embedding(old.0, old.1, "model-revision-1", &[1.0, 2.0])
                .expect("embed")
        );
        let updated = store
            .put_batch(&scan, &[doc("old", "/fixture/a.txt", "new content")])
            .expect("update")[0];
        assert!(
            !store
                .put_embedding(old.0, old.1, "model-revision-1", &[1.0, 2.0])
                .expect("stale")
        );
        assert!(
            store
                .put_embedding(updated.0, updated.1, "model-revision-1", &[1.0, 2.0])
                .expect("new")
        );
        store
            .put_batch(
                &scan,
                &[doc("replacement", "/fixture/a.txt", "replacement")],
            )
            .expect("replace");
        assert!(
            !store
                .put_embedding(old.0, old.1, "model-revision-1", &[1.0, 2.0])
                .expect("deleted identity")
        );
        assert_eq!(store.count().expect("count"), 1);
        assert!(search(&store, "old").is_empty());
        assert_eq!(
            store
                .connection
                .query_row("SELECT count(*) FROM embeddings", [], |r| r
                    .get::<_, i64>(0))
                .expect("cascade"),
            0
        );
    }

    #[test]
    fn embedding_pages_are_bounded_resume_and_skip_current_revisions() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).unwrap();
        let paths: Vec<_> = (0..130).map(|i| format!("/fixture/{i}.txt")).collect();
        let body = "é界".repeat(3000);
        let documents: Vec<_> = paths.iter().map(|path| doc(path, path, &body)).collect();
        store.put_batch(&scan, &documents).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let first = store
            .embedding_page(0, "fixture-v1", 2, Arc::clone(&cancel))
            .unwrap();
        assert_eq!(first.scanned, 128);
        assert_eq!(first.pending.len(), 128);
        for document in &first.pending {
            assert!(document.text.len() <= 4096);
            assert!(
                document
                    .text
                    .starts_with(document.path.rsplit('/').next().unwrap())
            );
            assert!(
                store
                    .put_embedding(document.id, document.revision, "fixture-v1", &[1.0, 0.0])
                    .unwrap()
            );
        }
        let second = store
            .embedding_page(first.after, "fixture-v1", 2, Arc::clone(&cancel))
            .unwrap();
        assert_eq!(second.scanned, 2);
        assert_eq!(second.pending.len(), 2);
        assert!(
            second
                .pending
                .iter()
                .all(|document| document.id > first.after)
        );
        assert_eq!(
            store
                .embedding_page(second.after, "fixture-v1", 2, Arc::clone(&cancel))
                .unwrap()
                .scanned,
            0
        );
        drop(store);
        let mut store = fixture.open();
        let resumed = store
            .embedding_page(0, "fixture-v1", 2, Arc::clone(&cancel))
            .unwrap();
        assert_eq!(resumed.scanned, 128);
        assert!(resumed.pending.is_empty());
        assert_eq!(resumed.after, first.after);
        store
            .put_batch(&scan, &[doc(&paths[0], &paths[0], "updated text")])
            .unwrap();
        let changed = store
            .embedding_page(0, "fixture-v1", 2, Arc::clone(&cancel))
            .unwrap();
        assert_eq!(changed.pending.len(), 1);
        assert!(changed.pending[0].revision > first.pending[0].revision);
        assert_eq!(
            store
                .embedding_page(0, "fixture-v2", 2, Arc::clone(&cancel))
                .unwrap()
                .pending
                .len(),
            128
        );
        assert_eq!(
            store
                .embedding_page(0, "fixture-v1", 3, cancel)
                .unwrap()
                .pending
                .len(),
            128
        );
    }

    #[test]
    fn embedding_page_rejects_cancelled_and_malformed_records() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).unwrap();
        store
            .put_batch(&scan, &[doc("a", "/fixture/a.txt", "text")])
            .unwrap();
        assert!(matches!(
            store.embedding_page(0, "fixture", 2, Arc::new(AtomicBool::new(true))),
            Err(Error::Cancelled)
        ));
        assert_eq!(
            store
                .embedding_page(0, "fixture", 2, Arc::new(AtomicBool::new(false)))
                .unwrap()
                .pending
                .len(),
            1
        );
        store
            .connection
            .execute("UPDATE documents SET path='/fixture/../outside'", [])
            .unwrap();
        assert!(
            store
                .embedding_page(0, "fixture", 2, Arc::new(AtomicBool::new(false)))
                .is_err()
        );
        assert!(
            store
                .embedding_page(-1, "fixture", 2, Arc::new(AtomicBool::new(false)))
                .is_err()
        );
    }

    fn downgrade_embeddings_to_v4(store: &ContentStore) {
        store
            .connection
            .execute_batch("ALTER TABLE embeddings RENAME TO embeddings_v5; ALTER TABLE documents DROP COLUMN extraction;")
            .unwrap();
        store.connection.execute_batch(SCHEMA_V2).unwrap();
        store
            .connection
            .execute_batch(
                "INSERT INTO embeddings(document_id,revision,model,dimensions,vector)
            SELECT document_id,revision,model,dimensions,vector FROM embeddings_v5;
            DROP TABLE embeddings_v5; DROP TABLE vector_shards; PRAGMA user_version=4;",
            )
            .unwrap();
    }

    fn write_schema_six(path: &Path, conflict: bool) -> Connection {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let connection = Connection::open(path).unwrap();
        connection.execute_batch(SCHEMA_V1).unwrap();
        connection.execute_batch(SCHEMA_V2).unwrap();
        connection
            .execute_batch(
                "ALTER TABLE documents ADD COLUMN changed_ns INTEGER NOT NULL DEFAULT 0;
                 CREATE TABLE scan_clock(id INTEGER PRIMARY KEY CHECK(id=1), generation INTEGER NOT NULL);
                 INSERT INTO scan_clock VALUES(1,0);
                 ALTER TABLE embeddings RENAME TO embeddings_v4;
                 CREATE TABLE embeddings(
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     document_id INTEGER NOT NULL UNIQUE REFERENCES documents(id) ON DELETE CASCADE,
                     revision INTEGER NOT NULL, model TEXT NOT NULL, dimensions INTEGER NOT NULL,
                     vector BLOB NOT NULL CHECK(length(vector)=dimensions*4)
                 );
                 INSERT INTO embeddings(document_id,revision,model,dimensions,vector)
                     SELECT document_id,revision,model,dimensions,vector FROM embeddings_v4;
                 DROP TABLE embeddings_v4;
                 CREATE TABLE vector_shards(
                     after_key INTEGER PRIMARY KEY CHECK(after_key>=0 AND after_key%65536=0 AND after_key<=9223372036854710271),
                     through_key INTEGER NOT NULL CHECK(through_key>after_key AND through_key-after_key<=65536),
                     model TEXT NOT NULL CHECK(length(CAST(model AS BLOB)) BETWEEN 1 AND 128),
                     dimensions INTEGER NOT NULL CHECK(dimensions BETWEEN 1 AND 2048),
                     token TEXT NOT NULL UNIQUE CHECK(length(token)=32 AND token NOT GLOB '*[^0-9a-f]*'),
                     checksum TEXT NOT NULL CHECK(length(checksum)=64 AND checksum NOT GLOB '*[^0-9a-f]*'),
                     count INTEGER NOT NULL CHECK(count BETWEEN 1 AND 65536),
                     bytes INTEGER NOT NULL CHECK(bytes BETWEEN 64 AND 402653184)
                 );
                 INSERT INTO documents(identity,root,path,title,body,modified_ns,bytes,seen,changed_ns)
                     VALUES('legacy','/fixture','/fixture/legacy.txt','legacy','legacy body',1,11,1,1);
                 INSERT INTO embeddings(id,document_id,revision,model,dimensions,vector)
                     VALUES(42,1,1,'fixture',2,x'0000803F00000000');
                 INSERT INTO vector_shards(after_key,through_key,model,dimensions,token,checksum,count,bytes)
                     VALUES(0,65536,'fixture',2,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                            'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',1,1024);",
            )
            .unwrap();
        if conflict {
            connection
                .execute_batch("ALTER TABLE documents ADD COLUMN extraction TEXT;")
                .unwrap();
        }
        connection
            .pragma_update(None, "application_id", APPLICATION_ID)
            .unwrap();
        connection.pragma_update(None, "user_version", 6).unwrap();
        connection
    }

    #[test]
    fn version_six_migration_preserves_documents_vectors_shards_and_rolls_back_conflict() {
        let fixture = Fixture::new();
        drop(write_schema_six(&fixture.0, false));
        let store = fixture.open();
        assert_eq!(store.count().unwrap(), 1);
        assert_eq!(
            store
                .connection
                .query_row("SELECT body FROM documents WHERE id=1", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "legacy body"
        );
        assert_eq!(
            store
                .connection
                .query_row("SELECT id || ':' || hex(vector) FROM embeddings", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "42:0000803F00000000"
        );
        assert_eq!(
            store
                .vector_catalog(Arc::new(AtomicBool::new(false)))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .connection
                .query_row("SELECT extraction FROM documents WHERE id=1", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(store);

        let conflict = Fixture::new();
        drop(write_schema_six(&conflict.0, true));
        assert!(ContentStore::open(&conflict.0).is_err());
        let connection = Connection::open(&conflict.0).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6
        );
        assert_eq!(
            connection
                .query_row("SELECT body FROM documents WHERE id=1", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "legacy body"
        );
        assert_eq!(
            connection
                .query_row("SELECT hex(vector) FROM embeddings WHERE id=42", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
            "0000803F00000000"
        );
    }

    #[test]
    fn filtered_search_narrows_ranked_hits_and_snippets_show_the_match() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).unwrap();
        store.put_batch(&scan, &[
            Document { identity: "a", path: Path::new("/fixture/a.pdf"), title: "a.pdf",
                body: "Intro words.\n The camera signaling protocol for genetec deployments. Closing.",
                modified_ns: 5_000_000_000, changed_ns: 1, bytes: 10, extraction: Extraction::Extracted },
            Document { identity: "b", path: Path::new("/fixture/b.go"), title: "b.go", body: "genetec client code",
                modified_ns: 1_000_000_000, changed_ns: 1, bytes: 5000, extraction: Extraction::Text },
        ]).unwrap();
        let cancel = || Arc::new(AtomicBool::new(false));
        let paths = |filter: SearchFilter| {
            store
                .search_filtered("genetec", &filter, 10, cancel())
                .unwrap()
                .hits
                .into_iter()
                .map(|hit| hit.path)
                .collect::<Vec<_>>()
        };
        assert_eq!(paths(SearchFilter::default()).len(), 2);
        assert_eq!(
            paths(SearchFilter {
                extensions: vec!["PDF".into()],
                ..SearchFilter::default()
            }),
            ["/fixture/a.pdf"]
        );
        assert_eq!(
            paths(SearchFilter {
                modified_after_ns: Some(2_000_000_000),
                ..SearchFilter::default()
            }),
            ["/fixture/a.pdf"]
        );
        assert_eq!(
            paths(SearchFilter {
                max_bytes: Some(100),
                ..SearchFilter::default()
            }),
            ["/fixture/a.pdf"]
        );
        assert_eq!(
            paths(SearchFilter {
                min_bytes: Some(100),
                modified_before_ns: Some(2_000_000_000),
                ..SearchFilter::default()
            }),
            ["/fixture/b.go"]
        );
        assert!(
            paths(SearchFilter {
                extensions: vec!["pdf' OR 1=1 --".into()],
                ..SearchFilter::default()
            })
            .is_empty()
        );
        let mut hits = store.search("signaling", 10, cancel()).unwrap().hits;
        store
            .annotate("signaling", &mut hits, 10, Snippet::Matched, cancel())
            .unwrap();
        assert!(
            hits[0]
                .snippet
                .as_deref()
                .is_some_and(|text| text.contains("signaling protocol") && !text.contains('\n')),
            "{hits:?}"
        );
        let mut all = store.search("genetec", 10, cancel()).unwrap().hits;
        store
            .keep_matching(
                &mut all,
                &SearchFilter {
                    extensions: vec!["go".into()],
                    ..SearchFilter::default()
                },
            )
            .unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn question_passages_and_compaction_keep_current_vectors() {
        let terms = question_terms("What did the capstone decide about WebRTC signaling?");
        assert!(
            terms.contains(&"signaling".to_owned())
                && terms.contains(&"capstone".to_owned())
                && !terms.contains(&"the".to_owned())
        );
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).unwrap();
        let filler = "unrelated words ".repeat(300);
        let body = format!("{filler} The capstone chose WebRTC signaling over polling. {filler}");
        let ids = store
            .put_batch(
                &scan,
                &[
                    Document {
                        identity: "a",
                        path: Path::new("/fixture/a.md"),
                        title: "a.md",
                        body: &body,
                        modified_ns: 1,
                        changed_ns: 1,
                        bytes: 1,
                        extraction: Extraction::Text,
                    },
                    Document {
                        identity: "b",
                        path: Path::new("/fixture/b.md"),
                        title: "b.md",
                        body: "nothing relevant here",
                        modified_ns: 1,
                        changed_ns: 1,
                        bytes: 1,
                        extraction: Extraction::Text,
                    },
                ],
            )
            .unwrap();
        let terms = question_terms("which signaling did the capstone choose");
        let hits = store
            .search_any(&terms, 5, Arc::new(AtomicBool::new(false)))
            .unwrap();
        assert_eq!(
            hits.iter().map(|hit| hit.path.as_str()).collect::<Vec<_>>(),
            ["/fixture/a.md"]
        );
        let passage = store.passage(hits[0].id, &terms, 600).unwrap().unwrap();
        assert!(
            passage.contains("WebRTC signaling") && passage.len() <= 600,
            "{passage}"
        );
        store
            .put_embedding(
                ids[0].0,
                ids[0].1,
                "[\"apple-contextual-en\",1,2]",
                &[1.0, 0.0],
            )
            .unwrap();
        store
            .put_embedding(
                ids[1].0,
                ids[1].1,
                "[\"apple-sentence-en\",1,2]",
                &[0.0, 1.0],
            )
            .unwrap();
        let report = store
            .compact(
                &fixture.0,
                "apple-contextual-en",
                Arc::new(AtomicBool::new(false)),
                |_| {},
            )
            .unwrap();
        assert_eq!(report.removed_vectors, 1);
        let models: Vec<String> = store
            .connection
            .prepare("SELECT model FROM embeddings")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(models, ["[\"apple-contextual-en\",1,2]"]);
        assert_eq!(store.count().unwrap(), 2);
        assert_eq!(
            store
                .search("polling", 5, Arc::new(AtomicBool::new(false)))
                .unwrap()
                .hits
                .len(),
            1
        );
    }

    #[test]
    fn reader_opens_after_the_last_writer_closes() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).unwrap();
        store
            .put_batch(&scan, &[doc("a", "/fixture/a.txt", "idle heron")])
            .unwrap();
        drop(store);
        // Closing the last WAL connection removes the side files, as when indexing goes idle; the
        // bundled SQLite must still let a read-only connection open and search.
        assert!(!std::path::PathBuf::from(format!("{}-wal", fixture.0.display())).exists());
        let reader = ContentStore::open_reader(&fixture.0).expect("reader on an idle index");
        assert_eq!(search(&reader, "heron").len(), 1);
        assert!(
            reader
                .connection
                .execute("DELETE FROM documents", [])
                .is_err(),
            "readers stay query-only"
        );
    }

    #[test]
    fn extraction_failure_retains_previous_body_and_invalidates_embedding() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let first = store.begin_scan(Path::new("/fixture")).unwrap();
        let (id, revision) = store
            .put_batch(
                &first,
                &[doc("a", "/fixture/report.pdf", "old extracted body")],
            )
            .unwrap()[0];
        store
            .put_embedding(id, revision, "fixture", &[1.0, 0.0])
            .unwrap();
        let next = store.begin_scan(Path::new("/fixture")).unwrap();
        store
            .mark_extraction_failure(
                &next,
                "a",
                Path::new("/fixture/report.pdf"),
                "report.pdf",
                2,
                2,
                99,
                Extraction::Unreadable,
            )
            .unwrap();
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT body,extraction,revision FROM documents WHERE id=?1",
                    [id],
                    |r| Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?
                    ))
                )
                .unwrap(),
            ("old extracted body".into(), 5, revision + 1)
        );
        assert_eq!(
            store
                .connection
                .query_row("SELECT count(*) FROM embeddings", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(search(&store, "old extracted").len(), 1);
        let retry = store.begin_scan(Path::new("/fixture")).unwrap();
        assert!(
            !store
                .mark_unchanged(&retry, "a", Path::new("/fixture/report.pdf"), 2, 2, 99)
                .unwrap()
        );
        store
            .put_batch(
                &retry,
                &[Document {
                    identity: "a",
                    path: Path::new("/fixture/report.pdf"),
                    title: "report.pdf",
                    body: "new extracted body",
                    modified_ns: 3,
                    changed_ns: 3,
                    bytes: 18,
                    extraction: Extraction::Extracted,
                }],
            )
            .unwrap();
        assert_eq!(
            store
                .connection
                .query_row("SELECT extraction FROM documents WHERE id=?1", [id], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );
    }

    #[test]
    fn version_four_migration_preserves_vectors_and_failure_rolls_back() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).unwrap();
        let (id, revision) = store
            .put_batch(&scan, &[doc("a", "/fixture/a.txt", "retained text")])
            .unwrap()[0];
        store
            .put_embedding(id, revision, "fixture", &[1.0, 0.0])
            .unwrap();
        downgrade_embeddings_to_v4(&store);
        store.connection.execute_batch("CREATE TABLE embeddings_v4(private TEXT); INSERT INTO embeddings_v4 VALUES('preserved');").unwrap();
        drop(store);
        assert!(ContentStore::open(&fixture.0).is_err());
        let connection = Connection::open(&fixture.0).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            4
        );
        assert_eq!(
            connection
                .query_row("SELECT private FROM embeddings_v4", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "preserved"
        );
        assert_eq!(
            connection
                .query_row("SELECT hex(vector) FROM embeddings", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "0000803F00000000"
        );
        connection
            .execute_batch("DROP TABLE embeddings_v4")
            .unwrap();
        drop(connection);
        let store = fixture.open();
        let watermark = store.embedding_watermark().unwrap();
        let page = store
            .embedding_vectors(0, watermark, "fixture", 2, Arc::new(AtomicBool::new(false)))
            .unwrap();
        assert_eq!(page.vectors.len(), 1);
        assert_eq!(page.vectors[0].values, vec![1.0, 0.0]);
        assert_eq!(
            store
                .resolve_embeddings(
                    "fixture",
                    2,
                    &[(page.vectors[0].key, 0.1)],
                    Arc::new(AtomicBool::new(false))
                )
                .unwrap()[0]
                .id,
            id
        );
        assert_eq!(search(&store, "retained").len(), 1);
        store.check_integrity().unwrap();
    }

    #[test]
    fn vector_keys_never_alias_replaced_deleted_or_erased_embeddings() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).unwrap();
        let (id, revision) = store
            .put_batch(&scan, &[doc("a", "/fixture/a.txt", "first")])
            .unwrap()[0];
        let resolve = |store: &ContentStore, key| {
            store
                .resolve_embeddings(
                    "fixture",
                    2,
                    &[(key, 0.0)],
                    Arc::new(AtomicBool::new(false)),
                )
                .unwrap()
        };
        store
            .put_embedding(id, revision, "fixture", &[1.0, 0.0])
            .unwrap();
        let first = store.embedding_watermark().unwrap();
        assert_eq!(resolve(&store, first).len(), 1);
        store.connection.execute_batch("CREATE TRIGGER fail_embedding BEFORE INSERT ON embeddings BEGIN SELECT RAISE(ABORT,'fixture failure'); END;").unwrap();
        assert!(
            store
                .put_embedding(id, revision, "fixture", &[0.0, 1.0])
                .is_err()
        );
        assert_eq!(
            resolve(&store, first).len(),
            1,
            "failed replacement must roll back its deletion"
        );
        store
            .connection
            .execute_batch("DROP TRIGGER fail_embedding")
            .unwrap();
        store
            .put_embedding(id, revision, "fixture", &[0.0, 1.0])
            .unwrap();
        let second = store.embedding_watermark().unwrap();
        assert!(second > first);
        assert!(resolve(&store, first).is_empty());
        assert_eq!(resolve(&store, second).len(), 1);
        let (_, revision) = store
            .put_batch(&scan, &[doc("a", "/fixture/a.txt", "changed")])
            .unwrap()[0];
        assert!(resolve(&store, second).is_empty());
        store
            .put_embedding(id, revision, "fixture", &[1.0, 0.0])
            .unwrap();
        let third = store.embedding_watermark().unwrap();
        store
            .erase(Arc::new(AtomicBool::new(false)), |_| {})
            .unwrap();
        assert_eq!(store.embedding_watermark().unwrap(), third);
        assert!(resolve(&store, third).is_empty());
        drop(store);
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).unwrap();
        let (id, revision) = store
            .put_batch(&scan, &[doc("new", "/fixture/new.txt", "replacement")])
            .unwrap()[0];
        store
            .put_embedding(id, revision, "fixture", &[1.0, 0.0])
            .unwrap();
        assert!(store.embedding_watermark().unwrap() > third);
        for key in [first, second, third] {
            assert!(resolve(&store, key).is_empty());
        }
    }

    #[test]
    fn vector_export_and_resolution_bound_and_validate_untrusted_data() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).unwrap();
        let paths: Vec<_> = (0..130).map(|i| format!("/fixture/{i}.txt")).collect();
        let documents: Vec<_> = paths
            .iter()
            .map(|path| doc(path, path, "fixture"))
            .collect();
        for (id, revision) in store.put_batch(&scan, &documents).unwrap() {
            store
                .put_embedding(id, revision, "fixture", &[1.0, 0.0])
                .unwrap();
        }
        let high = store.embedding_watermark().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let first = store
            .embedding_vectors(0, high, "fixture", 2, Arc::clone(&cancel))
            .unwrap();
        assert_eq!(first.vectors.len(), 64);
        let second = store
            .embedding_vectors(first.after, high, "fixture", 2, Arc::clone(&cancel))
            .unwrap();
        assert_eq!(second.vectors.len(), 64);
        assert!(second.vectors[0].key > first.after);
        assert_eq!(
            store
                .embedding_vectors(second.after, high, "fixture", 2, Arc::clone(&cancel))
                .unwrap()
                .vectors
                .len(),
            2
        );
        let other = store
            .embedding_vectors(0, high, "other", 2, Arc::clone(&cancel))
            .unwrap();
        assert_eq!(other.scanned, 64);
        assert!(other.vectors.is_empty());
        let key = first.vectors[0].key;
        let resolved = store
            .resolve_embeddings("fixture", 2, &[(key, 0.4), (key, 0.1)], Arc::clone(&cancel))
            .unwrap();
        assert_eq!(resolved.len(), 1);
        assert!((resolved[0].rank - 0.1).abs() < 0.00001);
        assert!(
            store
                .resolve_embeddings("other", 2, &[(key, 0.0)], Arc::clone(&cancel))
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .resolve_embeddings("fixture", 2, &[(key, f32::NAN)], Arc::clone(&cancel))
                .is_err()
        );
        assert!(
            store
                .resolve_embeddings("fixture", 2, &[(key, 0.0); 101], Arc::clone(&cancel))
                .is_err()
        );
        assert!(matches!(
            store.embedding_vectors(0, high, "fixture", 2, Arc::new(AtomicBool::new(true))),
            Err(Error::Cancelled)
        ));
        store
            .connection
            .execute(
                "UPDATE embeddings SET vector=x'0000c07f00000000' WHERE id=?1",
                [key],
            )
            .unwrap();
        assert!(
            store
                .embedding_vectors(0, high, "fixture", 2, cancel)
                .is_err()
        );
        assert_eq!(search(&store, "fixture").len(), 50);
    }

    #[test]
    fn validation_is_atomic_and_search_operators_remain_data() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).expect("scan");
        assert!(
            store
                .put_batch(
                    &scan,
                    &[
                        doc("a", "/fixture/a", "safe"),
                        doc("b", "/fixture/../secret", "bad")
                    ]
                )
                .is_err()
        );
        assert_eq!(store.count().expect("count"), 0);
        store
            .put_batch(&scan, &[doc("a", "/fixture/a", "safe content")])
            .expect("put");
        assert!(search(&store, "safe OR missing").is_empty());
        assert!(matches!(
            store.search("safe", 50, Arc::new(AtomicBool::new(true))),
            Err(Error::Cancelled)
        ));
        assert!(!search(&store, "safe").is_empty());
    }

    #[test]
    fn version_one_migrates_without_losing_content() {
        let fixture = Fixture::new();
        std::fs::create_dir_all(fixture.0.parent().expect("parent")).expect("mkdir");
        {
            let connection = Connection::open(&fixture.0).expect("open");
            connection.execute_batch(SCHEMA_V1).expect("v1");
            connection
                .pragma_update(None, "application_id", APPLICATION_ID)
                .expect("id");
            connection
                .pragma_update(None, "user_version", 1)
                .expect("version");
            connection.execute("INSERT INTO documents(identity,root,path,title,body,modified_ns,bytes,seen) VALUES('a','/fixture','/fixture/a','notes','retained history',1,16,1)",[]).expect("fixture");
        }
        let store = fixture.open();
        assert_eq!(search(&store, "retained").len(), 1);
        store.check_integrity().expect("integrity");
    }

    #[test]
    fn foreign_future_and_corrupt_databases_are_preserved() {
        let fixture = Fixture::new();
        let store = fixture.open();
        store
            .connection
            .pragma_update(None, "user_version", 999)
            .expect("future");
        drop(store);
        assert!(matches!(
            ContentStore::open(&fixture.0),
            Err(Error::UnsupportedSchema(999))
        ));
        let connection = Connection::open(&fixture.0).expect("read");
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .expect("version"),
            999
        );
        drop(connection);
        let other = Fixture::new();
        std::fs::create_dir_all(other.0.parent().expect("parent")).expect("mkdir");
        std::fs::write(&other.0, b"not a database").expect("fixture");
        assert!(ContentStore::open(&other.0).is_err());
        assert_eq!(
            std::fs::read(&other.0).expect("preserved"),
            b"not a database"
        );
    }

    #[test]
    fn database_symlink_is_rejected_for_readers_and_writers() {
        let fixture = Fixture::new();
        let store = fixture.open();
        let link = fixture.0.with_file_name("link.sqlite");
        std::os::unix::fs::symlink(&fixture.0, &link).expect("link");
        assert!(ContentStore::open(&link).is_err());
        assert!(ContentStore::open_reader(&link).is_err());
        assert_eq!(store.count().expect("untouched"), 0);
    }

    #[test]
    fn erasure_is_resumable_clears_segments_and_keeps_ids_and_scans_stale() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store
            .begin_scan(Path::new("/privatefixture"))
            .expect("scan");
        let paths: Vec<_> = (0..MAX_BATCH)
            .map(|i| format!("/privatefixture/{i}.txt"))
            .collect();
        let documents: Vec<_> = paths
            .iter()
            .map(|path| doc(path, path, "qzxeraseprivateexcerpt"))
            .collect();
        let id = store.put_batch(&scan, &documents).expect("put")[0];
        store
            .put_batch(
                &scan,
                &[doc(
                    "remaining",
                    "/privatefixture/last.txt",
                    "qzxeraseprivateexcerpt",
                )],
            )
            .expect("overflow");
        store
            .put_embedding(id.0, id.1, "fixture", &[1.0, 0.0])
            .expect("embedding");
        let cancel = Arc::new(AtomicBool::new(false));
        assert!(matches!(
            store.erase(Arc::clone(&cancel), |_| cancel
                .store(true, Ordering::Release)),
            Err(Error::Cancelled)
        ));
        assert_eq!(store.count().expect("partial"), 1);
        assert!(
            store
                .put_batch(&scan, &[doc("stale", "/privatefixture/stale.txt", "stale")])
                .is_err()
        );
        drop(store);
        let mut store = fixture.open();
        store
            .erase(Arc::new(AtomicBool::new(false)), |_| {})
            .expect("resume");
        assert_eq!(store.count().expect("empty"), 0);
        assert!(search(&store, "qzxeraseprivateexcerpt").is_empty());
        for table in ["scopes", "embeddings", "vector_shards"] {
            assert_eq!(
                store
                    .connection
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                        .get::<_, i64>(0))
                    .expect("empty table"),
                0
            );
        }
        let bytes = std::fs::read(&fixture.0).expect("database");
        for marker in [
            b"qzxeraseprivateexcerpt".as_slice(),
            b"/privatefixture".as_slice(),
        ] {
            assert!(
                !bytes.windows(marker.len()).any(|bytes| bytes == marker),
                "fixture payload must be cleared from the checkpointed database"
            );
        }
        let new_scan = store
            .begin_scan(Path::new("/privatefixture"))
            .expect("new scan");
        assert!(
            store
                .put_batch(&scan, &[doc("stale", "/privatefixture/stale.txt", "stale")])
                .is_err()
        );
        let replacement = store
            .put_batch(&new_scan, &[doc("new", "/privatefixture/new.txt", "new")])
            .expect("new")[0];
        assert!(replacement.0 > id.0);
        assert!(
            !store
                .put_embedding(id.0, id.1, "fixture", &[1.0, 0.0])
                .expect("stale embedding")
        );
        store.check_integrity().expect("integrity");
    }

    #[test]
    fn erasure_reports_a_pinned_journal_and_retry_finishes_after_the_reader_closes() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).expect("scan");
        store
            .put_batch(&scan, &[doc("file", "/fixture/a.txt", "private excerpt")])
            .expect("put");
        let reader = ContentStore::open_reader(&fixture.0).expect("reader");
        reader.connection.execute_batch("BEGIN").expect("snapshot");
        assert_eq!(reader.count().expect("pin"), 1);
        assert!(
            store
                .erase(Arc::new(AtomicBool::new(false)), |_| {})
                .is_err(),
            "Pinned journal must not report complete erasure"
        );
        assert_eq!(reader.count().expect("old snapshot"), 1);
        assert_eq!(store.count().expect("new snapshot"), 0);
        drop(reader);
        store
            .erase(Arc::new(AtomicBool::new(false)), |_| {})
            .expect("retry");
        assert_eq!(
            std::fs::metadata(fixture.0.with_file_name("content.sqlite-wal"))
                .expect("journal")
                .len(),
            0
        );
    }

    #[test]
    fn version_three_migration_keeps_scan_generations_monotonic() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).expect("scan");
        store
            .put_batch(&scan, &[doc("file", "/fixture/a.txt", "retained")])
            .expect("put");
        store
            .connection
            .execute_batch(
                "UPDATE scopes SET generation=99; DROP TABLE scan_clock;
            DROP TABLE embeddings; DROP TABLE vector_shards; ALTER TABLE documents DROP COLUMN extraction;",
            )
            .expect("remove newer schema");
        store
            .connection
            .execute_batch(SCHEMA_V2)
            .expect("v3 embeddings");
        store
            .connection
            .execute_batch("PRAGMA user_version=3;")
            .expect("v3 fixture");
        drop(store);
        let mut store = fixture.open();
        let scan = store
            .begin_scan(Path::new("/fixture"))
            .expect("migrated scan");
        assert_eq!(scan.generation, 100);
        assert_eq!(search(&store, "retained").len(), 1);
        store.check_integrity().expect("integrity");
    }

    #[test]
    fn deleted_terms_are_removed_from_fts_segments() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let marker = "qzxprivateuniquesecret";
        let scan = store.begin_scan(Path::new("/fixture")).expect("scan");
        store
            .put_batch(
                &scan,
                &[
                    doc("private", "/fixture/private.txt", marker),
                    doc("retained", "/fixture/retained.txt", "visible content"),
                ],
            )
            .expect("put");
        let contains_marker = |store: &ContentStore| {
            let mut statement = store
                .connection
                .prepare("SELECT block FROM content_fts_data")
                .expect("segments");
            statement
                .query_map([], |row| row.get::<_, Vec<u8>>(0))
                .expect("query")
                .any(|block| {
                    block
                        .expect("block")
                        .windows(marker.len())
                        .any(|bytes| bytes == marker.as_bytes())
                })
        };
        assert!(
            contains_marker(&store),
            "positive control must observe the stored term"
        );
        let scan = store.begin_scan(Path::new("/fixture")).expect("scan");
        store
            .mark_unchanged(
                &scan,
                "retained",
                Path::new("/fixture/retained.txt"),
                1,
                1,
                15,
            )
            .expect("keep");
        store
            .finish_scan(&scan, ScanOutcome::Complete, &AtomicBool::new(false))
            .expect("prune");
        assert_eq!(search(&store, "visible").len(), 1);
        assert!(
            !contains_marker(&store),
            "deletion must remove the term from FTS segments"
        );
        store.check_integrity().expect("integrity");
    }

    #[test]
    fn removing_roots_preserves_retained_records_and_invalidates_old_scans() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let keep = store.begin_scan(Path::new("/keep")).expect("scan");
        store
            .put_batch(&keep, &[doc("keep", "/keep/a.txt", "retained")])
            .expect("keep");
        let remove = store.begin_scan(Path::new("/remove")).expect("scan");
        let id = store
            .put_batch(&remove, &[doc("remove", "/remove/a.txt", "discarded")])
            .expect("remove")[0];
        store
            .put_embedding(id.0, id.1, "fixture", &[1.0, 0.0])
            .expect("embedding");
        assert!(matches!(
            store.retain_roots(&[], &AtomicBool::new(true)),
            Err(Error::Cancelled)
        ));
        assert_eq!(store.count().expect("unchanged"), 2);
        assert!(
            store
                .retain_roots(&["/keep/../other".into()], &AtomicBool::new(false))
                .is_err()
        );
        assert_eq!(store.count().expect("unchanged"), 2);
        assert_eq!(
            store
                .retain_roots(&["/keep".into()], &AtomicBool::new(false))
                .expect("cleanup"),
            1
        );
        assert_eq!(search(&store, "retained").len(), 1);
        assert!(search(&store, "discarded").is_empty());
        assert!(
            !store
                .put_embedding(id.0, id.1, "fixture", &[1.0, 0.0])
                .expect("stale embedding")
        );
        assert!(
            store
                .put_batch(&remove, &[doc("stale", "/remove/b.txt", "stale")])
                .is_err()
        );
        let _new_scan = store
            .begin_scan(Path::new("/remove"))
            .expect("readded root");
        assert!(
            store
                .put_batch(&remove, &[doc("stale", "/remove/b.txt", "stale")])
                .is_err()
        );
        store.check_integrity().expect("integrity");
        drop(store);
        assert_eq!(fixture.open().count().expect("reopen"), 1);
    }

    #[test]
    fn malformed_database_paths_are_not_published() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).expect("scan");
        store
            .put_batch(&scan, &[doc("file", "/fixture/safe.txt", "fixture text")])
            .expect("put");
        store
            .connection
            .execute("UPDATE documents SET path='/fixture/../outside.txt'", [])
            .expect("malformed fixture");
        assert!(search(&store, "fixture").is_empty());
    }

    #[test]
    fn a_version_seven_index_gains_chunks_without_losing_anything() {
        let fixture = Fixture::new();
        std::fs::create_dir_all(fixture.0.parent().expect("parent")).expect("mkdir");
        {
            // A v7 database, as an installed 0.2.8 leaves it: documents, a body, a published shard.
            let connection = Connection::open(&fixture.0).expect("open");
            connection.execute_batch(SCHEMA_V1).expect("v1");
            connection.execute_batch(SCHEMA_V2).expect("v2");
            connection
                .execute_batch(
                    "ALTER TABLE documents ADD COLUMN changed_ns INTEGER NOT NULL DEFAULT 0;
                     ALTER TABLE documents ADD COLUMN extraction INTEGER NOT NULL DEFAULT 0 CHECK(extraction BETWEEN 0 AND 5);
                     CREATE TABLE scan_clock(id INTEGER PRIMARY KEY CHECK(id=1), generation INTEGER NOT NULL);
                     INSERT INTO scan_clock VALUES(1,0);
                     CREATE TABLE vector_shards(after_key INTEGER PRIMARY KEY, through_key INTEGER NOT NULL,
                        model TEXT NOT NULL, dimensions INTEGER NOT NULL, token TEXT NOT NULL UNIQUE,
                        checksum TEXT NOT NULL, count INTEGER NOT NULL, bytes INTEGER NOT NULL);
                     INSERT INTO vector_shards VALUES(0,1,'apple-contextual-en',512,'0123456789abcdef0123456789abcdef','a',1,64);
                     INSERT INTO documents(identity,root,path,title,body,modified_ns,bytes,seen,revision)
                        VALUES('old','/fixture','/fixture/kept.md','kept.md','renewal terms',1,13,1,1);",
                )
                .expect("v7 shape");
            connection
                .pragma_update(None, "application_id", APPLICATION_ID)
                .expect("id");
            connection
                .pragma_update(None, "user_version", 7)
                .expect("v7");
        }
        let store = fixture.open();
        assert_eq!(
            store.count().expect("count"),
            1,
            "documents survive the migration"
        );
        assert_eq!(search(&store, "renewal")[0].path, "/fixture/kept.md");
        let shards: i64 = store
            .connection
            .query_row("SELECT count(*) FROM vector_shards", [], |row| row.get(0))
            .expect("shards");
        assert_eq!(shards, 1, "published vectors keep serving");
        let chunks: i64 = store
            .connection
            .query_row("SELECT count(*) FROM chunks", [], |row| row.get(0))
            .expect("chunks");
        assert_eq!(
            chunks, 0,
            "chunks fill in as files are re-read, not during migration"
        );
        store.check_integrity().expect("integrity");
    }

    #[test]
    fn passage_backfill_revisits_unchanged_metadata_and_keeps_legacy_excerpt_bounded() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).unwrap();
        let body = format!("{}\n\nuniquelatesection", "ordinary paragraph\n\n".repeat(4000));
        store.put_batch(&scan, &[doc("a", "/fixture/long.md", &body)]).unwrap();
        assert_eq!(store.connection.query_row("SELECT length(CAST(body AS BLOB)) FROM documents", [], |r| r.get::<_, i64>(0)).unwrap(), MAX_BODY_BYTES as i64);
        assert_eq!(store.passages("uniquelatesection", 10, Arc::new(AtomicBool::new(false))).unwrap().len(), 1);
        assert!(store.mark_unchanged(&scan, "a", Path::new("/fixture/long.md"), 1, 1, body.len() as u64).unwrap());
        store.connection.execute("UPDATE documents SET passage_version=0", []).unwrap();
        assert!(!store.mark_unchanged(&scan, "a", Path::new("/fixture/long.md"), 1, 1, body.len() as u64).unwrap());
        assert_eq!(store.passages("uniquelatesection", 10, Arc::new(AtomicBool::new(false))).unwrap().len(), 1);
    }

    #[test]
    fn office_text_does_not_invent_pdf_page_numbers() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).unwrap();
        store.put_batch(&scan, &[Document { extraction: Extraction::Extracted,
            ..doc("word", "/fixture/report.docx", "Renewal deadline") }]).unwrap();
        let found = store.passages("renewal", 10, Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(found[0].page, 0);
    }

    #[test]
    fn chunks_follow_their_document_through_rewrites_and_deletion() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).expect("scan");
        let long = format!(
            "# Notes\n\n{}\n\n## Later\n\n{}\n",
            "alpha ".repeat(300),
            "omega ".repeat(300)
        );
        store
            .put_batch(&scan, &[doc("a", "/fixture/notes.md", &long)])
            .expect("put");
        let counted = |store: &ContentStore| -> i64 {
            store
                .connection
                .query_row("SELECT count(*) FROM chunks", [], |row| row.get(0))
                .expect("count")
        };
        let first = counted(&store);
        assert!(first > 1, "a long note becomes several passages, not one");

        let scan = store.begin_scan(Path::new("/fixture")).expect("rescan");
        store
            .put_batch(
                &scan,
                &[doc("a", "/fixture/notes.md", "# Notes\n\nshort now\n")],
            )
            .expect("rewrite");
        assert_eq!(counted(&store), 1, "rewriting a file replaces its passages");
        assert!(
            store
                .passages("alpha", 10, Arc::new(AtomicBool::new(false)))
                .expect("passages")
                .is_empty()
        );

        let scan = store.begin_scan(Path::new("/fixture")).expect("third");
        store
            .finish_scan(&scan, ScanOutcome::Complete, &AtomicBool::new(false))
            .expect("finish");
        assert_eq!(
            counted(&store),
            0,
            "deleting a document takes its passages with it"
        );
        store.check_integrity().expect("integrity");
    }

    #[test]
    fn passages_carry_the_page_or_line_to_open() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).expect("scan");
        let report = Document {
            extraction: Extraction::Extracted,
            ..doc(
                "pdf",
                "/fixture/report.pdf",
                "Cover page\u{c}The renewal deadline is October 31\u{c}Signatures",
            )
        };
        let filler = "    let _ = 1;\n".repeat(30);
        let source = format!(
            "use std::fs;\n\nfn publish_shard() {{\n{filler}}}\n\nfn rebuild_shard() {{\n{filler}}}\n"
        );
        let code = doc("code", "/fixture/store.rs", &source);
        store.put_batch(&scan, &[report, code]).expect("put");
        let cancel = Arc::new(AtomicBool::new(false));

        let found = store
            .passages("renewal deadline", 10, Arc::clone(&cancel))
            .expect("pdf");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].page, 2, "the page a reader should be taken to");
        assert_eq!(found[0].line, 0);
        assert!(found[0].text.contains("October 31"));

        let found = store.passages("rebuild shard", 10, cancel).expect("code");
        assert_eq!(found[0].path, "/fixture/store.rs");
        assert_eq!(
            found[0].line, 36,
            "the second declaration starts its own passage, at its own line"
        );
        assert!(found[0].heading.contains("rebuild_shard"));
    }

    #[test]
    fn a_question_in_your_own_words_still_finds_the_passage() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).expect("scan");
        store
            .put_batch(
                &scan,
                &[doc(
                    "r",
                    "/fixture/releasing.md",
                    "# Releasing\n\nPush a version tag and the workflow publishes the release.\n",
                )],
            )
            .expect("put");
        let cancel = Arc::new(AtomicBool::new(false));
        // Every term matches, so the strict pass answers.
        assert!(
            !store
                .passages("version tag workflow", 10, Arc::clone(&cancel))
                .expect("all")
                .is_empty()
        );
        // "how" and "I" appear nowhere: all-terms finds nothing, any-terms still does.
        let asked = store
            .passages("how do I publish a release", 10, cancel)
            .expect("any");
        assert_eq!(
            asked.first().map(|passage| passage.path.as_str()),
            Some("/fixture/releasing.md")
        );
    }

    #[test]
    fn failed_migration_rolls_back_and_foreign_database_is_untouched() {
        let fixture = Fixture::new();
        std::fs::create_dir_all(fixture.0.parent().expect("parent")).expect("mkdir");
        let connection = Connection::open(&fixture.0).expect("open");
        connection.execute_batch(SCHEMA_V1).expect("v1");
        connection
            .execute_batch(
                "CREATE TABLE embeddings(private TEXT); INSERT INTO embeddings VALUES('retained');",
            )
            .expect("conflict");
        connection
            .pragma_update(None, "application_id", APPLICATION_ID)
            .expect("id");
        connection
            .pragma_update(None, "user_version", 1)
            .expect("v1");
        assert!(ContentStore::open(&fixture.0).is_err());
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .expect("version"),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT private FROM embeddings", [], |r| r
                    .get::<_, String>(0))
                .expect("preserved"),
            "retained"
        );
        connection
            .pragma_update(None, "application_id", 1234)
            .expect("foreign id");
        assert!(matches!(
            ContentStore::open(&fixture.0),
            Err(Error::Invalid(_))
        ));
        assert_eq!(
            connection
                .query_row("PRAGMA application_id", [], |r| r.get::<_, i64>(0))
                .expect("id"),
            1234
        );
    }

    #[test]
    fn reader_observes_commits_without_waiting_for_writer_and_results_are_bounded() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).expect("scan");
        let names: Vec<String> = (0..200).map(|i| format!("/fixture/item{i}")).collect();
        let documents: Vec<_> = names
            .iter()
            .map(|name| doc(name, name, "shared content"))
            .collect();
        store.put_batch(&scan, &documents).expect("put");
        let reader = ContentStore::open_reader(&fixture.0).expect("reader");
        let transaction = store.connection.transaction().expect("transaction");
        transaction
            .execute("UPDATE documents SET body='replacement'", [])
            .expect("uncommitted");
        assert_eq!(
            reader
                .search("shared", usize::MAX, Arc::new(AtomicBool::new(false)))
                .expect("snapshot")
                .hits
                .len(),
            MAX_RESULTS
        );
        transaction.commit().expect("commit");
        assert!(search(&reader, "shared").is_empty());
        assert_eq!(search(&reader, "replacement").len(), 50);
        store.check_integrity().expect("integrity");
    }

    #[test]
    fn broad_queries_report_limited_candidates_and_selective_queries_remain_complete() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).expect("scan");
        for offset in (0..MAX_CANDIDATES).step_by(MAX_BATCH) {
            let names: Vec<_> = (offset..(offset + MAX_BATCH).min(MAX_CANDIDATES))
                .map(|i| format!("/fixture/item{i}"))
                .collect();
            let documents: Vec<_> = names.iter().map(|name| doc(name, name, "shared")).collect();
            store.put_batch(&scan, &documents).expect("put");
        }
        let query = |store: &ContentStore, text| {
            store
                .search(text, 50, Arc::new(AtomicBool::new(false)))
                .expect("search")
        };
        assert!(!query(&store, "shared").limited);
        store
            .put_batch(
                &scan,
                &[doc("overflow", "/fixture/overflow", "shared unique")],
            )
            .expect("overflow");
        let page = query(&store, "shared");
        assert!(page.limited);
        assert_eq!(page.hits.len(), 50);
        assert_eq!(page, query(&store, "shared"), "stable ordering");
        let selective = query(&store, "unique");
        assert!(!selective.limited);
        assert_eq!(selective.hits.len(), 1);
    }

    #[test]
    fn a_version_eight_index_gains_chunk_vectors_without_losing_anything() {
        let fixture = Fixture::new();
        std::fs::create_dir_all(fixture.0.parent().expect("parent")).expect("mkdir");
        {
            // A v8 database: documents, chunks, a published shard and a document vector.
            let connection = Connection::open(&fixture.0).expect("open");
            connection.execute_batch(SCHEMA_V1).expect("v1");
            connection.execute_batch(SCHEMA_V2).expect("v2");
            connection
                .execute_batch(
                    "ALTER TABLE documents ADD COLUMN changed_ns INTEGER NOT NULL DEFAULT 0;
                     ALTER TABLE documents ADD COLUMN extraction INTEGER NOT NULL DEFAULT 0 CHECK(extraction BETWEEN 0 AND 5);
                     CREATE TABLE scan_clock(id INTEGER PRIMARY KEY CHECK(id=1), generation INTEGER NOT NULL);
                     INSERT INTO scan_clock VALUES(1,0);
                     CREATE TABLE vector_shards(after_key INTEGER PRIMARY KEY, through_key INTEGER NOT NULL,
                        model TEXT NOT NULL, dimensions INTEGER NOT NULL, token TEXT NOT NULL UNIQUE,
                        checksum TEXT NOT NULL, count INTEGER NOT NULL, bytes INTEGER NOT NULL);
                     INSERT INTO vector_shards VALUES(0,1,'apple-contextual-en',512,'0123456789abcdef0123456789abcdef','a',1,64);
                     INSERT INTO documents(identity,root,path,title,body,modified_ns,bytes,seen,revision)
                        VALUES('old','/fixture','/fixture/kept.md','kept.md','renewal terms',1,13,1,1);",
                )
                .expect("v8 shape");
            connection.execute_batch(SCHEMA_V8).expect("chunks");
            connection
                .execute_batch(
                    "INSERT INTO chunks(document_id,ordinal,page,line,heading,text,symbols)
                        VALUES(1,0,0,1,'Renewal','renewal terms','');
                     INSERT INTO embeddings(document_id,revision,model,dimensions,vector)
                        VALUES(1,1,'apple-contextual-en',2,x'0000803f00000000');",
                )
                .expect("v8 content");
            connection
                .pragma_update(None, "application_id", APPLICATION_ID)
                .expect("id");
            connection
                .pragma_update(None, "user_version", 8)
                .expect("v8");
        }
        let store = fixture.open();
        let counted = |table: &str| -> i64 {
            store
                .connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("count")
        };
        assert_eq!(store.count().expect("count"), 1, "documents survive");
        assert_eq!(search(&store, "renewal")[0].path, "/fixture/kept.md");
        assert_eq!(counted("chunks"), 1, "passages survive");
        assert_eq!(
            counted("vector_shards"),
            1,
            "published vectors keep serving"
        );
        assert_eq!(counted("embeddings"), 1, "document vectors are not retired");
        assert_eq!(
            counted("chunk_embeddings"),
            0,
            "passage vectors are embedded by a later pass, not by the migration"
        );
        // The passage is now offered to the model, which is the whole point of the migration.
        let page = store
            .chunk_embedding_page(0, "fixture", 2, Arc::new(AtomicBool::new(false)))
            .expect("page");
        assert_eq!(page.pending.len(), 1);
        assert!(page.pending[0].text.contains("renewal terms"));
        store.check_integrity().expect("integrity");
    }

    /// A unit vector of `dimensions` width, distinct per `seed`.
    fn vector(dimensions: usize, seed: usize) -> Vec<f32> {
        let mut values = vec![0.0; dimensions];
        values[seed % dimensions] = 1.0;
        values
    }

    #[test]
    fn chunk_embedding_pages_are_bounded_resume_and_skip_current_vectors() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).unwrap();
        let paths: Vec<_> = (0..130).map(|i| format!("/fixture/{i}.txt")).collect();
        let documents: Vec<_> = paths
            .iter()
            .map(|path| doc(path, path, "one short passage"))
            .collect();
        store.put_batch(&scan, &documents).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));

        let first = store
            .chunk_embedding_page(0, "fixture-v1", 2, Arc::clone(&cancel))
            .unwrap();
        assert_eq!(first.scanned, 128, "the page is bounded");
        assert_eq!(first.pending.len(), 128);
        assert!(first.pending.iter().all(|chunk| !chunk.text.is_empty()));

        let second = store
            .chunk_embedding_page(first.after, "fixture-v1", 2, Arc::clone(&cancel))
            .unwrap();
        assert_eq!(
            second.scanned, 2,
            "the cursor resumes where the page stopped"
        );

        // A stored vector removes that chunk from the next pass, so resuming re-embeds nothing.
        let embedded = first.pending[0].id;
        assert!(
            store
                .put_chunk_embedding(embedded, "fixture-v1", &vector(2, 0))
                .unwrap()
        );
        let again = store
            .chunk_embedding_page(0, "fixture-v1", 2, Arc::clone(&cancel))
            .unwrap();
        assert_eq!(again.scanned, 128, "it is still counted as examined");
        assert_eq!(again.pending.len(), 127, "but no longer needs the model");
        assert!(again.pending.iter().all(|chunk| chunk.id != embedded));

        // Another model has its own vectors: the same chunk is pending again.
        let other = store
            .chunk_embedding_page(0, "fixture-v2", 2, cancel)
            .unwrap();
        assert_eq!(other.pending.len(), 128);
    }

    #[test]
    fn chunk_embedding_page_rejects_cancelled_and_malformed_records() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).unwrap();
        store
            .put_batch(&scan, &[doc("a", "/fixture/a.txt", "text")])
            .unwrap();
        assert!(matches!(
            store.chunk_embedding_page(0, "fixture", 2, Arc::new(AtomicBool::new(true))),
            Err(Error::Cancelled)
        ));
        assert_eq!(
            store
                .chunk_embedding_page(0, "fixture", 2, Arc::new(AtomicBool::new(false)))
                .unwrap()
                .pending
                .len(),
            1
        );
        store
            .connection
            .execute("UPDATE documents SET path='/fixture/../outside'", [])
            .unwrap();
        assert!(
            store
                .chunk_embedding_page(0, "fixture", 2, Arc::new(AtomicBool::new(false)))
                .is_err(),
            "a path that escapes its root is refused"
        );
        assert!(
            store
                .chunk_embedding_page(-1, "fixture", 2, Arc::new(AtomicBool::new(false)))
                .is_err()
        );
        // A vector whose values cannot be normalized would be unreadable once stored.
        assert!(
            store
                .put_chunk_embedding(1, "fixture", &[0.0, 0.0])
                .is_err()
        );
        assert!(
            store
                .put_chunk_embedding(1, "fixture", &[f32::NAN, 1.0])
                .is_err()
        );
        assert!(store.put_chunk_embedding(1, "", &[1.0, 0.0]).is_err());
        assert!(
            !store
                .put_chunk_embedding(999_999, "fixture", &[1.0, 0.0])
                .unwrap(),
            "a chunk that no longer exists is not an error"
        );
    }

    #[test]
    fn code_passages_are_embedded_and_machine_data_is_not() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).unwrap();
        store
            .put_batch(
                &scan,
                &[
                    doc(
                        "code",
                        "/fixture/search.rs",
                        "fn rebuild_shards() { let x = 1; }",
                    ),
                    doc("data", "/fixture/telemetry.json", r#"{"a":1,"b":2}"#),
                    doc(
                        "notes",
                        "/fixture/notes.md",
                        "# Renewal\n\nThe deadline is March.",
                    ),
                ],
            )
            .unwrap();
        let page = store
            .chunk_embedding_page(0, "fixture", 2, Arc::new(AtomicBool::new(false)))
            .unwrap();
        let embedded: Vec<_> = page
            .pending
            .iter()
            .map(|chunk| chunk.path.as_str())
            .collect();
        assert!(
            embedded.contains(&"/fixture/search.rs"),
            "a passage is one declaration with its symbols, unlike a whole-file vector: {embedded:?}"
        );
        assert!(embedded.contains(&"/fixture/notes.md"), "{embedded:?}");
        assert!(
            !embedded.contains(&"/fixture/telemetry.json"),
            "a vector over machine data describes its shape, not its meaning: {embedded:?}"
        );
        assert_eq!(page.lexical_only, 1);
    }

    #[test]
    fn rewriting_or_deleting_a_document_takes_its_chunk_vectors_with_it() {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let scan = store.begin_scan(Path::new("/fixture")).unwrap();
        store
            .put_batch(&scan, &[doc("a", "/fixture/a.md", "the original passage")])
            .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let page = store
            .chunk_embedding_page(0, "fixture", 2, Arc::clone(&cancel))
            .unwrap();
        for chunk in &page.pending {
            assert!(
                store
                    .put_chunk_embedding(chunk.id, "fixture", &vector(2, 0))
                    .unwrap()
            );
        }
        let count = |store: &ContentStore| -> i64 {
            store
                .connection
                .query_row("SELECT count(*) FROM chunk_embeddings", [], |row| {
                    row.get(0)
                })
                .unwrap()
        };
        assert!(count(&store) > 0);

        // Rewriting replaces the chunks, and the vectors cascade away with them rather than
        // describing text the file no longer contains.
        store
            .put_batch(
                &scan,
                &[doc("a", "/fixture/a.md", "a completely different passage")],
            )
            .unwrap();
        assert_eq!(count(&store), 0, "stale vectors survived a rewrite");

        let page = store
            .chunk_embedding_page(0, "fixture", 2, Arc::clone(&cancel))
            .unwrap();
        assert!(!page.pending.is_empty(), "the new passage needs a vector");
        for chunk in &page.pending {
            store
                .put_chunk_embedding(chunk.id, "fixture", &vector(2, 1))
                .unwrap();
        }
        assert!(count(&store) > 0);
        // A later scan that no longer sees the file retires it, the way the indexer does.
        let next = store.begin_scan(Path::new("/fixture")).unwrap();
        store
            .remove_unseen_under(&next, Path::new("/fixture/a.md"), &cancel)
            .unwrap();
        assert_eq!(count(&store), 0, "vectors outlived their document");
    }
}
