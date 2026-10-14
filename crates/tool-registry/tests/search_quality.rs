// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in tool retrieval for
// large agent tool sets then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Spec: how well tool search finds the tool that was meant, measured on
//! generated catalogs and held-out queries.
//!
//! The corpus is seeded and split in two. The tuning split is what ranking
//! parameters may be fitted on. The validation split shares no catalog seed,
//! no domain, object or verb and no query template family with it, so a number
//! measured there says how search does on tools and phrasings it was not
//! shaped by. See `quality/corpus.rs` for what the generator assumes.

mod quality;

use quality::corpus::{catalogs, Plan, QueryKind, Split};
use quality::metrics::{evaluate, Scores};
use sven_tool_registry::{Bm25Ranker, ToolDescriptor};

const SHORTLIST: usize = 8;

fn current_ranker(query: &str, tools: &[ToolDescriptor]) -> Vec<String> {
    Bm25Ranker
        .rank_now(query, tools)
        .into_iter()
        .map(|r| r.name)
        .collect()
}

fn measure(split: Split, plan: &Plan) -> Scores {
    evaluate(&current_ranker, &catalogs(split, plan), SHORTLIST)
}

#[test]
fn the_corpus_is_large_deterministic_and_split_without_overlap() {
    let plan = Plan::standard();
    let tuning = catalogs(Split::Tuning, &plan);
    let validation = catalogs(Split::Validation, &plan);
    assert_eq!(
        tuning,
        catalogs(Split::Tuning, &plan),
        "same seed, same corpus"
    );

    let queries: usize = tuning
        .iter()
        .chain(&validation)
        .map(|c| c.queries.len())
        .sum();
    assert!(queries >= 300, "{queries} queries");
    let mut sizes: Vec<usize> = tuning.iter().map(|c| c.tools.len()).collect();
    sizes.sort_unstable();
    sizes.dedup();
    assert!(sizes.len() >= 4, "catalogs of different sizes: {sizes:?}");

    let seeds = |cs: &[quality::corpus::Catalog]| cs.iter().map(|c| c.seed).collect::<Vec<_>>();
    assert!(seeds(&tuning)
        .iter()
        .all(|s| !seeds(&validation).contains(s)));

    let tools = |cs: &[quality::corpus::Catalog]| -> std::collections::BTreeSet<String> {
        cs.iter()
            .flat_map(|c| c.tools.iter().map(|t| t.name.to_lowercase()))
            .collect()
    };
    assert!(tools(&tuning).is_disjoint(&tools(&validation)));
    let families =
        |cs: &[quality::corpus::Catalog]| -> std::collections::BTreeSet<(QueryKind, usize)> {
            cs.iter()
                .flat_map(|c| c.queries.iter().map(|q| (q.kind, q.family)))
                .collect()
        };
    assert!(families(&tuning).is_disjoint(&families(&validation)));
    for kind in [QueryKind::Exact, QueryKind::Paraphrase, QueryKind::Indirect] {
        assert!(families(&tuning).iter().filter(|(k, _)| *k == kind).count() >= 3);
    }
}

#[test]
fn every_query_has_exactly_one_target_in_its_catalog() {
    for split in [Split::Tuning, Split::Validation] {
        for catalog in catalogs(split, &Plan::standard()) {
            let names: Vec<&str> = catalog.tools.iter().map(|t| t.name.as_str()).collect();
            for query in &catalog.queries {
                assert_eq!(
                    names.iter().filter(|n| **n == query.target).count(),
                    1,
                    "{query:?}"
                );
            }
        }
    }
}

#[test]
fn the_metrics_score_a_perfect_ranker_and_a_blind_one() {
    let plan = Plan::standard();
    let mut all = catalogs(Split::Validation, &plan);
    // An oracle can only answer a text that means one tool.
    let mut seen = std::collections::BTreeMap::<String, usize>::new();
    for query in all.iter().flat_map(|c| &c.queries) {
        *seen.entry(query.text.clone()).or_default() += 1;
    }
    for catalog in &mut all {
        catalog.queries.retain(|q| seen[&q.text] == 1);
    }
    let truth: std::collections::BTreeMap<&str, &str> = all
        .iter()
        .flat_map(|c| {
            c.queries
                .iter()
                .map(|q| (q.text.as_str(), q.target.as_str()))
        })
        .collect();
    let oracle = |q: &str, _: &[ToolDescriptor]| vec![truth[q].to_string()];
    let perfect = evaluate(&oracle, &all, SHORTLIST);
    let blind = evaluate(&|_: &str, _: &[ToolDescriptor]| Vec::new(), &all, SHORTLIST);
    for kind in [QueryKind::Exact, QueryKind::Paraphrase, QueryKind::Indirect] {
        let (p, b) = (perfect.of(kind), blind.of(kind));
        assert!(p.queries >= 30);
        assert_eq!((p.recall, p.top1, p.mrr), (1.0, 1.0, 1.0));
        assert_eq!((b.recall, b.top1, b.mrr), (0.0, 0.0, 0.0));
    }
}

/// The baseline of the ranker as it stands, pinned as floors so that a change
/// which lowers a held-out number fails here. Raise the floors with the ranker.
#[test]
fn held_out_search_quality_does_not_fall_below_the_recorded_baseline() {
    let held_out = measure(Split::Validation, &Plan::standard());
    eprintln!("held-out\n{held_out}");
    for (kind, floor) in quality::BASELINE_RECALL_FLOOR {
        let got = held_out.of(kind).recall;
        assert!(
            got >= floor,
            "{kind:?} recall@{SHORTLIST} {got:.3} < {floor}"
        );
    }
}

#[test]
#[ignore = "larger sweep: cargo test -p sven-tool-registry --test search_quality -- --ignored --nocapture"]
fn larger_sweep_over_many_catalogs() {
    let plan = Plan::sweep();
    for split in [Split::Tuning, Split::Validation] {
        eprintln!("{split:?}\n{}", measure(split, &plan));
    }
}
