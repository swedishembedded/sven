// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Agent construction factory.
//!
//! This crate consolidates all agent-bootstrapping concerns:
//! - Tool-registry building (Full, SubAgent)
//! - Runtime-context detection and conversion
//! - The [`TaskTool`] implementation (subagents spawned as `sven acp serve`
//!   child processes)
//! - Kernel runtime assembly ([`RuntimeBuilder`]) and the
//!   [`KernelAgentSession`] adapter
//! - The modes this build can run ([`mode_registry`])
//!
//! Without default features this is the minimal assembly: no D-Bus, no image
//! or audio decoding, no Android device control. The `coding` and `research`
//! presets, `dbus`, `android`, `gdb` and `memory` add them (see Cargo.toml).
//!
//! Frontends (`sven-ci`, `sven-tui`, `sven-acp`, `sven-frontend`,
//! `sven-sdk`) depend on this crate instead of inlining their own
//! registry-building loops.

pub mod child_spawner;
pub mod context;
pub mod context_query;
pub mod context_tool;
pub mod kernel_bridge;
mod mode_policy;
pub mod modes;
pub mod registry;
pub mod runtime_builder;
pub mod session_handles;
pub mod supervisor;
pub mod task_tool;
#[cfg(feature = "android")]
pub mod ui_test_dispatch;

pub use context::{BuiltinTools, Questions, RuntimeContext, ToolSetProfile};
pub use context_query::{
    build_context_query_tools, ContextQueryTool, ContextReduceTool, ModelSubQueryRunner,
};
pub use context_tool::ContextTool;
pub use kernel_bridge::{spawn_observation_bridge, spawn_question_bridge, KernelAgentSession};
pub use modes::mode_registry;
pub use registry::{
    build_cli_tool_registry, build_tool_registry, build_tool_registry_with_integrations,
    IntegrationProviders,
};
pub use runtime_builder::{
    KernelChannels, RuntimeBuilder, RuntimeHandle, SessionBundle, ToolExecutorFactory,
};
pub use supervisor::{SessionId, SessionSupervisor};
pub use sven_mcp_client::McpManager;
/// What a session's `ask_question` tool sends a surface that answers it.
pub use sven_tools_agent::{Question, QuestionRequest};
pub use task_tool::{ChildApprover, TaskTool};
#[cfg(feature = "android")]
pub use ui_test_dispatch::{
    dispatch_ui_test_step, StateReporter, UiTestDevice, UiTestDispatchOverrides,
};

// Re-export compound tools for convenience.
pub use sven_tools_ctx::MemoryTool;
#[cfg(all(unix, feature = "gdb"))]
pub use sven_tools_gdb::GdbTool;

// Re-export OutputBufferStore so frontends can access it via sven-bootstrap.
pub use sven_tools_fs::OutputBufferStore;
