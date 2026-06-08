// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The default coding-agent machine.
//!
//! [`ReactiveAgentMachine`] models the full turn lifecycle of a streaming,
//! native-tool-calling coding agent.  It uses the shared [`super::loop_core`]
//! helpers and runs the tool loop **in-state** — there are no separate
//! `RunningTools` or `AwaitingApproval` states.
//!
//! # State hierarchy
//!
//! ```text
//! Top (root)
//! └── Session
//!     ├── Idle        ← waiting for the user's next message
//!     └── Generating  ← LLM turn in flight; tools executed in-state
//! ```
//!
//! # In-state tool loop
//!
//! On `UserMessage`, the machine initialises `LoopState` and emits a
//! `CallLlm kind="turn"`.  `TurnExecutor` streams the model and posts
//! `LlmTurnComplete`.  If the model proposed tool calls the machine emits
//! `CallTool` effects **and stays in `Generating`** (`Reaction::Handled`).
//! `ToolSucceeded` / `ToolFailed` / `ToolApprovalRequired` / `HumanApproved`
//! (tool) / `HumanRejected` (tool) are all handled by the shared
//! [`handle_tool_event`](super::loop_core::handle_tool_event) helper which
//! drains the pending set, requests human approval, and emits continuation
//! turns — all while staying in `Generating`.
//!
//! A tool-free `LlmTurnComplete` transitions the machine to `Idle`.

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

use super::loop_core::{
    build_turn_effect, handle_tool_event, init_loop, on_llm_turn_complete, GeneratingAction,
    LoopState,
};

/// The conversation thread name used by the reactive agent.
pub const CHAT_THREAD: &str = "chat";

/// Default maximum tool-call rounds before a forced wrap-up turn.
const DEFAULT_MAX_TOOL_ROUNDS: u32 = 16;

/// Default mode for all-tools resolution.
const AGENT_MODE: &str = "agent";

/// States of the reactive agent machine.
///
/// `Generating` now owns the full tool loop — no separate `RunningTools` or
/// `AwaitingApproval` states.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ReactiveState {
    /// Root (fixpoint: `superstate(Top) == Top`). Never the active leaf.
    Top,
    /// Composite parent of all session states; handles `UserCancelled`.
    Session,
    /// Waiting for the user's next message.
    Idle,
    /// LLM turn in flight; tool calls executed in-state.
    Generating,
}

