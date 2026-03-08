// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
pub mod context;
mod conversation;
pub mod index;
mod jsonl_export;
mod output;
pub mod pipe;
mod runner;
pub mod template;
#[cfg(test)]
mod tests;
pub mod toolcall_replay;

pub use conversation::{ConversationOptions, ConversationRunner};
pub use pipe::{MapOptions, ReduceOptions, TeeOptions};
pub use runner::{
    CiOptions, CiRunner, OutputFormat, EXIT_AGENT_ERROR, EXIT_BUDGET_EXHAUSTED, EXIT_INTERRUPT,
    EXIT_SUCCESS, EXIT_TIMEOUT, EXIT_TOOL_WARNINGS, EXIT_VALIDATION_ERROR,
};
pub use toolcall_replay::replay_tool_calls;
// Re-export runtime detection utilities for callers that import from sven_ci
pub use sven_runtime::{
    ci_template_vars, collect_git_context, detect_ci_context, find_project_root,
    load_project_context_file, GitContext,
};
