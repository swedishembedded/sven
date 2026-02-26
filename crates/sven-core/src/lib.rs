// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
mod session;
mod compact;
mod events;
mod agent;
mod prompts;
mod runtime_context;
#[cfg(test)]
mod tests;

pub use session::{Session, TurnRecord};
pub use events::{AgentEvent, CompactionStrategyUsed};
pub use agent::Agent;
pub use compact::{compact_session, compact_session_with_strategy, smart_truncate, emergency_compact};
pub use prompts::system_prompt;
pub use runtime_context::AgentRuntimeContext;
