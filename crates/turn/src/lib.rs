// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The impure turn-execution primitives: the single-turn LLM streaming call,
//! context compaction, tool-argument-JSON repair, and system-prompt
//! assembly.
//!
//! These are deliberately not part of the pure `Machine` state-transition
//! layer in `sven-machines`: `stream_turn` makes the real async LLM call
//! (`sven_model::ModelProvider`) and `compact` decides what to summarize —
//! both are I/O-adjacent turn primitives, not machine transitions. Sits at
//! "domain" tier, below `sven-executors` ("machines" tier), which drives
//! them from effects; `architecture.toml` forbids `sven-machines` from
//! reaching this crate at all.

mod compact;
mod tool_slots;

pub mod prompts;
mod runtime_context;
pub mod stream_turn;

pub use compact::{
    compact_session, compact_session_with_strategy, emergency_compact, finish_compaction,
    prepare_compaction, smart_truncate, CompactionPlan,
};
pub use prompts::{system_prompt, CollabEvent};
pub use runtime_context::AgentRuntimeContext;
pub use stream_turn::{
    set_thinking_budget_override, stream_turn, thinking_budget_override, to_model_schemas,
    AbortedError, ModelResolver, ThinkingBudget, TurnLimits,
};
