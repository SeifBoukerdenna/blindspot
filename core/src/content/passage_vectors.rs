use super::{*, vectors::{Page, Vector, Shard, SHARD_CAPACITY, MAX_SHARDS}};
use super::passage_search::{Match, read_match, scope_sql};
use std::path::PathBuf;

impl ContentStore {
    pub fn passage_models(&self) -> Result<Vec<(String, usize)>> {
        let mut statement = self.connection.prepare("SELECT model,dimensions FROM passage_models WHERE complete=1 ORDER BY active DESC,model LIMIT 8")?;
        let rows = statement.query_map([], |row| Ok((row.get::<_,String>(0)?,row.get::<_,u32>(1)? as usize)))?;
        let mut models = Vec::new();
        for row in rows { let (model,dimensions) = row?; validate(&model, dimensions)?; models.push((model,dimensions)); }
        Ok(models)
    }

    pub fn begin_passage_model(&self, model: &str, dimensions: usize) -> Result<()> {
        validate(model,dimensions)?;
        let count:i64=self.connection.query_row("SELECT count(*) FROM passage_models WHERE model<>?1",[model],|row|row.get(0))?;
        if count>=8 {return Err(Error::Invalid("Eight model generations retained; Compact before changing models"));}
        self.connection.execute("INSERT INTO passage_models(model,dimensions) VALUES(?1,?2) ON CONFLICT(model) DO NOTHING",params![model,dimensions as i64])?;
        Ok(())
    }

    pub fn retire_passage_models(&mut self,cancel:&AtomicBool)->Result<u64> {
        if cancel.load(Ordering::Acquire) {return Err(Error::Cancelled);}
        let transaction=self.connection.transaction()?;
        let retired=transaction.execute("DELETE FROM chunk_embeddings WHERE model IN(
            SELECT model FROM passage_models WHERE active=0 AND complete=1 AND model NOT GLOB '[[]\"apple-contextual-en\",*')",[])?;
        transaction.execute("DELETE FROM passage_shards WHERE model IN(
            SELECT model FROM passage_models WHERE active=0 AND complete=1 AND model NOT GLOB '[[]\"apple-contextual-en\",*')",[])?;
        transaction.execute("DELETE FROM passage_models WHERE active=0 AND complete=1 AND model NOT GLOB '[[]\"apple-contextual-en\",*'",[])?;
        if cancel.load(Ordering::Acquire) {return Err(Error::Cancelled);}
        transaction.commit()?;
        Ok(retired as u64)
    }

    pub fn activate_passage_model(&mut self, model: &str, cancel: &AtomicBool) -> Result<()> {
        if cancel.load(Ordering::Acquire) { return Err(Error::Cancelled); }
        let transaction = self.connection.transaction()?;
        transaction.execute("UPDATE passage_models SET active=0 WHERE active=1",[])?;
        if transaction.execute("UPDATE passage_models SET active=1,complete=1 WHERE model=?1",[model])? != 1 {
            return Err(Error::Invalid("Unknown passage model"));
        }
        if cancel.load(Ordering::Acquire) { return Err(Error::Cancelled); }
        transaction.commit()?;
        Ok(())
    }

