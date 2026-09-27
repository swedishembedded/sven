// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements delegated-task coding agents that feed
// their verified experience to a gated training path. If your team needs
// expertise in supervised fine-tuning pipelines, you can procure our
// services by sending an email to info@swedishembedded.com.

//! The loop's evidence pool: verified runs become chat-training records.
//!
//! Only a run the reviewer could already trust is worth training on: the
//! attempt completed AND its own completion checks passed. Anything less -
//! a failure, a timeout, a run with no check evidence - is refused with the
//! reason, because a model fine-tuned on unverified transcripts learns to
//! sound finished, not to be finished.
//!
//! The pool is one JSONL file, appended one record per learned run and
//! validated against the exact schema the trainer's parser enforces
//! (`brain::validate_chat_dataset`), so `learn` cannot write a pool the
//! `train` step will later reject over shape.

use crate::outcome::Status;
use crate::store;

/// The pool file every learned record is appended to.
pub(crate) fn pool_path() -> std::path::PathBuf {
    store::state_root()
        .join("datasets")
        .join("experience.jsonl")
}

/// A run is learning evidence only when the outcome says completed and at
/// least one completion check actually passed. The manifest's status alone
/// is not enough: it tracks the latest attempt, while the outcome carries
/// the check evidence.
fn verified(run_id: &str, dir: &std::path::Path) -> anyhow::Result<crate::outcome::Outcome> {
    let outcome = crate::outcome::Outcome::load(dir)?;
    anyhow::ensure!(
        outcome.status == Status::Completed,
        "run {run_id} is {} - only a completed run is learning evidence",
        outcome.status.as_str()
    );
    anyhow::ensure!(
        outcome.checks.iter().any(|c| c.passed),
        "run {run_id} passed no completion check - evidence without verification is not learning material"
    );
    Ok(outcome)
}

/// One run becomes one chat record: the task as the user message (context,
/// not supervised) and the final assistant reply as the supervised turn.
/// The reply is the outcome's verbatim `reply`, not a transcript guess.
fn record(run_id: &str, outcome: &crate::outcome::Outcome) -> anyhow::Result<serde_json::Value> {
    let task = store::read_manifest(run_id)?.task;
    let reply = outcome
        .reply
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty());
    anyhow::ensure!(
        reply.is_some(),
        "run {run_id} has no final reply to supervise"
    );
    let passed: Vec<&str> = outcome
        .checks
        .iter()
        .filter(|c| c.passed)
        .map(|c| c.command.as_str())
        .collect();
    Ok(serde_json::json!({
        "messages": [
            { "role": "user", "content": task.trim(), "train": false },
            { "role": "assistant", "content": reply.unwrap_or_default(), "train": true },
        ],
        "metadata": {
            "run_id": run_id,
            "verified_by": passed,
        },
    }))
}

/// Learn one run: verify it, append its record to the pool, and return what
/// was appended. Learning the same run twice is a no-op that says so - the
/// pool is keyed by run id, and a duplicated record would weight that
/// experience twice in every training run.
pub(crate) fn learn_run(run_id: &str) -> anyhow::Result<Learned> {
    let pool = pool_path();
    if pool.exists() && pool_has_run(&pool, run_id)? {
        return Ok(Learned::AlreadyRecorded);
    }
    let dir = store::run_dir(run_id);
    let outcome = verified(run_id, &dir)?;
    let line = record(run_id, &outcome)?;
    if let Some(parent) = pool.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = serde_json::to_string(&line)?;
    text.push('\n');
    // Appends only ever grow the pool, one line per learned run; O_APPEND
    // keeps a concurrent writer from splitting a line.
    use std::io::Write;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&pool)?
        .write_all(text.as_bytes())?;
    Ok(Learned::Appended)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Learned {
    Appended,
    AlreadyRecorded,
}

