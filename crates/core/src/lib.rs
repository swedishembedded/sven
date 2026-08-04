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

pub use stream_turn::{stream_turn, to_model_schemas, ModelResolver};
pub use compact::{
    compact_session, compact_session_with_strategy, emergency_compact, smart_truncate,
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
