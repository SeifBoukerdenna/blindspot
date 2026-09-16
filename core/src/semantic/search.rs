//! Local semantic retrieval and deterministic fusion, independent of lexical search lifetime.

use super::{Client, Failure, cache, indexing::model_key, vectors};
use crate::content::{ContentStore, Hit, SearchPage};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const CANDIDATES: usize = 100;
/// Semantic-only documents shown after every exact match. The contextual model's cosine scores
/// are too flat for an absolute cutoff (unrelated prose sits at 0.75–0.80), so a rank cap bounds
/// how much approximate material a query can add.
const RELATED_LIMIT: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Helpers {
    pub embedding: PathBuf,
    pub vectors: PathBuf,
}

pub struct Engine {
    helpers: Helpers,
    embedding: Client,
    vectors: Option<(PathBuf, vectors::Client)>,
}
impl Engine {
    pub fn new(helpers: Helpers) -> Self {
        Self {
            embedding: Client::new(helpers.embedding.clone()),
            helpers,
            vectors: None,
        }
    }

    pub fn close(&mut self) {
        self.embedding.close();
        self.vectors = None;
    }

    pub fn search(
        &mut self,
        database: &Path,
        query: &str,
        cancel: Arc<AtomicBool>,
    ) -> Result<Vec<Hit>, Failure> {
        let result = (|| {
            if cancel.load(Ordering::Acquire) {
                return Err(Failure::Cancelled);
            }
            let reader = ContentStore::open_reader(database).map_err(storage)?;
            let catalog = reader
                .vector_catalog(Arc::clone(&cancel))
                .map_err(storage)?;
            // No stored vectors at all: do not start the model helper for nothing.
            if catalog.is_empty() && reader.embedding_watermark().map_err(storage)? == 0 {
                return Ok(Vec::new());
            }
            let batch = self.embedding.embed(&[query.into()], &cancel)?;
            let key = model_key(&batch.model)?;
            let dimensions = batch.model.dimensions;
            let vector = batch.vectors.first().ok_or(Failure::InvalidResponse)?;
            let shards: Vec<_> = catalog
                .into_iter()
                .filter(|shard| shard.model == key && shard.dimensions == dimensions)
                .collect();
            let covered = shards.iter().map(|shard| shard.through).max().unwrap_or(0);
            let mut candidates = Vec::new();
            let mut unavailable = false;
            if !shards.is_empty() {
                let artifacts: Vec<_> = shards
                    .iter()
                    .map(|shard| vectors::Artifact {
                        token: shard.token.clone(),
                        count: shard.count,
                        bytes: shard.bytes,
                        checksum: shard.checksum.clone(),
                    })
                    .collect();
                let directory = cache::prepare(database).map_err(storage)?;
                if self
                    .vectors
                    .as_ref()
                    .is_none_or(|(path, _)| path != &directory)
                {
                    self.vectors = Some((
                        directory.clone(),
                        vectors::Client::new(self.helpers.vectors.clone(), directory),
                    ));
                }
                let (_, client) = self.vectors.as_mut().ok_or(Failure::Unavailable)?;
                let found = client.search(vector, &artifacts, CANDIDATES, &cancel)?;
                unavailable = found.unavailable == artifacts.len();
                candidates = found.candidates;
            }
            // Vectors written after the newest shard are searched exactly, so a new or edited note is
            // findable by meaning as soon as it is embedded rather than after the next cache rebuild.
            let delta = reader
                .vector_delta(covered, &key, dimensions, Arc::clone(&cancel))
                .map_err(storage)?;
            if unavailable && delta.is_empty() {
                return Err(Failure::Unavailable);
            }
            for stored in delta {
                let similarity: f32 = stored.values.iter().zip(vector).map(|(a, b)| a * b).sum();
                candidates.push((stored.key, (1.0 - similarity).clamp(0.0, 2.0)));
            }
            candidates.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
            candidates.truncate(CANDIDATES);
            if candidates.is_empty() {
                return Ok(Vec::new());
            }
            reader
                .resolve_embeddings(&key, dimensions, &candidates, cancel)
                .map_err(storage)
        })();
        if result.is_err() {
            self.close();
        }
        result
    }
}

