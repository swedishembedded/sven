// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Shared agent-wiring layer for Sven frontends.
//!
//! This crate provides the common abstractions and background tasks used by
//! both the `sven-tui` (ratatui terminal UI) and `sven-gui` (Slint desktop UI)
//! crates. It contains:
//!
//! - `AgentRequest` / `agent_task` — the background task that owns the Agent
//! - `node_agent_task` — WebSocket bridge to a running sven node
//! - `ChatSegment` — the display-layer chat data model
//! - `ModelDirective`, `QueuedMessage`, `NodeBackend` — shared config types
//!
//! ## Architecture
//!
//! ```text
//! sven (CLI/TUI)          sven-ui (Desktop GUI)
//!       │                         │
//!  sven-tui (ratatui)     sven-gui (slint)
//!       │                         │
//!       └──────── sven-frontend ──┘
//!                      │
//!           ┌──────────┼──────────┐
//!     sven-bootstrap  sven-core  sven-tools
//! ```

pub mod agent;
pub mod node_agent;
pub mod segment;
pub mod types;

// ── Convenience re-exports ────────────────────────────────────────────────────

pub use agent::{agent_task, AgentRequest};
pub use node_agent::{fetch_node_tools, node_agent_task};
pub use segment::{
    messages_for_resubmit, segment_at_line, segment_editable_text, segment_is_removable,
    segment_is_rerunnable, segment_short_preview, segment_tool_call_id, ChatSegment,
};
pub use types::{FrontendOptions, ModelDirective, NodeBackend, QueuedMessage, SessionMeta};
