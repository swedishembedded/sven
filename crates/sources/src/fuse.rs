// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Ranking arithmetic.

use std::collections::BTreeMap;

/// Fuses rankings by reciprocal rank (Cormack, Clarke and Buettcher, 2009): an
/// item scores the sum of `1 / (k + rank)` over the rankings that hold it, rank
/// counted from 1. Best first, ties in key order, so the result is the same
/// every time.
#[must_use]
pub fn reciprocal_rank_fusion<K: Ord + Clone>(rankings: &[Vec<K>], k: f64) -> Vec<(K, f64)> {
    let mut scores: BTreeMap<K, f64> = BTreeMap::new();
    for ranking in rankings {
        for (index, key) in ranking.iter().enumerate() {
            *scores.entry(key.clone()).or_insert(0.0) += 1.0 / (k + (index + 1) as f64);
        }
    }
    let mut fused: Vec<(K, f64)> = scores.into_iter().collect();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1));
    fused
}

/// Okapi BM25 (`k1` 1.2, `b` 0.75) of `query` against each document, over
/// lowercase alphanumeric words. Returns `(document index, score)` best first,
/// ties by index, leaving out every document that holds no query word.
#[must_use]
pub fn bm25_rank<D: AsRef<str>>(documents: &[D], query: &str) -> Vec<(usize, f64)> {
    const K1: f64 = 1.2;
    const B: f64 = 0.75;
    let words = |text: &str| -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let docs: Vec<Vec<String>> = documents.iter().map(|d| words(d.as_ref())).collect();
    let mut query_terms = words(query);
    query_terms.sort();
    query_terms.dedup();
    let n = docs.len() as f64;
    let average = docs.iter().map(Vec::len).sum::<usize>() as f64 / n.max(1.0);
    let mut ranked = Vec::new();
    for (index, doc) in docs.iter().enumerate() {
        let mut score = 0.0;
        for term in &query_terms {
            let tf = doc.iter().filter(|w| *w == term).count() as f64;
            if tf == 0.0 {
                continue;
            }
            let df = docs.iter().filter(|d| d.contains(term)).count() as f64;
            let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
            score += idf * tf * (K1 + 1.0)
                / (tf + K1 * (1.0 - B + B * doc.len() as f64 / average.max(1.0)));
        }
        if score > 0.0 {
            ranked.push((index, score));
        }
    }
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked
}
