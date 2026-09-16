use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, PartialEq)]
pub struct Ranked {
    pub id: i64,
    pub score: f64,
    pub words: bool,
    pub meaning: bool,
}

pub fn fuse_ranks(lexical: &[i64], semantic: &[i64]) -> Vec<Ranked> {
    let mut merged = BTreeMap::<i64, Ranked>::new();
    for (ids, weight, words) in [(lexical, 2.0, true), (semantic, 1.0, false)] {
        let mut seen = BTreeSet::new();
        for (rank, &id) in ids.iter().take(100).enumerate() {
            if !seen.insert(id) { continue; }
            let row = merged.entry(id).or_insert(Ranked { id, score: 0.0, words: false, meaning: false });
            row.score += weight / (61.0 + rank as f64);
            row.words |= words;
            row.meaning |= !words;
        }
    }
    let mut ranked: Vec<_> = merged.into_values().collect();
    ranked.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.id.cmp(&b.id)));
    ranked
}

pub fn substantially_overlaps(left: &str, right: &str) -> bool {
    if left == right { return true; }
    let (short, long) = if left.len() < right.len() { (left, right) } else { (right, left) };
    if short.is_empty() { return false; }
    if long.contains(short) { return true; }
    let minimum = short.len() * 4 / 5;
    for (at, _) in short.char_indices().take_while(|(at, _)| *at <= short.len() - minimum) {
        if long.starts_with(&short[at..]) { return true; }
    }
    for (at, _) in long.char_indices().filter(|(at, _)| long.len() - *at >= minimum) {
        if short.starts_with(&long[at..]) { return true; }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fusion_is_stable_deduplicated_and_explained() {
        let rows = fuse_ranks(&[8, 2, 2], &[2, 9]);
        assert_eq!(rows.iter().map(|row|row.id).collect::<Vec<_>>(), [2, 8, 9]);
        assert!(rows[0].words && rows[0].meaning);
        assert_eq!(rows, fuse_ranks(&[8, 2], &[2, 9]));
        assert!(substantially_overlaps("hello world", "hello world extra"));
        assert!(!substantially_overlaps("renewal deadline October", "support deadline November"));
    }
}
