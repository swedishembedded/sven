// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
mod events;
mod runtime_context;
mod session;

// ── HSM-based machines (Phase 2) ─────────────────────────────────────────────
pub mod completion;
pub mod machines;
pub mod mode;

// The impure turn-execution primitives (single-turn LLM streaming, context
// compaction, tool-arg JSON repair, system-prompt assembly) live in
// `sven-turn` (domain tier, below this "machines"-tier crate) as of Phase 4.1
// of the crate-architecture refactor plan. Re-exported here unchanged so
// existing `sven_machines::stream_turn`/`sven_machines::prompts::*` call sites don't
// need to change; `sven-executors` depends on `sven-turn` directly instead
// (Phase 4.2), which is what lets the `sven-executors -> sven-core` same-tier
// edge disappear.
pub use sven_turn::prompts;
pub use sven_turn::{
    compact_session, compact_session_with_strategy, emergency_compact, finish_compaction,
    prepare_compaction, smart_truncate, CompactionPlan,
};
pub use sven_turn::{
    set_thinking_budget_override, stream_turn, thinking_budget_override, to_model_schemas,
    AbortedError, ModelResolver, ThinkingBudget,
};
pub use sven_turn::{system_prompt, CollabEvent};

pub use completion::development_complete;
pub use events::{AgentEvent, CompactionStrategyUsed, PeerInfo};
pub use machines::{
    reactive_agent::ReactiveAgentMachine, sdlc::task::TaskMachine, sdlc::SdlcMachine,
    verified_task::{VerifiedTaskMachine, ERROR_FACT, NEEDS_HUMAN_FACT, VERDICT_FACT},
};
pub use mode::ModeRegistry;
pub use runtime_context::AgentRuntimeContext;
pub use session::{Session, TurnRecord};
