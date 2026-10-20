// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The single-turn request a machine hands the kernel inside
//! `Effect::CallLlm`: pure data, so a machine can build one without
//! depending on the model or executor layers.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The JSON `kind` tag that selects the single-turn streaming engine.
pub const TURN_KIND: &str = "turn";

/// The key under which a turn request carries the session's loaded tools; the
/// serialised name of [`TurnRequest::active_tools`].
pub const ACTIVE_TOOLS_KEY: &str = "active_tools";

/// The key under which a turn request carries the session's active
/// instructions; the serialised name of [`TurnRequest::active_instructions`].
pub const ACTIVE_INSTRUCTIONS_KEY: &str = "active_instructions";

/// The key under which a turn request carries the label of the state it was
/// emitted in; the serialised name of [`TurnRequest::state`].
pub const STATE_KEY: &str = "state";

/// A request for the HSM to run a **single streaming turn**: stream the model
/// against a conversation thread, collect proposed tool calls, and post
/// `sven_hsm::Event::LlmTurnComplete` back to the machine.
///
/// The executor (TurnExecutor) handles all I/O, appends the assistant turn to
/// the store, annotates each proposed tool call with its capability, and
/// registers `call_id → thread` in the shared registry.  The machine itself
/// stays pure and only sees the completion event.
///
/// Serialised into `sven_hsm::Effect::CallLlm`'s opaque `request` value with
/// an added `kind: "turn"` discriminator (see [`to_value`]).
///
/// [`to_value`]: TurnRequest::to_value
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct TurnRequest {
    /// Stable thread id (e.g. `"chat"`, `"discovery"`).
    pub thread: String,
    /// Optional user-turn instruction to append to the thread before streaming.
    ///
    /// When set, `TurnExecutor` calls
    /// `store.append(thread, Message::user(instruction))` before taking the
    /// snapshot, so the machine can initiate the first turn simply by putting
    /// the user's text here rather than managing the ThreadStore directly.
    #[serde(default)]
    pub instruction: String,
    /// Names of the tools this turn is allowed to call.
    ///
    /// When empty AND `all_tools_mode` is set, the executor resolves the full
    /// tool set for the given mode string.
    #[serde(default)]
    pub tools: Vec<String>,
    /// When non-empty and `tools` is empty, resolves all tools for this mode
    /// name (e.g. `"agent"`, `"code"`) via `ToolRegistry::schemas_for_mode`.
    #[serde(default)]
    pub all_tools_mode: String,
    /// Tools the session has loaded, in load order, on top of whatever the
    /// request's `tools` or `all_tools_mode` selects.
    ///
    /// Not set by machines: the kernel copies the session's loaded-tool set
    /// onto every turn request as it leaves a transition (see
    /// `Context::active_tools`), so a machine neither tracks nor forgets it.
    /// Meaningful only when tools are disclosed on demand.
    #[serde(default)]
    pub active_tools: Vec<String>,
    /// Ids of the skills, subagents and knowledge documents the session has
    /// activated, in activation order.
    ///
    /// Not set by machines: the kernel copies `Context::active_instructions`
    /// onto every turn request, like [`Self::active_tools`].
    #[serde(default)]
    pub active_instructions: Vec<String>,
    /// The label of the machine state the request was emitted in. Not set by
    /// machines: the kernel stamps it, so an executor can evaluate which
    /// instructions wait for that state.
    #[serde(default)]
    pub state: String,
    /// Optional JSON Schema for structured output (name under `schema_name`).
    #[serde(default)]
    pub schema: Value,
    /// Short name identifying the schema (for OpenAI strict mode).
    #[serde(default)]
    pub schema_name: String,
    /// Optional per-state model override (resolved via the model resolver).
    #[serde(default)]
    pub model: Option<String>,
    /// Volatile context suffix forwarded to providers that support uncached
    /// system blocks (e.g. Anthropic).
    #[serde(default)]
    pub dynamic_suffix: Option<String>,
    /// Maximum tool-call rounds before a forced wrap-up turn.
    #[serde(default)]
    pub max_tool_rounds: Option<u32>,
    /// Tool calls refused before they ran, with the reason, keyed by the
    /// kernel's call id. The executor answers each in the thread before the
    /// model is called: a history with a tool call and no result is invalid.
    #[serde(default)]
    pub refused_calls: Vec<RefusedCall>,
    /// What the instruction is, for the record: `task`, `follow_up`,
    /// `repair` or `correction`. Filed with the user turn it becomes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// The system message to start the thread with. Used only when the thread
    /// has no turns yet; a conversation already under way keeps its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Decoding settings for this turn only, laid over the run's own.
    #[serde(default, skip_serializing_if = "TurnDecoding::is_empty")]
    pub decoding: TurnDecoding,
}

/// Sampling and thinking settings of one turn. Every field is optional; an
/// unset one leaves the run's (or the provider's) setting in place. The same
/// shape is recorded with each model request that used them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TurnDecoding {
    /// Softmax temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Nucleus-sampling mass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Number of highest-probability tokens kept before sampling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Seed for providers that honour one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// Whether the model reasons before answering, for servers whose chat
    /// template has the switch. Providers without it ignore the setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<bool>,
    /// The most thinking tokens before the turn is cut off as a runaway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<u32>,
    /// The most output tokens one response may ask for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Seconds to wait for the server's first response before the turn fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_response_timeout: Option<u64>,
}

impl TurnDecoding {
    /// `true` when no setting is made.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A tool call the machine knows was not run, and why.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefusedCall {
    /// The kernel's id for the call (a `ToolCallId`'s UUID).
    pub call_id: String,
    /// Why it was not run, as the model should read it.
    pub reason: String,
}

impl TurnRequest {
    /// Serialise to a [`serde_json::Value`] with the `kind: "turn"` tag added
    /// so the composite executor can route it.
    ///
    /// # Panics
    ///
    /// Panics only if serialisation fails, which cannot happen for this type.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut v = serde_json::to_value(self).expect("TurnRequest serialisation infallible");
        if let Some(obj) = v.as_object_mut() {
            obj.insert("kind".into(), Value::String(TURN_KIND.into()));
        }
        v
    }

    /// Deserialise from the opaque value carried by `Effect::CallLlm`.
    ///
    /// # Errors
    ///
    /// Returns an error if the value is not a valid [`TurnRequest`].
    pub fn from_value(v: Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(v)
    }

    /// `true` if `request` carries the turn discriminator.
    #[must_use]
    pub fn is_turn(request: &Value) -> bool {
        request.get("kind").and_then(Value::as_str) == Some(TURN_KIND)
    }
}
