// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! MCP client library for sven.
//!
//! This crate adds support for connecting to external MCP (Model Context
//! Protocol) servers, discovering their tools, prompts, and resources, and
//! exposing them through sven's `Tool` trait so they are transparent to the
//! agent loop.
//!
//! # Key types
//!
//! - [`McpManager`] — connects to multiple MCP servers, aggregates tools/prompts.
//! - [`McpTool`] — implements `sven_tools::Tool`, routes calls to the MCP server.
//! - [`McpEvent`] — lifecycle events emitted by the manager.
//! - [`ServerStatusSummary`] — per-server status for the `/mcp` UI.
//!
//! # Config
//!
//! MCP servers are configured in `sven-config::McpServerConfig`.  The manager
//! accepts a `HashMap<String, McpServerConfig>` where the key is the server
//! name used as the tool prefix.

pub mod bridge;
pub mod client;
pub mod health;
pub mod manager;
pub mod oauth;
pub mod protocol;
pub mod transport;

// ── Public re-exports ─────────────────────────────────────────────────────────

pub use bridge::{McpPromptArgInfo, McpPromptInfo, McpTool};
pub use health::{ServerStatus, ServerStatusSummary};
pub use manager::{McpEvent, McpManager};
pub use oauth::{CredentialsStore, StoredTokens};
