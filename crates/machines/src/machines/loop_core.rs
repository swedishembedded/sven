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

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sven_hsm::{
    context::{Context, PendingApproval, PendingQuestion},
    effect::Effect,
    event::Event,
    ids::{ApprovalId, QuestionId, ToolCallId},
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
    /// When set, a tool call is parked awaiting a human answer under this
    /// `QuestionId` (see [`Event::QuestionAsked`]). A run with this set is
    /// not idle and not failed - it is waiting, possibly indefinitely.
    #[serde(default)]
    pub awaiting_answer: Option<QuestionId>,
    /// In-flight calls by id, mapped to their [`CallIdentity`] (tool name plus
    /// fingerprint). Written when calls are proposed, drained as results
    /// arrive; lets a `ToolFailed` event - which carries only the call id -
    /// be attributed to the call shape that keeps failing.
    #[serde(default)]
    pub call_registry: HashMap<ToolCallId, CallIdentity>,
    /// Consecutive identical failures per call fingerprint. An identical
    /// failing call repeated is a stall, not progress; crossing
    /// [`STALL_REDIRECT_THRESHOLD`] arms [`LoopState::stall_nudge`].
    #[serde(default)]
    pub failure_streaks: HashMap<String, u32>,
    /// Redirect instruction to embed in the next continuation turn. Consumed
    /// by [`LoopState::continuation_turn`]; re-armed by the next identical
    /// failure if the model did not change its behavior.
    #[serde(default)]
    pub stall_nudge: Option<String>,
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

    /// `true` when there are no pending tool calls and no approval or
    /// question in flight.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.pending.is_empty()
            && self.awaiting_tool_approval.is_none()
            && self.awaiting_answer.is_none()
    }

    /// Build a continuation `CallLlm { kind:"turn" }` from the current state.
    ///
    /// Consumes [`LoopState::stall_nudge`] when one is armed, delivering it as
    /// the turn's instruction.
    pub fn continuation_turn(&mut self) -> Effect {
        build_turn_effect(
            &self.thread,
            &self.tools,
            &self.all_tools_mode,
            None,
            self.stall_nudge.take().as_deref(),
            None,
            self.max_rounds,
            None,
            None,
        )
    }

    /// Build a continuation turn that also enforces a JSON response schema.
    ///
    /// Consumes [`LoopState::stall_nudge`] like [`LoopState::continuation_turn`];
    /// when both are armed the redirect rides along with the schema demand.
    pub fn continuation_turn_with_schema(&mut self, schema: Value, schema_name: &str) -> Effect {
        build_turn_effect(
            &self.thread,
            &self.tools,
            &self.all_tools_mode,
            None,
            self.stall_nudge.take().as_deref(),
            None,
            self.max_rounds,
            Some(schema),
            Some(schema_name),
        )
    }
}

/// How many identical failures of the same call shape arm the redirect.
///
/// One failure is a normal outcome; the second identical one is a stall the
/// model must be told to stop repeating.
pub const STALL_REDIRECT_THRESHOLD: u32 = 2;

/// Bounds the error text embedded in the redirect so a huge tool error cannot
/// inflate the persisted loop state.
const STALL_ERROR_EXCERPT: usize = 300;

/// Identity of an in-flight call: its tool name (for the redirect text) and
/// its fingerprint (tool name plus a hash of the arguments - the whole call
/// shape, so changed arguments are a different identity).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallIdentity {
    /// Tool name as proposed.
    pub name: String,
    /// Hash of name + canonical argument serialization. Deterministic across
    /// processes (`DefaultHasher::new()` uses fixed keys), so event-sourced
    /// replay reproduces the same fingerprints the live run recorded.
    pub fingerprint: String,
}

/// Identity of a tool call for stall detection.
fn call_fingerprint(name: &str, args: &Value) -> String {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(name.as_bytes());
    hasher.write_u8(0);
    hasher.write(serde_json::to_string(args).unwrap_or_default().as_bytes());
    format!("{:016x}", hasher.finish())
}

/// Attribute proposed calls to their identities, keeping only the calls now
/// in flight.
fn register_calls(ls: &mut LoopState, tool_calls: &[ProposedToolCall]) {
    let identities: HashMap<ToolCallId, CallIdentity> = tool_calls
        .iter()
        .map(|tc| {
            (
                tc.call_id,
                CallIdentity {
                    name: tc.name.clone(),
                    fingerprint: call_fingerprint(&tc.name, &tc.args),
                },
            )
        })
        .collect();
    ls.call_registry.retain(|id, _| identities.contains_key(id));
    ls.call_registry.extend(identities);
}

