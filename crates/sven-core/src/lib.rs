// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
mod agent;
mod compact;
mod events;
mod prompts;
mod runtime_context;
mod session;
#[cfg(test)]
mod tests;

pub use agent::Agent;
pub use compact::{
    compact_session, compact_session_with_strategy, emergency_compact, smart_truncate,
};
pub use events::{AgentEvent, CompactionStrategyUsed};
pub use prompts::system_prompt;
pub use runtime_context::AgentRuntimeContext;
pub use session::{Session, TurnRecord};
