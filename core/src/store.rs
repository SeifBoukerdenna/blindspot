//! Persistence for the frecency store, on redb.
//!
//! redb over rusqlite, settling the open question CLAUDE.md deferred to M3 on its own
//! stated criterion — "whichever is less friction". Measured: redb adds 1 transitive
//! crate and no C build, rusqlite adds 11 and a `libsqlite3-sys` build. The payload is
//! one 24-byte row per app, well under 10 KB total, written once per launch and read
//! once at startup; rusqlite's advantage is that you can poke at the file with the
//! `sqlite3` CLI, which does not pay for a C toolchain dependency at this size.
//!
//! Hand-rolling a serde file was the third option and lost to redb by one crate: redb
//! gives crash-safe commits for free, and an atomic-rename-with-tempfile dance is code
//! that has to be written and, worse, has to be right.

use std::path::{Path, PathBuf};

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

use crate::frecency::Visit;

/// `id -> (score, updated)`. A tuple rather than a struct because redb implements its
/// `Value` trait for tuples of primitives, which spares us a serialisation format.
const VISITS: TableDefinition<u64, (f64, u64)> = TableDefinition::new("visits");

/// `~/.local/share/blindspot`, where every database blindspot keeps lives.
///
/// Not beside `config.toml`: config is hand-edited and belongs in `~/.config`, while
/// these are opaque state the user never opens. Each concern gets its own file, so
/// deleting clipboard history cannot cost the user their launch history.
pub fn data_dir() -> Result<PathBuf, StoreError> {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .ok_or(StoreError::NoHome)?;
    Ok(PathBuf::from(home).join(".local/share/blindspot"))
}

#[derive(Debug)]
pub enum StoreError {
    Open(Box<redb::DatabaseError>),
    Transaction(Box<redb::Error>),
    Io(std::io::Error),
    NoHome,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(e) => write!(f, "could not open a blindspot store: {e}"),
            Self::Transaction(e) => write!(f, "store transaction failed: {e}"),
            Self::Io(e) => write!(f, "could not create the store directory: {e}"),
            Self::NoHome => write!(f, "$HOME is unset, so there is nowhere to keep state"),
        }
    }
}

impl std::error::Error for StoreError {}

impl<E: Into<redb::Error>> From<E> for StoreError {
    fn from(e: E) -> Self {
        Self::Transaction(Box::new(e.into()))
    }
}

pub struct Store {
    db: Database,
}

impl Store {
    /// `~/.local/share/blindspot/frecency.redb`.
    pub fn default_path() -> Result<PathBuf, StoreError> {
        Ok(data_dir()?.join("frecency.redb"))
    }

    /// Opens, creating the file and its parent directory if needed.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(StoreError::Io)?;
        }
        let db = Database::create(path).map_err(|e| StoreError::Open(Box::new(e)))?;
        Ok(Self { db })
    }

    /// Every row. An absent table is an empty history, not an error — that is simply
    /// what a first run looks like.
    pub fn load(&self) -> Result<Vec<(u64, Visit)>, StoreError> {
        let read = self.db.begin_read()?;
        let table = match read.open_table(VISITS) {
            Ok(table) => table,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for row in table.iter()? {
            let (id, value) = row?;
            let (score, updated) = value.value();
            out.push((id.value(), Visit { score, updated }));
        }
        Ok(out)
    }

    pub fn put(&self, id: u64, visit: Visit) -> Result<(), StoreError> {
        let write = self.db.begin_write()?;
        {
            let mut table = write.open_table(VISITS)?;
            table.insert(id, (visit.score, visit.updated))?;
        }
        write.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempPath(PathBuf);

    impl TempPath {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!(
                "blindspot-store-{tag}-{}-{n}/frecency.redb",
                std::process::id()
            )))
        }
    }

    impl Drop for TempPath {
        fn drop(&mut self) {
            if let Some(dir) = self.0.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    #[test]
    fn a_fresh_store_is_empty_rather_than_an_error() {
        let path = TempPath::new("fresh");
        let store = Store::open(&path.0).expect("opens");
        assert!(store.load().expect("loads").is_empty());
    }

    #[test]
    fn rows_survive_a_reopen() {
        let path = TempPath::new("reopen");
        {
            let store = Store::open(&path.0).expect("opens");
            store
                .put(
                    42,
                    Visit {
                        score: 2.5,
                        updated: 1_700_000_000,
                    },
                )
                .expect("writes");
        }
        let store = Store::open(&path.0).expect("reopens");
        let rows = store.load().expect("loads");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, 42);
        assert!((rows[0].1.score - 2.5).abs() < 1e-9);
        assert_eq!(rows[0].1.updated, 1_700_000_000);
    }

    #[test]
    fn writing_the_same_id_replaces_rather_than_appends() {
        let path = TempPath::new("replace");
        let store = Store::open(&path.0).expect("opens");
        store
            .put(
                1,
                Visit {
                    score: 1.0,
                    updated: 10,
                },
            )
            .expect("writes");
        store
            .put(
                1,
                Visit {
                    score: 9.0,
                    updated: 20,
                },
            )
            .expect("writes");
        let rows = store.load().expect("loads");
        assert_eq!(rows.len(), 1);
        assert!((rows[0].1.score - 9.0).abs() < 1e-9);
    }

    #[test]
    fn the_default_path_lives_under_local_share() {
        // Only meaningful when HOME is set, which it is under `cargo test`.
        if let Ok(path) = Store::default_path() {
            assert!(path.ends_with(".local/share/blindspot/frecency.redb"));
        }
    }
}