    pub fn passage_catalog(&self, model: Option<&str>, cancel: Arc<AtomicBool>) -> Result<Vec<Shard>> {
        self.passage_read(cancel, || {
            let mut statement = self.connection.prepare("SELECT after_key,through_key,model,dimensions,token,checksum,count,bytes
                FROM passage_shards WHERE ?1 IS NULL OR model=?1 ORDER BY model,after_key LIMIT 257")?;
            let rows = statement.query_map([model], |row| Ok(Shard { after: row.get(0)?, through: row.get(1)?, model: row.get(2)?,
                dimensions: row.get::<_,u32>(3)? as usize, token: row.get(4)?, checksum: row.get(5)?, count: row.get::<_,u32>(6)? as usize, bytes: u64::from(row.get::<_,u32>(7)?) }))?;
            let mut found = Vec::new();
            for row in rows { let shard = row?; shard.validate()?; found.push(shard); }
            if found.len() > MAX_SHARDS { return Err(Error::Invalid("Passage shard capacity exceeded")); }
            Ok(found)
        })
    }

    pub fn passage_vector_ranges(&self, model: &str, cancel: Arc<AtomicBool>) -> Result<Vec<(i64,i64,usize)>> {
        self.passage_read(cancel, || {
            let mut statement = self.connection.prepare("SELECT ((id-1)/65536)*65536,max(id),count(*) FROM chunk_embeddings
                WHERE model=?1 GROUP BY ((id-1)/65536) ORDER BY min(id) LIMIT 257")?;
            let ranges = statement.query_map([model], |row| Ok((row.get(0)?,row.get(1)?,row.get::<_,u32>(2)? as usize)))?
                .collect::<std::result::Result<Vec<_>,_>>()?;
            if ranges.len()>MAX_SHARDS { return Err(Error::Invalid("Passage shard capacity exceeded")); }
            Ok(ranges)
        })
    }

    pub fn passage_vector_page(&self, after: i64, through: i64, model: &str, dimensions: usize, cancel: Arc<AtomicBool>) -> Result<Page> {
        validate(model,dimensions)?;
        if after < 0 || through < after { return Err(Error::Invalid("Invalid passage vector range")); }
        self.passage_read(cancel, || {
            let mut statement = self.connection.prepare("SELECT id,substr(vector,1,8193),scale FROM chunk_embeddings
                WHERE id>?1 AND id<=?2 AND model=?3 AND dimensions=?4 ORDER BY id LIMIT 64")?;
            let rows = statement.query_map(params![after,through,model,dimensions as i64], |row| Ok((row.get::<_,i64>(0)?,row.get::<_,Vec<u8>>(1)?,row.get::<_,f32>(2)?)))?;
            let mut page = Page { after, ..Page::default() };
            for row in rows {
                let (key,bytes,scale) = row?;
                let values:Vec<f32>=if bytes.len()==dimensions && scale>0.0 {
                    retrieval::dequantize(&bytes,scale).ok_or(Error::Invalid("Invalid compressed passage vector"))?
                } else if bytes.len()==dimensions*4 && scale==0.0 {
                    bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0],b[1],b[2],b[3]])).collect()
                } else {return Err(Error::Invalid("Invalid passage vector size"));};
                let norm = values.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
                if values.iter().any(|v| !v.is_finite()) || (norm-1.0).abs()>0.001 { return Err(Error::Invalid("Invalid passage vector norm")); }
                page.after=key; page.scanned+=1; page.vectors.push(Vector { key, values });
            }
            Ok(page)
        })
    }

    pub fn scoped_passage_vectors(&self,model:&str,dimensions:usize,filter:&SearchFilter,roots:&[PathBuf],exclusions:&[PathBuf],cancel:Arc<AtomicBool>)->Result<Option<Vec<Vector>>> {
        validate(model,dimensions)?;
        let scope=scope_sql(roots,exclusions)?;
        self.passage_read(cancel,||{
            let mut statement=self.connection.prepare(&format!("SELECT e.id,substr(e.vector,1,8193),e.scale
                FROM chunk_embeddings e JOIN chunks c ON c.id=e.chunk_id JOIN documents d ON d.id=c.document_id
                WHERE e.model=?1 AND e.dimensions=?2 {} {scope} ORDER BY e.id LIMIT 2049",filter.sql()))?;
            let rows=statement.query_map(params![model,dimensions as i64],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,Vec<u8>>(1)?,row.get::<_,f32>(2)?)))?;
            let mut found=Vec::new();
            for row in rows {
                let (key,bytes,scale)=row?;
                if found.len()==2048 {return Ok(None);}
                let values=if bytes.len()==dimensions && scale>0.0 {
                    retrieval::dequantize(&bytes,scale).ok_or(Error::Invalid("Invalid compressed passage vector"))?
                } else if bytes.len()==dimensions*4 && scale==0.0 {
                    bytes.chunks_exact(4).map(|b|f32::from_le_bytes([b[0],b[1],b[2],b[3]])).collect()
                } else {return Err(Error::Invalid("Invalid passage vector"));};
                if values.iter().any(|value|!value.is_finite()) {return Err(Error::Invalid("Invalid passage vector"));}
                found.push(Vector {key,values});
            }
            Ok(Some(found))
        })
    }

    pub fn remove_empty_passage_shards(&self,model:&str)->Result<()> {
        self.connection.execute("DELETE FROM passage_shards WHERE model=?1 AND NOT EXISTS(
            SELECT 1 FROM chunk_embeddings e WHERE e.model=passage_shards.model AND e.id>after_key AND e.id<=after_key+65536)",[model])?;
        Ok(())
    }

    pub fn publish_passage_shard(&self, shard: &Shard, cancel: Arc<AtomicBool>) -> Result<bool> {
        shard.validate()?;
        self.passage_read(Arc::clone(&cancel), || {
            let transaction = rusqlite::Transaction::new_unchecked(&self.connection,rusqlite::TransactionBehavior::Immediate)?;
            let (count,high): (i64,i64) = transaction.query_row("SELECT count(*),coalesce(max(id),0) FROM chunk_embeddings
                WHERE id>?1 AND id<=?2 AND model=?3 AND dimensions=?4",params![shard.after,shard.after+SHARD_CAPACITY,shard.model,shard.dimensions as i64], |row| Ok((row.get(0)?,row.get(1)?)))?;
            if count!=shard.count as i64 || high!=shard.through { return Ok(false); }
            transaction.execute("INSERT INTO passage_shards(model,after_key,through_key,dimensions,token,checksum,count,bytes)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(model,after_key) DO UPDATE SET
                through_key=excluded.through_key,dimensions=excluded.dimensions,token=excluded.token,checksum=excluded.checksum,count=excluded.count,bytes=excluded.bytes",
                params![shard.model,shard.after,shard.through,shard.dimensions as i64,shard.token,shard.checksum,shard.count as i64,shard.bytes as i64])?;
            if cancel.load(Ordering::Acquire) { return Err(Error::Cancelled); }
            transaction.commit()?;
            Ok(true)
        })
    }

    pub fn resolve_passage_vectors(&self, model: &str, candidates: &[(i64,f32)], filter: &SearchFilter,
        roots: &[PathBuf], exclusions: &[PathBuf], cancel: Arc<AtomicBool>) -> Result<Vec<Match>> {
        if candidates.len()>100 || candidates.iter().any(|(key,distance)| *key<=0 || !distance.is_finite()) { return Err(Error::Invalid("Invalid passage candidates")); }
        let scope = scope_sql(roots,exclusions)?;
        self.passage_read(cancel, || {
            let mut statement = self.connection.prepare(&format!("SELECT c.id,d.revision,c.document_id,d.path,d.title,c.ordinal,c.page,c.line,c.heading,c.text,0.0
                FROM chunk_embeddings e JOIN chunks c ON c.id=e.chunk_id JOIN documents d ON d.id=c.document_id
                WHERE e.id=?1 AND e.model=?2 {} {scope}",filter.sql()))?;
            let mut found = Vec::new();
            for (key,distance) in candidates {
                if let Some(mut item) = statement.query_row(params![key,model],read_match).optional()?
                    && validated_path(Path::new(&item.passage.path)).is_ok() {
                    item.passage.rank=f64::from(*distance); item.words=false; item.meaning=true; found.push(item);
                }
            }
            Ok(found)
        })
    }
}

fn validate(model: &str, dimensions: usize) -> Result<()> {
    if model.is_empty() || model.len()>128 || !(1..=2048).contains(&dimensions) { return Err(Error::Invalid("Invalid passage model")); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_switch_preserves_old_vectors_until_publication_and_rewrite_retires_both() {
        let mut store = ContentStore { connection: Connection::open_in_memory().unwrap() };
        store.migrate().unwrap(); store.connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        let scan=store.begin_scan(Path::new("/fixture")).unwrap();
        let doc=Document { identity:"a",path:Path::new("/fixture/a.md"),title:"a",body:"passage",modified_ns:1,changed_ns:1,bytes:7,extraction:Extraction::Text };
        store.put_batch(&scan,&[doc]).unwrap();
        let cancel=Arc::new(AtomicBool::new(false));
        let chunk=store.chunk_embedding_page(0,"old",2,Arc::clone(&cancel)).unwrap().pending[0].id;
        for model in ["old","new"] { store.begin_passage_model(model,2).unwrap(); store.put_chunk_embedding(chunk,model,&[1.0,0.0]).unwrap(); }
        store.activate_passage_model("old",&cancel).unwrap();
        assert_eq!(store.passage_vector_page(0,i64::MAX,"old",2,Arc::clone(&cancel)).unwrap().vectors[0].values,[1.0,0.0]);
        assert_eq!(store.passage_models().unwrap(),[("old".into(),2)]);
        assert!(store.chunk_embedding_page(0,"old",2,Arc::clone(&cancel)).unwrap().pending.is_empty());
        store.activate_passage_model("new",&cancel).unwrap();
        assert_eq!(store.passage_models().unwrap()[0].0,"new");
        assert_eq!(store.retire_passage_models(&cancel).unwrap(),1);
        assert_eq!(store.passage_models().unwrap(),[("new".into(),2)]);
        store.connection.execute("DELETE FROM documents",[]).unwrap();
        assert!(store.passage_vector_ranges("old",Arc::clone(&cancel)).unwrap().is_empty());
        assert!(store.passage_vector_ranges("new",cancel).unwrap().is_empty());
    }
}
