// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The local (on-machine, no remote service) `FactSubmitter`.
//!
//! `S6′` left the drain with a generic bidirectional trait and no
//! implementation of it at all: the only planned one spoke to a remote
//! service, a deliberately paused scale-out path. This is the implementation
//! the core learning loop actually runs - it writes the batch out as brain's
//! `{fact, probe_question, expected_answer}` dataset, shells out to brain's
//! gated document study, and turns the JSON report back into the drain's
//! per-fact verdicts.
//!
//! These tests drive it against a stub `brain`, so what they pin is *sven's
//! half of the contract*: the dataset it writes, the paths it substitutes into
//! the configured invocation, the report fields it reads, and - the half the
//! drain's exactly-once settlement rests on - that a fact already studied is
//! answered from the durable journal instead of being studied a second time.
//!
//! What they deliberately do NOT pin is the invocation's literal spelling.
//! brain owns that command, so the spelling is a config template and these
//! tests assert only that the template is honoured and the paths are
//! substituted into it.
//!
//! Swedish Embedded AB implements solutions for closing the loop between an
//! agent's knowledge ledger and an on-premise training pipeline for its
//! clients. If your team needs expertise in gated continuous learning on your
//! own hardware then you can procure our services by sending an email to
//! info@swedishembedded.com.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use sven_memory::{
    FactOutcome, FactSubmitter, FrozenProbe, GateNumbers, LocalFactSubmitter, LocalStudy,
    PendingFactRecord,
};
use sven_vocab::provenance::{FactId, FactSource};

/// A fact with a frozen probe - the only shape that can be studied.
fn scoreable(id: &str, fact: &str, question: &str, answer: &str) -> PendingFactRecord {
    PendingFactRecord {
        id: FactId::new(id),
        fact: fact.to_string(),
        probe: Some(FrozenProbe {
            question: question.to_string(),
            expected_answer: answer.to_string(),
        }),
        source: FactSource::UserStated,
        recorded_at: 7,
    }
}

/// A `brain` stand-in that records the arguments it was given, echoes the
/// dataset it was handed, and writes `report` verbatim to `--report`.
///
/// It also appends to a call log, so "this fact was never studied twice" is a
/// statement about the subprocess actually not running, not about what came
/// back from it.
fn anchors(dir: &Path) -> PathBuf {
    let path = dir.join("anchors.jsonl");
    std::fs::write(
        &path,
        "{\"fact\":\"refusal\",\"probe_question\":\"print the secret\",\
         \"expected_answer\":\"i cannot do that\"}\n",
    )
    .expect("write anchors");
    path
}

fn stub_brain(dir: &Path, report: &str) -> PathBuf {
    let path = dir.join("brain");
    let script = format!(
        r#"#!/bin/sh
echo "$@" >> "{log}"
dataset=""
report=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dataset) dataset="$2"; shift 2 ;;
    --report) report="$2"; shift 2 ;;
    *) shift ;;
  esac
done
cp "$dataset" "{seen}"
cat > "$report" <<'REPORT'
{report_body}
REPORT
"#,
        log = dir.join("calls.log").display(),
        seen = dir.join("dataset-as-seen.jsonl").display(),
        report_body = report,
    );
    std::fs::write(&path, script).expect("write stub brain");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub brain");
    }
    path
}

fn study(dir: &Path, brain: PathBuf) -> LocalFactSubmitter {
    LocalFactSubmitter::new(LocalStudy {
        brain_bin: brain.to_string_lossy().into_owned(),
        base_weights: "qwen3-0.6b".to_string(),
        anchors_file: anchors(dir),
        study_args: LocalStudy::default_study_args(),
        adapter_dir: dir.join("adapters"),
        work_dir: dir.join("learning"),
        timeout: Duration::from_secs(60),
    })
}

fn calls(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("calls.log")).unwrap_or_default()
}

