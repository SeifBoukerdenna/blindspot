//! Read-only comparison of semantic representations over stored prose documents.
//!
//! Compares whole-document vectors, mean-centred vectors, and passage-level (chunk) vectors
//! using a marker word as a relevance proxy. Opens the database read-only and re-embeds
//! passages in memory only; paths are printed, so keep the output in a local terminal.
use blindspot_core::semantic::{Client, indexing::model_key};
use rusqlite::{Connection, OpenFlags};
use std::{path::PathBuf, sync::atomic::AtomicBool};

const PROSE: &str = "(lower(d.path) GLOB '*.md' OR lower(d.path) GLOB '*.markdown' OR lower(d.path) GLOB '*.txt'
    OR lower(d.path) GLOB '*.rst' OR lower(d.path) GLOB '*.html') AND d.path NOT LIKE '%/go/pkg/mod/%'";

struct Doc {
    path: String,
    marked: bool,
    whole: Vec<f32>,
    chunks: Vec<Vec<f32>>,
}

fn passages(title: &str, body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for piece in body
        .split(['\n', '.', '!', '?'])
        .map(str::trim)
        .filter(|p| p.len() > 2)
    {
        if current.len() + piece.len() > 480 && !current.is_empty() {
            out.push(std::mem::take(&mut current));
            if out.len() == 12 {
                break;
            }
        }
        if !current.is_empty() {
            current.push_str(". ");
        }
        current.push_str(&piece[..piece.floor_char_boundary(1000)]);
    }
    if !current.is_empty() && out.len() < 12 {
        out.push(current);
    }
    if out.is_empty() {
        out.push(title.to_owned());
    }
    out
}

fn unit(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
    v
}
fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
fn mean(vectors: &[&[f32]]) -> Vec<f32> {
    let mut m = vec![0.0; vectors[0].len()];
    for v in vectors {
        for (a, b) in m.iter_mut().zip(v.iter()) {
            *a += b / vectors.len() as f32;
        }
    }
    m
}
fn centred(v: &[f32], m: &[f32]) -> Vec<f32> {
    unit(v.iter().zip(m).map(|(a, b)| a - b).collect())
}

fn report(name: &str, scores: Vec<(f32, &Doc)>) {
    let mut scores = scores;
    scores.sort_by(|a, b| b.0.total_cmp(&a.0));
    let first = scores.iter().position(|s| s.1.marked);
    let top10 = scores.iter().take(10).filter(|s| s.1.marked).count();
    let top3: Vec<_> = scores
        .iter()
        .take(3)
        .map(|s| {
            format!(
                "{:.2}{}{}",
                s.0,
                if s.1.marked { "*" } else { " " },
                s.1.path.rsplit('/').next().unwrap_or("")
            )
        })
        .collect();
    println!("  {name:<16} first={first:?} top10={top10} {top3:?}");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let database = PathBuf::from(
        arguments
            .next()
            .ok_or("Expected database, helper, marker word, queries")?,
    );
    let helper = PathBuf::from(arguments.next().ok_or("Expected semantic helper path")?);
    let marker = arguments
        .next()
        .ok_or("Expected a marker word")?
        .to_lowercase();
    let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut client = Client::new(helper);
    let cancel = AtomicBool::new(false);
    let model = client.probe(&cancel).map_err(|e| format!("{e:?}"))?;
    let key = model_key(&model).map_err(|e| format!("{e:?}"))?;
    let mut statement = connection.prepare(&format!(
        "SELECT d.path, d.title, substr(d.body,1,16000), e.vector, instr(lower(d.title||d.body), ?2)>0
         FROM embeddings e JOIN documents d ON d.id=e.document_id WHERE e.model=?1 AND e.revision=d.revision AND {PROSE}"))?;
    let rows: Vec<(String, String, String, Vec<u8>, bool)> = statement
        .query_map(rusqlite::params![key, marker], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<Result<_, _>>()?;
    let started = std::time::Instant::now();
    let mut docs = Vec::new();
    let mut total = 0;
    for (path, title, body, vector, marked) in rows {
        let texts = passages(&title, &body);
        let mut chunks = Vec::new();
        for batch in texts.chunks(8) {
            match client.embed(batch, &cancel) {
                Ok(result) => chunks.extend(result.vectors),
                Err(_) => {
                    for text in batch {
                        if let Ok(r) = client.embed(std::slice::from_ref(text), &cancel) {
                            chunks.extend(r.vectors);
                        }
                    }
                }
            }
        }
        total += chunks.len();
        if chunks.is_empty() {
            continue;
        }
        let whole = vector
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        docs.push(Doc {
            path,
            marked,
            whole,
            chunks,
        });
    }
    println!(
        "{} docs, {} passages embedded in {:?}, {} marked",
        docs.len(),
        total,
        started.elapsed(),
        docs.iter().filter(|d| d.marked).count()
    );
    let whole_mean = mean(&docs.iter().map(|d| d.whole.as_slice()).collect::<Vec<_>>());
    let chunk_refs: Vec<&[f32]> = docs
        .iter()
        .flat_map(|d| d.chunks.iter().map(Vec::as_slice))
        .collect();
    let chunk_mean = mean(&chunk_refs);
    for query in arguments {
        let q = client
            .embed(std::slice::from_ref(&query), &cancel)
            .map_err(|e| format!("{e:?}"))?
            .vectors
            .remove(0);
        println!("== {query}");
        report(
            "whole",
            docs.iter().map(|d| (dot(&d.whole, &q), d)).collect(),
        );
        let qc = centred(&q, &whole_mean);
        report(
            "whole-centred",
            docs.iter()
                .map(|d| (dot(&centred(&d.whole, &whole_mean), &qc), d))
                .collect(),
        );
        report(
            "chunk-max",
            docs.iter()
                .map(|d| {
                    (
                        d.chunks.iter().map(|c| dot(c, &q)).fold(f32::MIN, f32::max),
                        d,
                    )
                })
                .collect(),
        );
        let qk = centred(&q, &chunk_mean);
        report(
            "chunk-max-centred",
            docs.iter()
                .map(|d| {
                    (
                        d.chunks
                            .iter()
                            .map(|c| dot(&centred(c, &chunk_mean), &qk))
                            .fold(f32::MIN, f32::max),
                        d,
                    )
                })
                .collect(),
        );
        report(
            "chunk-meanvec",
            docs.iter()
                .map(|d| {
                    (
                        dot(
                            &unit(mean(
                                &d.chunks.iter().map(Vec::as_slice).collect::<Vec<_>>(),
                            )),
                            &q,
                        ),
                        d,
                    )
                })
                .collect(),
        );
    }
    Ok(())
}
