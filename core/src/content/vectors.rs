//! Bounded vector export and authoritative resolution of derived-index candidates.

use super::{ContentStore, Error, Hit, Result, validated_path};
use rusqlite::{OptionalExtension, params};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq)]
pub struct Vector {
    pub key: i64,
    pub values: Vec<f32>,
}

#[derive(Debug, Default)]
pub struct Page {
    pub after: i64,
    pub scanned: usize,
    pub vectors: Vec<Vector>,
}

pub const SHARD_CAPACITY: i64 = 65_536;
pub const MAX_SHARDS: usize = 256;
/// Newer vectors searched exactly at query time before a shard rebuild is worth its cost.
pub const MAX_DELTA: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shard {
    pub after: i64,
    pub through: i64,
    pub model: String,
    pub dimensions: usize,
    pub token: String,
    pub checksum: String,
    pub count: usize,
    pub bytes: u64,
}

impl Shard {
    pub fn validate(&self) -> Result<()> {
        validate_model(&self.model, self.dimensions)?;
        let hex = |value: &str, size| {
            value.len() == size
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if self.after < 0
            || self.after % SHARD_CAPACITY != 0
            || self.after.checked_add(SHARD_CAPACITY).is_none()
            || self.through <= self.after
            || self.through - self.after > SHARD_CAPACITY
            || !hex(&self.token, 32)
            || !hex(&self.checksum, 64)
            || !(1..=SHARD_CAPACITY as usize).contains(&self.count)
            || !(64..=402_653_184).contains(&self.bytes)
        {
            return Err(Error::Invalid("Invalid vector shard"));
        }
        Ok(())
    }
}

impl ContentStore {
    pub fn vector_catalog(&self, cancel: Arc<AtomicBool>) -> Result<Vec<Shard>> {
        self.vector_read(cancel, || {
            let mut statement = self.connection.prepare(
                "SELECT after_key,through_key,substr(model,1,129),dimensions,substr(token,1,33),
                 substr(checksum,1,65),count,bytes FROM vector_shards ORDER BY after_key LIMIT 257",
            )?;
            let rows = statement.query_map([], |row| {
                Ok(Shard {
                    after: row.get(0)?,
                    through: row.get(1)?,
                    model: row.get(2)?,
                    dimensions: row.get::<_, u32>(3)? as usize,
                    token: row.get(4)?,
                    checksum: row.get(5)?,
                    count: row.get::<_, u32>(6)? as usize,
                    bytes: u64::from(row.get::<_, u32>(7)?),
                })
            })?;
            let mut catalog = Vec::new();
            for row in rows {
                let shard = row?;
                shard.validate()?;
                if catalog.len() == MAX_SHARDS {
                    return Err(Error::Invalid("Vector cache capacity exceeded"));
                }
                catalog.push(shard);
            }
            Ok(catalog)
        })
    }

    pub fn vector_shard_current(&self, shard: &Shard, cancel: Arc<AtomicBool>) -> Result<bool> {
        shard.validate()?;
        self.vector_read(cancel, || shard_current(&self.connection, shard))
    }

    /// The caller must finish and sync the immutable artifact before publishing it.
    pub fn publish_vector_shard(&self, shard: &Shard, cancel: Arc<AtomicBool>) -> Result<bool> {
        shard.validate()?;
        self.vector_read(Arc::clone(&cancel), || {
            let transaction = rusqlite::Transaction::new_unchecked(&self.connection,rusqlite::TransactionBehavior::Immediate)?;
            if !shard_current(&transaction,shard)? { return Ok(false); }
            let count: i64 = transaction.query_row(
                "SELECT count(*) FROM (SELECT after_key FROM vector_shards WHERE after_key<>?1 LIMIT 256)",
                [shard.after], |row|row.get(0))?;
            if count>=MAX_SHARDS as i64 { return Err(Error::Invalid("Vector cache capacity exceeded")); }
            transaction.execute("INSERT INTO vector_shards(after_key,through_key,model,dimensions,token,checksum,count,bytes)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(after_key) DO UPDATE SET
                through_key=excluded.through_key,model=excluded.model,dimensions=excluded.dimensions,
                token=excluded.token,checksum=excluded.checksum,count=excluded.count,bytes=excluded.bytes",
                params![shard.after,shard.through,shard.model,shard.dimensions as i64,shard.token,shard.checksum,shard.count as i64,shard.bytes as i64])?;
            if cancel.load(Ordering::Acquire) { return Err(Error::Cancelled); }
            transaction.commit()?;
            Ok(true)
        })
    }

