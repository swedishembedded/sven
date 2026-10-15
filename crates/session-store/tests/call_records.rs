// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent audit trails that keep every permission
// decision and every memory read reproducible, for its clients. If your team
// needs expertise in auditable agent runtimes you can procure our services by
// sending an email to info@swedishembedded.com.

//! What the step assembler records about the calls a step made: the gate's
//! decisions and the memory they read.

use atif::{
    validate_trajectory, ArgumentViolation, GateAuthority, GateDecision, GateOutcome, GateRecord,
    MemoryAnchor, MemoryRecord, MemoryResult, MemoryRetrieval, TraceStep,
};
use sven_model::{FunctionCall, Message, MessageContent, Role};
use sven_session_store::StepAssembler;

fn call(id: &str, name: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: MessageContent::ToolCall {
            tool_call_id: id.into(),
            function: FunctionCall {
                name: name.into(),
                arguments: "{}".into(),
            },
        },
    }
}

fn decision(call_id: &str, outcome: GateOutcome, decided_by: GateAuthority) -> GateDecision {
    GateDecision {
        decision_id: format!("decision-{call_id}"),
        call_id: call_id.into(),
        capability: "ExecuteShell".into(),
        policy: "requires_approval".into(),
        outcome,
        decided_by,
        reason: None,
        violations: Vec::new(),
    }
}

fn retrieval(call_id: &str) -> MemoryRetrieval {
    MemoryRetrieval {
        call_id: call_id.into(),
        query: "retry policy".into(),
        space_ids: vec!["space-1".into()],
        generation_id: "gen-a".into(),
        snapshot_id: None,
        results: vec![MemoryResult {
            unit_id: "unit-7".into(),
            revision: "sha256:abc".into(),
            anchor: MemoryAnchor {
                heading_path: vec!["Retries".into()],
                block_ordinal: 2,
                text_fingerprint: "f00d".into(),
            },
            score: 0.83,
            rank: 1,
            inserted: true,
        }],
    }
}

/// A user turn, then one agent step that makes `ids` and gets each answered.
fn conversation(ids: &[&str]) -> Vec<Message> {
    let mut messages = vec![Message::user("go")];
    messages.extend(ids.iter().map(|id| call(id, "shell")));
    messages.extend(ids.iter().map(|id| Message::tool_result(*id, "ok")));
    messages
}

fn steps_with(
    decisions: Vec<GateDecision>,
    retrievals: Vec<MemoryRetrieval>,
    messages: &[Message],
) -> Vec<TraceStep> {
    let mut assembler = StepAssembler::new();
    decisions
        .into_iter()
        .for_each(|d| assembler.push_gate_decision(d));
    retrievals
        .into_iter()
        .for_each(|r| assembler.push_memory_retrieval(r));
    messages.iter().for_each(|m| assembler.push_message(m));
    assembler.finish()
}

fn validated(steps: Vec<TraceStep>) -> Vec<TraceStep> {
    let mut trajectory = atif::Trajectory::new("ATIF-v1.7", atif::AgentProfile::new("sven", "1"));
    trajectory.steps = steps;
    assert_eq!(validate_trajectory(&trajectory), Ok(()));
    trajectory.steps
}

#[test]
fn trajectory_records_every_gate_decision_with_its_call_id() {
    let steps = steps_with(
        vec![
            decision("call_a", GateOutcome::Approved, GateAuthority::Human),
            decision("call_b", GateOutcome::Denied, GateAuthority::Policy),
        ],
        Vec::new(),
        &conversation(&["call_a", "call_b"]),
    );
    let steps = validated(steps);

    let record = GateRecord::from_extra(steps[1].extra.as_ref().expect("agent step extra"))
        .expect("a gate record");
    assert_eq!(record.schema, "sven.gate.v1");
    let seen: Vec<_> = record
        .decisions
        .iter()
        .map(|d| (d.call_id.as_str(), d.outcome, d.decided_by))
        .collect();
    assert_eq!(
        seen,
        [
            ("call_a", GateOutcome::Approved, GateAuthority::Human),
            ("call_b", GateOutcome::Denied, GateAuthority::Policy),
        ]
    );
    assert!(steps[0].extra.is_none(), "the user step carries no record");
}

