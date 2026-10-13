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
//! changing.

use std::collections::{BTreeMap, BTreeSet};

use crate::descriptor::ToolDescriptor;

/// A candidate with the score a ranker gave it.
#[derive(Clone, Debug, PartialEq)]
pub struct RankedTool {
    /// The candidate's name.
    pub name: String,
    /// Higher is a better match. Comparable only within one ranking.
    pub score: f64,
}

/// Orders candidate tools by how well they answer a query.
pub trait ToolRanker: Send + Sync {
    /// The candidates that match `query`, best first. A candidate that does
    /// not match at all is left out. Candidates of equal score are ordered by
    /// name, so the result never depends on the order of `candidates`.
    fn rank(&self, query: &str, candidates: &[ToolDescriptor]) -> Vec<RankedTool>;
}

/// Okapi BM25 over the tokens of each tool's name, summary and category.
///
/// A name token counts three times: the name is the most deliberate word
/// anybody chose for the tool. Tokens are lowercased runs of letters and
/// digits (so `read_file`, `read-file` and `readFile` all give `read` and
/// `file`) with plural and verb endings removed, and common words dropped.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bm25Ranker;

/// BM25 term-frequency saturation.
const K1: f64 = 1.2;
/// BM25 length normalisation.
const B: f64 = 0.75;
/// How much more a token of the tool's name weighs than one of its summary.
const NAME_WEIGHT: f64 = 3.0;

/// Words that say nothing about which tool is wanted.
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "any", "are", "as", "at", "be", "by", "can", "do", "for", "from", "how", "i",
    "in", "into", "is", "it", "its", "me", "my", "need", "of", "on", "or", "should", "that", "the",
    "then", "this", "to", "tool", "use", "want", "when", "which", "with", "you", "your",
];

/// The tokens of `text`, in order of appearance, stems only.
fn tokens(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut previous_lower = false;
    for c in text.chars() {
        if !c.is_alphanumeric() {
            flush(&mut word, &mut words);
            previous_lower = false;
            continue;
        }
        // `readFile` is two words.
        if c.is_uppercase() && previous_lower {
            flush(&mut word, &mut words);
        }
        previous_lower = c.is_lowercase() || c.is_ascii_digit();
        word.extend(c.to_lowercase());
    }
    flush(&mut word, &mut words);
    words
}

fn flush(word: &mut String, words: &mut Vec<String>) {
    if word.is_empty() {
        return;
    }
    let token = std::mem::take(word);
    if !STOP_WORDS.contains(&token.as_str()) {
        words.push(stem(token));
    }
}

/// Removes a plural or verb ending: enough to make `searches`, `searching`
/// and `search` one term, and no more.
fn stem(token: String) -> String {
    // `min_stem` keeps short words whole: `string` is not `str` + `ing`.
    let strip = |suffix: &str, replacement: &str, min_stem: usize| -> Option<String> {
        token
            .strip_suffix(suffix)
            .filter(|rest| rest.chars().count() >= min_stem)
            .map(|rest| format!("{rest}{replacement}"))
    };
    strip("ies", "y", 3)
        .or_else(|| strip("sses", "ss", 2))
        .or_else(|| strip("ing", "", 4))
        .or_else(|| strip("ed", "", 3))
        .or_else(|| strip("es", "", 3).filter(|s| s.ends_with(['s', 'x', 'z', 'h'])))
        .or_else(|| {
            let keeps_s = token.ends_with("ss") || token.ends_with("us");
            (!keeps_s).then(|| strip("s", "", 3)).flatten()
        })
        .unwrap_or(token)
}

/// A candidate reduced to weighted term counts.
struct Document<'a> {
    name: &'a str,
    terms: BTreeMap<String, f64>,
    length: f64,
}

fn document(tool: &ToolDescriptor) -> Document<'_> {
    let mut terms = BTreeMap::new();
    let mut length = 0.0;
    let mut add = |text: &str, weight: f64| {
        for token in tokens(text) {
            *terms.entry(token).or_insert(0.0) += weight;
            length += weight;
        }
    };
    add(&tool.name, NAME_WEIGHT);
    add(&tool.summary, 1.0);
    add(tool.category(), 1.0);
    Document {
        name: &tool.name,
        terms,
        length,
    }
}

impl ToolRanker for Bm25Ranker {
    fn rank(&self, query: &str, candidates: &[ToolDescriptor]) -> Vec<RankedTool> {
        let wanted: BTreeSet<String> = tokens(query).into_iter().collect();
        if wanted.is_empty() || candidates.is_empty() {
            return Vec::new();
        }
        let documents: Vec<Document<'_>> = candidates.iter().map(document).collect();
        let count = documents.len() as f64;
        let average_length = (documents.iter().map(|d| d.length).sum::<f64>() / count).max(1.0);

        // The query terms are visited in sorted order and each document's
        // score is a sum over them, so the floating-point result is the same
        // whatever order the candidates came in.
        let mut ranked: Vec<RankedTool> = documents
            .iter()
            .filter_map(|doc| {
                let score: f64 = wanted
                    .iter()
                    .map(|term| {
                        let frequency = doc.terms.get(term).copied().unwrap_or(0.0);
                        if frequency == 0.0 {
                            return 0.0;
                        }
                        let holding = documents
                            .iter()
                            .filter(|d| d.terms.contains_key(term))
                            .count() as f64;
                        let idf = (1.0 + (count - holding + 0.5) / (holding + 0.5)).ln();
                        let norm = K1 * (1.0 - B + B * doc.length / average_length);
                        idf * frequency * (K1 + 1.0) / (frequency + norm)
                    })
                    .sum();
                (score > 0.0).then(|| RankedTool {
                    name: doc.name.to_string(),
                    score,
                })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_styles_and_endings_give_the_same_terms() {
        assert_eq!(tokens("read_file"), ["read", "file"]);
        assert_eq!(tokens("read-file"), ["read", "file"]);
        assert_eq!(tokens("readFile"), ["read", "file"]);
        assert_eq!(tokens("Searches files"), tokens("search file"));
        assert_eq!(tokens("searching"), tokens("search"));
    }

    #[test]
    fn short_words_and_double_s_are_not_mangled() {
        assert_eq!(tokens("class bus"), ["class", "bus"]);
        assert_eq!(tokens("is"), Vec::<String>::new());
    }
}
