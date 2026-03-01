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
pub mod registry;
pub mod task_tool;

pub use agent::AgentBuilder;
pub use context::{RuntimeContext, ToolSetProfile};
pub use registry::build_tool_registry;
pub use task_tool::TaskTool;
