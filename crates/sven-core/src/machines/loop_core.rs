// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Shared agentic loop state-machine helpers.
//!
//! Every machine that runs the HSM-native model↔tool loop composes this module
//! rather than duplicating the logic.  The loop pattern is now **in-state**:
//! the active phase/state emits `CallTool` effects and **stays put** (via
//! `Reaction::Handled`) while tools execute.  There are no more shared
//! `RunningTools` / `AwaitingApproval` sibling states.
//!
//! # How it works
//!
//! 1. On phase entry the machine calls [`init_loop`] and emits `CallLlm`.
//! 2. On `LlmTurnComplete`, the machine calls [`on_llm_turn_complete`] and acts
//!    on the returned [`GeneratingAction`].
//! 3. If tools were proposed, the machine emits `CallTool` effects and stays
//!    (the [`LoopState::pending`] set is updated inside `on_llm_turn_complete`).
//! 4. On `ToolSucceeded` / `ToolFailed` / `ToolApprovalRequired` /
//!    `HumanApproved` (tool) / `HumanRejected` (tool), the machine delegates to
//!    [`handle_tool_event`].  It returns `None` for non-tool events so the phase
//!    can handle decision-level approvals itself.
//!
//! # `LoopState` — one typed fact key
//!
//! All bookkeeping lives in a single [`LoopState`] value serialized under
//! [`LOOP_STATE_KEY`].  This replaces the six `KEY_*` string-keyed JSON facts
//! used previously and makes state access type-safe and refactor-friendly.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sven_hsm::{
    context::{Context, PendingApproval},
    effect::Effect,
    event::Event,
    ids::{ApprovalId, ToolCallId},
    status::Reaction,
    ProposedToolCall,
};

use sven_llm::TurnRequest;

/// The single context key under which [`LoopState`] is serialized.
pub const LOOP_STATE_KEY: &str = "lc_state";

// ─── Typed loop bookkeeping ───────────────────────────────────────────────────

/// All loop bookkeeping in one serializable struct.
///
/// Stored under [`LOOP_STATE_KEY`] via [`LoopState::load`] / [`LoopState::store`].
/// Type-safe and refactor-friendly compared to the previous six `KEY_*` JSON
/// string keys.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LoopState {
    /// Conversation thread id (e.g. `"chat"`, `"intake"`).
    pub thread: String,
    /// Tool names allowed in this loop (passed to `TurnRequest`).
    pub tools: Vec<String>,
    /// `all_tools_mode` passed to `TurnRequest` when `tools` is empty.
    pub all_tools_mode: String,
    /// Current tool-call round counter (incremented on each `LlmTurnComplete`).
    pub round: u32,
    /// Maximum tool-call rounds before a wrap-up nudge.
    pub max_rounds: u32,
    /// In-flight `CallTool` call IDs.  Drained as results arrive.
    pub pending: HashSet<ToolCallId>,
    /// When set, a tool-level `RequestHumanApproval` is in flight under this
    /// `ApprovalId`.  `HumanApproved`/`HumanRejected` matching this ID are
    /// tool-loop events (stay in phase); those without it are decision-level
    /// approvals the phase handles itself.
    pub awaiting_tool_approval: Option<ApprovalId>,
}

impl LoopState {
    /// Load from context, returning a default value if not yet initialised.
    #[must_use]
    pub fn load(ctx: &Context) -> Self {
        ctx.fact(LOOP_STATE_KEY)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Persist to context.
    pub fn store(&self, ctx: &mut Context) {
        if let Ok(v) = serde_json::to_value(self) {
            ctx.set_fact(LOOP_STATE_KEY, v);
        }
    }

    /// `true` when there are no pending tool calls and no approval in flight.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.pending.is_empty() && self.awaiting_tool_approval.is_none()
    }