fn pool_has_run(pool: &std::path::Path, run_id: &str) -> anyhow::Result<bool> {
    let text = std::fs::read_to_string(pool)?;
    for line in text.lines() {
        let v: serde_json::Value = serde_json::from_str(line)?;
        if v.get("metadata")
            .and_then(|m| m.get("run_id"))
            .and_then(|r| r.as_str())
            == Some(run_id)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Read every record in the pool. A pool the trainer's own parser refuses is
/// an error here, not a silent skip - `train` is about to pay for a device
/// and a run, and the shape check is free now.
pub(crate) fn read_pool(pool: &std::path::Path) -> anyhow::Result<Vec<data::chat::ChatSample>> {
    let summary = brain::validate_chat_dataset(pool).map_err(|e| {
        anyhow::anyhow!(
            "dataset pool {} is not valid trainer input: {e}",
            pool.display()
        )
    })?;
    anyhow::ensure!(
        summary.trained_messages > 0,
        "dataset pool {} holds no supervised turns",
        pool.display()
    );
    Ok(data::chat::ChatSample::from_jsonl(pool)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::{Check, Outcome, Status, Usage};
    use crate::store::RunManifest;

    /// SVEN_LOOP_STATE is process-global, and cargo runs these tests in
    /// parallel threads: every fixture/learn sequence holds
    /// [`crate::store::ENV_LOCK`] so one test's state root cannot leak
    /// into another's.
    use crate::store::ENV_LOCK;

    fn fixture_run(
        state: &std::path::Path,
        run_id: &str,
        status: Status,
        checks: Vec<Check>,
        reply: &str,
    ) {
        std::env::set_var("SVEN_LOOP_STATE", state);
        let dir = store::run_dir(run_id);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = RunManifest {
            run_id: run_id.into(),
            task: "fix the add function".into(),
            status: status.as_str().into(),
            ..Default::default()
        };
        store::write_atomic(
            &dir.join("run.json"),
            &serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        let outcome = Outcome {
            schema: 1,
            run_id: run_id.into(),
            status,
            reply: Some(reply.into()),
            changed_files: vec![],
            changed_files_basis: "tool_evidence".into(),
            checks,
            tool_failures: vec![],
            usage: Usage::default(),
            unresolved: vec![],
            artifacts: vec![],
        };
        outcome.save(&dir).unwrap();
    }

    fn passing_check() -> Vec<Check> {
        vec![Check {
            command: "sh tests/run.sh".into(),
            exit: 0,
            passed: true,
            output_ref: None,
        }]
    }

    #[test]
    fn a_verified_run_becomes_a_user_task_and_a_supervised_reply() {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("loop-learn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        fixture_run(
            &root,
            "loop-learn-ok",
            Status::Completed,
            passing_check(),
            "fixed add to return the sum.",
        );
        let learned = learn_run("loop-learn-ok").unwrap();
        assert_eq!(learned, Learned::Appended);
        let pool = pool_path();
        let text = std::fs::read_to_string(&pool).unwrap();
        let v: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["train"], false);
        assert_eq!(msgs[0]["content"], "fix the add function");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["train"], true);
        assert_eq!(msgs[1]["content"], "fixed add to return the sum.");
        std::env::remove_var("SVEN_LOOP_STATE");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unverified_run_is_refused_not_learned() {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("loop-learn-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // A failed run, and a completed run without check evidence: neither
        // is verified, and each refusal must say which requirement failed.
        fixture_run(
            &root,
            "loop-learn-bad",
            Status::Failed,
            passing_check(),
            "tried and failed",
        );
        assert!(learn_run("loop-learn-bad").is_err());

        fixture_run(
            &root,
            "loop-learn-nocheck",
            Status::Completed,
            vec![],
            "said done",
        );
        let err = learn_run("loop-learn-nocheck").unwrap_err().to_string();
        assert!(
            err.contains("no completion check"),
            "refusal must name the missing verification: {err}"
        );
        assert!(!pool_path().exists());
        std::env::remove_var("SVEN_LOOP_STATE");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn learning_the_same_run_twice_records_it_once() {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("loop-learn-dup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        fixture_run(
            &root,
            "loop-learn-dup",
            Status::Completed,
            passing_check(),
            "done",
        );
        assert_eq!(learn_run("loop-learn-dup").unwrap(), Learned::Appended);
        assert_eq!(
            learn_run("loop-learn-dup").unwrap(),
            Learned::AlreadyRecorded
        );
        let summary = brain::validate_chat_dataset(pool_path()).unwrap();
        assert_eq!(
            summary.records, 1,
            "a repeated learn must not weight the run twice"
        );
        std::env::remove_var("SVEN_LOOP_STATE");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_pool_parses_as_trainer_input() {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("loop-learn-pool-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        fixture_run(
            &root,
            "loop-learn-p1",
            Status::Completed,
            passing_check(),
            "first fix",
        );
        fixture_run(
            &root,
            "loop-learn-p2",
            Status::Completed,
            passing_check(),
            "second fix",
        );
        learn_run("loop-learn-p1").unwrap();
        learn_run("loop-learn-p2").unwrap();
        let samples = read_pool(&pool_path()).unwrap();
        assert_eq!(samples.len(), 2);
        assert!(samples[0].messages[1].train);
        std::env::remove_var("SVEN_LOOP_STATE");
        let _ = std::fs::remove_dir_all(&root);
    }
}
