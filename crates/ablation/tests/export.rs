// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Training fixtures: auditable labels and a split that cannot leak.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use sven_ablation::export::{assign_splits, export, to_jsonl, LabelSource, Split, SplitPlan};
use sven_ablation::matrix::Matrix;
use sven_ablation::provider_server::{ServedDecision, ServedQuestion};
use sven_ablation::record::{Provenance, RunRecord, RunStatus, TokenRecord};
use sven_ablation::score::Score;
use sven_ablation::suites::generate;
use sven_ablation::task::Suite;

const MATRIX: &str = include_str!("../../../benchmarks/ablation/matrix.yaml");

fn provenance() -> Provenance {
    Provenance {
        sven_commit: "abc".into(),
        sven_dirty: false,
        sven_binary: "sven".into(),
        brain: None,
        matrix_sha256: "m".into(),
    }
}

/// Writes one run of `variant` under `out` as the runner does, with the
/// decisions its provider served.
fn write_run(
    out: &Path,
    suite: Suite,
    seed: u64,
    variant: &str,
    tokens: u64,
    with_decisions: bool,
) {
    let matrix = Matrix::from_yaml(MATRIX).unwrap();
    let v = matrix.variants.iter().find(|v| v.id == variant).unwrap();
    let generated = generate(suite, seed);
    let dir = out.join("runs").join(&generated.task.id).join(variant);
    generated.write(&dir).unwrap();
    let mut record = RunRecord::new(
        &generated.task.id,
        suite,
        seed,
        v,
        provenance(),
        RunStatus::Completed,
    );
    record.tokens = Some(TokenRecord {
        total: tokens,
        recovery_expansions: 2,
        ..TokenRecord::default()
    });
    record.score = Some(Score {
        suite,
        passed: true,
        hard_failures: Vec::new(),
        labels: Vec::new(),
        evidence: None,
        shortlist: None,
        security: None,
    });
    std::fs::write(
        dir.join("record.json"),
        serde_json::to_string(&record).unwrap(),
    )
    .unwrap();
    if with_decisions {
        let probe = &generated.task.probes[0];
        // The model was shown eight tools in name order; the right one is among them.
        let mut options: Vec<(String, Option<String>)> = (0..7)
            .map(|n| {
                (
                    format!("catalog-other_{n}"),
                    Some(format!("Does thing {n}.")),
                )
            })
            .collect();
        options.push((probe.target.clone(), Some("The right one.".into())));
        options.sort();
        let served = |opts: Vec<(String, Option<String>)>| ServedDecision {
            inputs_digest: "d1".into(),
            state: format!("Task: {}", probe.query),
            questions: vec![ServedQuestion {
                id: "tool".into(),
                kind: "choice".into(),
                instruction: "Which option fits the task best?".into(),
                options: opts,
                top: None,
                top_probability: None,
            }],
        };
        let mut reversed = options.clone();
        reversed.reverse();
        // Also a question no label covers.
        let mut unlabelled = served(options.clone());
        unlabelled.state = "Task: something nobody labelled".into();
        let lines = [served(options), served(reversed), unlabelled];
        let text: Vec<String> = lines
            .iter()
            .map(|d| serde_json::to_string(d).unwrap())
            .collect();
        std::fs::write(dir.join("decisions.jsonl"), text.join("\n") + "\n").unwrap();
    }
}

fn dir_with_runs(seeds: &[u64]) -> tempfile::TempDir {
    let out = tempfile::tempdir().unwrap();
    for seed in seeds {
        write_run(out.path(), Suite::ToolSelection, *seed, "A", 10_000, false);
        for variant in ["C", "G"] {
            write_run(
                out.path(),
                Suite::ToolSelection,
                *seed,
                variant,
                4_000,
                true,
            );
        }
    }
    out
}