    /// Build a continuation `CallLlm { kind:"turn" }` from the current state.
    #[must_use]
    pub fn continuation_turn(&self) -> Effect {
        build_turn_effect(
            &self.thread,
            &self.tools,
            &self.all_tools_mode,
            None,
            None,
            None,
            self.max_rounds,
            None,
            None,
        )
    }

    /// Build a continuation turn that also enforces a JSON response schema.
    #[must_use]
    pub fn continuation_turn_with_schema(&self, schema: Value, schema_name: &str) -> Effect {
        build_turn_effect(
            &self.thread,
            &self.tools,
            &self.all_tools_mode,
            None,
            None,
            None,
            self.max_rounds,
            Some(schema),
            Some(schema_name),
        )
    }
}

// ─── Turn effect builder ──────────────────────────────────────────────────────

/// Build a `CallLlm { kind: "turn" }` effect.
///
/// This is the single canonical way machines produce a turn effect so the
/// `TurnRequest` shape stays consistent.
pub fn build_turn_effect(
    thread: &str,
    tools: &[String],
    all_tools_mode: &str,
    model: Option<&str>,
    instruction: Option<&str>,
    dynamic_suffix: Option<String>,
    max_tool_rounds: u32,
    response_format_schema: Option<Value>,
    response_format_schema_name: Option<&str>,
) -> Effect {
    let req = TurnRequest {
        thread: thread.to_string(),
        instruction: instruction.unwrap_or("").to_string(),
        tools: tools.to_vec(),
        all_tools_mode: all_tools_mode.to_string(),
        schema: response_format_schema.unwrap_or(Value::Null),
        schema_name: response_format_schema_name.unwrap_or("").to_string(),
        model: model.map(str::to_string),
        dynamic_suffix,
        max_tool_rounds: Some(max_tool_rounds),
    };
    Effect::CallLlm {
        request: req.to_value(),
    }
}

// ─── Loop initialisation ──────────────────────────────────────────────────────

/// Initialise loop bookkeeping when a phase is entered for the first time.
///
/// Call this once from the phase entry handler before emitting the first
/// `CallLlm` effect.
pub fn init_loop(
    ctx: &mut Context,
    thread: &str,
    tools: &[String],
    all_tools_mode: &str,
    max_rounds: u32,
) {
    let ls = LoopState {
        thread: thread.to_string(),
        tools: tools.to_vec(),
        all_tools_mode: all_tools_mode.to_string(),
        round: 0,
        max_rounds,
        pending: HashSet::new(),
        awaiting_tool_approval: None,
    };
    ls.store(ctx);
}

// ─── `LlmTurnComplete` handler ────────────────────────────────────────────────

/// The action the machine should take after [`Event::LlmTurnComplete`].
pub enum GeneratingAction {
    /// The model produced no tool calls — `text` is the final answer.
    FinalAnswer { thread: String, text: String },

    /// The model proposed tool calls.  The machine should emit `tool_effects`
    /// (one `CallTool` per call) and **stay** in the current state.
    CallTools {
        thread: String,
        tool_effects: Vec<Effect>,
        calls: Vec<ProposedToolCall>,
    },

    /// The model produced neither text nor tool calls.  Emit the nudge turn
    /// and **stay**.
    EmptyTurn { nudge_effect: Effect },

    /// `max_rounds` was exceeded while tool calls were still present.  Emit the
    /// wrap-up turn and **stay** (or bail to `Failed`/`Recovery`).
    MaxRoundsReached { wrapup_effect: Effect },
}

/// Nudge instruction appended when the model emits an empty turn.
const EMPTY_TURN_NUDGE: &str =
    "Your last response was empty. Please provide your analysis, answer, or \
     use one of the available tools to make progress.";

/// Instruction appended when `max_tool_rounds` is exceeded.
const MAX_ROUNDS_NUDGE: &str =
    "You have reached the maximum tool-call budget. Do not call any more tools. \
     Respond now with your final structured decision per the required schema.";