    pub fn remove_vector_shard(&self, shard: &Shard, cancel: Arc<AtomicBool>) -> Result<()> {
        shard.validate()?;
        self.vector_read(cancel, || {
            self.connection.execute(
                "DELETE FROM vector_shards WHERE after_key=?1 AND token=?2",
                params![shard.after, shard.token],
            )?;
            Ok(())
        })
    }

    pub fn next_vector_range(
        &self,
        after: i64,
        through: i64,
        model: &str,
        dimensions: usize,
        cancel: Arc<AtomicBool>,
    ) -> Result<Option<(i64, i64)>> {
        validate_model(model, dimensions)?;
        if after < 0 || through < after {
            return Err(Error::Invalid("Invalid vector cursor"));
        }
        self.vector_read(cancel, || {
            let next: Option<i64>=self.connection.query_row(
                "SELECT e.id FROM embeddings e JOIN documents d ON d.id=e.document_id
                 WHERE e.id>?1 AND e.id<=?2 AND e.model=?3 AND e.dimensions=?4 AND e.revision=d.revision
                 ORDER BY e.id LIMIT 1",params![after,through,model,dimensions as i64],|row|row.get(0)).optional()?;
            let Some(next)=next else { return Ok(None); };
            if next<=after { return Err(Error::Invalid("Invalid vector identity")); }
            let start=(next-1)/SHARD_CAPACITY*SHARD_CAPACITY;
            let end=start.checked_add(SHARD_CAPACITY).ok_or(Error::Invalid("Vector identity limit exceeded"))?;
            Ok(Some((start,end.min(through))))
        })
    }

    pub fn embedding_count(
        &self,
        after: i64,
        through: i64,
        model: &str,
        dimensions: usize,
        cancel: Arc<AtomicBool>,
    ) -> Result<usize> {
        validate_model(model, dimensions)?;
        if after < 0 || through < after || through - after > 65_536 {
            return Err(Error::Invalid("Invalid vector shard range"));
        }
        self.vector_read(cancel, || {
            let count:i64=self.connection.query_row(
                "SELECT count(*) FROM embeddings e JOIN documents d ON d.id=e.document_id
                 WHERE e.id>?1 AND e.id<=?2 AND e.model=?3 AND e.dimensions=?4 AND e.revision=d.revision",
                params![after,through,model,dimensions as i64], |row|row.get(0))?;
            usize::try_from(count).map_err(|_|Error::Invalid("Invalid vector count"))
        })
    }

    pub fn embedding_watermark(&self) -> Result<i64> {
        let value: Option<i64> = self
            .connection
            .query_row(
                "SELECT seq FROM sqlite_sequence WHERE name='embeddings'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        value
            .filter(|value| *value >= 0)
            .or_else(|| value.is_none().then_some(0))
            .ok_or(Error::Invalid("Invalid embedding sequence"))
    }

    pub fn embedding_vectors(
        &self,
        after: i64,
        through: i64,
        model: &str,
        dimensions: usize,
        cancel: Arc<AtomicBool>,
    ) -> Result<Page> {
        validate_model(model, dimensions)?;
        if after < 0 || through < after {
            return Err(Error::Invalid("Invalid vector cursor"));
        }
        self.vector_read(cancel, || {
            let mut statement = self.connection.prepare(
                "SELECT e.id,CASE WHEN e.model=?3 AND e.dimensions=?4 AND e.revision=d.revision
                 THEN substr(e.vector,1,8193) ELSE NULL END
                 FROM embeddings e JOIN documents d ON d.id=e.document_id
                 WHERE e.id>?1 AND e.id<=?2 ORDER BY e.id LIMIT 64",
            )?;
            let rows = statement
                .query_map(params![after, through, model, dimensions as i64], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, Option<Vec<u8>>>(1)?))
                })?;
            let mut page = Page {
                after,
                ..Page::default()
            };
            for row in rows {
                let (key, bytes) = row?;
                if key <= page.after {
                    return Err(Error::Invalid("Invalid embedding identity"));
                }
                page.after = key;
                page.scanned += 1;
                let Some(bytes) = bytes else {
                    continue;
                };
                if bytes.len() != dimensions * 4 {
                    return Err(Error::Invalid("Invalid stored vector size"));
                }
                let values: Vec<_> = bytes
                    .chunks_exact(4)
                    .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                    .collect();
                let norm = values
                    .iter()
                    .map(|value| f64::from(*value).powi(2))
                    .sum::<f64>();
                if values.iter().any(|value| !value.is_finite()) || (norm - 1.0).abs() > 0.001 {
                    return Err(Error::Invalid("Invalid stored vector norm"));
                }
                page.vectors.push(Vector { key, values });
            }
            Ok(page)
        })
    }

