// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in tool retrieval for
// large agent tool sets then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The searchable form of a set of tools: weighted terms per tool and the
//! tools that hold each term.

use std::collections::BTreeMap;

use crate::analysis::terms;
use crate::descriptor::ToolDescriptor;

/// How much a term counts for, by where it was found. A weight of 0 leaves the
/// field out of the index.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldWeights {
    /// The name: the most deliberate word anybody chose for the tool.
    pub name: f64,
    pub summary: f64,
    /// The category label (`read`, `mcp`, ...).
    pub category: f64,
    /// The sentences of the description after the summary.
    pub description: f64,
    pub parameter_descriptions: f64,
}

impl Default for FieldWeights {
    fn default() -> Self {
        Self {
            name: 2.0,
            summary: 1.0,
            category: 1.0,
            description: 0.4,
            parameter_descriptions: 0.2,
        }
    }
}

/// One tool reduced to weighted term counts.
pub struct Document<'a> {
    pub name: &'a str,
    /// The distinct terms of the name, sorted.
    pub name_terms: Vec<String>,
    pub terms: BTreeMap<String, f64>,
    pub length: f64,
}

pub fn document<'a>(tool: &'a ToolDescriptor, weights: &FieldWeights) -> Document<'a> {
    let mut terms_of = BTreeMap::new();
    let mut length = 0.0;
    let mut add = |text: &str, weight: f64| {
        if weight <= 0.0 {
            return;
        }
        for term in terms(text) {
            *terms_of.entry(term).or_insert(0.0) += weight;
            length += weight;
        }
    };
    add(&tool.name, weights.name);
    add(&tool.summary, weights.summary);
    add(tool.category(), weights.category);
    add(&tool.description, weights.description);
    for text in &tool.parameter_descriptions {
        add(text, weights.parameter_descriptions);
    }
    let mut name_terms = terms(&tool.name);
    name_terms.sort();
    name_terms.dedup();
    Document {
        name: &tool.name,
        name_terms,
        terms: terms_of,
        length,
    }
}

/// The documents of a candidate set, in name order, and for every term the
/// documents that hold it.
pub struct Index<'a> {
    pub documents: Vec<Document<'a>>,
    pub postings: BTreeMap<String, Vec<usize>>,
    pub average_length: f64,
}

impl<'a> Index<'a> {
    /// Sorting by name makes every sum below independent of the order the
    /// candidates came in.
    pub fn build(candidates: &'a [ToolDescriptor], weights: &FieldWeights) -> Self {
        let mut ordered: Vec<&ToolDescriptor> = candidates.iter().collect();
        ordered.sort_by(|a, b| a.name.cmp(&b.name));
        let documents: Vec<Document<'a>> = ordered
            .into_iter()
            .map(|tool| document(tool, weights))
            .collect();
        let mut postings: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (at, doc) in documents.iter().enumerate() {
            for term in doc.terms.keys() {
                postings.entry(term.clone()).or_default().push(at);
            }
        }
        let total: f64 = documents.iter().map(|d| d.length).sum();
        let average_length = (total / documents.len().max(1) as f64).max(1.0);
        Self {
            documents,
            postings,
            average_length,
        }
    }

    /// How many documents hold `term`.
    pub fn holding(&self, term: &str) -> usize {
        self.postings.get(term).map_or(0, Vec::len)
    }
}