#[test]
fn a_step_whose_calls_were_never_gated_carries_no_gate_record() {
    let steps = steps_with(Vec::new(), Vec::new(), &conversation(&["call_a"]));
    assert!(steps[1].extra.is_none());
}

#[test]
fn a_schema_invalid_tool_call_is_recorded_with_a_typed_violation() {
    let mut refused = decision("call_a", GateOutcome::Approved, GateAuthority::Policy);
    refused.policy = "allowed".into();
    refused.violations = vec![ArgumentViolation {
        path: "".into(),
        rule: "required".into(),
        message: "\"command\" is a required property".into(),
    }];
    let steps = validated(steps_with(
        vec![refused],
        Vec::new(),
        &conversation(&["call_a"]),
    ));

    let record = GateRecord::from_extra(steps[1].extra.as_ref().unwrap()).unwrap();
    let violation = &record.decisions[0].violations[0];
    assert_eq!(violation.rule, "required");
    assert!(violation.message.contains("command"), "{violation:?}");
}

#[test]
fn memory_use_is_recorded_with_unit_ids_and_generation() {
    let steps = validated(steps_with(
        Vec::new(),
        vec![retrieval("call_a")],
        &conversation(&["call_a"]),
    ));

    let record = MemoryRecord::from_extra(steps[1].extra.as_ref().unwrap()).unwrap();
    assert_eq!(record.schema, "sven.memory.v1");
    let retrieval = &record.retrievals[0];
    assert_eq!(retrieval.call_id, "call_a");
    assert_eq!(retrieval.generation_id, "gen-a");
    assert_eq!(retrieval.results[0].unit_id, "unit-7");
    assert!(retrieval.results[0].inserted);
}

#[test]
fn a_record_arriving_after_its_step_closed_still_lands_on_that_step() {
    let mut assembler = StepAssembler::new();
    conversation(&["call_a"])
        .iter()
        .for_each(|m| assembler.push_message(m));
    // A new user turn closes the agent step that made the call.
    assembler.push_message(&Message::user("next"));
    assembler.push_gate_decision(decision(
        "call_a",
        GateOutcome::Approved,
        GateAuthority::Auto,
    ));
    let steps = validated(assembler.finish());

    let record = GateRecord::from_extra(steps[1].extra.as_ref().unwrap()).unwrap();
    assert_eq!(record.decisions[0].decided_by, GateAuthority::Auto);
}

#[test]
fn a_record_for_a_call_still_pending_lands_when_the_step_closes() {
    let mut assembler = StepAssembler::new();
    assembler.push_message(&Message::user("go"));
    assembler.push_message(&call("call_a", "shell"));
    assembler.push_gate_decision(decision(
        "call_a",
        GateOutcome::Denied,
        GateAuthority::HandlerDropped,
    ));
    let snapshot = assembler.snapshot_including_pending();
    let record = GateRecord::from_extra(snapshot[1].extra.as_ref().unwrap()).unwrap();
    assert_eq!(
        record.decisions[0].decided_by,
        GateAuthority::HandlerDropped
    );
}

#[test]
fn a_decision_for_a_call_that_never_appears_does_not_corrupt_the_trajectory() {
    let steps = validated(steps_with(
        vec![decision(
            "ghost",
            GateOutcome::Denied,
            GateAuthority::Policy,
        )],
        Vec::new(),
        &conversation(&["call_a"]),
    ));
    assert!(steps[1].extra.is_none());
}

#[test]
fn gate_and_memory_records_share_the_step_and_the_compaction_namespace() {
    let mut assembler = StepAssembler::new();
    assembler.push_context_compacted(100, 40, Some("summary"), Some(1));
    conversation(&["call_a"])
        .iter()
        .for_each(|m| assembler.push_message(m));
    assembler.push_gate_decision(decision(
        "call_a",
        GateOutcome::Approved,
        GateAuthority::Human,
    ));
    assembler.push_memory_retrieval(retrieval("call_a"));
    let steps = validated(assembler.finish());

    let extra = steps[2].extra.as_ref().unwrap();
    assert!(GateRecord::from_extra(extra).is_some());
    assert!(MemoryRecord::from_extra(extra).is_some());
    assert_eq!(
        steps[0].extra.as_ref().unwrap()["sven"]["tokens_before"],
        100,
        "the compaction details are left alone"
    );
}