    pub fn resolve_embeddings(
        &self,
        model: &str,
        dimensions: usize,
        candidates: &[(i64, f32)],
        cancel: Arc<AtomicBool>,
    ) -> Result<Vec<Hit>> {
        validate_model(model, dimensions)?;
        if candidates.len() > 100
            || candidates.iter().any(|(key, distance)| {
                *key <= 0 || !distance.is_finite() || !(-0.001..=2.001).contains(distance)
            })
        {
            return Err(Error::Invalid("Invalid vector candidates"));
        }
        self.vector_read(cancel, || {
            let mut statement = self.connection.prepare(
                "SELECT d.id,d.identity,d.path,d.title,d.revision FROM embeddings e JOIN documents d ON d.id=e.document_id
                 WHERE e.id=?1 AND e.model=?2 AND e.dimensions=?3 AND e.revision=d.revision
                 AND length(CAST(d.identity AS BLOB))<=128 AND length(CAST(d.path AS BLOB))<=4096
                 AND length(CAST(d.title AS BLOB))<=1024")?;
            let mut ordered = candidates.to_vec();
            ordered.sort_by(|a,b|a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
            let mut found = Vec::new();
            let mut seen = std::collections::HashSet::with_capacity(ordered.len());
            for (key,distance) in ordered {
                if !seen.insert(key) { continue; }
                let hit = statement.query_row(params![key,model,dimensions as i64], |row|Ok(Hit {
                    id:row.get(0)?,identity:row.get(1)?,path:row.get(2)?,title:row.get(3)?,revision:row.get(4)?,rank:f64::from(distance.clamp(0.0,2.0)),related:true,snippet:None,
                })).optional()?;
                if let Some(hit) = hit
                    && hit.id>0 && hit.revision>0 && !hit.identity.is_empty() && validated_path(Path::new(&hit.path)).is_ok() {
                    found.push(hit);
                }
            }
            Ok(found)
        })
    }

    pub fn vector_shard_reusable(
        &self,
        shard: &Shard,
        last: bool,
        cancel: Arc<AtomicBool>,
    ) -> Result<bool> {
        shard.validate()?;
        self.vector_read(cancel, || shard_reusable(&self.connection, shard, last))
    }

    /// Current vectors newer than every published shard, searched exactly until the next rebuild.
    pub fn vector_delta(
        &self,
        after: i64,
        model: &str,
        dimensions: usize,
        cancel: Arc<AtomicBool>,
    ) -> Result<Vec<Vector>> {
        validate_model(model, dimensions)?;
        if after < 0 {
            return Err(Error::Invalid("Invalid vector cursor"));
        }
        self.vector_read(cancel, || {
            let mut statement = self.connection.prepare(
                "SELECT e.id,substr(e.vector,1,8193) FROM embeddings e JOIN documents d ON d.id=e.document_id
                 WHERE e.id>?1 AND e.model=?2 AND e.dimensions=?3 AND e.revision=d.revision ORDER BY e.id LIMIT ?4")?;
            let rows = statement.query_map(params![after, model, dimensions as i64, MAX_DELTA as i64],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)))?;
            let mut vectors = Vec::new();
            for row in rows {
                let (key, bytes) = row?;
                if key <= after || bytes.len() != dimensions * 4 {
                    return Err(Error::Invalid("Invalid stored vector"));
                }
                let values: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
                if values.iter().any(|value| !value.is_finite()) {
                    return Err(Error::Invalid("Invalid stored vector"));
                }
                vectors.push(Vector { key, values });
            }
            Ok(vectors)
        })
    }

    fn vector_read<T>(
        &self,
        cancel: Arc<AtomicBool>,
        read: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
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
        let result = read();
        self.connection.progress_handler(0, None::<fn() -> bool>)?;
        if cancel.load(Ordering::Acquire) || started.elapsed() > Duration::from_millis(250) {
            return Err(Error::Cancelled);
        }
        result
    }
}

fn validate_model(model: &str, dimensions: usize) -> Result<()> {
    if model.is_empty() || model.len() > 128 || !(1..=2048).contains(&dimensions) {
        return Err(Error::Invalid("Invalid vector model"));
    }
    Ok(())
}

/// A published shard stays in service while nearly all of its vectors are current. Stale
/// candidates are dropped at resolution and a bounded delta of newer vectors is searched exactly,
/// so one edited note no longer forces a full rebuild during which semantic search is empty.
/// Only the last shard may have a delta: newer keys in an earlier range would never be searched.
fn shard_reusable(connection: &rusqlite::Connection, shard: &Shard, last: bool) -> Result<bool> {
    let (inside, beyond): (i64, i64) = connection.query_row(
        "SELECT coalesce(sum(e.id<=?2),0),coalesce(sum(e.id>?2),0) FROM embeddings e JOIN documents d ON d.id=e.document_id
         WHERE e.id>?1 AND e.id<=?3 AND e.model=?4 AND e.dimensions=?5 AND e.revision=d.revision",
        params![shard.after, shard.through, shard.after + SHARD_CAPACITY, shard.model, shard.dimensions as i64],
        |row| Ok((row.get(0)?, row.get(1)?)))?;
    let count = shard.count as i64;
    Ok(inside <= count
        && (count - inside) * 10 <= count
        && (beyond == 0 || (last && beyond <= MAX_DELTA as i64)))
}

fn shard_current(connection: &rusqlite::Connection, shard: &Shard) -> Result<bool> {
    let (count,last): (i64,i64) = connection.query_row(
        "SELECT count(*),coalesce(max(e.id),0) FROM embeddings e JOIN documents d ON d.id=e.document_id
         WHERE e.id>?1 AND e.id<=?2 AND e.model=?3 AND e.dimensions=?4 AND e.revision=d.revision",
        params![shard.after,shard.after+SHARD_CAPACITY,shard.model,shard.dimensions as i64],
        |row|Ok((row.get(0)?,row.get(1)?)))?;
    Ok(count == shard.count as i64 && last <= shard.through)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::{APPLICATION_ID, Document};

    fn store() -> ContentStore {
        let mut store = ContentStore {
            connection: rusqlite::Connection::open_in_memory().unwrap(),
        };
        store.migrate().unwrap();
        store
            .connection
            .execute_batch("PRAGMA foreign_keys=ON")
            .unwrap();
        store
    }
    fn cancel() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }
    fn insert(store: &mut ContentStore, body: &str) -> (i64, i64) {
        let scan = store.begin_scan(Path::new("/fixture")).unwrap();
        store
            .put_batch(
                &scan,
                &[Document {
                    identity: "a",
                    path: Path::new("/fixture/a.txt"),
                    title: "fixture",
                    body,
                    modified_ns: 1,
                    changed_ns: 1,
                    bytes: 1,
                    extraction: crate::content::Extraction::Text,
                }],
            )
            .unwrap()[0]
    }
    fn shard(store: &ContentStore) -> Shard {
        Shard {
            after: 0,
            through: store.embedding_watermark().unwrap(),
            model: "fixture".into(),
            dimensions: 2,
            token: "a".repeat(32),
            checksum: "b".repeat(64),
            count: 1,
            bytes: 1024,
        }
    }

