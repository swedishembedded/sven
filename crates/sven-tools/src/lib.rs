// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>

// SPDX-License-Identifier: Apache-2.0
pub mod builtin;
pub mod display;
pub mod events;
pub(crate) mod params;
pub mod policy;
pub mod registry;
pub mod tool;
pub mod tool_summary;

pub use display::format_tools_list;
pub use events::{TodoItem, TodoStatus, ToolEvent};
pub use policy::{ApprovalPolicy, PermissionRequester, RolePolicy, ToolPolicy};
pub use registry::{SharedToolDisplays, SharedTools, ToolRegistry, ToolSchema};
pub use tool::{
    OutputCategory, Tool, ToolCall, ToolDisplay, ToolDisplayRegistry, ToolOutput, ToolOutputPart,
};
pub use tool_summary::{shorten_path, tool_category, tool_icon, tool_smart_summary};

// File operation tools
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
pub use builtin::system::list_dir::ListDirTool;
pub use builtin::system::load_skill::LoadSkillTool;
pub use builtin::system::memory::MemoryTool;
pub use builtin::system::read_lints::ReadLintsTool;
pub use builtin::system::switch_mode::SwitchModeTool;
pub use builtin::system::todo_write::TodoWriteTool;
pub use builtin::system::update_memory::UpdateMemoryTool;

// Terminal tools
pub use builtin::terminal::run_terminal_command::RunTerminalCommandTool;

// Web tools
pub use builtin::web::web_fetch::WebFetchTool;
pub use builtin::web::web_search::WebSearchTool;

// Knowledge tools
pub use builtin::knowledge::list_knowledge::ListKnowledgeTool;

// Shell tool
pub use builtin::shell::ShellTool;

// GDB debugging tools (compound + individual kept for internal use)
pub use builtin::gdb::state::GdbSessionState;
pub use builtin::gdb::GdbTool;
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

// Data URL parsing — re-exported from sven-image so consumers (e.g. sven-mcp)
// don't need to depend on sven-image directly.
pub use sven_image::parse_data_url;