/// Record a finished call's outcome: a success clears its streak, a failure
/// extends it and - past the threshold - arms the redirect. Returns the
/// redirect text to embed in the next continuation turn, if any.
fn record_outcome(ls: &mut LoopState, call_id: &ToolCallId, error: Option<&str>) {
    let Some(identity) = ls.call_registry.remove(call_id) else {
        return;
    };
    match error {
        None => {
            ls.failure_streaks.remove(&identity.fingerprint);
        }
        Some(err) => {
            let streak = ls.failure_streaks.entry(identity.fingerprint).or_default();
            *streak += 1;
            if *streak >= STALL_REDIRECT_THRESHOLD {
                let excerpt: String = err.chars().take(STALL_ERROR_EXCERPT).collect();
                ls.stall_nudge = Some(format!(
                    "You have now made the same tool call (`{}`) {} times and it failed \
                     identically every time. The last error was: {excerpt}. Do not repeat \
                     this identical call. Change the arguments, use a different tool or \
                     approach, or report the blocker in your final reply.",
                    identity.name, *streak,
                ));
            }
        }
    }
}

// ─── Turn effect builder ──────────────────────────────────────────────────────

/// Build a `CallLlm { kind: "turn" }` effect.
///
/// This is the single canonical way machines produce a turn effect so the
/// `TurnRequest` shape stays consistent.
#[allow(clippy::too_many_arguments)]
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
        awaiting_answer: None,
        call_registry: HashMap::new(),
        failure_streaks: HashMap::new(),
        stall_nudge: None,
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
        if text.trim().is_empty() {
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
    register_calls(&mut ls, tool_calls.as_slice());
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
/// agent passes a plain `continuation_turn`. Either receives the armed stall
/// redirect (if any) as part of the state.
pub fn handle_tool_event<S>(
    ctx: &mut Context,
    make_turn: impl Fn(&mut LoopState) -> Effect,
    event: &Event,
) -> Option<Reaction<S>> {
    match event {
        // ── Tool results: drain pending, emit continuation when all done ──────
        Event::ToolSucceeded { call_id, .. } => {
            let mut ls = LoopState::load(ctx);
            ls.pending.remove(call_id);
            record_outcome(&mut ls, call_id, None);
            if ls.is_idle() {
                let cont = make_turn(&mut ls);
                ls.store(ctx);
                Some(Reaction::effects(vec![cont]))
            } else {
                ls.store(ctx);
                Some(Reaction::handled())
            }
        }
        Event::ToolFailed { call_id, error } => {
            let mut ls = LoopState::load(ctx);
            ls.pending.remove(call_id);
            record_outcome(&mut ls, call_id, Some(error.as_str()));
            if ls.is_idle() {
                let cont = make_turn(&mut ls);
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
            // Derived from the call it gates, never minted fresh: transitions
            // must be pure for event-sourced replay to reproduce state. A
            // random id makes the recorded `HumanApproved { approval_id }`
            // fail the guard below on replay, after which the event falls
            // through to the SDLC phase transition and the replayed machine
            // advances a phase the original never did.
            let approval_id = ApprovalId::from_uuid(call_id.as_uuid());
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
                    let cont = make_turn(&mut ls);
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
                    let cont = make_turn(&mut ls);
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

        // ── Question parked: remove from pending, wait for a human ────────────
        Event::QuestionAsked {
            call_id,
            prompt,
            options,
        } => {
            let mut ls = LoopState::load(ctx);
            // Remove from pending — it won't produce a ToolSucceeded on its
            // own; HumanAnswered (or a stale run simply never resuming) is
            // its only way out.
            ls.pending.remove(call_id);
            // Derived from the call it parks, never minted fresh — see
            // `ToolApprovalRequired`'s identical reasoning: replay must
            // reproduce the same id the live run chose.
            let question_id = QuestionId::from_uuid(call_id.as_uuid());
            ls.awaiting_answer = Some(question_id);
            ctx.set_pending_question(PendingQuestion {
                question_id,
                call_id: *call_id,
                prompt: prompt.clone(),
                options: options.clone(),
            });
            ls.store(ctx);
            Some(Reaction::effects(vec![Effect::RequestHumanAnswer {
                question_id,
                call_id: *call_id,
                prompt: prompt.clone(),
                options: options.clone(),
            }]))
        }

        // ── Question answered: resume the loop, no capability to grant ────────
        Event::HumanAnswered { question_id, .. } => {
            let mut ls = LoopState::load(ctx);
            if ls.awaiting_answer == Some(*question_id) {
                ctx.resolve_question(*question_id);
                ls.awaiting_answer = None;
                if ls.is_idle() {
                    let cont = make_turn(&mut ls);
                    ls.store(ctx);
                    Some(Reaction::effects(vec![cont]))
                } else {
                    ls.store(ctx);
                    Some(Reaction::handled())
                }
            } else {
                // Stale/duplicate answer (already resolved, or for a
                // different loop entirely) — ignore.
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
    register_calls(&mut ls, calls);
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

    /// Replaying a recorded event stream must reproduce the same state *and*
    /// the same permission set.
    ///
    /// The approval id used to be minted with `ApprovalId::new()` inside the
    /// transition. Replay therefore generated a different id than the one in
    /// the recorded `HumanApproved`, the guard failed, and the capability was
    /// never granted — silently, because `Context::approve` no-ops on a
    /// mismatch. Asserting on `granted_capabilities` is what catches it;
    /// asserting on state alone does not.
    #[test]
    fn replaying_an_approval_grants_the_same_capability() {
        let call_id = ToolCallId::new();
        let required = Event::ToolApprovalRequired {
            call_id,
            capability: ToolCapability::ExecuteShell,
            description: "run tests".into(),
        };

        // ── Live run: record the id the machine chose. ──────────────────────
        let mut live = make_ctx();
        init_loop(&mut live, "chat", &[], "", 16);
        let _: Option<Reaction<u8>> =
            handle_tool_event(&mut live, |ls| ls.continuation_turn(), &required);
        let recorded = LoopState::load(&live)
            .awaiting_tool_approval
            .expect("approval requested");
        let approved = Event::HumanApproved {
            approval_id: recorded,
        };
        let _: Option<Reaction<u8>> =
            handle_tool_event(&mut live, |ls| ls.continuation_turn(), &approved);

        // ── Replay: same events, including the *recorded* approval id. ──────
        let mut replayed = make_ctx();
        init_loop(&mut replayed, "chat", &[], "", 16);
        let _: Option<Reaction<u8>> =
            handle_tool_event(&mut replayed, |ls| ls.continuation_turn(), &required);
        assert_eq!(
            LoopState::load(&replayed).awaiting_tool_approval,
            Some(recorded),
            "replay must derive the same approval id, not mint a fresh one"
        );
        let _: Option<Reaction<u8>> =
            handle_tool_event(&mut replayed, |ls| ls.continuation_turn(), &approved);

        assert!(
            LoopState::load(&replayed).awaiting_tool_approval.is_none(),
            "replayed approval must resolve"
        );
        assert!(
            replayed.has_granted(ToolCapability::ExecuteShell),
            "replay must grant the same capability the live run did"
        );
        assert_eq!(
            live.has_granted(ToolCapability::ExecuteShell),
            replayed.has_granted(ToolCapability::ExecuteShell),
            "live and replayed permission sets must agree"
        );
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

    #[test]
    fn question_asked_parks_the_call_and_leaves_the_loop_not_idle() {
        let mut ctx = make_ctx();
        init_loop(&mut ctx, "chat", &[], "", 16);
        let call_id = ToolCallId::new();
        {
            let mut ls = LoopState::load(&ctx);
            ls.pending.insert(call_id);
            ls.store(&mut ctx);
        }
        let asked = Event::QuestionAsked {
            call_id,
            prompt: "Which framework?".into(),
            options: vec!["Axum".into(), "Actix".into()],
        };
        let reaction: Option<Reaction<u8>> =
            handle_tool_event(&mut ctx, |ls| ls.continuation_turn(), &asked);
        assert!(matches!(
            reaction,
            Some(Reaction::Handled(effs)) if matches!(effs.as_slice(), [Effect::RequestHumanAnswer { .. }])
        ));
        let ls = LoopState::load(&ctx);
        assert!(
            !ls.pending.contains(&call_id),
            "parked call leaves the pending set"
        );
        assert!(!ls.is_idle(), "a parked question must not read as idle");
        assert!(ls.awaiting_answer.is_some());
        assert!(ctx.pending_question.is_some());
    }

    #[test]
    fn human_answered_roundtrip_resumes_the_loop() {
        let mut ctx = make_ctx();
        init_loop(&mut ctx, "chat", &[], "", 16);
        let call_id = ToolCallId::new();
        {
            let mut ls = LoopState::load(&ctx);
            ls.pending.insert(call_id);
            ls.store(&mut ctx);
        }
        let asked = Event::QuestionAsked {
            call_id,
            prompt: "Which framework?".into(),
            options: vec![],
        };
        let _: Option<Reaction<u8>> =
            handle_tool_event(&mut ctx, |ls| ls.continuation_turn(), &asked);
        let question_id = LoopState::load(&ctx)
            .awaiting_answer
            .expect("question parked");

        let answered = Event::HumanAnswered {
            question_id,
            answer: "Axum".into(),
        };
        let reaction: Option<Reaction<u8>> =
            handle_tool_event(&mut ctx, |ls| ls.continuation_turn(), &answered);
        assert!(
            reaction.is_some(),
            "resolving the last pending question must resume the loop"
        );
        let ls = LoopState::load(&ctx);
        assert!(ls.awaiting_answer.is_none());
        assert!(ls.is_idle());
        assert!(ctx.pending_question.is_none());
    }

    #[test]
    fn human_answered_with_stale_id_is_ignored() {
        let mut ctx = make_ctx();
        init_loop(&mut ctx, "chat", &[], "", 16);
        let answered = Event::HumanAnswered {
            question_id: QuestionId::new(),
            answer: "whatever".into(),
        };
        let reaction: Option<Reaction<u8>> =
            handle_tool_event(&mut ctx, |ls| ls.continuation_turn(), &answered);
        assert!(reaction.is_none());
    }

    /// Same replay-safety property as `replaying_an_approval_grants_the_same_capability`:
    /// the question id must be derived from the call id, not minted fresh, or
    /// a replayed `HumanAnswered` (carrying the id the live run chose) would
    /// never match and the replayed session would hang parked forever.
    #[test]
    fn replaying_a_question_reproduces_the_same_id() {
        let call_id = ToolCallId::new();
        let asked = Event::QuestionAsked {
            call_id,
            prompt: "x".into(),
            options: vec![],
        };

        let mut live = make_ctx();
        init_loop(&mut live, "chat", &[], "", 16);
        let _: Option<Reaction<u8>> =
            handle_tool_event(&mut live, |ls| ls.continuation_turn(), &asked);
        let recorded = LoopState::load(&live).awaiting_answer.expect("parked");

        let mut replayed = make_ctx();
        init_loop(&mut replayed, "chat", &[], "", 16);
        let _: Option<Reaction<u8>> =
            handle_tool_event(&mut replayed, |ls| ls.continuation_turn(), &asked);
        assert_eq!(
            LoopState::load(&replayed).awaiting_answer,
            Some(recorded),
            "replay must derive the same question id, not mint a fresh one"
        );
    }

    /// The instruction a continuation `CallLlm` effect carries, for asserting
    /// on the redirect text.
    fn continuation_instruction(reaction: Option<Reaction<u8>>) -> String {
        let Some(Reaction::Handled(effects)) = reaction else {
            panic!("expected the loop to continue");
        };
        let Effect::CallLlm { request } = &effects[0] else {
            panic!("expected a CallLlm effect, got {:?}", effects[0]);
        };
        request["instruction"]
            .as_str()
            .expect("instruction is a string")
            .to_string()
    }

    /// A tool call proposed, failed, and re-proposed verbatim is a stall, not
    /// progress. From the second identical failure on, the continuation turn
    /// must carry an explicit redirect so the model stops burning rounds on the
    /// same payload (observed in the wild as a 12-round identical-`todo` storm
    /// that ignored the schema error in the result it was shown).
    #[test]
    fn an_identical_failing_call_repeated_gets_a_redirect() {
        fn propose(call_id: ToolCallId) -> Event {
            Event::LlmTurnComplete {
                thread: "chat".into(),
                text: String::new(),
                tool_calls: vec![sven_hsm::ProposedToolCall {
                    call_id,
                    name: "todo".into(),
                    args: serde_json::json!({ "action": "add", "items": ["1"] }),
                    capability: ToolCapability::WriteFile,
                }],
            }
        }
        fn fail(call_id: ToolCallId) -> Event {
            Event::ToolFailed {
                call_id,
                error: "todo '1' is missing required field 'content'".into(),
            }
        }

        let mut ctx = make_ctx();
        init_loop(&mut ctx, "chat", &["todo".to_string()], "agent", 16);

        // First failure of the shape: an ordinary failure, no redirect yet.
        let first = ToolCallId::new();
        on_llm_turn_complete(&mut ctx, &propose(first));
        let reaction: Option<Reaction<u8>> =
            handle_tool_event(&mut ctx, |ls| ls.continuation_turn(), &fail(first));
        let instruction = continuation_instruction(reaction);
        assert!(
            !instruction.contains("same tool call"),
            "the first failure must not trigger the redirect: {instruction}"
        );

        // Second identical failure (a fresh call id, the same payload): the
        // continuation turn now redirects instead of inviting another blind
        // retry.
        let second = ToolCallId::new();
        on_llm_turn_complete(&mut ctx, &propose(second));
        let reaction: Option<Reaction<u8>> =
            handle_tool_event(&mut ctx, |ls| ls.continuation_turn(), &fail(second));
        let instruction = continuation_instruction(reaction);
        assert!(
            instruction.contains("same tool call") && instruction.contains("todo"),
            "the repeated failure must carry a redirect naming the call: {instruction}"
        );
    }

    /// The redirect keys on the whole call - name and arguments - so a model
    /// that changed its arguments (or switched tools) is not treated as
    /// stuck, and a success clears the streak: a failure after real progress
    /// is an ordinary failure again.
    #[test]
    fn changed_arguments_or_a_success_reset_the_stall() {
        fn propose(name: &str, args: serde_json::Value, call_id: ToolCallId) -> Event {
            Event::LlmTurnComplete {
                thread: "chat".into(),
                text: String::new(),
                tool_calls: vec![sven_hsm::ProposedToolCall {
                    call_id,
                    name: name.into(),
                    args,
                    capability: ToolCapability::WriteFile,
                }],
            }
        }
        let fail = |call_id: ToolCallId| Event::ToolFailed {
            call_id,
            error: "missing required field 'content'".into(),
        };
        let succeed = |call_id: ToolCallId| Event::ToolSucceeded {
            call_id,
            observation: serde_json::json!("ok"),
        };

        let mut ctx = make_ctx();
        init_loop(&mut ctx, "chat", &["todo".to_string()], "agent", 16);

        // Fail twice identically - the stall is armed - then propose the same
        // call with different arguments: that is not the same failing call.
        let a = ToolCallId::new();
        on_llm_turn_complete(&mut ctx, &propose("todo", serde_json::json!({"items": ["1"]}), a));
        let _: Option<Reaction<u8>> = handle_tool_event(&mut ctx, |ls| ls.continuation_turn(), &fail(a));
        let b = ToolCallId::new();
        on_llm_turn_complete(&mut ctx, &propose("todo", serde_json::json!({"items": ["1"]}), b));
        let _: Option<Reaction<u8>> = handle_tool_event(&mut ctx, |ls| ls.continuation_turn(), &fail(b));

        let c = ToolCallId::new();
        on_llm_turn_complete(
            &mut ctx,
            &propose("todo", serde_json::json!({"action": "add", "items": [{"id": "1", "content": "x"}]}), c),
        );
        let instruction = continuation_instruction(handle_tool_event(
            &mut ctx,
            |ls| ls.continuation_turn(),
            &fail(c),
        ));
        assert!(
            !instruction.contains("same tool call"),
            "changed arguments are a new attempt, not the same failing call: {instruction}"
        );

        // A success clears the streak: fail once after succeeding and there is
        // again no redirect.
        let d = ToolCallId::new();
        on_llm_turn_complete(&mut ctx, &propose("todo", serde_json::json!({"items": ["1"]}), d));
        let _: Option<Reaction<u8>> =
            handle_tool_event(&mut ctx, |ls| ls.continuation_turn(), &succeed(d));
        let e = ToolCallId::new();
        on_llm_turn_complete(&mut ctx, &propose("todo", serde_json::json!({"items": ["1"]}), e));
        let instruction = continuation_instruction(handle_tool_event(
            &mut ctx,
            |ls| ls.continuation_turn(),
            &fail(e),
        ));
        assert!(
            !instruction.contains("same tool call"),
            "a success must clear the streak: {instruction}"
        );
    }

    /// A reply made of nothing but whitespace is an empty turn: the model said
    /// nothing, and the nudge - not a silent final answer - is the response.
    #[test]
    fn a_whitespace_only_reply_is_an_empty_turn() {
        let mut ctx = make_ctx();
        init_loop(&mut ctx, "chat", &[], "agent", 16);
        let event = Event::LlmTurnComplete {
            thread: "chat".into(),
            text: "   \n\t".into(),
            tool_calls: vec![],
        };
        let action = on_llm_turn_complete(&mut ctx, &event);
        assert!(
            matches!(action, GeneratingAction::EmptyTurn { .. }),
            "whitespace-only text is not a final answer"
        );
    }
}
