//! The default coding-agent machine.
//!
//! [`ReactiveAgentMachine`] is the HSM-era replacement for the legacy
//! `Agent::run_agentic_loop`. It models the high-level turn lifecycle of a
//! streaming, native-tool-calling coding agent while delegating the actual
//! model↔tool round loop (streaming, parallel tools, XML fallback, empty-turn
//! retries, `max_tool_rounds`, compaction) to a converse-style executor that
//! reproduces the legacy fidelity exactly and reports completion as a single
//! inward [`Event`].
//!
//! # Why the loop lives in the executor
//!
//! The legacy agentic loop is a tightly-coupled streaming pipeline: tool calls
//! are dispatched the instant their JSON arguments complete *during* the model
//! stream, results are interleaved with progress drains, and compaction can
//! fire mid-loop. Faithfully reproducing that UX while round-tripping every
//! token through the inward event queue would break run-to-completion. Instead
//! the machine issues one `CallLlm` "converse" effect per user turn; the
//! executor performs the entire multi-round loop, streams `UiEvent`s outward,
//! and posts exactly one [`Event::LlmProposedResponse`] (or
//! [`Event::LlmFailed`]) back inward when the turn settles. The machine remains
//! the gatekeeper for cancellation and turn lifecycle.
//!
//! # State hierarchy
//!
//! ```text
//! Top (root)
//! └── Session
//!     ├── Idle        ← waiting for the user's next message
//!     └── Generating  ← a converse turn is in flight (streaming + tools)
//! ```

use serde_json::json;
use sven_hsm::{
    context::Context,
    effect::Effect,
    event::{Event, InternalEvent},
    ids::MachineId,
    machine::Machine,
    permissions::{PermissionPolicy, ToolCapability},
    status::Reaction,
};

/// The JSON `kind` tag used for the converse effect this machine emits. The
/// converse executor matches on this to drive the legacy agentic loop.
pub const CONVERSE_KIND: &str = "converse";

/// States of the reactive agent machine (flat enum; hierarchy via `superstate`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ReactiveState {
    /// Root (fixpoint: `superstate(Top) == Top`). Never the active leaf.
    Top,
    /// Composite parent of the session states.
    Session,
    /// Waiting for the user's next message.
    Idle,
    /// A converse turn (model streaming + tool rounds) is in flight.
    Generating,
}

/// The default streaming coding-agent machine.
///
/// Stateless beyond its [`MachineId`]; the conversation buffer and all token
/// accounting live in the converse executor's shared session, while turn
/// control flow lives here.
pub struct ReactiveAgentMachine {
    id: MachineId,
}

impl Default for ReactiveAgentMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl ReactiveAgentMachine {
    /// Creates a new instance with a fresh [`MachineId`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: MachineId::new(),
        }
    }

    /// Build the converse effect for a user turn.
    fn converse_effect(text: &str) -> Effect {
        Effect::CallLlm {
            request: json!({
                "kind": CONVERSE_KIND,
                "text": text,
            }),
        }
    }

    /// The permission policy for the general coding agent.
    ///
    /// The agent is broad by design: read/write/network/git are globally
    /// allowed (the converse executor and tool registry enforce finer-grained
    /// approval through their own `PermissionRequester`). Rollback still
    /// requires explicit approval.
    #[must_use]
    pub fn permission_policy() -> PermissionPolicy {
        PermissionPolicy::builder()
            .allow_globally([
                ToolCapability::ReadFile,
                ToolCapability::WriteFile,
                ToolCapability::NetworkAccess,
                ToolCapability::GitOperation,
                ToolCapability::ExecuteShell,
            ])
            .require_approval([ToolCapability::Rollback])
            .build()
    }
}

impl Machine for ReactiveAgentMachine {
    type State = ReactiveState;

    fn id(&self) -> MachineId {
        self.id
    }

    fn top(&self) -> ReactiveState {
        ReactiveState::Top
    }

    fn initial(&self) -> ReactiveState {
        ReactiveState::Session
    }

    fn superstate(&self, state: ReactiveState) -> ReactiveState {
        match state {
            ReactiveState::Top => ReactiveState::Top,
            ReactiveState::Session => ReactiveState::Top,
            ReactiveState::Idle | ReactiveState::Generating => ReactiveState::Session,
        }
    }