    #[test]
    fn publication_rejects_replaced_keys_and_removal_cannot_delete_a_new_publication() {
        let mut store = store();
        let (id, revision) = insert(&mut store, "original");
        store
            .put_embedding(id, revision, "fixture", &[1.0, 0.0])
            .unwrap();
        let first = shard(&store);
        assert!(store.publish_vector_shard(&first, cancel()).unwrap());
        assert!(store.vector_shard_current(&first, cancel()).unwrap());
        store
            .put_embedding(id, revision, "fixture", &[0.0, 1.0])
            .unwrap();
        assert!(!store.vector_shard_current(&first, cancel()).unwrap());
        assert!(!store.publish_vector_shard(&first, cancel()).unwrap());
        assert_eq!(store.vector_catalog(cancel()).unwrap(), vec![first.clone()]);
        let mut second = shard(&store);
        second.token = "c".repeat(32);
        assert!(store.publish_vector_shard(&second, cancel()).unwrap());
        store.remove_vector_shard(&first, cancel()).unwrap();
        assert_eq!(
            store.vector_catalog(cancel()).unwrap(),
            vec![second.clone()]
        );
        insert(&mut store, "changed");
        assert!(!store.publish_vector_shard(&second, cancel()).unwrap());
        store.remove_vector_shard(&second, cancel()).unwrap();
        assert!(store.vector_catalog(cancel()).unwrap().is_empty());
    }

