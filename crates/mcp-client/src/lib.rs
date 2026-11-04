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
//! - [`McpManager`] - connects to multiple MCP servers, aggregates tools/prompts.
//! - [`McpTool`] - implements `sven_tool_api::Tool`, routes calls to the MCP server.
//! - [`McpEvent`] - lifecycle events emitted by the manager.
//! - [`ServerStatusSummary`] - per-server status for the `/mcp` UI.
//!
//! # Config
//!
//! MCP servers are configured by a [`McpServerConfig`] each, under the
//! `mcp_servers:` section of the configuration file. The manager accepts a
//! [`McpServers`] map where the key is the server name used as the tool prefix.

pub mod auth;
pub mod bridge;
mod callback;
pub mod config;
pub mod config_edit;
mod connection;
mod dedupe;
mod events;
pub mod exchange;
pub mod files;
pub mod health;
pub mod manager;
pub mod served_skills;

// ── Public re-exports ─────────────────────────────────────────────────────────

pub use auth::TokenStore;
pub use bridge::{ForwardedTool, McpPromptArgInfo, McpPromptInfo, McpTool};
pub use callback::{
    bind_loopback, open_browser, sign_in_link, wait_showing, Redirect, CALLBACK_TIMEOUT,
};
pub use config::{
    mcp_servers_schema, McpOAuthConfig, McpServerConfig, McpServers, McpToolPolicy, McpTransport,
};
pub use events::{McpEvent, NoticeLevel};
pub use exchange::{connect_account, AccountConnection, AccountConnector, TokenExchange};
pub use health::{ServerStatus, ServerStatusSummary};
pub use manager::McpManager;