/// The default streaming coding-agent machine.
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

    /// The permission policy for the general coding agent (global allow for
    /// common capabilities; approval required for destructive rollback).
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

    /// Build the first `CallLlm { kind:"turn" }` effect for a user message.
    fn first_turn_effect(text: &str) -> Effect {
        build_turn_effect(
            CHAT_THREAD,
            &[],
            AGENT_MODE,
            None,
            Some(text),
            None,
            DEFAULT_MAX_TOOL_ROUNDS,
            None,
            None,
        )
    }

    /// Build a continuation turn from the current `LoopState`.
    fn continuation_turn(ls: &LoopState) -> Effect {
        ls.continuation_turn()
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
                Event::UserCancelled => {
                    Reaction::transition(Idle, vec![], "user cancelled; returning to Idle")
                }
                _ => Reaction::Super(Top),
            },

            // Idle: a user message starts a new turn.
            Idle => match event {
                Event::UserMessage { text } => {
                    init_loop(ctx, CHAT_THREAD, &[], AGENT_MODE, DEFAULT_MAX_TOOL_ROUNDS);
                    Reaction::transition(
                        Generating,
                        vec![Self::first_turn_effect(text)],
                        "user sent message; starting turn",
                    )
                }
                _ => Reaction::Super(Session),
            },

            // Generating: LLM turn in flight; tool loop handled in-state.
            Generating => {
                // ── 1. Delegate tool/approval events to the shared helper ──────
                // Returns Some for ToolSucceeded/ToolFailed/ToolApprovalRequired
                // and for HumanApproved/HumanRejected when a tool approval is
                // in flight.  Returns None otherwise so we can handle LLM events.
                if let Some(r) = handle_tool_event(ctx, Self::continuation_turn, event) {
                    return r;
                }

                // ── 2. LLM and other events ───────────────────────────────────
                match event {
                    Event::LlmTurnComplete { .. } => match on_llm_turn_complete(ctx, event) {
                        GeneratingAction::FinalAnswer { text, .. } => {
                            ctx.set_fact("last_response", json!(text));
                            Reaction::transition(Idle, vec![], "turn complete; final answer")
                        }
                        GeneratingAction::CallTools { tool_effects, .. } => {
                            // `on_llm_turn_complete` already registered the pending
                            // calls in LoopState.  Emit CallTool effects and stay.
                            Reaction::effects(tool_effects)
                        }
                        GeneratingAction::EmptyTurn { nudge_effect } => {
                            Reaction::effects(vec![nudge_effect])
                        }
                        GeneratingAction::MaxRoundsReached { wrapup_effect } => {
                            Reaction::effects(vec![wrapup_effect])
                        }
                    },

                    Event::LlmFailed { error } => {
                        ctx.set_fact("last_error", json!(error));
                        Reaction::transition(Idle, vec![], "turn failed")
                    }

                    _ => Reaction::Super(Session),
                }
            }
        }
    }

    fn is_terminal(&self, _state: ReactiveState) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_hsm::{
        dispatch::Hsm,
        effect::EffectKind,
        ids::ToolCallId,
        permissions::ToolCapability,
    };

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
    fn user_message_starts_turn() {
        let (mut hsm, mut ctx) = make_hsm();
        let out = hsm.dispatch(&Event::user_message("fix the bug"), &mut ctx);
        assert_eq!(hsm.state(), ReactiveState::Generating);
        assert_eq!(out.effects.len(), 1);
        assert_eq!(out.effects[0].kind(), EffectKind::CallLlm);
        if let Effect::CallLlm { request } = &out.effects[0] {
            assert_eq!(request["kind"], "turn");
            assert_eq!(request["thread"], CHAT_THREAD);
            assert_eq!(request["instruction"], "fix the bug");
        } else {
            panic!("expected CallLlm turn effect");
        }
    }

    #[test]
    fn llm_turn_complete_with_no_tools_returns_to_idle() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("hi"), &mut ctx);
        let out = hsm.dispatch(
            &Event::LlmTurnComplete {
                thread: CHAT_THREAD.to_string(),
                text: "Hello!".to_string(),
                tool_calls: vec![],
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), ReactiveState::Idle);
        assert!(out.transitioned);
        assert_eq!(ctx.fact("last_response").unwrap(), &json!("Hello!"));
    }

    #[test]
    fn llm_turn_complete_with_tools_stays_in_generating() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("run tests"), &mut ctx);
        let call_id = ToolCallId::new();
        let out = hsm.dispatch(
            &Event::LlmTurnComplete {
                thread: CHAT_THREAD.to_string(),
                text: String::new(),
                tool_calls: vec![sven_hsm::ProposedToolCall {
                    call_id,
                    name: "shell".to_string(),
                    args: json!({"command": "cargo test"}),
                    capability: ToolCapability::ExecuteShell,
                }],
            },
            &mut ctx,
        );
        // Machine stays in Generating (in-state tool loop).
        assert_eq!(hsm.state(), ReactiveState::Generating);
        // One CallTool effect emitted.
        assert_eq!(out.effects.len(), 1);
        assert_eq!(out.effects[0].kind(), EffectKind::CallTool);
    }

    #[test]
    fn all_tools_done_emits_continuation_turn() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("do work"), &mut ctx);
        let call_id = ToolCallId::new();
        hsm.dispatch(
            &Event::LlmTurnComplete {
                thread: CHAT_THREAD.to_string(),
                text: String::new(),
                tool_calls: vec![sven_hsm::ProposedToolCall {
                    call_id,
                    name: "shell".to_string(),
                    args: json!({}),
                    capability: ToolCapability::ExecuteShell,
                }],
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), ReactiveState::Generating);
        // Simulate tool success.
        let out = hsm.dispatch(
            &Event::ToolSucceeded {
                call_id,
                observation: json!("ok"),
            },
            &mut ctx,
        );
        // Still in Generating; emitted a continuation turn.
        assert_eq!(hsm.state(), ReactiveState::Generating);
        assert_eq!(out.effects.len(), 1);
        assert_eq!(out.effects[0].kind(), EffectKind::CallLlm);
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
                &Event::LlmTurnComplete {
                    thread: CHAT_THREAD.to_string(),
                    text: format!("done {i}"),
                    tool_calls: vec![],
                },
                &mut ctx,
            );
            assert_eq!(hsm.state(), ReactiveState::Idle);
        }
    }
}