    #[test]
    fn catalog_rejects_bad_metadata_cancellation_and_failed_publication_is_atomic() {
        let mut store = store();
        let (id, revision) = insert(&mut store, "original");
        store
            .put_embedding(id, revision, "fixture", &[1.0, 0.0])
            .unwrap();
        let first = shard(&store);
        assert!(matches!(
            store.publish_vector_shard(&first, Arc::new(AtomicBool::new(true))),
            Err(Error::Cancelled)
        ));
        assert!(store.vector_catalog(cancel()).unwrap().is_empty());
        assert!(store.publish_vector_shard(&first, cancel()).unwrap());
        let mut bad = first.clone();
        bad.token = "../escape".into();
        assert!(store.publish_vector_shard(&bad, cancel()).is_err());
        bad = first.clone();
        bad.after = i64::MAX;
        bad.through = i64::MAX;
        assert!(bad.validate().is_err());
        store.connection.execute_batch("CREATE TRIGGER refuse_shard BEFORE UPDATE ON vector_shards BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        let mut second = first.clone();
        second.token = "c".repeat(32);
        assert!(store.publish_vector_shard(&second, cancel()).is_err());
        assert_eq!(store.vector_catalog(cancel()).unwrap(), vec![first]);
        store
            .connection
            .execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE vector_shards SET model='';")
            .expect_err("trigger protects fixture");
        store
            .connection
            .execute_batch("DROP TRIGGER refuse_shard; UPDATE vector_shards SET model='';")
            .unwrap();
        assert!(store.vector_catalog(cancel()).is_err());
    }

    #[test]
    fn catalog_survives_reopen_and_erasure_clears_publication_without_reusing_keys() {
        let root = std::env::temp_dir().join(format!("blindspot-catalog-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("content.sqlite");
        let mut store = ContentStore::open(&path).unwrap();
        let (id, revision) = insert(&mut store, "retained");
        store
            .put_embedding(id, revision, "fixture", &[1.0, 0.0])
            .unwrap();
        let published = shard(&store);
        assert!(store.publish_vector_shard(&published, cancel()).unwrap());
        drop(store);
        let reader = ContentStore::open_reader(&path).unwrap();
        assert_eq!(
            reader.vector_catalog(cancel()).unwrap(),
            vec![published.clone()]
        );
        drop(reader);
        let mut store = ContentStore::open(&path).unwrap();
        store.erase(cancel(), |_| {}).unwrap();
        assert!(store.vector_catalog(cancel()).unwrap().is_empty());
        let (id, revision) = insert(&mut store, "replacement");
        store
            .put_embedding(id, revision, "fixture", &[1.0, 0.0])
            .unwrap();
        assert!(!store.publish_vector_shard(&published, cancel()).unwrap());
        assert!(store.embedding_watermark().unwrap() > published.through);
        drop(store);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn catalog_reads_and_publication_are_bounded_when_database_contains_excess_rows() {
        let mut store = store();
        let (id, revision) = insert(&mut store, "fixture");
        store
            .put_embedding(id, revision, "fixture", &[1.0, 0.0])
            .unwrap();
        let published = shard(&store);
        for i in 1..=MAX_SHARDS {
            store
                .connection
                .execute(
                    "INSERT INTO vector_shards VALUES(?1,?2,'fixture',2,?3,?4,1,1024)",
                    params![
                        (i as i64) * SHARD_CAPACITY,
                        (i as i64) * SHARD_CAPACITY + 1,
                        format!("{i:032x}"),
                        "b".repeat(64)
                    ],
                )
                .unwrap();
        }
        assert_eq!(store.vector_catalog(cancel()).unwrap().len(), MAX_SHARDS);
        assert!(store.publish_vector_shard(&published, cancel()).is_err());
        store
            .connection
            .execute(
                "INSERT INTO vector_shards VALUES(0,1,'fixture',2,?1,?2,1,1024)",
                params![published.token, published.checksum],
            )
            .unwrap();
        assert!(store.vector_catalog(cancel()).is_err());
    }

    #[test]
    fn range_discovery_skips_deleted_ranges_and_other_models() {
        let mut store = store();
        let (id, revision) = insert(&mut store, "fixture");
        store
            .put_embedding(id, revision, "other", &[1.0, 0.0])
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE sqlite_sequence SET seq=?1 WHERE name='embeddings'",
                [SHARD_CAPACITY * 300],
            )
            .unwrap();
        store
            .put_embedding(id, revision, "fixture", &[1.0, 0.0])
            .unwrap();
        let high = store.embedding_watermark().unwrap();
        assert_eq!(
            store
                .next_vector_range(0, high, "fixture", 2, cancel())
                .unwrap(),
            Some((SHARD_CAPACITY * 300, high))
        );
        assert_eq!(
            store
                .next_vector_range(high, high, "fixture", 2, cancel())
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .next_vector_range(0, high, "other", 2, cancel())
                .unwrap(),
            None
        );
        assert!(
            store
                .next_vector_range(-1, high, "fixture", 2, cancel())
                .is_err()
        );
    }

    #[test]
    fn version_five_migration_preserves_embeddings_and_rolls_back_on_conflict() {
        let mut store = store();
        let (id, revision) = insert(&mut store, "retained");
        store
            .put_embedding(id, revision, "fixture", &[1.0, 0.0])
            .unwrap();
        store
            .connection
            .execute_batch(
                "DROP TABLE vector_shards; CREATE TABLE vector_shards(private TEXT);
            INSERT INTO vector_shards VALUES('retained'); PRAGMA user_version=5;",
            )
            .unwrap();
        assert!(store.migrate().is_err());
        assert_eq!(
            store
                .connection
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            5
        );
        assert_eq!(
            store
                .connection
                .query_row("SELECT private FROM vector_shards", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "retained"
        );
        store
            .connection
            .execute_batch("DROP TABLE vector_shards")
            .unwrap();
        store.migrate().unwrap();
        assert!(store.vector_catalog(cancel()).unwrap().is_empty());
        assert_eq!(
            store
                .embedding_vectors(0, 1, "fixture", 2, cancel())
                .unwrap()
                .vectors
                .len(),
            1
        );
        assert_eq!(
            store
                .connection
                .query_row("PRAGMA application_id", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            APPLICATION_ID
        );
    }
}
