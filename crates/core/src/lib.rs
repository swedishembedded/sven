// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
mod compact;
mod events;
pub mod prompts;
mod runtime_context;
mod session;
pub mod stream_turn;
mod tool_slots;

// ── HSM-based machines (Phase 2) ─────────────────────────────────────────────
pub mod completion;
pub mod machines;
pub mod mode;

pub use stream_turn::{
    set_thinking_budget_override, stream_turn, thinking_budget_override, to_model_schemas,
    AbortedError, ModelResolver, ThinkingBudget,
};
pub use compact::{
    compact_session, compact_session_with_strategy, emergency_compact, finish_compaction,
    prepare_compaction, smart_truncate, CompactionPlan,
};
pub use completion::development_complete;
pub use events::{AgentEvent, AgentEventVisitor, CompactionStrategyUsed, PeerInfo};
pub use machines::{
    graph::{policy::policy_from_graph, GraphMachine},
    reactive_agent::ReactiveAgentMachine,
    sdlc::task::TaskMachine,
    sdlc::SdlcMachine,
};
pub use mode::ModeRegistry;
pub use prompts::{system_prompt, CollabEvent};
pub use runtime_context::AgentRuntimeContext;
pub use session::{Session, TurnRecord};