/// Decide what the machine in its generating state should do after receiving
/// [`Event::LlmTurnComplete`].
///
/// Increments the round counter, updates `pending` (for tool calls), and
/// returns a [`GeneratingAction`] for the machine to convert to effects.
pub fn on_llm_turn_complete(ctx: &mut Context, event: &Event) -> GeneratingAction {
    let (thread, text, tool_calls) = match event {
        Event::LlmTurnComplete {
            thread,
            text,
            tool_calls,
        } => (thread.clone(), text.clone(), tool_calls.clone()),
        _ => {
            let ls = LoopState::load(ctx);
            return GeneratingAction::FinalAnswer {
                thread: ls.thread.clone(),
                text: String::new(),
            };
        }
    };

    let mut ls = LoopState::load(ctx);
    ls.round += 1;
    let round = ls.round;
    let max = ls.max_rounds;

    // Max rounds exceeded AND there are tool calls → force a wrap-up turn.
    if round > max && !tool_calls.is_empty() {
        let wrapup_effect = build_turn_effect(
            &thread,
            &[],
            "",
            None,
            Some(MAX_ROUNDS_NUDGE),
            None,
            0,
            None,
            None,
        );
        ls.store(ctx);
        return GeneratingAction::MaxRoundsReached { wrapup_effect };
    }

    if tool_calls.is_empty() {
        ls.store(ctx);
        if text.is_empty() {
            let nudge_effect = build_turn_effect(
                &thread,
                &ls.tools,
                &ls.all_tools_mode,
                None,
                Some(EMPTY_TURN_NUDGE),
                None,
                max.saturating_sub(round),
                None,
                None,
            );
            return GeneratingAction::EmptyTurn { nudge_effect };
        }
        return GeneratingAction::FinalAnswer { thread, text };
    }

    // Tool calls proposed: register them as pending and return their effects.
    ls.pending = tool_calls.iter().map(|tc| tc.call_id).collect();
    ls.store(ctx);

    let tool_effects: Vec<Effect> = tool_calls
        .iter()
        .map(|tc| Effect::CallTool {
            call_id: tc.call_id,
            name: tc.name.clone(),
            capability: tc.capability,
            args: tc.args.clone(),
        })
        .collect();

    GeneratingAction::CallTools {
        thread,
        tool_effects,
        calls: tool_calls,
    }
}

// ─── Shared in-state tool-loop helper ─────────────────────────────────────────

