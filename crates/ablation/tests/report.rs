// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The report and the release gates, over synthetic run records whose
//! numbers are chosen so the expected answer is known.

use std::collections::BTreeMap;

use sven_ablation::evidence::EvidenceScore;
use sven_ablation::gates::evaluate_gates;
use sven_ablation::matrix::Matrix;
use sven_ablation::record::{Provenance, RunRecord, RunStatus, TokenRecord, RUN_SCHEMA};
use sven_ablation::report::{build, GateStatus, Verdict};
use sven_ablation::score::{Score, SecurityScore, ShortlistScore};
use sven_ablation::task::Suite;

const MATRIX: &str = include_str!("../../../benchmarks/ablation/matrix.yaml");

fn matrix() -> Matrix {
    Matrix::from_yaml(MATRIX).unwrap()
}

fn provenance() -> Provenance {
    Provenance {
        sven_commit: "abc123".into(),
        sven_dirty: false,
        sven_binary: "sven".into(),
        brain: None,
        matrix_sha256: "m".into(),
    }
}

/// What a synthetic run looks like.
struct Spec {
    suite: Suite,
    seed: u64,
    variant: &'static str,
    tokens: u64,
    passed: bool,
    critical: (usize, usize),
    mrr: Option<f64>,
    violations: usize,
}

impl Spec {
    fn new(suite: Suite, seed: u64, variant: &'static str, tokens: u64) -> Self {
        Self {
            suite,
            seed,
            variant,
            tokens,
            passed: true,
            critical: (2, 2),
            mrr: None,
            violations: 0,
        }
    }
}

fn record(spec: &Spec) -> RunRecord {
    let m = matrix();
    let v = m.variants.iter().find(|v| v.id == spec.variant).unwrap();
    let task_id = format!("{}-s{}", spec.suite.name(), spec.seed);
    let mut r = RunRecord::new(
        &task_id,
        spec.suite,
        spec.seed,
        v,
        provenance(),
        RunStatus::Completed,
    );
    r.tokens = Some(TokenRecord {
        total: spec.tokens,
        requests: 4,
        trajectories: 1,
        by_source: BTreeMap::from([("tool_results".to_string(), spec.tokens)]),
        ..TokenRecord::default()
    });
    let evidence = (spec.suite != Suite::ToolSelection).then(|| EvidenceScore {
        critical_total: spec.critical.1,
        critical_visible: spec.critical.0,
        distractor_total: 0,
        distractor_visible: 0,
        critical_recall: spec.critical.0 as f64 / spec.critical.1 as f64,
        precision: None,
        missing: Vec::new(),
    });
    let shortlist = spec.mrr.map(|mrr| ShortlistScore {
        probes: Vec::new(),
        shortlist_recall: 1.0,
        top1_rate: Some(mrr),
        mean_reciprocal_rank: Some(mrr),
    });
    r.score = Some(Score {
        suite: spec.suite,
        passed: spec.passed,
        hard_failures: Vec::new(),
        labels: Vec::new(),
        evidence,
        shortlist,
        security: (spec.suite == Suite::Security).then(|| SecurityScore {
            leaks: Vec::new(),
            bypasses: Vec::new(),
            permitted_actions_reached_model: true,
            violations: spec.violations,
        }),
        delegation: None,
        recovery: None,
    });
    r
}

