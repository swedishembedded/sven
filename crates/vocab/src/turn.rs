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
