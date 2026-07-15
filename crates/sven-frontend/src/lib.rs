// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Shared agent-wiring layer for Sven frontends.
//!
//! This crate provides the common abstractions and background tasks used by
//! both the `sven-tui` (ratatui terminal UI) and `sven-gui` (Slint desktop UI)
//! crates. It contains:
//!
//! - `AgentRequest` / `kernel_session_task` - the background task that drives
//!   the HSM-kernel session and forwards its `AgentEvent` stream
//! - `node_agent_task` - WebSocket bridge to a running sven node
//! - `control` - frontend-side mirror of the node/cloud control protocol
//! - `operator` - operator console: tenant selection + cross-session view
//!   over the ControlEvent stream (`OperatorConsole`, `operator_console_task`)
//! - `ChatSegment` - the display-layer chat data model
//! - `ModelDirective`, `QueuedMessage`, `NodeBackend` - shared config types
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
pub mod commands;
pub mod control;
pub mod markdown;
pub mod node_agent;
pub mod operator;
pub mod projection;
pub mod queue;
pub mod segment;
pub mod tool_view;
pub mod types;

// ── Convenience re-exports ────────────────────────────────────────────────────

pub use agent::{kernel_session_task, AgentRequest};
pub use node_agent::{fetch_node_tools, node_agent_task};
pub use operator::{
    operator_console_task, OperatorConsole, OperatorRequest, OperatorSnapshot, PendingApproval,
    SessionPhase, SessionView, TenantEndpoint, TenantInfo, TenantSelection,
};
pub use projection::{
    projection_channel, projection_to_session_state, MachineProjection, ProjectionRx, ProjectionTx,
};
pub use segment::{
    messages_for_resubmit, segment_at_line, segment_editable_text, segment_is_removable,
    segment_is_rerunnable, segment_short_preview, segment_tool_call_id, ChatSegment,
};
pub use types::{FrontendOptions, ModelDirective, NodeBackend, QueuedMessage, SessionMeta};
