// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Agent construction factory.
//!
//! This crate consolidates all agent-bootstrapping concerns:
//! - Tool-registry building (Full, SubAgent)
//! - Runtime-context detection and conversion
//! - The [`TaskTool`] implementation (moved here to avoid a circular dep
//!   between `sven-core` and the tool-registry builder)
//!
//! Frontends (`sven-ci`, `sven-tui`) depend on this crate instead of
//! inlining their own registry-building loops.

pub mod agent;
pub mod context;
pub mod context_query;
pub mod context_tool;
pub mod registry;
pub mod task_tool;

pub use agent::AgentBuilder;
pub use context::{RuntimeContext, ToolSetProfile};
pub use context_query::{
    build_context_query_tools, ContextQueryTool, ContextReduceTool, ModelSubQueryRunner,
};
pub use context_tool::ContextTool;
pub use registry::{
    build_cli_tool_registry, build_tool_registry, build_tool_registry_with_integrations,
    IntegrationProviders,
};
pub use sven_mcp_client::McpManager;
pub use task_tool::TaskTool;

// Re-export compound tools from sven-tools for convenience.
#[cfg(unix)]
pub use sven_tools::GdbTool;
pub use sven_tools::MemoryTool;

// Re-export OutputBufferStore so frontends can access it via sven-bootstrap.
pub use sven_tools::OutputBufferStore;
