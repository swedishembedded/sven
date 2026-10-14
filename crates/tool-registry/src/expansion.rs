// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in tool retrieval for
// large agent tool sets then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Related terms learned from the catalog itself.
//!
//! A word the query uses and the tools do not (`package` for a `parcel`) is
//! often said once or twice in a description beside the word the tool is named
//! by. Where a term occurs only in tools that also hold another term, the other
//! term is what it means in this catalog. Nothing is listed by hand: the
//! relation is the share of the tools holding a term that also hold another.

use std::collections::BTreeMap;

use crate::index::Index;

/// When and how strongly a query term is widened with the terms that go with
/// it. A `weight` of 0 turns it off.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Expansion {
    /// What an added term counts for, relative to a term of the query, at
    /// full confidence.
    pub weight: f64,
    /// A term is widened only if at least this many tools hold it.
    pub min_documents: usize,
    /// ... and only with terms held by at least this share of those tools.
    pub min_confidence: f64,
    /// The most terms one query term is widened with.
    pub max_terms: usize,
}

impl Default for Expansion {
    fn default() -> Self {
        Self {
            weight: 1.0,
            min_documents: 2,
            min_confidence: 0.9,
            max_terms: 8,
        }
    }
}

/// The terms to search for: the query's own at weight 1, then what they
/// widen to.
pub fn widen(index: &Index<'_>, wanted: &[String], expansion: &Expansion) -> BTreeMap<String, f64> {
    let mut weights: BTreeMap<String, f64> = wanted.iter().map(|t| (t.clone(), 1.0)).collect();
    if expansion.weight <= 0.0 {
        return weights;
    }
    let documents = index.documents.len() as f64;
    for term in wanted {
        let Some(holding) = index.postings.get(term) else {
            continue;
        };
        // A term in most tools says nothing; one in a single tool is a name.
        if holding.len() < expansion.min_documents || holding.len() as f64 > documents / 2.0 {
            continue;
        }
        let mut together: BTreeMap<&str, usize> = BTreeMap::new();
        for &at in holding {
            for other in index.documents[at].terms.keys() {
                if other != term && !wanted.contains(other) {
                    *together.entry(other.as_str()).or_default() += 1;
                }
            }
        }
        let mut related: Vec<(&str, f64)> = together
            .into_iter()
            .map(|(other, n)| (other, n as f64 / holding.len() as f64))
            .filter(|(other, confidence)| {
                *confidence >= expansion.min_confidence
                    && index.holding(other) as f64 <= documents / 2.0
            })
            .collect();
        // Most confident first, then the rarest term, then by name.
        related.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then_with(|| index.holding(a.0).cmp(&index.holding(b.0)))
                .then_with(|| a.0.cmp(b.0))
        });
        for (other, confidence) in related.into_iter().take(expansion.max_terms) {
            let slot = weights.entry(other.to_string()).or_insert(0.0);
            *slot = slot.max(expansion.weight * confidence);
        }
    }
    weights
}

/// Replaces `term`, if no tool holds it, with the same word with an `e` added
/// or removed when some tool holds that: `delet` (from `deleting`) becomes
/// `delete` in a catalog that says `delete`. Only forms the catalog has are
/// used, so two words that happen to differ by an `e` are never merged.
pub fn align_ending(index: &Index<'_>, term: &mut String) {
    if index.holding(term) > 0 {
        return;
    }
    let with_e = format!("{term}e");
    if index.holding(&with_e) > 0 {
        *term = with_e;
    } else if let Some(without) = term.strip_suffix('e') {
        if index.holding(without) > 0 {
            *term = without.to_string();
        }
    }
}
