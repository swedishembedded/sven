// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `Tool` trait and its interface -- the kernel-tier vocabulary every
//! tool implementation (in a domain-tier `sven-tools-*` crate) and every
//! consumer that only needs to *name* a tool (rather than hold a live
//! `ToolRegistry`) depends on.
//!
//! Split out of the god-crate `sven-tools` (Phase 5.1 of the refactor plan):
//! this crate holds the trait/type side, `sven-tool-registry` (one tier up)
//! holds the concrete `ToolRegistry` and the config-driven policy engines.
//! See that crate's docs for why `ApprovalPolicy` lives here rather than
//! there, despite the plan's original one-line split description.
pub mod display;
pub mod events;
pub mod grep_match;
pub mod params;
pub mod policy;
pub mod tool;
pub mod tool_summary;

pub use display::format_tools_list;
pub use events::{TodoItem, TodoStatus, ToolEvent};
pub use grep_match::GrepMatch;
pub use policy::{ApprovalPolicy, PermissionRequester};
pub use sven_hsm::ToolCapability;
pub use tool::{
    OutputCategory, Tool, ToolCall, ToolDisplay, ToolDisplayRegistry, ToolOutput, ToolOutputPart,
};
pub use tool_summary::{shorten_path, tool_category, tool_icon, tool_smart_summary};
