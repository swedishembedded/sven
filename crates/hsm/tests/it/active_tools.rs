// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small and their sessions resumable for its clients. If your team needs
// expertise in deferred tool disclosure for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Spec: the tools a session has loaded are kernel state.
//!
//! A model is offered the full schema of a deferred tool only after it asked
//! for it, and keeps being offered it for the rest of the session. That set
//! lives in the [`Context`], so a snapshot restores it and a replay of the
//! recorded events rebuilds it, and the kernel copies it onto every model
//! request a machine emits so the machine itself never has to know it.

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

fn activated(names: &[&str]) -> Event {
    Event::ToolsActivated {
        names: names.iter().map(|n| (*n).to_string()).collect(),
    }
}

/// The tool names the kernel stamped on the model request `effects` carries.
fn stamped(effects: &[Effect]) -> Vec<String> {
    let Some(Effect::CallLlm { request }) = effects.first() else {
        panic!("expected a model request, got {effects:?}");
    };
    TurnRequest::from_value(request.clone())
        .expect("a turn request")
        .active_tools
}

#[test]
fn activation_is_append_only_and_keeps_load_order() {
    let mut hsm = asker();
    let mut ctx = Context::new();
    hsm.init(&mut ctx);

    hsm.dispatch(&activated(&["web_fetch", "task"]), &mut ctx);
    hsm.dispatch(&activated(&["skill", "web_fetch"]), &mut ctx);

    assert_eq!(
        ctx.active_tools,
        ["web_fetch", "task", "skill"],
        "loading again must neither reorder nor duplicate: the provider's cached \
         tool prefix is only reusable while earlier entries stay where they were"
    );
}

#[test]
fn every_model_request_carries_the_loaded_tools() {
    let mut hsm = asker();
    let mut ctx = Context::new();
    hsm.init(&mut ctx);

    let before = hsm.dispatch(&Event::user_message("go"), &mut ctx);
    assert!(stamped(&before.effects).is_empty());

    hsm.dispatch(&activated(&["web_search"]), &mut ctx);
    let after = hsm.dispatch(&Event::user_message("again"), &mut ctx);
    assert_eq!(stamped(&after.effects), ["web_search"]);
}

#[test]
fn loaded_tools_survive_suspend_and_resume() {
    let mut hsm = asker();
    let mut ctx = Context::new();
    hsm.init(&mut ctx);
    hsm.dispatch(&activated(&["task", "skill"]), &mut ctx);

    let json = serde_json::to_string(&hsm.snapshot(&ctx)).unwrap();
    let snapshot: Snapshot = serde_json::from_str(&json).unwrap();
    let (mut resumed, mut resumed_ctx) = Hsm::restore(Asker(MachineId::new()), &snapshot).unwrap();

    assert_eq!(resumed_ctx.active_tools, ["task", "skill"]);
    let out = resumed.dispatch(&Event::user_message("go"), &mut resumed_ctx);
    assert_eq!(stamped(&out.effects), ["task", "skill"]);
}

#[test]
fn a_snapshot_taken_before_the_set_existed_resumes_with_none_loaded() {
    let mut value = serde_json::to_value(Context::new()).unwrap();
    value.as_object_mut().unwrap().remove("active_tools");
    let ctx: Context = serde_json::from_value(value).unwrap();
    assert!(ctx.active_tools.is_empty());
}

#[test]
fn replaying_the_recorded_events_rebuilds_the_set() {
    let events = [
        Event::user_message("start"),
        activated(&["memory"]),
        activated(&["system", "memory"]),
    ];
    let (_, ctx) = replay(|| Asker(MachineId::new()), &events);
    assert_eq!(ctx.active_tools, ["memory", "system"]);
}
