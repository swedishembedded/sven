// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the instruction
// catalog of an LLM coding agent from taxing every request for its clients.
// If your team needs expertise in agent context budgets then you can procure
// our services by sending an email to info@swedishembedded.com.
//! Spec: the instructions a session has activated are kernel state.
//!
//! A skill, subagent or knowledge document whose activation condition has
//! held stays listed to the model for the rest of the session, however often
//! its context is rebuilt. The set lives in the [`Context`], so a snapshot
//! restores it and a replay of the recorded events rebuilds it, and the kernel
//! copies it onto every model request a machine emits so the machine never has
//! to know it.

use sven_hsm::{replay, Context, Effect, Event, Hsm, Machine, MachineId, Reaction, Snapshot};
use sven_vocab::TurnRequest;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum St {
    Top,
    Idle,
}

/// Answers every user message with a turn request that names no loaded tools.
struct Asker(MachineId);

impl Machine for Asker {
    type State = St;
    fn id(&self) -> MachineId {
        self.0
    }
    fn top(&self) -> St {
        St::Top
    }
    fn initial(&self) -> St {
        St::Idle
    }
    fn superstate(&self, _: St) -> St {
        St::Top
    }
    fn is_terminal(&self, _: St) -> bool {
        false
    }
    fn all_states(&self) -> Vec<St> {
        vec![St::Top, St::Idle]
    }
    fn dispatch_state(&mut self, s: St, e: &Event, _: &mut Context) -> Reaction<St> {
        match (s, e) {
            (St::Idle, Event::UserMessage { .. }) => Reaction::Handled(vec![Effect::CallLlm {
                request: TurnRequest::default().to_value(),
            }]),
            _ => Reaction::Ignored,
        }
    }
}

fn asker() -> Hsm<Asker> {
    Hsm::new(Asker(MachineId::new()))
}

fn activated(ids: &[&str]) -> Event {
    Event::InstructionsActivated {
        ids: ids.iter().map(|n| (*n).to_string()).collect(),
    }
}

/// The turn request the kernel stamped on the model request `effects` carries.
fn stamped(effects: &[Effect]) -> TurnRequest {
    let Some(Effect::CallLlm { request }) = effects.first() else {
        panic!("expected a model request, got {effects:?}");
    };
    TurnRequest::from_value(request.clone()).expect("a turn request")
}

#[test]
fn activation_is_append_only_and_keeps_order() {
    let mut hsm = asker();
    let mut ctx = Context::new();
    hsm.init(&mut ctx);

    hsm.dispatch(&activated(&["skill:auth", "agent:verifier"]), &mut ctx);
    hsm.dispatch(
        &activated(&["knowledge:auth-design", "skill:auth"]),
        &mut ctx,
    );

    assert_eq!(
        ctx.active_instructions,
        ["skill:auth", "agent:verifier", "knowledge:auth-design"]
    );
}

#[test]
fn every_model_request_carries_the_active_instructions() {
    let mut hsm = asker();
    let mut ctx = Context::new();
    hsm.init(&mut ctx);

    let before = hsm.dispatch(&Event::user_message("go"), &mut ctx);
    assert!(stamped(&before.effects).active_instructions.is_empty());

    hsm.dispatch(&activated(&["skill:auth"]), &mut ctx);
    let after = hsm.dispatch(&Event::user_message("again"), &mut ctx);
    assert_eq!(stamped(&after.effects).active_instructions, ["skill:auth"]);
}

#[test]
fn every_model_request_names_the_state_it_was_emitted_in() {
    let mut hsm = asker();
    let mut ctx = Context::new();
    hsm.init(&mut ctx);
    let out = hsm.dispatch(&Event::user_message("go"), &mut ctx);
    assert_eq!(stamped(&out.effects).state, "Idle");
}

#[test]
fn active_instructions_survive_suspend_and_resume() {
    let mut hsm = asker();
    let mut ctx = Context::new();
    hsm.init(&mut ctx);
    hsm.dispatch(&activated(&["skill:auth", "agent:verifier"]), &mut ctx);

    let json = serde_json::to_string(&hsm.snapshot(&ctx)).unwrap();
    let snapshot: Snapshot = serde_json::from_str(&json).unwrap();
    let (mut resumed, mut resumed_ctx) = Hsm::restore(Asker(MachineId::new()), &snapshot).unwrap();

    assert_eq!(
        resumed_ctx.active_instructions,
        ["skill:auth", "agent:verifier"]
    );
    let out = resumed.dispatch(&Event::user_message("go"), &mut resumed_ctx);
    assert_eq!(
        stamped(&out.effects).active_instructions,
        ["skill:auth", "agent:verifier"]
    );
}

#[test]
fn a_snapshot_taken_before_the_set_existed_resumes_with_none_active() {
    let mut value = serde_json::to_value(Context::new()).unwrap();
    value.as_object_mut().unwrap().remove("active_instructions");
    let ctx: Context = serde_json::from_value(value).unwrap();
    assert!(ctx.active_instructions.is_empty());
}

#[test]
fn replaying_the_recorded_events_rebuilds_the_set() {
    let events = [
        Event::user_message("start"),
        activated(&["knowledge:auth-design"]),
        activated(&["skill:auth", "knowledge:auth-design"]),
    ];
    let (_, ctx) = replay(|| Asker(MachineId::new()), &events);
    assert_eq!(
        ctx.active_instructions,
        ["knowledge:auth-design", "skill:auth"]
    );
}
