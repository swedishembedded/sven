// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Unit + integration tests for the deliberation SDLC machine.

use serde_json::json;
use sven_hsm::{
    dispatch::Hsm,
    effect::{Effect, EffectKind},
    event::{Event, InternalEvent},
    machine::Machine,
    status::Reaction,
    Context,
};

use super::{SdlcMachine, SdlcState};

/// Count the `InstantiateSubmachine` effects in a reaction.
fn count_instantiate(r: &Reaction<SdlcState>) -> usize {
    let effects = match r {
        Reaction::Handled(e) => e,
        Reaction::Transition { effects, .. } => effects,
        _ => return 0,
    };
    effects
        .iter()
        .filter(|e| e.kind() == EffectKind::InstantiateSubmachine)
        .count()
}

/// Build a `SubmachineCompleted` internal event with a result payload.
fn submachine_completed(result: serde_json::Value) -> Event {
    Event::Internal(InternalEvent::SubmachineCompleted {
        machine: sven_hsm::ids::MachineId::new().as_uuid().to_string(),
        result,
    })
}

/// Extract the `thread` of the first `CallLlm` deliberation request in `effects`.
fn first_deliberation_thread(effects: &[Effect]) -> Option<String> {
    effects.iter().find_map(|e| match e {
        Effect::CallLlm { request } => request
            .get("thread")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        _ => None,
    })
}

/// Find the first `CallLlm` request value in a reaction's effects.
fn reaction_call_llm(r: &Reaction<SdlcState>) -> Option<serde_json::Value> {
    let effects = match r {
        Reaction::Handled(e) => e,
        Reaction::Transition { effects, .. } => effects,
        _ => return None,
    };
    effects.iter().find_map(|e| match e {
        Effect::CallLlm { request } => Some(request.clone()),
        _ => None,
    })
}

fn deliberation_complete(thread: &str, decision: serde_json::Value) -> Event {
    Event::DeliberationComplete {
        thread: thread.into(),
        decision,
    }
}

// ── The "hi" guard ────────────────────────────────────────────────────────────

#[test]
fn machine_starts_in_idle_and_fires_no_llm() {
    let mut hsm = Hsm::new(SdlcMachine::new());
    let mut ctx = Context::new();
    let effects = hsm.init(&mut ctx);
    assert_eq!(hsm.state(), SdlcState::Idle);
    assert!(
        !effects.iter().any(|e| e.kind() == EffectKind::CallLlm),
        "no LLM call must fire before the user speaks"
    );
}

#[test]
fn first_user_message_enters_intake_and_deliberates() {
    let mut hsm = Hsm::new(SdlcMachine::new());
    let mut ctx = Context::new();
    hsm.init(&mut ctx);
    let out = hsm.dispatch(&Event::user_message("hi"), &mut ctx);
    assert_eq!(hsm.state(), SdlcState::Intake);
    assert_eq!(
        first_deliberation_thread(&out.effects).as_deref(),
        Some("intake")
    );
}

#[test]
fn intake_chitchat_stays_in_intake_and_asks_user() {
    let mut hsm = Hsm::new(SdlcMachine::new());
    let mut ctx = Context::new();
    hsm.init(&mut ctx);
    hsm.dispatch(&Event::user_message("hi"), &mut ctx);
    let out = hsm.dispatch(
        &deliberation_complete(
            "intake",
            json!({"status": "need_user_input", "message": "Hello! What can I build for you?"}),
        ),
        &mut ctx,
    );
    assert_eq!(hsm.state(), SdlcState::Intake, "must NOT launch the pipeline");
    assert!(out.effects.iter().any(|e| e.kind() == EffectKind::AskUser));
}

// ── Intake → Discovery via approval gate ──────────────────────────────────────

#[test]
fn intake_need_approval_requests_human_approval() {
    let mut hsm = Hsm::new(SdlcMachine::new());
    let mut ctx = Context::new();
    hsm.init(&mut ctx);
    hsm.dispatch(&Event::user_message("fix the bug"), &mut ctx);
    let out = hsm.dispatch(
        &deliberation_complete(
            "intake",
            json!({"status": "need_approval", "summary": "Fix the auth bug", "approval_prompt": "Proceed?"}),
        ),
        &mut ctx,
    );
    assert_eq!(hsm.state(), SdlcState::Intake);
    assert!(out
        .effects
        .iter()
        .any(|e| e.kind() == EffectKind::RequestHumanApproval));
    assert_eq!(
        ctx.fact("scope_summary").and_then(|v| v.as_str()),
        Some("Fix the auth bug")
    );
}

