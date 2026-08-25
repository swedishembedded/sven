// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Phase 5.1 re-export shim over `sven-tool-api` (kernel tier: the `Tool`
//! trait and its interface) and `sven-tool-registry` (services tier: the
//! concrete `ToolRegistry` and config-driven policy engines).
//!
//! `builtin/`'s ~18k LOC of concrete tool implementations -- everything this
//! crate used to hold directly -- has now been fully carved into the
//! domain-tier crates the refactor plan named: `sven-tools-{fs,exec,web,ctx,
//! agent,gdb}`. This crate now exists purely so the ~20 crates still writing
//! `sven_tools::ToolRegistry` / `sven_tools::Tool` / `sven_tools::ToolCall`
//! (etc. -- the shared kernel-tier vocabulary, not a concrete tool) keep
//! compiling unchanged. Repointing every one of those call sites directly at
//! `sven-tool-api`/`sven-tool-registry` and deleting this crate outright is
//! the tracked follow-up (see CHANGELOG.md and the `sven-tools ->
//! sven-tool-registry` `architecture.toml` same-layer exception, which
//! updates its own resolution condition to that repointing sweep now that
//! the builtin/ split itself is done).
pub use sven_tool_api::{display, events, tool, tool_summary};

/// Merged view of the approval-decision vocabulary
/// ([`sven_tool_api::policy`]) and the config-driven engines that decide it
/// ([`sven_tool_registry::policy`]). The two halves are disjoint (no name
/// collisions) -- see `sven-tool-api::policy`'s module docs for why
/// `ApprovalPolicy` had to move down a tier from where the original,
/// pre-split plan placed it.
pub mod policy {
    pub use sven_tool_api::policy::{ApprovalPolicy, PermissionRequester};
    pub use sven_tool_registry::policy::{RolePolicy, ToolPolicy};
}
pub use sven_tool_registry::registry;

pub use display::format_tools_list;
pub use events::{TodoItem, TodoStatus, ToolEvent};
pub use policy::{ApprovalPolicy, PermissionRequester, RolePolicy, ToolPolicy};
pub use registry::{SharedToolDisplays, SharedTools, ToolRegistry, ToolSchema};
pub use sven_hsm::ToolCapability;
pub use tool::{
    OutputCategory, Tool, ToolCall, ToolDisplay, ToolDisplayRegistry, ToolOutput, ToolOutputPart,
};
pub use tool_summary::{shorten_path, tool_category, tool_icon, tool_smart_summary};