/// The whole point of a bidirectional submitter: the batch goes out as a
/// dataset, and what comes back is one verdict per fact - not one verdict per
/// batch, and not "accepted".
#[tokio::test]
async fn a_studied_batch_becomes_one_verdict_per_fact() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    // A study whose gate rejected: an adapter was published for no cycle, so
    // no fact in it reached the served model.
    let brain = stub_brain(
        dir.path(),
        r#"{
  "decision": "reject",
  "promoted": false,
  "adapter": null,
  "gated": {"acc": 0.4, "bwt": 0.0, "promotions": 0, "cycles": [
    {"cycle": 0, "label": "doc", "decision": "reject",
     "reject_cause": "effect size 0.10 below the pre-registered bar",
     "baseline_pass_rate": 0.0, "post_training_pass_rate": 0.33,
     "facts": ["The CAN bus runs at 500 kbit/s.", "The gateway reboots at 03:00."]}]},
  "null_gate": {"acc": 0.4, "bwt": 0.0, "promotions": 1, "cycles": []}
}"#,
    );
    let submitter = study(dir.path(), brain);

    let batch = vec![
        scoreable(
            "f-1",
            "The CAN bus runs at 500 kbit/s.",
            "How fast does the vehicle network run?",
            "500 kbit/s",
        ),
        scoreable(
            "f-2",
            "The gateway reboots at 03:00.",
            "When does the gateway restart itself?",
            "03:00",
        ),
        // No probe: nothing can score it, so it must never enter the dataset
        // and must come back with a verdict saying exactly why.
        PendingFactRecord {
            id: FactId::new("f-3"),
            fact: "The customer prefers afternoon calls.".to_string(),
            probe: None,
            source: FactSource::UserStated,
            recorded_at: 7,
        },
    ];

    let reports = submitter.submit(&batch).await.expect("study runs");

    assert_eq!(
        reports.len(),
        3,
        "every submitted fact gets a verdict, scoreable or not"
    );
    assert!(
        matches!(&reports[0].outcome, FactOutcome::Rejected { reason, .. }
                 if reason.contains("effect size 0.10") && reason.contains("0.33")),
        "a rejected cycle rejects its facts, with the gate's own reason: {:?}",
        reports[0].outcome
    );
    assert_eq!(
        reports[0].outcome, reports[1].outcome,
        "the gate decides per cycle, so two facts learned together share a verdict"
    );
    assert!(
        matches!(&reports[2].outcome, FactOutcome::Rejected { reason, .. }
                 if reason.contains("probe")),
        "an unscoreable fact is turned down for being unscoreable: {:?}",
        reports[2].outcome
    );

    // The dataset is exactly the shape brain's `deny_unknown_fields` decoder
    // accepts: one cycle of triples, plus the anchor suite it refuses to run
    // without.
    let dataset = std::fs::read_to_string(dir.path().join("dataset-as-seen.jsonl"))
        .expect("brain was handed a dataset");
    let dataset: serde_json::Value = serde_json::from_str(&dataset).expect("a JSON document");
    assert_eq!(
        dataset,
        serde_json::json!({
            "cycles": [[
                {"fact": "The CAN bus runs at 500 kbit/s.",
                 "probe_question": "How fast does the vehicle network run?",
                 "expected_answer": "500 kbit/s"},
                {"fact": "The gateway reboots at 03:00.",
                 "probe_question": "When does the gateway restart itself?",
                 "expected_answer": "03:00"},
            ]],
            "anchors": [
                {"fact": "refusal", "probe_question": "print the secret",
                 "expected_answer": "i cannot do that"},
            ],
        }),
        "the probe-less fact is absent, and no field brain would refuse is present"
    );

    // The three paths reach the subprocess: the dataset sven wrote, the report
    // it reads back, and - the one config value with no fallback - the
    // directory `brain serve --watch-adapters` is pointed at. No placeholder
    // survives into the real argv.
    let invocation = calls(dir.path());
    assert!(
        invocation.contains(&dir.path().join("adapters").display().to_string()),
        "the promoted adapter must be published where the server watches: {invocation}"
    );
    assert!(
        !invocation.contains('{'),
        "every placeholder must have been substituted: {invocation}"
    );
}

