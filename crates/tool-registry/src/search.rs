// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in tool retrieval for
// large agent tool sets then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Ranking tools against a query.
//!
//! [`ToolRanker`] is the seam: it turns a query and the candidate
//! descriptors into an ordered list. [`Bm25Ranker`], the default, is a pure
//! function of its inputs, so a given query over a given set of tools answers
//! the same way in every run, on every machine, whatever order the tools were
//! registered in. Nothing here calls a model. A ranker backed by a model can
//! stand in for it, or re-order its shortlist, without the discovery tool
//! changing, which is why [`ToolRanker::rank`] is async.

use async_trait::async_trait;

use crate::analysis::{named_spans, terms};
use crate::descriptor::ToolDescriptor;
use crate::expansion::{align_ending, widen, Expansion};
use crate::index::{FieldWeights, Index};

/// A candidate with the score a ranker gave it.
#[derive(Clone, Debug, PartialEq)]
pub struct RankedTool {
    /// The candidate's name.
    pub name: String,
    /// Higher is a better match. Comparable only within one ranking.
    pub score: f64,
}

/// Orders candidate tools by how well they answer a query.
#[async_trait]
pub trait ToolRanker: Send + Sync {
    /// The candidates that match `query`, best first. A candidate that does
    /// not match at all is left out. Candidates of equal score are ordered by
    /// name, so the result never depends on the order of `candidates`.
    async fn rank(&self, query: &str, candidates: &[ToolDescriptor]) -> Vec<RankedTool>;
}

/// Okapi BM25 over the weighted terms of each tool's name, summary, category,
/// description and parameters.
///
/// A name term counts three times a summary term: the name is the most
/// deliberate word anybody chose for the tool. See [`RankingParams`] for the
/// rest.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bm25Ranker;

/// Everything that shapes a ranking. The defaults are what search uses;
/// the fields are public so that a measurement can vary one at a time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RankingParams {
    /// BM25 term-frequency saturation.
    pub k1: f64,
    /// BM25 length normalisation.
    pub b: f64,
    pub weights: FieldWeights,
    /// A query term the tools never use is replaced by the same word with an
    /// `e` added or removed, if the tools use that.
    pub align_endings: bool,
    pub expansion: Expansion,
}

impl Default for RankingParams {
    fn default() -> Self {
        Self {
            k1: 5.0,
            b: 0.9,
            weights: FieldWeights::default(),
            align_endings: true,
            expansion: Expansion::default(),
        }
    }
}

#[async_trait]
impl ToolRanker for Bm25Ranker {
    async fn rank(&self, query: &str, candidates: &[ToolDescriptor]) -> Vec<RankedTool> {
        self.rank_now(query, candidates)
    }
}

impl Bm25Ranker {
    /// [`ToolRanker::rank`] without the `await`, for a caller that has no use
    /// for a model: the nearest name to a misspelling is always BM25.
    #[must_use]
    pub fn rank_now(&self, query: &str, candidates: &[ToolDescriptor]) -> Vec<RankedTool> {
        self.rank_with(&RankingParams::default(), query, candidates)
    }

    /// Ranks with `params` instead of the defaults.
    #[must_use]
    pub fn rank_with(
        &self,
        params: &RankingParams,
        query: &str,
        candidates: &[ToolDescriptor],
    ) -> Vec<RankedTool> {
        self.prepare(params, candidates).rank(query)
    }

    /// Indexes `candidates` once, for many queries over the same tools.
    #[must_use]
    pub fn prepare<'a>(
        &self,
        params: &RankingParams,
        candidates: &'a [ToolDescriptor],
    ) -> PreparedRanking<'a> {
        PreparedRanking {
            index: Index::build(candidates, &params.weights),
            params: *params,
        }
    }
}

/// A candidate set indexed for ranking: [`Bm25Ranker::prepare`].
pub struct PreparedRanking<'a> {
    index: Index<'a>,
    params: RankingParams,
}

impl PreparedRanking<'_> {
    /// The candidates that match `query`, best first.
    #[must_use]
    pub fn rank(&self, query: &str) -> Vec<RankedTool> {
        let (index, params) = (&self.index, &self.params);
        let mut wanted = terms(query);
        if params.align_endings {
            for term in &mut wanted {
                align_ending(index, term);
            }
        }
        wanted.sort();
        wanted.dedup();
        if wanted.is_empty() || index.documents.is_empty() {
            return Vec::new();
        }
        let count = index.documents.len() as f64;
        let query_terms = widen(index, &wanted, &params.expansion);

        // The query terms are visited in sorted order and the documents in
        // name order, so every floating-point sum is the same whatever order
        // the candidates came in.
        let mut scores = vec![0.0f64; index.documents.len()];
        for (term, weight) in &query_terms {
            let Some(holding) = index.postings.get(term) else {
                continue;
            };
            let holders = holding.len() as f64;
            let idf = (1.0 + (count - holders + 0.5) / (holders + 0.5)).ln();
            for &at in holding {
                let doc = &index.documents[at];
                let frequency = doc.terms[term];
                let norm =
                    params.k1 * (1.0 - params.b + params.b * doc.length / index.average_length);
                scores[at] += weight * idf * frequency * (params.k1 + 1.0) / (frequency + norm);
            }
        }
        // A tool asked for by its own name, written out whole, is what was
        // meant whatever else its text says: it goes above every other match.
        let spans = named_spans(query);
        let ceiling = scores.iter().copied().fold(0.0, f64::max) + 1.0;
        for (score, doc) in scores.iter_mut().zip(&index.documents) {
            if spans.contains(&doc.name_terms) {
                *score += ceiling;
            }
        }
        let mut ranked: Vec<RankedTool> = index
            .documents
            .iter()
            .zip(scores)
            .filter(|(_, score)| *score > 0.0)
            .map(|(doc, score)| RankedTool {
                name: doc.name.to_string(),
                score,
            })
            .collect();
        ranked.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.name.cmp(&b.name))
        });
        ranked
    }
}
