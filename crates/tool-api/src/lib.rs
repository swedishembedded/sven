// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `Tool` trait and its interface -- the kernel-tier vocabulary every
//! tool implementation (in a domain-tier `sven-tools-*` crate) and every
//! consumer that only needs to *name* a tool (rather than hold a live
//! `ToolRegistry`) depends on.
//!
//! This crate holds the trait/type side; `sven-tool-registry` (one tier up)
//! holds the concrete `ToolRegistry` and the config-driven policy engines.
//! See the `policy` module docs for why `ApprovalPolicy` lives here rather
//! than there.
pub mod display;
pub mod events;
pub mod grep_match;
pub mod params;
pub mod path_scope;
pub mod policy;
pub mod tool;
pub mod tool_summary;

pub use display::format_tools_list;
pub use events::{TodoItem, TodoStatus, ToolEvent};
pub use grep_match::GrepMatch;
pub use path_scope::{PathScope, PathScopeError};
pub use policy::{ApprovalPolicy, PermissionRequester};
pub use sven_hsm::ToolCapability;
/// What a question gets when no person can answer it.
pub use sven_vocab::NO_USER_ANSWER;
pub use tool::{
    capability_by_action, OutputCategory, Tool, ToolCall, ToolDisplay, ToolDisplayRegistry,
    ToolOutput, ToolOutputPart,
};
pub use tool_summary::{shorten_path, tool_category, tool_icon, tool_smart_summary};
