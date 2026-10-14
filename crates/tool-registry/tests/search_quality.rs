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
use sven_tool_registry::{present_matches, Bm25Ranker, RankingParams, ToolDescriptor};

const SHORTLIST: usize = 8;

/// The held-out recall@8 on the standard plan, floored under what was
/// measured so the pin catches regressions, not noise.
const RECALL_FLOOR: [(QueryKind, f64); 4] = [
    (QueryKind::Exact, 0.98),
    (QueryKind::Paraphrase, 0.93),
    (QueryKind::Indirect, 0.90),
    (QueryKind::Misspelled, 0.98),
];

/// Ranks each of `queries` over `tools` with the ranker search uses, indexing
/// the tools once.
const MISSPELLED_TOP1_FLOOR: f64 = 0.90;

fn current_ranker(tools: &[ToolDescriptor], queries: &[&str]) -> Vec<Vec<String>> {
    let prepared = Bm25Ranker.prepare(&RankingParams::default(), tools);
    queries
        .iter()
        .map(|q| prepared.rank(q).into_iter().map(|r| r.name).collect())
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
    for kind in QueryKind::ALL {
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
    let oracle = |_: &[ToolDescriptor], queries: &[&str]| -> Vec<Vec<String>> {
        queries.iter().map(|q| vec![truth[q].to_string()]).collect()
    };
    let perfect = evaluate(&oracle, &all, SHORTLIST);
    let blind = evaluate(
        &|_: &[ToolDescriptor], queries: &[&str]| vec![Vec::new(); queries.len()],
        &all,
        SHORTLIST,
    );
    for kind in QueryKind::ALL {
        let (p, b) = (perfect.of(kind), blind.of(kind));
        assert!(p.queries >= 30);
        assert_eq!((p.recall, p.top1, p.mrr), (1.0, 1.0, 1.0));
        assert_eq!((b.recall, b.top1, b.mrr), (0.0, 0.0, 0.0));
    }
}

/// The recall of the ranker as it stands on the held-out split, pinned as
/// floors so that a change which lowers it fails here.
#[test]
fn held_out_recall_does_not_fall_below_the_recorded_floor() {
    let held_out = measure(Split::Validation, &Plan::standard());
    eprintln!("held-out\n{held_out}");
    for (kind, floor) in RECALL_FLOOR {
        let got = held_out.of(kind).recall;
        assert!(
            got >= floor,
            "{kind:?} recall@{SHORTLIST} {got:.3} < {floor}"
        );
    }
}

/// A model that knows a tool's exact name must not be second-guessed by
/// similar tools, whatever the style the name is written in.
/// A name with a letter wrong is the nearest tool's name: that is what
/// "did you mean" says, so the first match has to be right.
#[test]
fn a_misspelled_name_finds_its_tool_first() {
    let held_out = measure(Split::Validation, &Plan::standard());
    let got = held_out.of(QueryKind::Misspelled).top1;
    assert!(got >= MISSPELLED_TOP1_FLOOR, "top-1 {got:.3}");
}

#[test]
fn a_tool_asked_for_by_its_exact_name_is_first() {
    for catalog in catalogs(Split::Validation, &Plan::standard()) {
        let tools = catalog.descriptors();
        let asked: Vec<String> = tools.iter().map(|t| format!("\"{}\"", t.name)).collect();
        let asked: Vec<&str> = asked.iter().map(String::as_str).collect();
        for (tool, ranked) in tools.iter().zip(current_ranker(&tools, &asked)) {
            assert_eq!(ranked.first(), Some(&tool.name), "{}", tool.name);
        }
    }
}

#[test]
#[ignore = "larger sweep: cargo test -p sven-tool-registry --test search_quality -- --ignored --nocapture"]
fn larger_sweep_over_many_catalogs() {
    let plan = Plan::sweep();
    for split in [Split::Tuning, Split::Validation] {
        eprintln!("{split:?}\n{}", measure(split, &plan));
        let rich: Vec<_> = catalogs(split, &plan)
            .into_iter()
            .filter(|c| c.evidence >= 0.4)
            .collect();
        eprintln!(
            "{split:?}, catalogs whose text holds evidence for paraphrases\n{}",
            evaluate(&current_ranker, &rich, SHORTLIST)
        );
        eprintln!("{split:?}: {}", result_size(&catalogs(split, &plan)));
        eprintln!("{split:?}\n{}", ambiguity(&catalogs(split, &plan)));
        eprintln!(
            "{split:?}, shortlist and the 8 names after it (recall@{})\n{}",
            2 * SHORTLIST,
            evaluate(&current_ranker, &catalogs(split, &plan), 2 * SHORTLIST)
        );
    }
}

/// What a search result costs, in the estimate the token ledger uses (one
/// token per four bytes): the matches shown in full and the names after them.
fn result_size(catalogs: &[quality::corpus::Catalog]) -> String {
    let (mut shown, mut related, mut searches) = (0usize, 0usize, 0usize);
    for catalog in catalogs {
        let tools = catalog.descriptors();
        let prepared = Bm25Ranker.prepare(&RankingParams::default(), &tools);
        for query in &catalog.queries {
            let ranked = prepared.rank(&query.text);
            let out = present_matches(&query.text, &tools, &ranked, SHORTLIST);
            let (head, tail) = out.split_once("\nAlso related").unwrap_or((&out, ""));
            shown += head.len();
            related += tail.len();
            searches += 1;
        }
    }
    format!(
        "per search, {:.0} estimated tokens for the {SHORTLIST} matches in full, {:.0} for the names after them",
        shown as f64 / searches as f64 / 4.0,
        related as f64 / searches as f64 / 4.0
    )
}

/// How often the two best matches are within a relative margin of each other
/// (the condition under which a decision model is asked to break the tie) and,
/// of those, how often the best one is the wrong one: a margin earns its
/// model calls when the asked searches are mostly the wrongly ordered ones.
fn ambiguity(catalogs: &[quality::corpus::Catalog]) -> String {
    let margins = [0.0, 0.01, 0.02, 0.05, 0.10, 0.20];
    let mut asked = [0usize; 6];
    let mut wrong_when_asked = [0usize; 6];
    let mut wrong_in_pool = [0usize; 6];
    let (mut searches, mut wrong) = (0usize, 0usize);
    for catalog in catalogs {
        let tools = catalog.descriptors();
        let prepared = Bm25Ranker.prepare(&RankingParams::default(), &tools);
        for query in &catalog.queries {
            let ranked = prepared.rank(&query.text);
            searches += 1;
            let first_wrong = ranked.first().is_none_or(|r| r.name != query.target);
            wrong += usize::from(first_wrong);
            let gap = match ranked.as_slice() {
                [a, b, ..] => (a.score - b.score) / a.score,
                _ => f64::INFINITY,
            };
            let in_pool = ranked.iter().take(8).any(|r| r.name == query.target);
            for (i, margin) in margins.iter().enumerate() {
                if gap <= *margin {
                    asked[i] += 1;
                    wrong_when_asked[i] += usize::from(first_wrong);
                    wrong_in_pool[i] += usize::from(first_wrong && in_pool);
                }
            }
        }
    }
    let mut out = format!(
        "{searches} searches, the best match is wrong in {:.1}%\n| margin | asked | best wrong among asked | fixable among asked | wrong bests asked |\n|---:|---:|---:|---:|---:|\n",
        100.0 * wrong as f64 / searches as f64
    );
    for (i, margin) in margins.iter().enumerate() {
        out.push_str(&format!(
            "| {margin} | {:.1}% | {:.1}% | {:.1}% | {:.1}% |\n",
            100.0 * asked[i] as f64 / searches as f64,
            100.0 * wrong_when_asked[i] as f64 / asked[i].max(1) as f64,
            100.0 * wrong_in_pool[i] as f64 / asked[i].max(1) as f64,
            100.0 * wrong_when_asked[i] as f64 / wrong.max(1) as f64
        ));
    }
    out
}

/// Search over a few thousand tools must stay cheap: the index of a catalog
/// is built per search call, so that is what is timed.
/// `cargo test --release -p sven-tool-registry --test search_quality -- --ignored --nocapture latency`
#[test]
#[ignore = "timing; run in release"]
fn latency_on_a_catalog_of_two_thousand_tools() {
    let plan = Plan {
        catalogs: 5,
        sizes: &[400],
        queries_per_kind: 20,
    };
    let mut tools: Vec<ToolDescriptor> = Vec::new();
    let mut queries: Vec<String> = Vec::new();
    for (i, split) in [Split::Tuning, Split::Validation]
        .into_iter()
        .cycle()
        .take(5)
        .enumerate()
    {
        let catalog = catalogs(split, &plan).remove(i / 2);
        tools.extend(catalog.descriptors().into_iter().map(|mut t| {
            t.name = format!("k{i}-{}", t.name);
            t
        }));
        queries.extend(catalog.queries.into_iter().map(|q| q.text));
    }
    let params = RankingParams::default();
    let started = std::time::Instant::now();
    let prepared = Bm25Ranker.prepare(&params, &tools);
    let index = started.elapsed();
    let started = std::time::Instant::now();
    let hits: usize = queries.iter().map(|q| prepared.rank(q).len()).sum();
    let per_query = started.elapsed() / queries.len() as u32;
    let started = std::time::Instant::now();
    let whole: usize = queries
        .iter()
        .take(20)
        .map(|q| Bm25Ranker.rank_with(&params, q, &tools).len())
        .sum();
    let per_search = started.elapsed() / 20;
    eprintln!(
        "{} tools: index {index:?}, query on a prepared index {per_query:?}, index and query per search call {per_search:?} ({hits} and {whole} matches)",
        tools.len()
    );
}
