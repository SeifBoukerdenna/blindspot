//! How well content search finds a known passage: `make bench-search`.
//!
//! `search_bench <database> <queries.toml> [--helpers <dir>] [--limit <n>] [--lexical]`
//!
//! Read-only: it opens the index the way the launcher's reader does and never writes. Each query in
//! the set names the file it should find; the benchmark reports where that file actually landed,
//! for exact search alone and for exact plus semantic fused, so a change to either can be judged
//! against the same numbers.
//!
//! It prints the paths from the query set, which are yours, so keep the output local.

use blindspot_core::{
    content::{ContentStore, SearchFilter, SearchPage, passage_search::{Match, fuse}},
    content_service::passage_engine::Engine,
    semantic::search::Helpers,
};
use serde::Deserialize;
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

#[derive(Deserialize)]
struct Set {
    query: Vec<Query>,
}

#[derive(Deserialize)]
struct Query {
    text: String,
    expect: Vec<String>,
    /// "words" when the query's words are in the file, "meaning" when they are not.
    kind: String,
    page: Option<i64>,
    line: Option<i64>,
}

/// The query set and this plan quote every query verbatim, so a checkout that indexes them would
/// score its own sources instead of the files they point at.
const EXCLUDED: &[&str] = &["bench/search-queries.toml", "docs/semantic-plan.md", "docs/semantic-p2-handoff.md", "docs/search-implementation.md"];

fn scored(path: &str) -> bool {
    !EXCLUDED.iter().any(|excluded| path.ends_with(excluded))
}

/// Where the expected file landed, and how long the search took.
struct Outcome {
    rank: Option<usize>,
    elapsed: Duration,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let database = PathBuf::from(arguments.next().ok_or(
        "usage: search_bench <database> <queries.toml> [--helpers <dir>] [--limit <n>] [--lexical]",
    )?);
    let queries = PathBuf::from(arguments.next().ok_or("expected a queries file")?);
    let (mut helpers, mut limit, mut lexical_only) = (None, 50usize, false);
    let mut check=false;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--helpers" => helpers = arguments.next().map(PathBuf::from),
            "--limit" => {
                limit = arguments
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(limit)
            }
            "--lexical" => lexical_only = true,
            "--check" => check = true,
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let home = std::env::var("HOME")?;
    let set: Set = toml::from_str(&std::fs::read_to_string(&queries)?)?;
    let reader = ContentStore::open_reader(&database)?;
    let cancel = Arc::new(AtomicBool::new(false));
    let roots=vec![PathBuf::from(&home)];
    let checkout=std::env::current_dir()?;
    let exclusions:Vec<_>=EXCLUDED.iter().map(|path|checkout.join(path)).collect();
    let mut engine = (!lexical_only)
        .then_some(helpers.as_ref())
        .flatten()
        .map(|directory| {
            Engine::new(Helpers {
                embedding: directory.join("blindspot-semantic"),
                vectors: directory.join("blindspot-vectors"),
            },"127.0.0.1:11434".into()).with_code_roots(roots.clone())
        });

    println!(
        "{} queries · {} · {}\n",
        set.query.len(),
        database.display(),
        match (&engine, lexical_only) {
            (Some(_), _) => "exact and semantic",
            (None, true) => "exact only (--lexical)",
            (None, false) => "exact only (no --helpers given)",
        }
    );

    let mut exact = Vec::new();
    let mut passages = Vec::new();
    let mut fused = Vec::new();
    let mut misses = Vec::new();
    for query in &set.query {
        let wanted: Vec<String> = query
            .expect
            .iter()
            .map(|path| format!("{home}/{path}"))
            .collect();

        let started = Instant::now();
        let lexical = reader.search_filtered(
            &query.text,
            &SearchFilter::default(),
            limit,
            Arc::clone(&cancel),
        )?;
        let lexical_elapsed = started.elapsed();
        let lexical_rank = rank(&lexical, &wanted);
        exact.push(Outcome {
            rank: lexical_rank,
            elapsed: lexical_elapsed,
        });

        // Passage search: chunk-level, with the all-terms then any-terms fallback.
        let started = Instant::now();
        let found = reader.search_passages(&query.text,&SearchFilter::default(),&roots,&exclusions,Arc::clone(&cancel))?;
        let lexical_passages=fuse(found.clone(),Vec::new(),2);
        passages.push(Outcome {
            rank: passage_rank(&lexical_passages, &wanted,query),
            elapsed: started.elapsed(),
        });

        if let Some(engine) = engine.as_mut() {
            let started = Instant::now();
            let semantic = engine
                .search(&database, &query.text,&SearchFilter::default(),&roots,&exclusions, Arc::clone(&cancel))
                .unwrap_or_default();
            let page = fuse(
                found,
                semantic,2,
            );
            let outcome = Outcome {
                rank: passage_rank(&page, &wanted,query),
                elapsed: started.elapsed(),
            };
            if outcome.rank.is_none_or(|rank| rank > 5) {
                misses.push((query, outcome.rank));
            }
            fused.push(outcome);
        } else if passages.last().is_some_and(|outcome| outcome.rank.is_none_or(|rank| rank > 5)) {
            misses.push((query, passages.last().and_then(|outcome| outcome.rank)));
        }
    }

