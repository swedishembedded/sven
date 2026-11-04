// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements trajectory formats that keep what an agent
// decided and what it remembered auditable, for its clients. If your team needs
// expertise in agent audit trails you can procure our services by sending an
// email to info@swedishembedded.com.

//! The `extra.sven.gate`, `extra.sven.memory` and `extra.sven.timing` records: their wire shape,
//! and the rules `validate_trajectory` enforces on them.

use atif::{
    validate_trajectory, AgentProfile, ArgumentViolation, CallTiming, GateAuthority, GateDecision,
    GateOutcome, GateRecord, MemoryAnchor, MemoryRecord, MemoryResult, MemoryRetrieval, StepOrigin,
    TimingRecord, ToolInvocation, TraceStep, Trajectory, ValidationError,
};
use serde_json::{json, Value};

fn decision(call_id: &str) -> GateDecision {
    GateDecision {
        decision_id: format!("decision-{call_id}"),
        call_id: call_id.to_string(),
        capability: "ExecuteShell".to_string(),
        policy: "requires_approval".to_string(),
        outcome: GateOutcome::Denied,
        decided_by: GateAuthority::Human,
        reason: Some("not in this directory".to_string()),
        violations: Vec::new(),
    }
}

fn retrieval(call_id: &str) -> MemoryRetrieval {
    MemoryRetrieval {
        call_id: call_id.to_string(),
        query: "retry policy".to_string(),
        space_ids: vec!["space-1".to_string()],
        generation_id: "gen-a".to_string(),
        snapshot_id: None,
        results: vec![MemoryResult {
            unit_id: "unit-7".to_string(),
            revision: "sha256:abc".to_string(),
            anchor: MemoryAnchor {
                heading_path: vec!["Retries".to_string()],
                block_ordinal: 2,
                text_fingerprint: "f00d".to_string(),
            },
            score: 0.83,
            rank: 1,
            inserted: true,
        }],
    }
}

/// A trajectory whose only agent step makes the call `call_1`.
fn trajectory_with(mutate: impl FnOnce(&mut TraceStep)) -> Trajectory {
    let mut step = TraceStep::new(1, StepOrigin::Agent, "done");
    step.tool_calls = Some(vec![ToolInvocation::new("call_1", "shell")]);
    mutate(&mut step);
    let mut t = Trajectory::new("ATIF-v1.7", AgentProfile::new("sven", "1.0.0"));
    t.steps.push(step);
    t
}

#[test]
fn a_gate_record_is_written_under_extra_sven_gate_with_its_schema() {
    let mut extra = None;
    GateRecord::new(vec![decision("call_1")]).insert_into_extra(&mut extra);
    let extra = extra.unwrap();
    let gate = &extra["sven"]["gate"];
    assert_eq!(gate["schema"], "sven.gate.v1");
    let d = &gate["decisions"][0];
    assert_eq!(d["call_id"], "call_1");
    assert_eq!(d["decision"], "denied");
    assert_eq!(d["decided_by"], "human");
    assert_eq!(d["reason"], "not in this directory");
    assert!(
        d.get("violations").is_none(),
        "empty violations are omitted"
    );
}

#[test]
fn records_share_the_sven_namespace_without_replacing_each_other() {
    let mut extra = Some(json!({"sven": {"tokens_before": 10}, "other": 1}));
    GateRecord::new(vec![decision("call_1")]).insert_into_extra(&mut extra);
    MemoryRecord::new(vec![retrieval("call_1")]).insert_into_extra(&mut extra);
    let extra = extra.unwrap();
    assert_eq!(extra["sven"]["tokens_before"], 10);
    assert_eq!(extra["other"], 1);
    assert_eq!(extra["sven"]["gate"]["schema"], "sven.gate.v1");
    assert_eq!(extra["sven"]["memory"]["schema"], "sven.memory.v1");
}

#[test]
fn a_well_formed_gate_and_memory_record_validates() {
    let t = trajectory_with(|step| {
        GateRecord::new(vec![decision("call_1")]).insert_into_extra(&mut step.extra);
        MemoryRecord::new(vec![retrieval("call_1")]).insert_into_extra(&mut step.extra);
    });
    assert_eq!(validate_trajectory(&t), Ok(()));
}

#[test]
fn a_gate_record_survives_a_json_round_trip() {
    let mut original = decision("call_1");
    original.violations = vec![ArgumentViolation {
        path: "/path".to_string(),
        rule: "required".to_string(),
        message: "\"path\" is a required property".to_string(),
    }];
    let record = GateRecord::new(vec![original]);
    let back: GateRecord = serde_json::from_str(&serde_json::to_string(&record).unwrap()).unwrap();
    assert_eq!(back, record);
}