fn storage(error: crate::content::Error) -> Failure {
    match error {
        crate::content::Error::Cancelled => Failure::Cancelled,
        _ => Failure::Unavailable,
    }
}

/// Exact-term matches always come first; semantic agreement only reorders them (reciprocal-rank
/// fusion with lexical weighted double), then at most [`RELATED_LIMIT`] documents that matched by
/// meaning alone are appended and flagged. Interleaving them measured worse: approximate neighbours
/// displaced files that contain the user's words.
pub fn fuse(lexical: SearchPage, semantic: Vec<Hit>) -> SearchPage {
    const RANK_OFFSET: f64 = 60.0;
    let mut semantic_rank = HashMap::<String, usize>::new();
    for (rank, hit) in semantic.iter().enumerate() {
        semantic_rank.entry(hit.path.clone()).or_insert(rank);
    }
    let mut shown = HashSet::new();
    let mut ranked: Vec<(Hit, f64)> = lexical
        .hits
        .into_iter()
        .take(CANDIDATES)
        .enumerate()
        .filter_map(|(rank, hit)| {
            if !shown.insert(hit.path.clone()) {
                return None;
            }
            let agreement = semantic_rank
                .get(&hit.path)
                .map_or(0.0, |rank| 1.0 / (RANK_OFFSET + *rank as f64 + 1.0));
            Some((hit, 2.0 / (RANK_OFFSET + rank as f64 + 1.0) + agreement))
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.path.cmp(&b.0.path)));
    let related = semantic
        .into_iter()
        .filter(|hit| shown.insert(hit.path.clone()))
        .take(RELATED_LIMIT)
        .map(|mut hit| {
            hit.related = true;
            hit
        });
    let hits = ranked
        .into_iter()
        .map(|(mut hit, score)| {
            hit.rank = -score;
            hit.related = false;
            hit
        })
        .chain(related)
        .take(CANDIDATES)
        .collect();
    SearchPage {
        hits,
        limited: lexical.limited,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hit(id: i64) -> Hit {
        Hit {
            id,
            identity: id.to_string(),
            path: format!("/fixture/{id}.txt"),
            title: id.to_string(),
            rank: 0.0,
            revision: 1,
            related: false,
            snippet: None,
        }
    }
    fn ids(page: &SearchPage) -> Vec<i64> {
        page.hits.iter().map(|hit| hit.id).collect()
    }

    #[test]
    fn fusion_rewards_agreement_preserves_lexical_priority_and_deduplicates() {
        let lexical = SearchPage {
            hits: vec![hit(1), hit(2), hit(3)],
            limited: true,
        };
        let result = fuse(lexical, vec![hit(2), hit(4), hit(4)]);
        assert_eq!(ids(&result), [2, 1, 3, 4]);
        assert!(result.limited);
        assert_eq!(
            result
                .hits
                .iter()
                .map(|hit| hit.related)
                .collect::<Vec<_>>(),
            [false, false, false, true]
        );
        assert_eq!(
            ids(&fuse(SearchPage::default(), vec![hit(4), hit(2)])),
            [4, 2]
        );
        assert_eq!(
            ids(&fuse(
                SearchPage {
                    hits: vec![hit(1), hit(2)],
                    limited: false
                },
                Vec::new()
            )),
            [1, 2]
        );
    }

    #[test]
    fn semantic_only_documents_never_outrank_exact_matches_and_are_capped() {
        let lexical = SearchPage {
            hits: (1..=30).map(hit).collect(),
            limited: false,
        };
        let semantic: Vec<_> = (100..140).map(hit).collect();
        let result = fuse(lexical, semantic);
        assert_eq!(&ids(&result)[..30], (1..=30).collect::<Vec<_>>().as_slice());
        assert_eq!(result.hits.len(), 30 + RELATED_LIMIT);
        assert!(result.hits[30..].iter().all(|hit| hit.related));
    }
}
