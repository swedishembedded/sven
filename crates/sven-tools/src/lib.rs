// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
pub mod policy;
pub mod registry;
pub mod tool;
pub mod builtin;
pub mod events;

pub use policy::{ApprovalPolicy, ToolPolicy};
pub use registry::{ToolRegistry, ToolSchema};
pub use tool::{ToolCall, ToolOutput, ToolOutputPart, Tool};
pub use events::{TodoItem, ToolEvent};

// New tool exports
pub use builtin::run_terminal_command::RunTerminalCommandTool;
pub use builtin::read_file::ReadFileTool;
pub use builtin::read_image::ReadImageTool;
pub use builtin::write::WriteTool;
pub use builtin::list_dir::ListDirTool;
pub use builtin::delete_file::DeleteFileTool;
pub use builtin::glob_file_search::GlobFileSearchTool;
pub use builtin::edit_file::EditFileTool;
pub use builtin::grep::GrepTool;
pub use builtin::search_codebase::SearchCodebaseTool;
pub use builtin::apply_patch::ApplyPatchTool;
pub use builtin::read_lints::ReadLintsTool;
pub use builtin::todo_write::TodoWriteTool;
pub use builtin::web_fetch::WebFetchTool;
pub use builtin::web_search::WebSearchTool;
pub use builtin::update_memory::UpdateMemoryTool;
pub use builtin::ask_question::{AskQuestionTool, Question, QuestionRequest};
pub use builtin::switch_mode::SwitchModeTool;

// GDB debugging tools
pub use builtin::gdb::{
    GdbStartServerTool, GdbConnectTool, GdbCommandTool,
    GdbInterruptTool, GdbWaitStoppedTool, GdbStatusTool, GdbStopTool,
};
pub use builtin::gdb::state::GdbSessionState;

// Legacy exports preserved for backwards compatibility
pub use builtin::shell::ShellTool;
pub use builtin::fs::FsTool;
pub use builtin::glob::GlobTool;