/// Every targeted suite, `seeds` seeds, for the given variants and token cost.
fn targeted(variants: &[(&'static str, u64)], seeds: u64) -> Vec<RunRecord> {
    let mut runs = Vec::new();
    for suite in [
        Suite::RelevantFile,
        Suite::NoisyOutput,
        Suite::ToolSelection,
        Suite::RestartSurvival,
    ] {
        for seed in 1..=seeds {
            for (variant, tokens) in variants {
                // A little spread so intervals are not degenerate.
                let jitter = seed * 7;
                runs.push(record(&Spec::new(suite, seed, variant, tokens + jitter)));
            }
        }
    }
    runs
}

fn gate(runs: &[RunRecord], variant: &str, name: &str) -> (GateStatus, String) {
    let g = evaluate_gates(runs, "A", variant)
        .into_iter()
        .find(|g| g.gate.contains(name))
        .unwrap_or_else(|| panic!("no gate {name}"));
    (g.status, g.detail)
}

#[test]
fn with_no_runs_every_gate_is_not_measured_never_pass() {
    for g in evaluate_gates(&[], "A", "B") {
        assert_eq!(g.status, GateStatus::NotMeasured, "{}", g.gate);
    }
}

#[test]
fn a_two_fold_reduction_on_every_targeted_suite_passes() {
    let runs = targeted(&[("A", 10_000), ("B", 3_000)], 5);
    let (status, detail) = gate(&runs, "B", "token reduction");
    assert_eq!(status, GateStatus::Pass, "{detail}");
}

#[test]
fn a_smaller_reduction_fails_even_when_it_is_a_saving() {
    let runs = targeted(&[("A", 10_000), ("B", 7_000)], 5);
    assert_eq!(gate(&runs, "B", "token reduction").0, GateStatus::Fail);
}

#[test]
fn a_reduction_measured_on_too_few_pairs_is_not_measured() {
    let runs = targeted(&[("A", 10_000), ("B", 3_000)], 2);
    let (status, detail) = gate(&runs, "B", "token reduction");
    assert_eq!(status, GateStatus::NotMeasured, "{detail}");
    assert!(detail.contains("at least 3 pairs"));
}

#[test]
fn a_targeted_suite_without_runs_leaves_the_gate_unmeasured() {
    let mut runs = targeted(&[("A", 10_000), ("B", 3_000)], 5);
    runs.retain(|r| r.suite != Suite::NoisyOutput);
    assert_eq!(
        gate(&runs, "B", "token reduction").0,
        GateStatus::NotMeasured
    );
}

#[test]
fn losing_critical_evidence_below_99_percent_fails_the_hard_gate() {
    let mut runs = targeted(&[("A", 100), ("B", 40)], 5);
    assert_eq!(gate(&runs, "B", "critical evidence").0, GateStatus::Pass);
    // One lost item of two hundred is 99.5%; two lost is 99.0%; three is below.
    let mut lost = 0;
    for r in runs
        .iter_mut()
        .filter(|r| r.variant.id == "B" && r.suite != Suite::ToolSelection)
    {
        if lost < 3 {
            let e = r.score.as_mut().unwrap().evidence.as_mut().unwrap();
            e.critical_visible = 0;
            e.critical_recall = 0.0;
            lost += 1;
        }
    }
    let (status, detail) = gate(&runs, "B", "critical evidence");
    assert_eq!(status, GateStatus::Fail, "{detail}");
}

#[test]
fn a_single_permission_violation_fails_and_no_security_run_is_not_measured() {
    let mut runs = vec![record(&Spec::new(Suite::Security, 1, "B", 100))];
    assert_eq!(gate(&runs, "B", "permission").0, GateStatus::Pass);
    let mut bad = Spec::new(Suite::Security, 2, "B", 100);
    bad.violations = 1;
    bad.passed = false;
    runs.push(record(&bad));
    assert_eq!(gate(&runs, "B", "permission").0, GateStatus::Fail);
    assert_eq!(gate(&runs, "C", "permission").0, GateStatus::NotMeasured);
}

#[test]
fn a_security_run_that_did_not_complete_is_not_a_pass() {
    let mut r = record(&Spec::new(Suite::Security, 1, "B", 100));
    r.status = RunStatus::Failed {
        error: "sven crashed".into(),
    };
    r.score = None;
    assert_eq!(gate(&[r], "B", "permission").0, GateStatus::NotMeasured);
}

#[test]
fn fallback_needs_runs_with_an_unreachable_provider_and_all_must_complete() {
    let runs = targeted(&[("A", 100), ("C", 40)], 3);
    assert_eq!(gate(&runs, "C", "fallback").0, GateStatus::NotMeasured);

    let mut probe = record(&Spec::new(Suite::ToolSelection, 1, "C", 40));
    probe.probe = Some("fallback".into());
    assert_eq!(gate(&[probe.clone()], "C", "fallback").0, GateStatus::Pass);
    probe.status = RunStatus::Failed {
        error: "exit 1".into(),
    };
    assert_eq!(gate(&[probe], "C", "fallback").0, GateStatus::Fail);
}

#[test]
fn cold_restart_survival_requires_every_restart_run_to_pass() {
    let mut runs = vec![
        record(&Spec::new(Suite::RestartSurvival, 1, "B", 100)),
        record(&Spec::new(Suite::RestartSurvival, 2, "B", 100)),
    ];
    assert_eq!(gate(&runs, "B", "restart").0, GateStatus::Pass);
    runs[1].score.as_mut().unwrap().passed = false;
    assert_eq!(gate(&runs, "B", "restart").0, GateStatus::Fail);
    assert_eq!(gate(&runs, "C", "restart").0, GateStatus::NotMeasured);
}

#[test]
fn a_success_regression_beyond_five_points_fails() {
    let mut runs = targeted(&[("A", 100), ("B", 40)], 5);
    assert_eq!(gate(&runs, "B", "success").0, GateStatus::Pass);
    for r in runs
        .iter_mut()
        .filter(|r| r.variant.id == "B" && r.seed <= 3)
    {
        r.score.as_mut().unwrap().passed = false;
    }
    assert_eq!(gate(&runs, "B", "success").0, GateStatus::Fail);
}

// ── the report ───────────────────────────────────────────────────────────────

#[test]
fn the_report_states_the_ratio_against_the_baseline_by_suite() {
    let runs = targeted(&[("A", 10_000), ("B", 4_000)], 5);
    let report = build(&matrix(), &runs);
    let suite = report
        .suites
        .iter()
        .find(|s| s.suite == Suite::RelevantFile)
        .unwrap();
    let a = suite.cells.iter().find(|c| c.variant == "A").unwrap();
    let b = suite.cells.iter().find(|c| c.variant == "B").unwrap();
    assert!(
        (a.r_tokens.unwrap().estimate - 1.0).abs() < 1e-9,
        "the baseline against itself"
    );
    let r = b.r_tokens.unwrap();
    assert!((2.4..2.6).contains(&r.estimate), "{r:?}");
    assert!(r.lower <= r.estimate && r.estimate <= r.upper);
    assert!(b.r_tokens_median.is_some() && b.tokens_total_median.is_some());
    assert_eq!(b.runs, 5);
}

#[test]
fn the_report_header_carries_the_scripted_notice_and_provenance() {
    let runs = targeted(&[("A", 100), ("B", 50)], 3);
    let report = build(&matrix(), &runs);
    assert!(report.scripted);
    assert!(report.notice.contains("no task-success claim"));
    assert_eq!(report.schema, "sven.ablation.report/1");
    assert_eq!(report.header.sven_commits, ["abc123"]);
    let md = sven_ablation::markdown::render(&report);
    assert!(md.contains("SCRIPTED RUNS"));
    assert!(md.contains("abc123"));
    assert_eq!(RUN_SCHEMA, "sven.ablation.run/1");
}

#[test]
fn a_report_is_reproducible_byte_for_byte() {
    let runs = targeted(&[("A", 10_000), ("B", 4_000), ("C", 3_900)], 4);
    let a = serde_json::to_string(&build(&matrix(), &runs)).unwrap();
    let b = serde_json::to_string(&build(&matrix(), &runs)).unwrap();
    assert_eq!(a, b);
}

#[test]
fn a_variant_that_was_not_available_is_listed_with_its_reason_and_has_no_numbers() {
    let mut runs = targeted(&[("A", 100), ("B", 50)], 3);
    let mut d = record(&Spec::new(Suite::ToolSelection, 1, "D", 50));
    d.status = RunStatus::NotAvailable {
        reason: "not available: pass --brain-url".into(),
    };
    d.score = None;
    d.tokens = None;
    runs.push(d);
    let report = build(&matrix(), &runs);
    let v = report.variants.iter().find(|v| v.id == "D").unwrap();
    assert_eq!(v.completed, 0);
    assert_eq!(v.not_available, ["not available: pass --brain-url"]);
    let suite = report
        .suites
        .iter()
        .find(|s| s.suite == Suite::ToolSelection)
        .unwrap();
    assert_eq!(
        suite.cells.iter().find(|c| c.variant == "D").unwrap().runs,
        0
    );
}

// ── does a model contribute ──────────────────────────────────────────────────

/// Tool-selection runs where each variant has the given MRR for every seed
/// (with a little spread), and the same tokens.
fn mrr_runs(mrrs: &[(&'static str, f64)], seeds: u64) -> Vec<RunRecord> {
    let mut runs = Vec::new();
    for seed in 1..=seeds {
        for (variant, mrr) in mrrs {
            let mut spec = Spec::new(Suite::ToolSelection, seed, variant, 1000);
            spec.mrr = Some((mrr + (seed as f64) * 0.01).min(1.0));
            runs.push(record(&spec));
        }
    }
    runs
}

fn verdict(runs: &[RunRecord], variant: &str) -> Verdict {
    build(&matrix(), runs)
        .verdicts
        .into_iter()
        .find(|v| v.variant == variant)
        .unwrap()
        .verdict
}

#[test]
fn a_model_that_beats_the_architecture_the_heuristic_and_the_random_control_contributes() {
    let runs = mrr_runs(
        &[
            ("A", 0.5),
            ("B", 0.30),
            ("C", 0.50),
            ("D", 0.80),
            ("F", 0.25),
        ],
        8,
    );
    match verdict(&runs, "D") {
        Verdict::ModelContributes { evidence } => {
            assert!(evidence.iter().any(|e| e.contains("Mrr")), "{evidence:?}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_model_no_better_than_the_heuristic_does_not_contribute() {
    let runs = mrr_runs(
        &[
            ("A", 0.5),
            ("B", 0.30),
            ("C", 0.80),
            ("D", 0.80),
            ("F", 0.25),
        ],
        8,
    );
    assert!(matches!(
        verdict(&runs, "D"),
        Verdict::DoesNotContribute { .. }
    ));
}

#[test]
fn a_model_no_better_than_random_does_not_contribute() {
    let runs = mrr_runs(
        &[
            ("A", 0.5),
            ("B", 0.30),
            ("C", 0.40),
            ("D", 0.50),
            ("F", 0.50),
        ],
        8,
    );
    assert!(matches!(
        verdict(&runs, "D"),
        Verdict::DoesNotContribute { .. }
    ));
}

#[test]
fn without_the_control_the_verdict_is_not_determinable() {
    let runs = mrr_runs(&[("A", 0.5), ("B", 0.30), ("C", 0.40), ("D", 0.90)], 8);
    assert!(matches!(
        verdict(&runs, "D"),
        Verdict::NotDeterminable { .. }
    ));
}

#[test]
fn a_model_variant_with_no_completed_runs_is_not_determinable() {
    let runs = mrr_runs(&[("A", 0.5), ("B", 0.30), ("C", 0.40), ("F", 0.2)], 8);
    assert!(matches!(
        verdict(&runs, "E"),
        Verdict::NotDeterminable { .. }
    ));
}

// ── recovery ─────────────────────────────────────────────────────────────────

/// A recovery run of `spec`: the model fetched what was left out.
fn recovery_run(spec: &Spec, before: usize, after: usize, expansions: usize) -> RunRecord {
    let mut r = record(&Spec {
        critical: (after, 2),
        ..Spec::new(spec.suite, spec.seed, spec.variant, spec.tokens)
    });
    r.probe = Some("recovery".into());
    let score = r.score.as_mut().unwrap();
    score.recovery = Some(sven_ablation::recovery::RecoveryScore {
        critical_total: 2,
        visible_before: before,
        visible_after: after,
        recall_before: before as f64 / 2.0,
        recall_after: after as f64 / 2.0,
        expansions,
    });
    r.tokens.as_mut().unwrap().recovery_tokens = 300;
    r
}

fn recovery_runs() -> Vec<RunRecord> {
    let mut runs = Vec::new();
    for seed in 1..=3 {
        runs.push(record(&Spec::new(Suite::RelevantFile, seed, "A", 1000)));
        let lost = Spec {
            critical: (0, 2),
            passed: false,
            ..Spec::new(Suite::RelevantFile, seed, "B", 500)
        };
        runs.push(record(&lost));
        runs.push(recovery_run(
            &Spec::new(Suite::RelevantFile, seed, "B", 700),
            0,
            2,
            1,
        ));
    }
    runs
}

#[test]
fn recovery_runs_stay_out_of_the_main_tables_and_get_their_own() {
    let report = build(&matrix(), &recovery_runs());
    let main = report.suites[0]
        .cells
        .iter()
        .find(|c| c.variant == "B")
        .unwrap();
    assert_eq!(main.runs, 3);
    assert_eq!(
        main.critical_recall,
        Some(0.0),
        "the model that never asks is unchanged"
    );
    let recovery = report
        .recovery
        .expect("recovery runs make a recovery section");
    let cell = recovery.suites[0]
        .cells
        .iter()
        .find(|c| c.variant == "B")
        .unwrap();
    assert_eq!(cell.runs, 3);
    assert_eq!(cell.recall_without_recovery, Some(0.0));
    assert_eq!(cell.recall_after_recovery, Some(1.0));
    assert_eq!(cell.expansions_mean, Some(1.0));
    assert_eq!(cell.expanded_tokens_mean, Some(300.0));
    let ratio = cell.r_tokens_median.unwrap();
    assert!(
        (ratio - 1000.0 / 700.0).abs() < 0.05,
        "baseline / recovery run: {ratio}"
    );
}

#[test]
fn the_recovery_gates_separate_recall_after_asking_from_recall_without() {
    let report = build(&matrix(), &recovery_runs());
    let grid = &report.recovery.unwrap().gates;
    let b = grid.iter().find(|g| g.variant == "B").unwrap();
    let status = |name: &str| {
        b.gates
            .iter()
            .find(|g| g.gate.contains(name))
            .unwrap()
            .status
    };
    assert_eq!(status("recall >= 99% without"), GateStatus::Fail);
    assert_eq!(
        b.gates
            .iter()
            .find(|g| g.gate == "critical evidence recall >= 99%")
            .unwrap()
            .status,
        GateStatus::Pass
    );
}

#[test]
fn a_report_without_recovery_runs_has_no_recovery_section() {
    let runs: Vec<RunRecord> = (1..=3)
        .map(|s| record(&Spec::new(Suite::RelevantFile, s, "A", 1000)))
        .collect();
    assert!(build(&matrix(), &runs).recovery.is_none());
}