    fn dispatch_state(
        &mut self,
        state: ReactiveState,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<ReactiveState> {
        use ReactiveState::*;

        match state {
            Top => Reaction::Ignored,

            // Session: drills into Idle on Init; handles global cancel.
            Session => match event {
                Event::Internal(InternalEvent::Init) => Reaction::goto(Idle),
                Event::UserCancelled => Reaction::transition(
                    Idle,
                    vec![],
                    "user cancelled in-flight work; returning to Idle",
                ),
                _ => Reaction::Super(Top),
            },

            // Idle: a user message starts a converse turn.
            Idle => match event {
                Event::UserMessage { text } => Reaction::transition(
                    Generating,
                    vec![Self::converse_effect(text)],
                    "user sent message; starting converse turn",
                ),
                _ => Reaction::Super(Session),
            },

            // Generating: the converse executor runs the whole agentic loop and
            // settles the turn with exactly one completion event.
            Generating => match event {
                Event::LlmProposedResponse { text } => {
                    ctx.set_fact("last_response", json!(text));
                    Reaction::transition(Idle, vec![], "converse turn complete")
                }
                Event::LlmFailed { error } => {
                    ctx.set_fact("last_error", json!(error));
                    Reaction::transition(Idle, vec![], "converse turn failed")
                }
                // A second user message while generating is treated as a cancel
                // of the current turn followed by the new turn on re-entry to
                // Idle is not automatic; bubble to Session for the cancel rule.
                _ => Reaction::Super(Session),
            },
        }
    }

    fn is_terminal(&self, _state: ReactiveState) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_hsm::{dispatch::Hsm, effect::EffectKind};

    fn make_hsm() -> (Hsm<ReactiveAgentMachine>, Context) {
        let mut hsm = Hsm::new(ReactiveAgentMachine::new());
        let mut ctx = Context::new();
        hsm.init(&mut ctx);
        (hsm, ctx)
    }

    #[test]
    fn initial_state_is_idle() {
        let (hsm, _) = make_hsm();
        assert_eq!(hsm.state(), ReactiveState::Idle);
    }

    #[test]
    fn user_message_starts_converse_turn() {
        let (mut hsm, mut ctx) = make_hsm();
        let out = hsm.dispatch(&Event::user_message("fix the bug"), &mut ctx);
        assert_eq!(hsm.state(), ReactiveState::Generating);
        assert_eq!(out.effects.len(), 1);
        assert_eq!(out.effects[0].kind(), EffectKind::CallLlm);
        if let Effect::CallLlm { request } = &out.effects[0] {
            assert_eq!(request["kind"], CONVERSE_KIND);
            assert_eq!(request["text"], "fix the bug");
        } else {
            panic!("expected CallLlm converse effect");
        }
    }

    #[test]
    fn response_completes_turn_and_returns_to_idle() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("hi"), &mut ctx);
        let out = hsm.dispatch(
            &Event::LlmProposedResponse {
                text: "hello there".into(),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), ReactiveState::Idle);
        assert!(out.transitioned);
        assert_eq!(ctx.fact("last_response").unwrap(), &json!("hello there"));
    }

    #[test]
    fn failure_returns_to_idle() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("hi"), &mut ctx);
        let out = hsm.dispatch(
            &Event::LlmFailed {
                error: "boom".into(),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), ReactiveState::Idle);
        assert!(out.transitioned);
    }

    #[test]
    fn cancel_during_generation_returns_to_idle() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("long task"), &mut ctx);
        assert_eq!(hsm.state(), ReactiveState::Generating);
        hsm.dispatch(&Event::UserCancelled, &mut ctx);
        assert_eq!(hsm.state(), ReactiveState::Idle);
    }

    #[test]
    fn multiple_turns_loop_through_idle() {
        let (mut hsm, mut ctx) = make_hsm();
        for i in 0..3 {
            hsm.dispatch(&Event::user_message(format!("turn {i}")), &mut ctx);
            assert_eq!(hsm.state(), ReactiveState::Generating);
            hsm.dispatch(
                &Event::LlmProposedResponse {
                    text: format!("done {i}"),
                },
                &mut ctx,
            );
            assert_eq!(hsm.state(), ReactiveState::Idle);
        }
    }
}
