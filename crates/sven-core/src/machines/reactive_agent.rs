// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The default coding-agent machine.
//!
//! [`ReactiveAgentMachine`] is the HSM-era replacement for the legacy
//! `Agent::run_agentic_loop`.  It models the full turn lifecycle of a
//! streaming, native-tool-calling coding agent using the shared
//! [`loop_core`] helpers.
//!
//! # State hierarchy
//!
//! ```text
//! Top (root)
//! └── Session
//!     ├── Idle                 ← waiting for the user's next message
//!     ├── Generating           ← a TurnExecutor turn is in flight
//!     ├── RunningTools         ← parallel tool calls dispatched, awaiting results
//!     └── AwaitingApproval     ← one or more tools need human approval
//! ```
//!
//! # HSM-native loop
//!
//! On a `UserMessage`, the machine appends the text to the `"chat"` thread
//! (via the `instruction` field of `TurnRequest`) and emits a
//! `CallLlm kind="turn"` effect.  `TurnExecutor` streams the model, appends
//! the assistant turn, and posts `LlmTurnComplete`.  If the model proposed
//! tool calls, the machine emits `CallTool` effects (one per call) and enters
//! `RunningTools`.  When all tools complete, it re-enters `Generating` with a
//! fresh turn.  On a tool-free `LlmTurnComplete`, the machine goes to `Idle`.
//!
//! # Backward compatibility
//!
//! The legacy `CallLlm kind="converse"` path still compiles to ease the
//! transition: `Generating` also accepts `LlmProposedResponse` (posted by the
//! `ConverseExecutor`) to return to `Idle`.  This path will be removed in
//! Phase E once all callers switch to `TurnExecutor`.

use serde_json::json;
use sven_hsm::{
    context::Context,
    effect::Effect,
    event::{Event, InternalEvent},
    ids::{ApprovalId, MachineId},
    machine::Machine,
    permissions::{PermissionPolicy, ToolCapability},
    status::Reaction,
};

use super::loop_core::{
    self, all_tools_mode, build_turn_effect, current_thread, current_tools, init_loop,
    mark_calls_pending, on_llm_turn_complete, on_tool_result, GeneratingAction,
};

/// The conversation thread name used by the reactive agent.
pub const CHAT_THREAD: &str = "chat";

/// Default maximum tool-call rounds before a forced wrap-up turn.
const DEFAULT_MAX_TOOL_ROUNDS: u32 = 16;

/// Default mode for all-tools resolution.
const AGENT_MODE: &str = "agent";

/// The JSON `kind` tag for the legacy converse effect (still accepted for
/// backward compat; new code uses `kind="turn"`).
pub const CONVERSE_KIND: &str = "converse";

