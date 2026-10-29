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

pub mod child_records;
pub mod child_spawner;
pub mod config;
pub mod context;
pub mod context_query;
pub mod context_tool;
mod context_wiring;
pub mod decision_ranker;
mod delegation_decision;
#[cfg(feature = "document")]
mod document_models;
mod evidence_share;
mod instruction_wiring;
pub mod kernel_bridge;
mod mcp_setup;
mod mode_policy;
pub mod modes;
pub mod registry;
pub mod runtime_builder;
mod session_executor;
pub mod session_handles;
pub mod supervisor;
pub mod task_tool;
pub mod tool_discovery;
mod tool_events;
#[cfg(feature = "android")]
pub mod ui_test_dispatch;

pub use config::{
    AgentConfig, AssemblyConfig, AssemblyMode, Config, ContextSettings, DisclosureConfig,
    ListingConfig, ToolsConfig,
};
pub use context::{BuiltinTools, Questions, RuntimeContext, ToolSetProfile};
pub use context_query::{
    build_context_query_tools, ContextConfig, ContextQueryTool, ContextReduceTool,
    ModelSubQueryRunner,
};
pub use context_tool::ContextTool;
#[cfg(feature = "document")]
pub use document_models::{convert_options, ConfigOcrModels};
pub use evidence_share::evidence_store_path;
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
pub use sven_tool_registry::Disclosure;
/// What a session's `ask_question` tool sends a surface that answers it.
pub use sven_tools_agent::{Question, QuestionRequest};
pub use sven_turn::instructions::ListingDisclosure;
pub use sven_turn::TimeBudget;
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