/// Handle a tool-related event while the phase is running its tool loop.
///
/// Returns `Some(reaction)` when the event was handled as a tool/approval
/// event; returns `None` for all other events so the phase's own arms can
/// handle decision-level approvals (advancing to the next phase).
///
/// `make_turn` is a closure that builds the continuation `CallLlm` effect.
/// SDLC phases pass a closure that includes the decision schema; the reactive
/// agent passes a plain `continuation_turn`.
pub fn handle_tool_event<S>(
    ctx: &mut Context,
    make_turn: impl Fn(&LoopState) -> Effect,
    event: &Event,
) -> Option<Reaction<S>> {
    match event {
        // ── Tool results: drain pending, emit continuation when all done ──────
        Event::ToolSucceeded { call_id, .. } | Event::ToolFailed { call_id, .. } => {
            let mut ls = LoopState::load(ctx);
            ls.pending.remove(call_id);
            if ls.is_idle() {
                let cont = make_turn(&ls);
                ls.store(ctx);
                Some(Reaction::effects(vec![cont]))
            } else {
                ls.store(ctx);
                Some(Reaction::handled())
            }
        }

        // ── Tool approval required: request human consent, stay in phase ──────
        Event::ToolApprovalRequired {
            call_id,
            capability,
            description,
        } => {
            let mut ls = LoopState::load(ctx);
            // Remove from pending — it won't produce a ToolSucceeded.
            ls.pending.remove(call_id);
            let approval_id = ApprovalId::new();
            ls.awaiting_tool_approval = Some(approval_id);
            ctx.set_pending_approval(PendingApproval {
                approval_id,
                capability: *capability,
                description: description.clone(),
            });
            ls.store(ctx);
            Some(Reaction::effects(vec![Effect::RequestHumanApproval {
                approval_id,
                capability: *capability,
                description: description.clone(),
            }]))
        }

        // ── Approval resolved: grant / no-grant, resume loop if idle ─────────
        Event::HumanApproved { approval_id } => {
            let mut ls = LoopState::load(ctx);
            if ls.awaiting_tool_approval == Some(*approval_id) {
                // Grant the capability so the continuation turn can use it.
                ctx.approve(*approval_id);
                ls.awaiting_tool_approval = None;
                if ls.is_idle() {
                    let cont = make_turn(&ls);
                    ls.store(ctx);
                    Some(Reaction::effects(vec![cont]))
                } else {
                    ls.store(ctx);
                    Some(Reaction::handled())
                }
            } else {
                // Decision-level approval (not tool-level) — let phase handle it.
                None
            }
        }

        Event::HumanRejected { approval_id } => {
            let mut ls = LoopState::load(ctx);
            if ls.awaiting_tool_approval == Some(*approval_id) {
                ls.awaiting_tool_approval = None;
                // Rejected tool: resume loop (LLM sees the failure result).
                if ls.is_idle() {
                    let cont = make_turn(&ls);
                    ls.store(ctx);
                    Some(Reaction::effects(vec![cont]))
                } else {
                    ls.store(ctx);
                    Some(Reaction::handled())
                }
            } else {
                // Decision-level rejection — let phase handle it.
                None
            }
        }

        _ => None,
    }
}

// ─── Backward-compat helpers (thin wrappers) ──────────────────────────────────
// Kept so callers that have not yet been migrated compile without changes.

/// Read `all_tools_mode` from context.
pub fn all_tools_mode(ctx: &Context) -> String {
    LoopState::load(ctx).all_tools_mode
}

/// `true` when the pending call set is empty and no approval is in flight.
pub fn all_tools_done(ctx: &Context) -> bool {
    LoopState::load(ctx).is_idle()
}

/// Read the current round counter.
pub fn current_round(ctx: &Context) -> u64 {
    LoopState::load(ctx).round as u64
}

/// Read the configured maximum round limit.
pub fn max_rounds(ctx: &Context) -> u64 {
    LoopState::load(ctx).max_rounds as u64
}

/// Read the current thread id.
pub fn current_thread(ctx: &Context) -> String {
    LoopState::load(ctx).thread
}

/// Read the configured tool names.
pub fn current_tools(ctx: &Context) -> Vec<String> {
    LoopState::load(ctx).tools
}

/// Register a batch of pending call IDs (compat; `on_llm_turn_complete` now
/// does this automatically).
pub fn mark_calls_pending(ctx: &mut Context, calls: &[ProposedToolCall]) {
    let mut ls = LoopState::load(ctx);
    ls.pending = calls.iter().map(|c| c.call_id).collect();
    ls.store(ctx);
}

/// Remove a completed call.  Returns `true` when the pending set is empty.
pub fn on_tool_result(ctx: &mut Context, call_id: &ToolCallId) -> bool {
    let mut ls = LoopState::load(ctx);
    ls.pending.remove(call_id);
    let done = ls.is_idle();
    ls.store(ctx);
    done
}

