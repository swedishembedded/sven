// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Shared agentic loop state-machine helpers.
//!
//! Every machine that runs the HSM-native model↔tool loop composes this module
//! rather than duplicating the logic.  The loop states are:
//!
//! ```text
//! Idle → Generating → RunningTools ↔ AwaitingApproval → Generating → … → Idle
//! ```
//!
//! # Context keys
//!
//! `loop_core` stores its bookkeeping under well-known keys in the kernel
//! [`Context`] so machines stay pure and don't carry ad-hoc fields:
//!
//! - [`KEY_PENDING_CALLS`]  — JSON array of pending `ToolCallId` UUIDs
//! - [`KEY_TOOL_ROUND`]     — current tool-call round counter (u64)
//! - [`KEY_MAX_ROUNDS`]     — configured round limit (u64)
//! - [`KEY_THREAD`]         — current conversation thread id (string)
//! - [`KEY_TOOLS`]          — JSON array of allowed tool names (strings)
//!
//! # Usage
//!
//! A machine's `Generating` state calls [`on_llm_turn_complete`] and acts on
//! the returned [`GeneratingAction`]; `RunningTools` calls [`on_tool_result`]
//! and checks [`all_tools_done`].  [`build_turn_effect`] produces the correct
//! `CallLlm` effect for the next (or first) turn.

use serde_json::{json, Value};
use sven_hsm::{
    context::Context,
    effect::Effect,
    event::Event,
    ids::ToolCallId,
    ProposedToolCall,
};

use sven_llm::TurnRequest;

// ─── Context key constants ────────────────────────────────────────────────────

/// JSON array of pending ToolCallId UUIDs (as strings).
pub const KEY_PENDING_CALLS: &str = "lc_pending_calls";
/// Current tool-call round counter.
pub const KEY_TOOL_ROUND: &str = "lc_tool_round";
/// Configured maximum tool-call rounds.
pub const KEY_MAX_ROUNDS: &str = "lc_max_rounds";
/// The conversation thread this loop is running against.
pub const KEY_THREAD: &str = "lc_thread";
/// JSON array of allowed tool names for this loop.
pub const KEY_TOOLS: &str = "lc_tools";
/// Mode string for "all tools" resolution (empty = use `KEY_TOOLS`).
pub const KEY_ALL_TOOLS_MODE: &str = "lc_all_tools_mode";

// ─── Turn effect builder ─────────────────────────────────────────────────────

/// Build a `CallLlm { kind: "turn" }` effect for the given parameters.
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

// ─── Context helpers ──────────────────────────────────────────────────────────

/// Initialise loop bookkeeping in context when entering `Generating`.
///
/// Call this once when the machine first transitions from `Idle` to
/// `Generating` to set the thread, tools, and round limit.
pub fn init_loop(
    ctx: &mut Context,
    thread: &str,
    tools: &[String],
    all_tools_mode: &str,
    max_rounds: u32,
) {
    ctx.set_fact(KEY_THREAD, json!(thread));
    ctx.set_fact(KEY_TOOLS, json!(tools));
    ctx.set_fact(KEY_ALL_TOOLS_MODE, json!(all_tools_mode));
    ctx.set_fact(KEY_MAX_ROUNDS, json!(max_rounds));
    ctx.set_fact(KEY_TOOL_ROUND, json!(0u64));
    ctx.set_fact(KEY_PENDING_CALLS, json!([]));
}