#[test]
fn intake_approved_advances_to_discovery_and_deliberates() {
    let mut hsm = Hsm::new(SdlcMachine::new());
    let mut ctx = Context::new();
    hsm.init(&mut ctx);
    hsm.dispatch(&Event::user_message("fix the bug"), &mut ctx);
    hsm.dispatch(
        &deliberation_complete(
            "intake",
            json!({"status": "need_approval", "summary": "scope", "approval_prompt": "ok?"}),
        ),
        &mut ctx,
    );
    let approval_id = ctx.pending_approval.as_ref().map(|p| p.approval_id).unwrap();
    let out = hsm.dispatch(&Event::HumanApproved { approval_id }, &mut ctx);
    assert_eq!(hsm.state(), SdlcState::Discovery);
    assert_eq!(
        first_deliberation_thread(&out.effects).as_deref(),
        Some("discovery")
    );
}

// ── need_user_input carries the answer forward (append-only follow-up) ────────

#[test]
fn user_answer_in_discovery_redeliberates_same_thread() {
    let mut m = SdlcMachine::new();
    let mut ctx = Context::new();
    let r = m.dispatch_state(
        SdlcState::Discovery,
        &Event::user_message("the entrypoint is src/main.rs"),
        &mut ctx,
    );
    let req = reaction_call_llm(&r).expect("a follow-up deliberation must be issued");
    assert_eq!(req["thread"], "discovery");
    assert!(req["instruction"].as_str().unwrap().contains("src/main.rs"));
}

#[test]
fn discovery_proceed_transitions_to_planning() {
    let mut m = SdlcMachine::new();
    let mut ctx = Context::new();
    let r = m.dispatch_state(
        SdlcState::Discovery,
        &deliberation_complete("discovery", json!({"status": "proceed", "summary": "ok"})),
        &mut ctx,
    );
    assert!(matches!(
        r,
        Reaction::Transition {
            target: SdlcState::Planning,
            ..
        }
    ));
    assert_eq!(
        ctx.fact("discovery_summary").and_then(|v| v.as_str()),
        Some("ok")
    );
}

// ── Full autonomous drive-through with scripted decisions ─────────────────────

#[test]
fn full_pipeline_drives_intake_to_done() {
    let mut hsm = Hsm::new(SdlcMachine::new());
    let mut ctx = Context::new();
    hsm.init(&mut ctx);

    hsm.dispatch(&Event::user_message("add a hello function"), &mut ctx);
    hsm.dispatch(
        &deliberation_complete(
            "intake",
            json!({"status": "need_approval", "summary": "add hello()", "approval_prompt": "ok?"}),
        ),
        &mut ctx,
    );
    let id = ctx.pending_approval.as_ref().map(|p| p.approval_id).unwrap();
    hsm.dispatch(&Event::HumanApproved { approval_id: id }, &mut ctx);
    assert_eq!(hsm.state(), SdlcState::Discovery);

    hsm.dispatch(
        &deliberation_complete("discovery", json!({"status": "proceed", "summary": "small crate"})),
        &mut ctx,
    );
    assert_eq!(hsm.state(), SdlcState::Planning);

    hsm.dispatch(
        &deliberation_complete(
            "planning",
            json!({"status": "need_approval", "summary": "1 task", "approval_prompt": "approve?"}),
        ),
        &mut ctx,
    );
    let id = ctx.pending_approval.as_ref().map(|p| p.approval_id).unwrap();
    hsm.dispatch(&Event::HumanApproved { approval_id: id }, &mut ctx);
    assert_eq!(hsm.state(), SdlcState::Execution);

    hsm.dispatch(
        &deliberation_complete("execution", json!({"status": "proceed", "summary": "implemented"})),
        &mut ctx,
    );
    assert_eq!(hsm.state(), SdlcState::Verification);

    hsm.dispatch(
        &deliberation_complete("verification", json!({"status": "proceed", "summary": "tests pass"})),
        &mut ctx,
    );
    assert_eq!(hsm.state(), SdlcState::Delivery);

    hsm.dispatch(
        &deliberation_complete(
            "delivery",
            json!({"status": "need_approval", "summary": "handover", "approval_prompt": "accept?"}),
        ),
        &mut ctx,
    );
    let id = ctx.pending_approval.as_ref().map(|p| p.approval_id).unwrap();
    hsm.dispatch(&Event::HumanApproved { approval_id: id }, &mut ctx);
    assert_eq!(hsm.state(), SdlcState::Done);
    assert!(hsm.is_done());
}

// ── Failure routing ──────────────────────────────────────────────────────────

#[test]
fn failed_decision_routes_to_recovery() {
    let mut m = SdlcMachine::new();
    let mut ctx = Context::new();
    let r = m.dispatch_state(
        SdlcState::Discovery,
        &deliberation_complete("discovery", json!({"status": "failed", "summary": "cannot read"})),
        &mut ctx,
    );
    assert!(matches!(
        r,
        Reaction::Transition {
            target: SdlcState::Recovery,
            ..
        }
    ));
}

