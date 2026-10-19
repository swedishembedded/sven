// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Spec: a real model's run is verified by what its final answer cites. The
//! answer succeeds when it cites every critical piece of evidence of the task;
//! citing a distractor is reported but does not fail it.

use sven_ablation::answer::score_answer;
use sven_ablation::suites::generate;
use sven_ablation::task::Suite;
use sven_ablation::trajectory::final_reply;

#[test]
fn an_answer_that_cites_all_critical_evidence_succeeds() {
    let task = generate(Suite::RelevantFile, 2).task;
    let critical: Vec<&str> = task.critical().map(|e| e.marker.as_str()).collect();
    let reply = format!(
        "The cutoff drops the last entry ({}) so callers see {}.",
        critical[0], critical[1]
    );
    let score = score_answer(&task, &reply);
    assert!(score.success);
    assert_eq!((score.critical_cited, score.critical_total), (2, 2));
    assert_eq!(score.distractors_cited, 0);
}

#[test]
fn missing_critical_evidence_fails_and_distractors_are_counted() {
    let task = generate(Suite::RelevantFile, 2).task;
    let critical = task.critical().next().unwrap().marker.clone();
    let distractor = task.distractors().next().unwrap().marker.clone();
    let score = score_answer(&task, &format!("{critical} and also {distractor}"));
    assert!(!score.success);
    assert_eq!(score.critical_cited, 1);
    assert_eq!(score.distractors_cited, 1);
}

#[test]
fn the_final_reply_is_the_last_agent_message_of_the_trajectory() {
    let trajectory = serde_json::json!({"steps": [
        {"step_id": 1, "source": "user", "message": "question"},
        {"step_id": 2, "source": "agent", "message": "first"},
        {"step_id": 3, "source": "user", "message": "next"},
        {"step_id": 4, "source": "agent", "message": "last answer"},
    ]});
    assert_eq!(final_reply(&trajectory).as_deref(), Some("last answer"));
    assert_eq!(final_reply(&serde_json::Value::Null), None);
}
