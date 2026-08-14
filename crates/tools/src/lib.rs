// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>

// SPDX-License-Identifier: Apache-2.0
pub mod builtin;

// ── Phase 5.1 of the refactor plan: the Tool trait / ToolDisplay /
// ApprovalPolicy / PermissionRequester interface moved to sven-tool-api
// (kernel tier), and the concrete ToolRegistry / ToolPolicy / RolePolicy
// moved to sven-tool-registry (services tier). This crate re-exports both at
// their original module paths so every existing `sven_tools::...` call site
// (still ~20 crates deep across the workspace) keeps compiling unchanged;
// only `builtin/`'s ~18k LOC of concrete tool implementations still live
// here. Repointing those call sites directly at sven-tool-api /
// sven-tool-registry, splitting builtin/ into its own per-concern domain
// crates, and then deleting this shim is the tracked follow-up (see
// CHANGELOG.md).
pub use sven_tool_api::{display, events, tool, tool_summary};
pub(crate) use sven_tool_api::params;

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

// File and buffer tools moved to sven-tools-fs (5.2 of the refactor plan's
// god-crate splits). Consumers depend on sven-tools-fs directly now.

// Search tools
pub use builtin::search::grep::GrepTool;
pub use builtin::search::search_codebase::SearchCodebaseTool;
pub use builtin::search::search_knowledge::SearchKnowledgeTool;

// System tools
pub use builtin::system::ask_question::{AskQuestionTool, Question, QuestionRequest};
pub use builtin::system::memory::MemoryTool;
pub use builtin::system::read_lints::ReadLintsTool;
pub use builtin::system::skill::SkillTool;
pub use builtin::system::system::{ModelCatalogEntry, SystemTool};
pub use builtin::system::todo::TodoTool;

// Web tools
pub use builtin::web::web_fetch::WebFetchTool;
pub use builtin::web::web_search::WebSearchTool;

// Knowledge tools
pub use builtin::knowledge::list_knowledge::ListKnowledgeTool;

// Shell/terminal tools moved to sven-tools-exec (5.3 of the refactor plan's
// god-crate splits). Consumers depend on sven-tools-exec directly now.

// GDB debugging tools moved to sven-tools-gdb (5.7 of the refactor plan's
// god-crate splits; Unix only -- GDB signal APIs are not available on
// Windows). Consumers depend on sven-tools-gdb directly now.

// Context (RLM memory-mapped) tools
pub use builtin::context::{
    ContextGrepTool, ContextOpenTool, ContextReadTool, ContextStore, SubQueryRunner,
};

// Image tool (still at root level)
pub use builtin::read_image::ReadImageTool;

// Data URL parsing - re-exported from sven-image so consumers (e.g. sven-mcp)
// don't need to depend on sven-image directly.
pub use sven_image::parse_data_url;