/// States of the reactive agent machine.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ReactiveState {
    /// Root (fixpoint: `superstate(Top) == Top`). Never the active leaf.
    Top,
    /// Composite parent of all session states.
    Session,
    /// Waiting for the user's next message.
    Idle,
    /// A `TurnExecutor` turn (or legacy `ConverseExecutor` turn) is in flight.
    Generating,
    /// Tool calls have been dispatched; awaiting `ToolSucceeded`/`ToolFailed`.
    RunningTools,
    /// One or more tools require human approval before execution.
    AwaitingApproval,
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

    /// The permission policy for the general coding agent.
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

    /// Build the turn effect for the first model call after a user message.
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

    /// Build the continuation turn effect (after tool results are appended).
    fn continue_turn_effect(ctx: &Context) -> Effect {
        let thread = current_thread(ctx);
        let tools = current_tools(ctx);
        let mode = all_tools_mode(ctx);
        build_turn_effect(
            &thread,
            &tools,
            &mode,
            None,
            None,
            None,
            DEFAULT_MAX_TOOL_ROUNDS,
            None,
            None,
        )
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
            ReactiveState::Idle
            | ReactiveState::Generating
            | ReactiveState::RunningTools
            | ReactiveState::AwaitingApproval => ReactiveState::Session,
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

            // Idle: a user message starts a new turn.
            Idle => match event {
                Event::UserMessage { text } => {
                    // Initialise loop bookkeeping for the first turn.
                    init_loop(ctx, CHAT_THREAD, &[], AGENT_MODE, DEFAULT_MAX_TOOL_ROUNDS);
                    Reaction::transition(
                        Generating,
                        vec![Self::first_turn_effect(text)],
                        "user sent message; starting turn",
                    )
                }
                _ => Reaction::Super(Session),
            },

            // Generating: awaiting LlmTurnComplete (new path) or
            // LlmProposedResponse (legacy converse path).
            Generating => match event {
                // ── New HSM-native path ───────────────────────────────────────
                Event::LlmTurnComplete { .. } => {
                    match on_llm_turn_complete(ctx, event) {
                        GeneratingAction::FinalAnswer { text, .. } => {
                            ctx.set_fact("last_response", json!(text));
                            Reaction::transition(Idle, vec![], "turn complete; final answer")
                        }
                        GeneratingAction::CallTools { calls, tool_effects, .. } => {
                            mark_calls_pending(ctx, &calls);
                            Reaction::transition(
                                RunningTools,
                                tool_effects,
                                "model proposed tool calls; dispatching",
                            )
                        }
                        GeneratingAction::EmptyTurn { nudge_effect } => {
                            Reaction::transition(
                                Generating,
                                vec![nudge_effect],
                                "model returned empty turn; nudging",
                            )
                        }
                        GeneratingAction::MaxRoundsReached { wrapup_effect } => {
                            Reaction::transition(
                                Generating,
                                vec![wrapup_effect],
                                "max tool rounds reached; requesting wrap-up",
                            )
                        }
                    }
                }

                // ── Legacy converse path (ConverseExecutor, Phase E cleanup) ──
                Event::LlmProposedResponse { text } => {
                    ctx.set_fact("last_response", json!(text));
                    Reaction::transition(Idle, vec![], "converse turn complete (legacy)")
                }

                Event::LlmFailed { error } => {
                    ctx.set_fact("last_error", json!(error));
                    Reaction::transition(Idle, vec![], "turn failed")
                }

                _ => Reaction::Super(Session),
            },

            // RunningTools: waiting for all dispatched tool calls to complete.
            RunningTools => match event {
                Event::ToolSucceeded { call_id, .. } | Event::ToolFailed { call_id, .. } => {
                    let all_done = on_tool_result(ctx, call_id);
                    if all_done {
                        // All tool results received → re-enter Generating.
                        let next_turn = Self::continue_turn_effect(ctx);
                        Reaction::transition(
                            Generating,
                            vec![next_turn],
                            "all tools complete; requesting next turn",
                        )
                    } else {
                        Reaction::Handled(vec![])
                    }
                }
                Event::ToolApprovalRequired { call_id, capability, description } => {
                    // Mark this call as no longer pending (it will restart after
                    // approval via HumanApproved or resolve as ToolFailed after
                    // HumanRejected).
                    let _ = on_tool_result(ctx, call_id);
                    ctx.set_fact("approval_pending_call_id", json!(call_id.as_uuid().to_string()));
                    Reaction::transition(
                        AwaitingApproval,
                        vec![Effect::RequestHumanApproval {
                            approval_id: ApprovalId::new(),
                            capability: *capability,
                            description: description.clone(),
                        }],
                        "tool approval required",
                    )
                }
                _ => Reaction::Super(Session),
            },

            // AwaitingApproval: human must approve or reject the pending tool.
            AwaitingApproval => match event {
                Event::HumanApproved { .. } => {
                    // Resume with a fresh re-prompt; tool results should already
                    // be in the thread from the ToolExecutor.
                    if loop_core::all_tools_done(ctx) {
                        let next_turn = Self::continue_turn_effect(ctx);
                        Reaction::transition(
                            Generating,
                            vec![next_turn],
                            "human approved; all tools done; resuming generation",
                        )
                    } else {
                        Reaction::transition(
                            RunningTools,
                            vec![],
                            "human approved; waiting for remaining tools",
                        )
                    }
                }
                Event::HumanRejected { .. } => {
                    // Treat rejection as a ToolFailed (already synthesised by
                    // the kernel's approval-gating path).
                    if loop_core::all_tools_done(ctx) {
                        let next_turn = Self::continue_turn_effect(ctx);
                        Reaction::transition(
                            Generating,
                            vec![next_turn],
                            "human rejected; all tools done; resuming generation",
                        )
                    } else {
                        Reaction::transition(
                            RunningTools,
                            vec![],
                            "human rejected; waiting for remaining tools",
                        )
                    }
                }
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
        // Simulate a TurnExecutor completion with no tool calls.
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
    fn llm_turn_complete_with_tools_enters_running_tools() {
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
        assert_eq!(hsm.state(), ReactiveState::RunningTools);
        assert_eq!(out.effects.len(), 1);
        assert_eq!(out.effects[0].kind(), EffectKind::CallTool);
    }

    #[test]
    fn all_tools_done_triggers_next_generating() {
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
        assert_eq!(hsm.state(), ReactiveState::RunningTools);
        // Simulate tool success.
        let out = hsm.dispatch(&Event::ToolSucceeded { call_id, observation: json!("ok") }, &mut ctx);
        assert_eq!(hsm.state(), ReactiveState::Generating);
        assert_eq!(out.effects.len(), 1);
        assert_eq!(out.effects[0].kind(), EffectKind::CallLlm);
    }

    #[test]
    fn legacy_llm_proposed_response_returns_to_idle() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("hi"), &mut ctx);
        let out = hsm.dispatch(
            &Event::LlmProposedResponse {
                text: "legacy answer".into(),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), ReactiveState::Idle);
        assert!(out.transitioned);
        assert_eq!(
            ctx.fact("last_response").unwrap(),
            &json!("legacy answer")
        );
    }

    #[test]
    fn failure_returns_to_idle() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("hi"), &mut ctx);
        let out = hsm.dispatch(
            &Event::LlmFailed { error: "boom".into() },
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
