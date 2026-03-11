// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
mod agent;
mod compact;
mod events;
pub mod prompts;
mod runtime_context;
mod session;
#[cfg(test)]
mod tests;
mod tool_slots;

pub use agent::{Agent, ModelResolver};
pub use compact::{
    compact_session, compact_session_with_strategy, emergency_compact, smart_truncate,
};
pub use events::{AgentEvent, AgentEventVisitor, CompactionStrategyUsed, PeerInfo};
pub use prompts::{system_prompt, CollabEvent};
pub use runtime_context::AgentRuntimeContext;
pub use session::{Session, TurnRecord};
