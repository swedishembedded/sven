// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in tool retrieval for
// large agent tool sets then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Fits the ranking parameters on the tuning split and reports what the
//! validation split makes of them. Opt in, with a release build:
//!
//! `cargo test --release -p sven-tool-registry --test search_tuning -- --ignored --nocapture`
//!
//! Each parameter is moved over its grid, one at a time, keeping a value only
//! when it raises the tuning objective, until a pass changes nothing. The
//! validation numbers play no part in the choice; they are printed beside it
//! so that an addition which does not carry over can be seen and dropped.

mod quality;

use quality::corpus::{catalogs, Plan, QueryKind, Split};
use quality::metrics::{evaluate, Scores};
use sven_tool_registry::{Bm25Ranker, RankingParams, ToolDescriptor};

const SHORTLIST: usize = 8;
const KINDS: [QueryKind; 3] = [QueryKind::Exact, QueryKind::Paraphrase, QueryKind::Indirect];

/// A parameter and the values tried for it.
struct Knob {
    name: &'static str,
    values: &'static [f64],
    set: fn(&mut RankingParams, f64),
}

const KNOBS: &[Knob] = &[
    Knob {
        name: "align_endings",
        values: &[0.0, 1.0],
        set: |p, v| p.align_endings = v > 0.0,
    },
    Knob {
        name: "w.description",
        values: &[0.0, 0.2, 0.4, 0.7, 1.0],
        set: |p, v| p.weights.description = v,
    },
    Knob {
        name: "w.parameter_descriptions",
        values: &[0.0, 0.2, 0.5, 1.0],
        set: |p, v| p.weights.parameter_descriptions = v,
    },
    Knob {
        name: "w.name",
        values: &[1.0, 2.0, 3.0, 4.0],
        set: |p, v| p.weights.name = v,
    },
    Knob {
        name: "w.category",
        values: &[0.0, 0.5, 1.0],
        set: |p, v| p.weights.category = v,
    },
    Knob {
        name: "expansion.weight",
        values: &[0.0, 0.5, 1.0, 1.5, 2.5, 4.0],
        set: |p, v| p.expansion.weight = v,
    },
    Knob {
        name: "expansion.min_confidence",
        values: &[0.4, 0.6, 0.8, 0.9],
        set: |p, v| p.expansion.min_confidence = v,
    },
    Knob {
        name: "expansion.max_terms",
        values: &[2.0, 4.0, 8.0],
        set: |p, v| p.expansion.max_terms = v as usize,
    },
    Knob {
        name: "expansion.min_documents",
        values: &[2.0, 3.0, 5.0],
        set: |p, v| p.expansion.min_documents = v as usize,
    },
    Knob {
        name: "k1",
        values: &[0.6, 1.2, 2.0, 3.0, 5.0],
        set: |p, v| p.k1 = v,
    },
    Knob {
        name: "b",
        values: &[0.2, 0.4, 0.6, 0.75, 0.9],
        set: |p, v| p.b = v,
    },
];

fn scores(params: &RankingParams, split: Split, plan: &Plan, min_evidence: f64) -> Scores {
    let rank = |tools: &[ToolDescriptor], queries: &[&str]| -> Vec<Vec<String>> {
        let prepared = Bm25Ranker.prepare(params, tools);
        queries
            .iter()
            .map(|q| prepared.rank(q).into_iter().map(|r| r.name).collect())
            .collect()
    };
    let chosen: Vec<_> = catalogs(split, plan)
        .into_iter()
        .filter(|c| c.evidence >= min_evidence)
        .collect();
    evaluate(&rank, &chosen, SHORTLIST)
}

/// Recall first, order second: the shortlist is what the model reads, and
/// its order is what a model that reads only the first lines goes by.
fn objective(scores: &Scores) -> f64 {
    KINDS
        .iter()
        .map(|k| scores.of(*k).recall + 0.5 * scores.of(*k).mrr)
        .sum::<f64>()
        / KINDS.len() as f64
}

fn report(label: &str, params: &RankingParams, plan: &Plan) {
    eprintln!("{label}");
    for (stratum, min_evidence) in [("all catalogs", 0.0), ("evidence >= 0.4", 0.4)] {
        let tuning = scores(params, Split::Tuning, plan, min_evidence);
        let validation = scores(params, Split::Validation, plan, min_evidence);
        eprintln!(
            "  {stratum}\n    tuning     {}\n    validation {}",
            line(&tuning),
            line(&validation)
        );
    }
}

fn line(scores: &Scores) -> String {
    KINDS
        .iter()
        .map(|k| {
            let s = scores.of(*k);
            format!(
                "{:?} {:.1}/{:.1}/{:.3}  ",
                k,
                s.recall * 100.0,
                s.top1 * 100.0,
                s.mrr
            )
        })
        .collect()
}

#[test]
#[ignore = "fits parameters; prints a report"]
fn fit_on_the_tuning_split_and_report_the_validation_split() {
    let plan = Plan::sweep();
    let mut params = RankingParams::default();
    report("start (recall@8 / top-1 / MRR, in %)", &params, &plan);
    let mut best = objective(&scores(&params, Split::Tuning, &plan, 0.0));
    for pass in 1..=4 {
        let mut changed = false;
        for knob in KNOBS {
            for &value in knob.values {
                let mut trial = params;
                (knob.set)(&mut trial, value);
                if trial == params {
                    continue;
                }
                let got = objective(&scores(&trial, Split::Tuning, &plan, 0.0));
                if got > best + 1e-9 {
                    eprintln!(
                        "pass {pass}: {} = {value}: tuning objective {best:.4} -> {got:.4}",
                        knob.name
                    );
                    best = got;
                    params = trial;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    eprintln!("\nfitted: {params:#?}");
    report("fitted", &params, &plan);
    report(
        "fitted, on the plan the pinned test runs",
        &params,
        &Plan::standard(),
    );

    eprintln!("\nwhat each knob left at its first value would add: the fitted setting with its last value");
    for knob in KNOBS {
        let mut with = params;
        (knob.set)(&mut with, knob.values[0]);
        if with == params {
            let last = *knob.values.last().expect("a knob has values");
            let mut with = params;
            (knob.set)(&mut with, last);
            report(&format!("with {} = {last}", knob.name), &with, &plan);
        }
    }

    eprintln!("\nwhat each knob adds: the fitted setting against the knob's first value");
    for knob in KNOBS {
        let mut without = params;
        (knob.set)(&mut without, knob.values[0]);
        if without != params {
            report(
                &format!("without {} (= {})", knob.name, knob.values[0]),
                &without,
                &plan,
            );
        }
    }
}