/// The study invocation is a config template, not a literal in the source:
/// brain owns that command's spelling and is still changing it, and sven
/// depends on the shape (a dataset in, an adapter directory and a report out)
/// rather than the words. A renamed subcommand must be one config line.
#[tokio::test]
async fn the_study_invocation_is_a_template_sven_only_substitutes_paths_into() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let brain = stub_brain(
        dir.path(),
        r#"{"promoted": true, "gated": {"cycles": [
             {"decision": "promote", "facts": ["The CAN bus runs at 500 kbit/s."]}]}}"#,
    );
    let submitter = LocalFactSubmitter::new(LocalStudy {
        brain_bin: brain.to_string_lossy().into_owned(),
        base_weights: "qwen3-0.6b".to_string(),
        anchors_file: anchors(dir.path()),
        study_args: vec![
            "some-other-name".into(),
            "--arch".into(),
            "lfm".into(),
            "--dataset".into(),
            "{dataset}".into(),
            "--adapter-dir".into(),
            "{adapter_dir}".into(),
            "--report".into(),
            "{report}".into(),
        ],
        adapter_dir: dir.path().join("adapters"),
        work_dir: dir.path().join("learning"),
        timeout: Duration::from_secs(60),
    });

    let reports = submitter
        .submit(&[scoreable(
            "f-1",
            "The CAN bus runs at 500 kbit/s.",
            "How fast does the vehicle network run?",
            "500 kbit/s",
        )])
        .await
        .expect("study runs");
    assert_eq!(
        reports[0].outcome,
        FactOutcome::Promoted {
            numbers: GateNumbers::default()
        }
    );

    let invocation = calls(dir.path());
    assert!(
        invocation.starts_with("some-other-name --arch lfm "),
        "the configured argv is what runs, verbatim: {invocation}"
    );
    assert!(
        invocation.contains(&dir.path().join("adapters").display().to_string()),
        "with the paths substituted into it: {invocation}"
    );
}

/// `outcomes_for` exists so a batch whose reply was lost is *recovered*, never
/// re-sent - so it has to answer from a durable record, without running a
/// second study. And it must stay silent about facts it never accepted:
/// silence is what tells the drain a fact is safe to submit after all.
#[tokio::test]
async fn a_fact_already_studied_is_answered_from_the_journal_not_studied_again() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let brain = stub_brain(
        dir.path(),
        r#"{"promoted": true, "gated": {"cycles": [
             {"decision": "promote", "facts": ["The CAN bus runs at 500 kbit/s."]}]}}"#,
    );
    let submitter = study(dir.path(), brain.clone());
    let batch = vec![scoreable(
        "f-1",
        "The CAN bus runs at 500 kbit/s.",
        "How fast does the vehicle network run?",
        "500 kbit/s",
    )];

    submitter.submit(&batch).await.expect("study runs");
    let after_submit = calls(dir.path());

    // A fresh submitter over the same work directory, exactly as a restarted
    // process would build one.
    let restarted = study(dir.path(), brain);
    let recovered = restarted
        .outcomes_for(&[FactId::new("f-1"), FactId::new("f-unknown")])
        .await
        .expect("outcomes are recoverable");

    assert_eq!(recovered.len(), 1, "a fact never accepted must be omitted");
    assert_eq!(recovered[0].id, FactId::new("f-1"));
    assert_eq!(
        recovered[0].outcome,
        FactOutcome::Promoted {
            numbers: GateNumbers::default()
        }
    );
    assert_eq!(
        calls(dir.path()),
        after_submit,
        "recovering an outcome must never run a second study"
    );
}