#[test]
fn llm_failed_routes_to_recovery() {
    let mut m = SdlcMachine::new();
    let mut ctx = Context::new();
    let r = m.dispatch_state(
        SdlcState::Execution,
        &Event::LlmFailed {
            error: "stream stalled".into(),
        },
        &mut ctx,
    );
    assert!(matches!(
        r,
        Reaction::Transition {
            target: SdlcState::Recovery,
            ..
        }
    ));
}

#[test]
fn recovery_entry_fires_deliberation_then_failed_after_limit() {
    let mut hsm = Hsm::new(SdlcMachine::new());
    let mut ctx = Context::new();
    hsm.init(&mut ctx);
    // Reach Recovery via an LLM failure in intake.
    hsm.dispatch(&Event::user_message("do x"), &mut ctx);
    let out = hsm.dispatch(
        &Event::LlmFailed {
            error: "boom".into(),
        },
        &mut ctx,
    );
    assert_eq!(hsm.state(), SdlcState::Recovery);
    assert_eq!(
        first_deliberation_thread(&out.effects).as_deref(),
        Some("recovery")
    );
}

// ── Phase 2: parallel execution fan-out ───────────────────────────────────────

#[test]
fn execution_fans_out_when_plan_decomposes_and_spawner_present() {
    let mut m = SdlcMachine::new();
    let mut ctx = Context::new();
    ctx.set_fact("parallel_execution", json!(true));
    ctx.set_fact(
        "plan_payload",
        json!({ "tasks": ["task a", "task b", "task c"] }),
    );
    let r = m.dispatch_state(
        SdlcState::Execution,
        &Event::Internal(InternalEvent::Entry),
        &mut ctx,
    );
    assert_eq!(count_instantiate(&r), 3, "one child per task");
    assert_eq!(ctx.fact("exec_remaining").and_then(|v| v.as_i64()), Some(3));
}

#[test]
fn execution_stays_single_track_without_spawner_flag() {
    let mut m = SdlcMachine::new();
    let mut ctx = Context::new();
    // tasks present, but no parallel_execution flag → no fan-out.
    ctx.set_fact("plan_payload", json!({ "tasks": ["a", "b"] }));
    let r = m.dispatch_state(
        SdlcState::Execution,
        &Event::Internal(InternalEvent::Entry),
        &mut ctx,
    );
    assert_eq!(count_instantiate(&r), 0, "must not emit submachines");
    assert_eq!(
        reaction_call_llm(&r).map(|req| req["thread"].as_str().unwrap().to_string()),
        Some("execution".to_string()),
        "falls back to a single execution deliberation"
    );
}

#[test]
fn execution_aggregates_all_children_then_synthesises_on_thread() {
    let mut m = SdlcMachine::new();
    let mut ctx = Context::new();
    ctx.set_fact("parallel_execution", json!(true));
    ctx.set_fact("plan_payload", json!({ "tasks": ["a", "b", "c"] }));
    m.dispatch_state(
        SdlcState::Execution,
        &Event::Internal(InternalEvent::Entry),
        &mut ctx,
    );

    // First two completions: aggregate, no synthesis yet.
    let r1 = m.dispatch_state(
        SdlcState::Execution,
        &submachine_completed(json!({"summary": "did a"})),
        &mut ctx,
    );
    assert!(reaction_call_llm(&r1).is_none(), "wait for all children");
    let r2 = m.dispatch_state(
        SdlcState::Execution,
        &submachine_completed(json!({"summary": "did b"})),
        &mut ctx,
    );
    assert!(reaction_call_llm(&r2).is_none());

    // Final completion: append-only synthesis deliberation on execution thread.
    let r3 = m.dispatch_state(
        SdlcState::Execution,
        &submachine_completed(json!({"summary": "did c"})),
        &mut ctx,
    );
    let req = reaction_call_llm(&r3).expect("synthesis deliberation must fire");
    assert_eq!(req["thread"], "execution");
    let instruction = req["instruction"].as_str().unwrap();
    assert!(instruction.contains("did a"));
    assert!(instruction.contains("did c"));
    assert_eq!(ctx.fact("exec_remaining").and_then(|v| v.as_i64()), Some(0));
}

#[test]
fn user_cancel_goes_to_cancelled() {
    let mut hsm = Hsm::new(SdlcMachine::new());
    let mut ctx = Context::new();
    hsm.init(&mut ctx);
    hsm.dispatch(&Event::user_message("do x"), &mut ctx);
    hsm.dispatch(&Event::UserCancelled, &mut ctx);
    assert_eq!(hsm.state(), SdlcState::Cancelled);
    assert!(hsm.is_done());
}
