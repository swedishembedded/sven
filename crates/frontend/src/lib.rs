// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Shared agent-wiring layer for Sven frontends.
//!
//! This crate provides the common abstractions and background tasks used by
//! the `sven-tui` (ratatui terminal UI), `sven-ci` (headless) and `sven-acp`
//! (editor) surfaces. It contains:
//!
//! - [`SessionController`] - the one controller of a session's lifecycle: it
//!   assembles a kernel session from the configuration, wires its questions
//!   and approvals to whoever answers them ([`Gates`]), and rebuilds it with
//!   the conversation carried over when the mode or the model changes. Every
//!   surface opens its sessions through it.
//! - [`Settings`] / [`TuiConfig`] - the whole configuration file, the
//!   interactive UI's own `tui:` section included
//! - `AgentRequest` / `kernel_session_task` - the background task that drives
//!   a controller session for the TUI and forwards its `AgentEvent` stream
//! - `node_agent_task` - WebSocket bridge to a remote agent session, speaking
//!   the canonical control protocol from [`sven_control`]
//! - `ChatSegment` - the display-layer chat data model
//! - `ModelDirective`, `QueuedMessage`, `NodeBackend` - shared config types
//!
//! ## Architecture
//!
//! ```text
//! sven (CLI)      sven-ci (headless)      sven-acp        sven-tui
//!      │                  │                   │               │
//!      └──────────────────┴────── sven-frontend ─────────────┘
//!                       (SessionController)
//!                                │
//! ┌─────────────┬────────────────┼──────────────┬──────────────┐
//! sven-bootstrap  sven-machines  sven-tool-registry  sven-control  sven-commands
//! ```
//!
//! The slash-command vocabulary (`SlashCommand`, `CommandRegistry`, and the
//! builtin `/…` commands) lives in the lower-tier `sven-commands` crate, not
//! here — this crate re-exports it at its historical `commands` module path
//! (`pub use sven_commands as commands`) so `sven-tui` is unaffected.

pub mod agent;
pub mod controller;
pub mod markdown;
pub mod node_agent;
pub mod projection;
pub mod queue;
pub mod segment;
pub mod settings;
pub mod tool_view;
pub mod types;

/// Compatibility re-export of the `SlashCommand` vocabulary, which lives in
/// `sven-commands` (a lower-tier crate — the commands only need `sven-vocab`,
/// `sven-workspace`, `sven-mcp-client`, `sven-model`, and `sven-machines`,
/// never anything `sven-frontend`-specific), and this crate depends on it.
/// Every existing `sven_frontend::commands::*` path keeps compiling.
pub use sven_commands as commands;

// ── Convenience re-exports ────────────────────────────────────────────────────

pub use agent::{kernel_session_task, AgentRequest};
pub use controller::{
    machine_for, AbortSlot, Gates, SessionController, SessionOptions, SessionSpec, SubmitError,
};
pub use node_agent::{fetch_node_tools, node_agent_task};
pub use projection::{
    projection_channel, projection_to_session_state, MachineProjection, ProjectionRx, ProjectionTx,
};
pub use segment::{
    messages_for_resubmit, segment_at_line, segment_editable_text, segment_is_removable,
    segment_is_rerunnable, segment_short_preview, segment_tool_call_id,
    tool_result_insert_position, ChatSegment,
};
pub use settings::{Settings, TuiConfig};
pub use types::{FrontendOptions, ModelDirective, NodeBackend, QueuedMessage, SessionMeta};