/// Increment the round counter and return the new value.
pub fn increment_round(ctx: &mut Context) -> u64 {
    let mut ls = LoopState::load(ctx);
    ls.round += 1;
    let r = ls.round;
    ls.store(ctx);
    r as u64
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sven_hsm::{context::Context, permissions::ToolCapability};

    fn make_ctx() -> Context {
        Context::new()
    }

    #[test]
    fn loop_state_round_trips() {
        let mut ctx = make_ctx();
        init_loop(&mut ctx, "chat", &["read_file".to_string()], "agent", 16);
        let ls = LoopState::load(&ctx);
        assert_eq!(ls.thread, "chat");
        assert_eq!(ls.max_rounds, 16);
        assert!(ls.pending.is_empty());
        assert!(ls.is_idle());
    }

    #[test]
    fn on_llm_turn_complete_increments_round() {
        let mut ctx = make_ctx();
        init_loop(&mut ctx, "chat", &[], "agent", 16);
        let call_id = ToolCallId::new();
        let event = Event::LlmTurnComplete {
            thread: "chat".to_string(),
            text: String::new(),
            tool_calls: vec![sven_hsm::ProposedToolCall {
                call_id,
                name: "read_file".to_string(),
                args: serde_json::json!({}),
                capability: ToolCapability::ReadFile,
            }],
        };
        let action = on_llm_turn_complete(&mut ctx, &event);
        assert!(matches!(action, GeneratingAction::CallTools { .. }));
        let ls = LoopState::load(&ctx);
        assert_eq!(ls.round, 1);
        assert!(ls.pending.contains(&call_id));
    }

    #[test]
    fn handle_tool_event_drains_pending_and_continues() {
        let mut ctx = make_ctx();
        init_loop(&mut ctx, "chat", &[], "agent", 16);
        let call_id = ToolCallId::new();
        // Simulate pending call registered
        {
            let mut ls = LoopState::load(&ctx);
            ls.pending.insert(call_id);
            ls.store(&mut ctx);
        }
        let event = Event::ToolSucceeded {
            call_id,
            observation: serde_json::json!("ok"),
        };
        let reaction: Option<Reaction<u8>> =
            handle_tool_event(&mut ctx, |ls| ls.continuation_turn(), &event);
        assert!(reaction.is_some());
        let ls = LoopState::load(&ctx);
        assert!(ls.pending.is_empty());
    }

    #[test]
    fn handle_tool_event_returns_none_for_unrelated_events() {
        let mut ctx = make_ctx();
        init_loop(&mut ctx, "chat", &[], "", 16);
        let event = Event::UserMessage { text: "hi".into() };
        let reaction: Option<Reaction<u8>> =
            handle_tool_event(&mut ctx, |ls| ls.continuation_turn(), &event);
        assert!(reaction.is_none());
    }

    #[test]
    fn handle_tool_event_approval_roundtrip() {
        let mut ctx = make_ctx();
        init_loop(&mut ctx, "chat", &[], "", 16);
        let call_id = ToolCallId::new();
        // ToolApprovalRequired sets awaiting_tool_approval
        let approval_event = Event::ToolApprovalRequired {
            call_id,
            capability: ToolCapability::ExecuteShell,
            description: "run tests".into(),
        };
        let reaction: Option<Reaction<u8>> =
            handle_tool_event(&mut ctx, |ls| ls.continuation_turn(), &approval_event);
        assert!(reaction.is_some());
        let ls = LoopState::load(&ctx);
        let approval_id = ls.awaiting_tool_approval.expect("should be set");

        // HumanApproved with matching id → clears awaiting and emits continuation
        let approved = Event::HumanApproved { approval_id };
        let reaction: Option<Reaction<u8>> =
            handle_tool_event(&mut ctx, |ls| ls.continuation_turn(), &approved);
        assert!(reaction.is_some());
        let ls2 = LoopState::load(&ctx);
        assert!(ls2.awaiting_tool_approval.is_none());
    }

    #[test]
    fn human_approved_without_tool_approval_returns_none() {
        let mut ctx = make_ctx();
        init_loop(&mut ctx, "chat", &[], "", 16);
        // No awaiting_tool_approval set — this is a decision-level event
        let event = Event::HumanApproved {
            approval_id: ApprovalId::new(),
        };
        let reaction: Option<Reaction<u8>> =
            handle_tool_event(&mut ctx, |ls| ls.continuation_turn(), &event);
        assert!(reaction.is_none());
    }
}
