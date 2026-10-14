// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in tool retrieval for
// large agent tool sets then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Shortlist recall, top-1 and mean reciprocal rank per query kind.

use std::fmt;

use sven_tool_registry::ToolDescriptor;

use super::corpus::{Catalog, QueryKind};

/// Ranks all the queries of one catalog at once, so that the catalog is indexed
/// once: for each query, names best first.
pub type RankFn<'a> = dyn Fn(&[ToolDescriptor], &[&str]) -> Vec<Vec<String>> + 'a;

#[derive(Clone, Copy, Debug, Default)]
pub struct KindScores {
    pub queries: usize,
    /// Share of queries whose tool is within the first `k` results.
    pub recall: f64,
    /// Share whose tool is first.
    pub top1: f64,
    /// Mean of 1 / rank of the tool; 0 when it is not ranked at all.
    pub mrr: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Scores {
    k: usize,
    kinds: [KindScores; 3],
}

impl Scores {
    pub fn of(&self, kind: QueryKind) -> KindScores {
        self.kinds[kind as usize]
    }
}

impl fmt::Display for Scores {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "| kind | queries | recall@{} | top-1 | MRR |", self.k)?;
        writeln!(f, "|---|---:|---:|---:|---:|")?;
        for (kind, scores) in [QueryKind::Exact, QueryKind::Paraphrase, QueryKind::Indirect]
            .into_iter()
            .zip(&self.kinds)
        {
            writeln!(
                f,
                "| {kind:?} | {} | {:.1}% | {:.1}% | {:.3} |",
                scores.queries,
                scores.recall * 100.0,
                scores.top1 * 100.0,
                scores.mrr
            )?;
        }
        Ok(())
    }
}

pub fn evaluate(rank: &RankFn<'_>, catalogs: &[Catalog], k: usize) -> Scores {
    let mut sums = [(0usize, 0usize, 0usize, 0.0f64); 3];
    for catalog in catalogs {
        let tools = catalog.descriptors();
        let texts: Vec<&str> = catalog.queries.iter().map(|q| q.text.as_str()).collect();
        let answers = rank(&tools, &texts);
        for (query, ranked) in catalog.queries.iter().zip(answers) {
            let at = ranked.iter().position(|n| *n == query.target);
            let sum = &mut sums[query.kind as usize];
            sum.0 += 1;
            sum.1 += usize::from(at.is_some_and(|i| i < k));
            sum.2 += usize::from(at == Some(0));
            sum.3 += at.map_or(0.0, |i| 1.0 / (i + 1) as f64);
        }
    }
    let mut scores = Scores {
        k,
        ..Scores::default()
    };
    for (slot, (n, hit, first, reciprocal)) in scores.kinds.iter_mut().zip(sums) {
        let n_f = n.max(1) as f64;
        *slot = KindScores {
            queries: n,
            recall: hit as f64 / n_f,
            top1: first as f64 / n_f,
            mrr: reciprocal / n_f,
        };
    }
    scores
}