#[test]
fn a_gate_decision_naming_a_call_the_step_did_not_make_is_refused() {
    let t = trajectory_with(|step| {
        GateRecord::new(vec![decision("call_9")]).insert_into_extra(&mut step.extra);
    });
    let errs = validate_trajectory(&t).unwrap_err();
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::DanglingSvenCallId { step_id: 1, record: "sven.gate", call_id }
                if call_id == "call_9"
        )),
        "{errs:?}"
    );
}

#[test]
fn a_memory_retrieval_naming_a_call_the_step_did_not_make_is_refused() {
    let t = trajectory_with(|step| {
        MemoryRecord::new(vec![retrieval("call_9")]).insert_into_extra(&mut step.extra);
    });
    let errs = validate_trajectory(&t).unwrap_err();
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::DanglingSvenCallId {
                record: "sven.memory",
                ..
            }
        )),
        "{errs:?}"
    );
}

#[test]
fn an_unknown_schema_version_is_refused_not_guessed_at() {
    let t = trajectory_with(|step| {
        step.extra = Some(json!({"sven": {"gate": {"schema": "sven.gate.v9", "decisions": []}}}));
    });
    let errs = validate_trajectory(&t).unwrap_err();
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::UnknownSvenSchema { record: "sven.gate", found, .. } if found == "sven.gate.v9"
        )),
        "{errs:?}"
    );
}

#[test]
fn a_decider_outside_the_closed_set_is_refused() {
    let mut record: Value =
        serde_json::to_value(GateRecord::new(vec![decision("call_1")])).unwrap();
    record["decisions"][0]["decided_by"] = json!("the_model");
    let t = trajectory_with(|step| step.extra = Some(json!({"sven": {"gate": record}})));
    let errs = validate_trajectory(&t).unwrap_err();
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::InvalidSvenRecord {
                record: "sven.gate",
                ..
            }
        )),
        "{errs:?}"
    );
}

#[test]
fn a_memory_result_missing_its_unit_id_is_refused() {
    let mut record: Value =
        serde_json::to_value(MemoryRecord::new(vec![retrieval("call_1")])).unwrap();
    record["retrievals"][0]["results"][0]
        .as_object_mut()
        .unwrap()
        .remove("unit_id");
    let t = trajectory_with(|step| step.extra = Some(json!({"sven": {"memory": record}})));
    let errs = validate_trajectory(&t).unwrap_err();
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::InvalidSvenRecord {
                record: "sven.memory",
                ..
            }
        )),
        "{errs:?}"
    );
}

fn timing(call_id: &str, duration_ms: f64) -> CallTiming {
    CallTiming {
        call_id: call_id.to_string(),
        duration_ms,
    }
}

#[test]
fn a_timing_record_is_written_under_extra_sven_timing_and_validates() {
    let t = trajectory_with(|step| {
        TimingRecord::new(vec![timing("call_1", 12.5)]).insert_into_extra(&mut step.extra);
        GateRecord::new(vec![decision("call_1")]).insert_into_extra(&mut step.extra);
    });
    let extra = t.steps[0].extra.as_ref().unwrap();
    assert_eq!(extra["sven"]["timing"]["schema"], "sven.timing.v1");
    assert_eq!(extra["sven"]["timing"]["calls"][0]["call_id"], "call_1");
    assert_eq!(extra["sven"]["timing"]["calls"][0]["duration_ms"], 12.5);
    assert_eq!(extra["sven"]["gate"]["schema"], "sven.gate.v1");
    assert_eq!(validate_trajectory(&t), Ok(()));
    let back = TimingRecord::from_extra(extra).unwrap();
    assert_eq!(back.calls, vec![timing("call_1", 12.5)]);
}

#[test]
fn a_timing_naming_a_call_the_step_did_not_make_or_a_negative_duration_is_refused() {
    let dangling = trajectory_with(|step| {
        TimingRecord::new(vec![timing("call_9", 1.0)]).insert_into_extra(&mut step.extra);
    });
    let errs = validate_trajectory(&dangling).unwrap_err();
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::DanglingSvenCallId { record: "sven.timing", call_id, .. }
                if call_id == "call_9"
        )),
        "{errs:?}"
    );
    let negative = trajectory_with(|step| {
        TimingRecord::new(vec![timing("call_1", -1.0)]).insert_into_extra(&mut step.extra);
    });
    let errs = validate_trajectory(&negative).unwrap_err();
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::InvalidSvenRecord {
                record: "sven.timing",
                ..
            }
        )),
        "{errs:?}"
    );
}
