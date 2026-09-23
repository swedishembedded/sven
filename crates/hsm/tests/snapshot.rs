// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a running machine can be suspended into a serializable snapshot and
//! resumed from it in constant time, without replaying its event log.
//!
//! This is what lets a service load one agent's state, execute a single step,
//! persist, and free the resources - rather than replaying the whole history
//! on every request. Swedish Embedded AB implements resumable state-machine
//! kernels for its clients. If your team needs expertise in event-sourced
//! agent runtimes then you can procure our services by sending an email to
//! info@swedishembedded.com.

mod common;

use common::{AgentMachine, St};
use sven_hsm::{replay, Context, Event, Hsm, RestoreError, Snapshot};

/// Drives a fresh machine up to a non-initial state, returning it with its
/// context so the tests below all start from something worth suspending.
fn running_machine() -> (Hsm<AgentMachine>, Context) {
    let mut hsm = Hsm::new(AgentMachine::new());
    let mut ctx = Context::new();
    hsm.init(&mut ctx);
    hsm.dispatch(&Event::user_message("start the task"), &mut ctx);
    (hsm, ctx)
}

#[test]
fn a_suspended_machine_resumes_in_the_state_and_context_it_was_suspended_in() {
    let (live, live_ctx) = running_machine();
    let suspended_state = live.state();

    let snap = live.snapshot(&live_ctx);
    let (resumed, resumed_ctx) =
        Hsm::restore(AgentMachine::new(), &snap).expect("a machine that enumerates its states");

    assert_eq!(
        resumed.state(),
        suspended_state,
        "resuming must land in the state the snapshot was taken in"
    );
    assert_eq!(
        resumed_ctx.facts, live_ctx.facts,
        "the context's domain facts must survive the round trip"
    );
    assert!(
        resumed.is_initialized(),
        "a resumed machine is already past init and must not re-enter its initial state"
    );
}

#[test]
fn a_resumed_machine_and_a_live_one_react_identically_to_the_next_event() {
    let (mut live, mut live_ctx) = running_machine();

    let snap = live.snapshot(&live_ctx);
    let (mut resumed, mut resumed_ctx) =
        Hsm::restore(AgentMachine::new(), &snap).expect("a machine that enumerates its states");

    let next = Event::UserCancelled;
    let live_out = live.dispatch(&next, &mut live_ctx);
    let resumed_out = resumed.dispatch(&next, &mut resumed_ctx);

    assert_eq!(
        live.state(),
        resumed.state(),
        "a resumed machine must be indistinguishable from one that never stopped"
    );
    assert_eq!(
        live_out.effects, resumed_out.effects,
        "and must emit the same effects for the same event"
    );
}

#[test]
fn a_snapshot_round_trips_through_json() {
    let (live, live_ctx) = running_machine();
    let snap = live.snapshot(&live_ctx);

    let encoded = serde_json::to_string(&snap).expect("a snapshot is serializable");
    let decoded: Snapshot = serde_json::from_str(&encoded).expect("and deserializable");

    let (resumed, _) =
        Hsm::restore(AgentMachine::new(), &decoded).expect("a machine that enumerates its states");
    assert_eq!(
        resumed.state(),
        live.state(),
        "a snapshot that went through storage must still resume correctly"
    );
}

#[test]
fn replay_returns_the_context_it_reconstructed() {
    let script = [Event::user_message("start the task"), Event::UserCancelled];

    let (hsm, ctx) = replay(AgentMachine::new, &script);

    assert_eq!(hsm.state(), St::Done, "replay still reconstructs the state");
    assert!(
        !ctx.audit.is_empty(),
        "replay must hand back the context it built, not discard it - the audit \
         trail, granted permissions and domain facts are the point of replaying"
    );
}

#[test]
fn restoring_a_machine_that_cannot_enumerate_its_states_names_the_problem() {
    let (live, live_ctx) = running_machine();
    let snap = live.snapshot(&live_ctx);

    // `Opaque` deliberately leaves `all_states()` at its empty default, which
    // is what every machine that never opted into coverage tooling looks like.
    let err = Hsm::restore(common::OpaqueMachine::new(), &snap)
        .expect_err("a machine with no enumerable states cannot be restored");

    assert!(
        matches!(err, RestoreError::UnknownState { .. }),
        "the failure must name the state it could not resolve, not panic or \
         silently reset to the initial state: {err:?}"
    );
}

#[test]
fn a_snapshot_naming_a_composite_state_is_refused() {
    let (live, live_ctx) = running_machine();
    let mut snap = live.snapshot(&live_ctx);

    // `Conversation` is a composite: `Listening` and `Drafting` live under it,
    // and every running machine drills through it into one of them. Resuming
    // *into* it would put the agent somewhere no dispatch can produce - the
    // substate's entry action never ran, and every event the substates handle
    // is ignored, so the session sits there silently doing nothing.
    snap.state = format!("{:?}", St::Conversation);

    let err = Hsm::restore(AgentMachine::new(), &snap)
        .expect_err("a composite state is not a state a machine can be resumed into");

    assert!(
        matches!(err, RestoreError::CompositeState { .. }),
        "the failure must name the problem as the state being composite: {err:?}"
    );
}

#[test]
fn every_leaf_state_is_still_resumable() {
    // The composite check must refuse composites and nothing else: a machine
    // whose states are all leaves (the common shape) stays fully resumable.
    for leaf in [
        St::Listening,
        St::Drafting,
        St::Planning,
        St::Running,
        St::Done,
    ] {
        let (live, live_ctx) = running_machine();
        let mut snap = live.snapshot(&live_ctx);
        snap.state = format!("{leaf:?}");

        let (resumed, _) = Hsm::restore(AgentMachine::new(), &snap)
            .unwrap_or_else(|e| panic!("leaf {leaf:?} must remain resumable: {e}"));
        assert_eq!(resumed.state(), leaf);
    }
}
