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

// File operation tools
pub use builtin::file::asr::{AsrError, Transcript};
pub use builtin::file::attach_file::AttachFileTool;
pub use builtin::file::attachment::{
    classify as classify_attachment, load_attachment, AttachError, AttachOptions, AttachmentKind,
    LoadedAttachment,
};
pub use builtin::file::delete_file::DeleteFileTool;
pub use builtin::file::edit_file::EditFileTool;
pub use builtin::file::find_file::FindFileTool;
pub use builtin::file::read_file::ReadFileTool;
pub use builtin::file::write_file::WriteTool;

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

// Terminal tools
pub use builtin::terminal::run_terminal_command::RunTerminalCommandTool;

// Web tools
pub use builtin::web::web_fetch::WebFetchTool;
pub use builtin::web::web_search::WebSearchTool;

// Knowledge tools
pub use builtin::knowledge::list_knowledge::ListKnowledgeTool;

// Shell tool
pub use builtin::shell::ShellTool;

// GDB debugging tools (Unix only - GDB signal APIs are not available on Windows)
#[cfg(unix)]
pub use builtin::gdb::state::GdbSessionState;
#[cfg(unix)]
pub use builtin::gdb::GdbTool;
#[cfg(unix)]
pub use builtin::gdb::{
    GdbCommandTool, GdbConnectTool, GdbInterruptTool, GdbStartServerTool, GdbStatusTool,
    GdbStopTool, GdbWaitStoppedTool,
};

// Context (RLM memory-mapped) tools
pub use builtin::context::{
    ContextGrepTool, ContextOpenTool, ContextReadTool, ContextStore, SubQueryRunner,
};

// Streaming output buffer tools
pub use builtin::buffer::{
    BufGrepTool, BufReadTool, BufStatusTool, BufferSource, BufferStatus, OutputBufferStore,
};

// Image tool (still at root level)
pub use builtin::read_image::ReadImageTool;

// Data URL parsing - re-exported from sven-image so consumers (e.g. sven-mcp)
// don't need to depend on sven-image directly.
pub use sven_image::parse_data_url;
