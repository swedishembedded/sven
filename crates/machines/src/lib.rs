// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
mod events;

pub mod completion;
pub mod machines;
pub mod mode;

pub use completion::development_complete;
pub use events::{AgentEvent, CompactionStrategyUsed, PeerInfo};
pub use machines::{
    reactive_agent::{ReactiveAgentMachine, MAX_TOOL_ROUNDS_FACT},
    sdlc::task::TaskMachine,
    sdlc::SdlcMachine,
    ui_test::UiTestMachine,
    verified_task::{VerifiedTaskMachine, ERROR_FACT, NEEDS_HUMAN_FACT, VERDICT_FACT},
};
pub use mode::ModeRegistry;