    report("exact only (documents)", &set, &exact);
    report("passages (chunks)", &set, &passages);
    if !fused.is_empty() {
        report("exact + semantic", &set, &fused);
    }
    if !misses.is_empty() {
        println!("\nNot in the top five:");
        for (query, rank) in &misses {
            let place = rank.map_or("not found".to_owned(), |rank| format!("rank {rank}"));
            println!("  [{}] {:<58} {place}", query.kind, query.text);
            println!("      wanted {}", query.expect.join(" or "));
        }
    }
    if check {
        if fused.len()!=passages.len() { return Err("Quality gate requires semantic results".into()); }
        let score=|outcomes:&[Outcome],kind:Option<&str>| -> (usize,f64) {
            set.query.iter().zip(outcomes).filter(|(query,_)|kind.is_none_or(|kind|query.kind==kind))
                .fold((0,0.0),|(hits,mrr),(_,outcome)| (hits+usize::from(outcome.rank.is_some_and(|rank|rank<=5)),
                    mrr+outcome.rank.filter(|rank|*rank<=10).map_or(0.0,|rank|1.0/rank as f64)))
        };
        for kind in [None,Some("words")] {
            let before=score(&passages,kind); let after=score(&fused,kind);
            if after.0<before.0 || after.1+1e-9<before.1 {return Err("Blended ranking regresses lexical quality".into());}
        }
        if score(&fused,Some("meaning")).0<=score(&passages,Some("meaning")).0 {return Err("Blended ranking does not improve meaning hit@5".into());}
        println!("Quality gate passed");
    }
    Ok(())
}

/// One-based position of the first wanted path among the passages.
fn passage_rank(passages: &[Match], wanted: &[String], query:&Query) -> Option<usize> {
    passages
        .iter()
        .map(|item|&item.passage)
        .filter(|passage| scored(&passage.path))
        .position(|passage| wanted.contains(&passage.path)
            && query.page.is_none_or(|page|page==passage.page)
            && query.line.is_none_or(|line|line>=passage.line && line<passage.line+passage.text.lines().count() as i64))
        .map(|at| at + 1)
}

/// One-based position of the first wanted path in the page.
fn rank(page: &SearchPage, wanted: &[String]) -> Option<usize> {
    page.hits
        .iter()
        .filter(|hit| scored(&hit.path))
        .position(|hit| wanted.contains(&hit.path))
        .map(|at| at + 1)
}

fn report(label: &str, set: &Set, outcomes: &[Outcome]) {
    let kinds = ["words", "meaning"];
    println!("{label}");
    println!(
        "  {:<10} {:>7} {:>7} {:>8} {:>8} {:>9} {:>9}",
        "queries", "hit@1", "hit@5", "hit@10", "MRR@10", "median", "p95"
    );
    for kind in kinds.into_iter().map(Some).chain(std::iter::once(None)) {
        let chosen: Vec<&Outcome> = set
            .query
            .iter()
            .zip(outcomes)
            .filter(|(query, _)| kind.is_none_or(|kind| query.kind == kind))
            .map(|(_, outcome)| outcome)
            .collect();
        if chosen.is_empty() {
            continue;
        }
        let total = chosen.len() as f64;
        let within = |limit: usize| {
            chosen
                .iter()
                .filter(|outcome| outcome.rank.is_some_and(|rank| rank <= limit))
                .count() as f64
                / total
        };
        let mrr: f64 = chosen
            .iter()
            .map(|outcome| {
                outcome
                    .rank
                    .filter(|rank| *rank <= 10)
                    .map_or(0.0, |rank| 1.0 / rank as f64)
            })
            .sum::<f64>()
            / total;
        let mut times: Vec<u128> = chosen
            .iter()
            .map(|outcome| outcome.elapsed.as_millis())
            .collect();
        times.sort_unstable();
        let at = |fraction: f64| times[((times.len() as f64 - 1.0) * fraction).round() as usize];
        println!(
            "  {:<10} {:>6.0}% {:>6.0}% {:>7.0}% {:>8.2} {:>8}ms {:>8}ms",
            kind.unwrap_or("all"),
            within(1) * 100.0,
            within(5) * 100.0,
            within(10) * 100.0,
            mrr,
            at(0.5),
            at(0.95)
        );
    }
    println!();
}
