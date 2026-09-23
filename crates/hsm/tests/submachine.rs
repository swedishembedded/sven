//! Submachine composition tests: child-first routing, bubbling of unhandled
//! events to the parent, and `SubmachineCompleted` on child termination.

mod common;

use common::{parent_handled, AgentMachine, PState, ParentMachine};
use sven_hsm::{Context, Event, Hsm, Submachine};

#[test]
fn child_completion_notifies_parent_with_submachine_completed() {
    let mut sm = Submachine::new(ParentMachine::new());
    let mut ctx = Context::new();
    sm.init(&mut ctx);
    assert_eq!(sm.parent_state(), PState::Working);

    // Install a child agent machine (initializes to "Listening").
    let child = Box::new(Hsm::new(AgentMachine::new()));
    sm.instantiate_child(child, &mut ctx);
    assert!(sm.has_child());
    assert_eq!(sm.child_state_label().as_deref(), Some("Listening"));

    // UserCancelled drives the child to its terminal Done state. The host then
    // drops the child and delivers SubmachineCompleted to the parent, which
    // transitions Working -> Finished.
    let out = sm.dispatch(&Event::UserCancelled, &mut ctx);
    assert!(out.child_was_active);
    assert!(out.child_completed);
    assert!(!sm.has_child(), "completed child must be dropped");
    assert_eq!(sm.parent_state(), PState::Finished);
}

#[test]
fn unhandled_child_events_bubble_up_to_the_parent() {
    let mut sm = Submachine::new(ParentMachine::new());
    let mut ctx = Context::new();
    sm.init(&mut ctx);

    let child = Box::new(Hsm::new(AgentMachine::new()));
    sm.instantiate_child(child, &mut ctx);

    // The child (in "Listening") ignores HumanApproved; it bubbles to the parent
    // which handles it with an effect. The child remains active.
    let out = sm.dispatch(
        &Event::HumanApproved {
            approval_id: sven_hsm::ApprovalId::new(),
        },
        &mut ctx,
    );
    assert!(sm.has_child(), "child still active after a bubbled event");
    assert!(
        out.effects.contains(&parent_handled()),
        "parent's handler effect must appear: {:?}",
        out.effects
    );
}

#[test]
fn events_route_directly_to_parent_when_no_child_is_active() {
    let mut sm = Submachine::new(ParentMachine::new());
    let mut ctx = Context::new();
    sm.init(&mut ctx);
    assert!(!sm.has_child());

    let out = sm.dispatch(
        &Event::HumanApproved {
            approval_id: sven_hsm::ApprovalId::new(),
        },
        &mut ctx,
    );
    assert!(!out.child_was_active);
    assert!(out.effects.contains(&parent_handled()));
}

/// Spec: a child's terminal state reaches the parent exactly once, whenever
/// the child reaches it - including in its own initial transition.
///
/// The host looks for completion after routing an event, which is every way a
/// child can finish except the first one. A child built for work that was
/// already done finished before any event existed, so it was installed as a
/// live child, the parent was never told, and the next event - if one ever
/// came - was dispatched into a machine that had already ended.
#[test]
fn a_child_that_is_already_done_when_installed_completes_immediately() {
    use common::probes::DoneOnArrivalMachine;

    let mut sm = Submachine::new(ParentMachine::new());
    let mut ctx = Context::new();
    sm.init(&mut ctx);
    assert_eq!(sm.parent_state(), PState::Working);

    sm.instantiate_child(Box::new(Hsm::new(DoneOnArrivalMachine::new())), &mut ctx);

    assert!(
        !sm.has_child(),
        "a child that its own initial transition finished must not be installed"
    );
    assert_eq!(
        sm.parent_state(),
        PState::Finished,
        "the parent must be told at install time, not at the next event"
    );
}