/// Read the `all_tools_mode` string from context.
pub fn all_tools_mode(ctx: &Context) -> String {
    ctx.fact(KEY_ALL_TOOLS_MODE)
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Record the start of a new tool-call round in context.
///
/// Returns the new round counter.
pub fn increment_round(ctx: &mut Context) -> u64 {
    let round = ctx
        .fact(KEY_TOOL_ROUND)
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + 1;
    ctx.set_fact(KEY_TOOL_ROUND, json!(round));
    round
}

/// Register a batch of pending call IDs in context (entering `RunningTools`).
///
/// The IDs are stored as UUID strings; when a `ToolSucceeded`/`ToolFailed`
/// event arrives they are removed via [`on_tool_result`].
pub fn mark_calls_pending(ctx: &mut Context, calls: &[ProposedToolCall]) {
    let ids: Vec<Value> = calls
        .iter()
        .map(|c| json!(c.call_id.as_uuid().to_string()))
        .collect();
    ctx.set_fact(KEY_PENDING_CALLS, json!(ids));
}

/// Remove a completed call from the pending set.
///
/// Returns `true` when the pending set is now empty (all tools done).
pub fn on_tool_result(ctx: &mut Context, call_id: &ToolCallId) -> bool {
    let id_str = call_id.as_uuid().to_string();
    let mut pending: Vec<Value> = ctx
        .fact(KEY_PENDING_CALLS)
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    pending.retain(|v| v.as_str() != Some(&id_str));
    ctx.set_fact(KEY_PENDING_CALLS, json!(pending));
    pending.is_empty()
}

/// `true` when the pending call set is empty.
pub fn all_tools_done(ctx: &Context) -> bool {
    ctx.fact(KEY_PENDING_CALLS)
        .and_then(|v| v.as_array())
        .map(Vec::is_empty)
        .unwrap_or(true)
}

/// Read the current round counter from context.
pub fn current_round(ctx: &Context) -> u64 {
    ctx.fact(KEY_TOOL_ROUND)
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

/// Read the configured maximum round limit from context.
pub fn max_rounds(ctx: &Context) -> u64 {
    ctx.fact(KEY_MAX_ROUNDS)
        .and_then(Value::as_u64)
        .unwrap_or(16)
}

/// Read the current thread id from context.
pub fn current_thread(ctx: &Context) -> String {
    ctx.fact(KEY_THREAD)
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Read the configured tool names from context.
pub fn current_tools(ctx: &Context) -> Vec<String> {
    ctx.fact(KEY_TOOLS)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

// ─── LlmTurnComplete handler ─────────────────────────────────────────────────

/// The action the machine should take in response to [`Event::LlmTurnComplete`].
pub enum GeneratingAction {
    /// The model produced no tool calls — the text is the final answer.
    ///
    /// The machine should transition to `Idle` and surface `text`.
    FinalAnswer {
        thread: String,
        text: String,
    },

    /// The model proposed one or more tool calls.
    ///
    /// The machine should emit each [`Effect::CallTool`] effect, store the
    /// pending call IDs, and transition to `RunningTools`.
    CallTools {
        thread: String,
        tool_effects: Vec<Effect>,
        calls: Vec<ProposedToolCall>,
    },

    /// The model produced neither text nor tool calls (empty turn).
    ///
    /// The machine should emit a nudge turn effect and stay in `Generating`.
    EmptyTurn {
        nudge_effect: Effect,
    },

    /// The configured `max_tool_rounds` was reached while tool calls were
    /// still present; the wrap-up turn instructs the model to conclude.
    MaxRoundsReached {
        wrapup_effect: Effect,
    },
}

/// Nudge message appended when the model emits an empty turn.
const EMPTY_TURN_NUDGE: &str =
    "Your last response was empty. Please provide your analysis, answer, or \
     use one of the available tools to make progress.";

/// Wrap-up nudge appended when `max_tool_rounds` is exceeded.
const MAX_ROUNDS_NUDGE: &str =
    "You have reached the maximum tool-call budget. Do not call any more tools. \
     Respond now with your final structured decision per the required schema.";

/// Decide what the machine in `Generating` should do after receiving
/// [`Event::LlmTurnComplete`].
///
/// Increments the round counter and checks against `max_rounds`.  Returns a
/// [`GeneratingAction`] the caller converts into effects and a state transition.
pub fn on_llm_turn_complete(ctx: &mut Context, event: &Event) -> GeneratingAction {
    let (thread, text, tool_calls) = match event {
        Event::LlmTurnComplete {
            thread,
            text,
            tool_calls,
        } => (thread.clone(), text.clone(), tool_calls.clone()),
        _ => {
            return GeneratingAction::FinalAnswer {
                thread: current_thread(ctx),
                text: String::new(),
            };
        }
    };

    let round = increment_round(ctx);
    let max = max_rounds(ctx);
    let tools = current_tools(ctx);

    let mode = all_tools_mode(ctx);

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
        return GeneratingAction::MaxRoundsReached { wrapup_effect };
    }

    if tool_calls.is_empty() {
        if text.is_empty() {
            // Empty turn: nudge the model.
            let nudge_effect = build_turn_effect(
                &thread,
                &tools,
                &mode,
                None,
                Some(EMPTY_TURN_NUDGE),
                None,
                (max.saturating_sub(round)) as u32,
                None,
                None,
            );
            return GeneratingAction::EmptyTurn { nudge_effect };
        }
        return GeneratingAction::FinalAnswer { thread, text };
    }

    // The model proposed tool calls — build one `Effect::CallTool` per call.
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