#[test]
fn an_example_is_a_state_options_and_the_labelled_option_with_its_provenance() {
    let out = dir_with_runs(&[1]);
    let examples = export(out.path(), &SplitPlan::default()).unwrap();
    // One example per variant that served decisions; the second option order
    // and the unlabelled question add none.
    assert_eq!(examples.len(), 2, "{examples:#?}");
    let e = &examples[0];
    assert_eq!(e.schema, "sven.ablation.fixture/1");
    assert_eq!(e.decision_type, "tool_routing");
    assert_eq!(e.label_source, LabelSource::Oracle);
    assert_eq!(e.options.len(), 8);
    assert_eq!(e.options[e.label_index].id, e.label);
    assert!(e.options[e.label_index].description.as_deref() == Some("The right one."));
    assert!(e.question.state.starts_with("Task: "));
    assert_eq!(e.state_ref.task, "tool_selection-s1");
    assert_eq!(e.state_ref.inputs_digest, "d1");
    assert_eq!(e.outcome.llm_tokens_saved, Some(6_000));
    assert_eq!(e.outcome.recovery_calls, 2);
    assert!(e.outcome.task_passed);
}

#[test]
fn the_output_is_json_lines_that_read_back() {
    let out = dir_with_runs(&[1]);
    let text = to_jsonl(&export(out.path(), &SplitPlan::default()).unwrap()).unwrap();
    for line in text.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        for key in [
            "decision_type",
            "state_ref",
            "question",
            "options",
            "label",
            "label_index",
            "label_source",
            "outcome",
            "split",
        ] {
            assert!(v.get(key).is_some(), "{key}");
        }
        assert_eq!(v["label_source"], "oracle");
    }
}

#[test]
fn every_example_of_a_task_is_in_one_split_whatever_the_variant() {
    let out = dir_with_runs(&[1, 2, 3, 4, 5]);
    let examples = export(out.path(), &SplitPlan::default()).unwrap();
    let mut by_task: BTreeMap<&str, BTreeSet<Split>> = BTreeMap::new();
    for e in &examples {
        by_task
            .entry(&e.state_ref.task)
            .or_default()
            .insert(e.split);
    }
    assert_eq!(by_task.len(), 5);
    assert!(by_task.values().all(|s| s.len() == 1), "{by_task:?}");
    let splits: BTreeSet<Split> = by_task.values().flatten().copied().collect();
    assert_eq!(
        splits.len(),
        3,
        "five seeds fill train, validation and test"
    );
}

#[test]
fn the_same_seed_gives_the_same_split_and_another_seed_may_not() {
    let groups: BTreeSet<(Suite, u64)> = (1..=10).map(|s| (Suite::ToolSelection, s)).collect();
    let a = assign_splits(
        &groups,
        &SplitPlan {
            seed: 1,
            ..SplitPlan::default()
        },
    );
    assert_eq!(
        a,
        assign_splits(
            &groups,
            &SplitPlan {
                seed: 1,
                ..SplitPlan::default()
            }
        )
    );
    assert_ne!(
        a,
        assign_splits(
            &groups,
            &SplitPlan {
                seed: 2,
                ..SplitPlan::default()
            }
        )
    );
    let count = |s: Split| a.values().filter(|x| **x == s).count();
    assert_eq!(
        (
            count(Split::Test),
            count(Split::Validation),
            count(Split::Train)
        ),
        (2, 2, 6)
    );
}

#[test]
fn a_family_with_fewer_than_three_seeds_is_all_train_and_a_holdout_family_is_all_test() {
    let groups: BTreeSet<(Suite, u64)> = [
        (Suite::ToolSelection, 1),
        (Suite::ToolSelection, 2),
        (Suite::RelevantFile, 1),
        (Suite::RelevantFile, 2),
        (Suite::RelevantFile, 3),
    ]
    .into();
    let plan = SplitPlan {
        holdout_suites: vec![Suite::RelevantFile],
        ..SplitPlan::default()
    };
    let s = assign_splits(&groups, &plan);
    assert!(s
        .iter()
        .filter(|((suite, _), _)| *suite == Suite::ToolSelection)
        .all(|(_, v)| *v == Split::Train));
    assert!(s
        .iter()
        .filter(|((suite, _), _)| *suite == Suite::RelevantFile)
        .all(|(_, v)| *v == Split::Test));
}

#[test]
fn a_run_without_served_decisions_contributes_nothing() {
    let out = tempfile::tempdir().unwrap();
    write_run(out.path(), Suite::ToolSelection, 1, "A", 10_000, false);
    write_run(out.path(), Suite::ToolSelection, 1, "B", 4_000, false);
    assert!(export(out.path(), &SplitPlan::default())
        .unwrap()
        .is_empty());
}
