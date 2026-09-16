use blindspot_core::content::ContentStore;
use rusqlite::{Connection,OpenFlags};
use std::{path::{Path, PathBuf},time::Duration};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn verify_preserved(connection: &Connection, backup: &Path) -> Result<()> {
    let uri = format!("file:{}?mode=ro", backup.to_str().ok_or("Non-UTF8 backup path")?
        .replace('%', "%25").replace('?', "%3F").replace('#', "%23"));
    connection.execute("ATTACH DATABASE ?1 AS original", [uri])?;
    let tables = connection.prepare("SELECT name FROM original.sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE 'content_fts%' AND name NOT LIKE 'chunks_fts%' ORDER BY name")?
        .query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for table in &tables {
        let name = quote(table);
        let columns = connection.prepare(&format!("PRAGMA original.table_info({name})"))?
            .query_map([], |row| row.get::<_, String>(1))?.collect::<rusqlite::Result<Vec<_>>>()?
            .iter().map(|column| quote(column)).collect::<Vec<_>>().join(",");
        let changed: bool = connection.query_row(&format!(
            "SELECT (SELECT count(*) FROM main.{name}) != (SELECT count(*) FROM original.{name})
             OR EXISTS(SELECT {columns} FROM main.{name} EXCEPT SELECT {columns} FROM original.{name})
             OR EXISTS(SELECT {columns} FROM original.{name} EXCEPT SELECT {columns} FROM main.{name})"), [], |row| row.get(0))?;
        if changed { return Err(format!("Authoritative table changed: {table}").into()); }
    }
    connection.execute_batch("DETACH DATABASE original")?;
    println!("Exact original-column comparison: {} authoritative tables unchanged", tables.len());
    Ok(())
}

fn validate(connection: &Connection, passages: bool) -> Result<()> {
    let status: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if status != "ok" { return Err("SQLite integrity check failed".into()); }
    if connection.prepare("PRAGMA foreign_key_check")?.query([])?.next()?.is_some() {
        return Err("Foreign-key integrity check failed".into());
    }
    connection.execute("INSERT INTO content_fts(content_fts,rank) VALUES('integrity-check',1)", [])?;
    if passages { connection.execute("INSERT INTO chunks_fts(chunks_fts,rank) VALUES('integrity-check',1)", [])?; }
    Ok(())
}

fn counts(connection:&Connection)->rusqlite::Result<(i64,i64)> {
    Ok((connection.query_row("SELECT count(*) FROM documents",[],|row|row.get(0))?,
        connection.query_row("SELECT count(*) FROM embeddings",[],|row|row.get(0))?))
}

fn main()->Result<()> {
    let mut args=std::env::args().skip(1);
    let database=PathBuf::from(args.next().ok_or("Expected database [new snapshot directory] [--rebuild-copy]")?);
    let directory=args.next().map(PathBuf::from);
    let rebuild = match args.next().as_deref() {
        None => false,
        Some("--rebuild-copy") if directory.is_some() => true,
        _ => return Err("Only --rebuild-copy is supported, with a new snapshot directory".into()),
    };
    if args.next().is_some() {return Err("Too many arguments".into());}
    let connection=Connection::open_with_flags(&database,OpenFlags::SQLITE_OPEN_READ_ONLY|OpenFlags::SQLITE_OPEN_NOFOLLOW)?;
    connection.busy_timeout(Duration::from_secs(2))?;
    let schema:i64=connection.query_row("PRAGMA user_version",[],|row|row.get(0))?;
    let (documents,embeddings)=counts(&connection)?;
    println!("schema={schema} documents={documents} legacy_embeddings={embeddings}");
    if schema>=8 {println!("passages={}",connection.query_row("SELECT count(*) FROM chunks",[],|row|row.get::<_,i64>(0))?);}
    if schema>=10 {
        println!("passage_embeddings={} active_generations={} partial_documents={}",
            connection.query_row("SELECT count(*) FROM chunk_embeddings",[],|row|row.get::<_,i64>(0))?,
            connection.query_row("SELECT count(*) FROM passage_models WHERE active=1",[],|row|row.get::<_,i64>(0))?,
            connection.query_row("SELECT count(*) FROM documents WHERE partial=1",[],|row|row.get::<_,i64>(0))?);
    }
    if let Some(directory)=directory {
        if !directory.is_absolute() || directory.exists() {return Err("Snapshot directory must be new and absolute".into());}
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        let backup=directory.join("before.sqlite");
        connection.execute("VACUUM main INTO ?1",[backup.to_str().ok_or("Non-UTF8 output path")?])?;
        let fixture=directory.join("migration-check.sqlite");
        std::fs::copy(&backup,&fixture)?;
        let before=Connection::open_with_flags(&backup,OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let expected=counts(&before)?;
        let baseline=Connection::open(&fixture)?;
        let internal=baseline.execute("INSERT INTO content_fts(content_fts) VALUES('integrity-check')",[]);
        println!("Pre-migration copy FTS structure: {internal:?}");
        let baseline_integrity=baseline.execute("INSERT INTO content_fts(content_fts,rank) VALUES('integrity-check',1)",[]);
        println!("Pre-migration copy FTS: {baseline_integrity:?}");
        if rebuild {
            baseline.execute_batch("BEGIN IMMEDIATE; INSERT INTO content_fts(content_fts) VALUES('rebuild'); COMMIT;")?;
            validate(&baseline, schema>=8)?;
            verify_preserved(&baseline, &backup)?;
            println!("Copy-only FTS rebuild: integrity and source/vector preservation PASS");
        }
        drop(baseline);
        let migrated=ContentStore::open(&fixture)?;
        migrated.check_integrity()?;
        drop(migrated);
        let checked=Connection::open(&fixture)?;
        if counts(&checked)? != expected {return Err("Migration changed document or legacy-vector counts".into());}
        validate(&checked, true)?;
        verify_preserved(&checked, &backup)?;
        println!("Copy-only repair/migration PASS: documents={} legacy_embeddings={}; live database unchanged",expected.0,expected.1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_rebuild_restores_missing_postings_and_preserves_values() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE documents(id INTEGER PRIMARY KEY,title TEXT,body TEXT);
            INSERT INTO documents VALUES(1,'fixture','searchable words');
            CREATE VIRTUAL TABLE content_fts USING fts5(title,body,content='documents',content_rowid='id');
            CREATE TABLE embeddings(document_id INTEGER PRIMARY KEY,vector BLOB);
            INSERT INTO embeddings VALUES(1,x'0123abcd');").unwrap();
        assert!(validate(&connection, false).is_err());
        connection.execute("INSERT INTO content_fts(content_fts) VALUES('rebuild')", []).unwrap();
        validate(&connection, false).unwrap();
        assert_eq!(connection.query_row("SELECT count(*) FROM content_fts WHERE content_fts MATCH 'searchable'", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(connection.query_row("SELECT hex(vector) FROM embeddings", [], |row| row.get::<_, String>(0)).unwrap(), "0123ABCD");
        assert_eq!(connection.query_row("SELECT body FROM documents", [], |row| row.get::<_, String>(0)).unwrap(), "searchable words");
    }
}
