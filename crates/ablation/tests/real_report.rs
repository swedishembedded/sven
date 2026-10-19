// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Spec: a real-model comparison of decisions disabled against heuristic
//! decisions states, for each variant, how often the verified answer was right,
//! what it cost per verified success (tokens, uncached input, wall time), and
//! what the decision runtime did; and for the pair, on the tasks both
//! completed, how success and tokens differ.

use std::collections::BTreeMap;

use sven_ablation::answer::AnswerScore;
use sven_ablation::matrix::Matrix;
use sven_ablation::real_report::{build_real, render_real};
use sven_ablation::record::{Provenance, RunRecord, RunStatus, TokenRecord};
use sven_ablation::task::Suite;

fn matrix() -> Matrix {
    Matrix::from_yaml(include_str!("../../../benchmarks/ablation/matrix.yaml")).unwrap()
}

fn run(variant: &str, seed: u64, tokens: u64, success: bool) -> RunRecord {
    let m = matrix();
    let v = m.variants.iter().find(|v| v.id == variant).unwrap();
    let mut r = RunRecord::new(
        &format!("relevant_file-s{seed}"),
        Suite::RelevantFile,
        seed,
        v,
        Provenance {
            sven_commit: "abc".into(),
            sven_dirty: false,
            sven_binary: "sven".into(),
            brain: None,
            matrix_sha256: "m".into(),
        },
        RunStatus::Completed,
    );
    r.scripted = false;
    r.tokens = Some(TokenRecord {
        total: tokens,
        requests: 5,
        input: tokens,
        cache_read: tokens / 2,
        wall_ms: 1000,
        by_source: BTreeMap::new(),
        ..TokenRecord::default()
    });
    r.answer = Some(AnswerScore {
        critical_total: 2,
        critical_cited: if success { 2 } else { 1 },
        distractors_cited: 0,
        success,
    });
    r
}

#[test]
fn each_variant_reports_success_and_cost_per_verified_success() {
    let runs = vec![
        run("B", 1, 1000, true),
        run("B", 2, 1000, false),
        run("C", 1, 800, true),
        run("C", 2, 800, true),
    ];
    let report = build_real(&runs, "B", "C");
    let b = report.variants.iter().find(|v| v.variant == "B").unwrap();
    let c = report.variants.iter().find(|v| v.variant == "C").unwrap();
    assert_eq!((b.runs, b.successes), (2, 1));
    assert_eq!(b.tokens_per_success, Some(2000.0));
    assert_eq!(c.tokens_per_success, Some(800.0));
    assert_eq!(b.uncached_input_per_success, Some(1000.0));
    assert_eq!(c.success_rate, 1.0);
}

#[test]
fn the_pair_is_compared_on_tasks_both_completed() {
    let runs = vec![
        run("B", 1, 1000, true),
        run("B", 2, 1000, false),
        run("B", 3, 1000, true),
        run("C", 1, 500, true),
        run("C", 2, 500, true),
    ];
    let report = build_real(&runs, "B", "C");
    assert_eq!(report.paired_tasks, 2, "seed 3 has no C run");
    assert_eq!(report.success_gain, 1, "C won seed 2");
    assert_eq!(report.success_loss, 0);
    assert!((report.token_ratio.unwrap() - 2.0).abs() < 1e-9);
    let text = render_real(&report);
    assert!(text.contains("tokens per verified success"));
}

#[test]
fn scripted_runs_are_not_a_real_comparison() {
    let mut scripted = run("B", 1, 1000, true);
    scripted.scripted = true;
    scripted.answer = None;
    let report = build_real(&[scripted], "B", "C");
    assert_eq!(report.variants.iter().map(|v| v.runs).sum::<usize>(), 0);
}
