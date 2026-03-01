// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
pub mod builtin;
pub mod events;
pub mod policy;
pub mod registry;
pub mod tool;

pub use events::{TodoItem, ToolEvent};
pub use policy::{ApprovalPolicy, ToolPolicy};
pub use registry::{ToolRegistry, ToolSchema};
pub use tool::{OutputCategory, Tool, ToolCall, ToolOutput, ToolOutputPart};

// New tool exports
pub use builtin::ask_question::{AskQuestionTool, Question, QuestionRequest};
pub use builtin::delete_file::DeleteFileTool;
pub use builtin::edit_file::EditFileTool;
pub use builtin::find_file::FindFileTool;
pub use builtin::grep::GrepTool;
pub use builtin::list_dir::ListDirTool;
pub use builtin::read_file::ReadFileTool;
pub use builtin::read_image::ReadImageTool;
pub use builtin::read_lints::ReadLintsTool;
pub use builtin::run_terminal_command::RunTerminalCommandTool;
pub use builtin::search_codebase::SearchCodebaseTool;
pub use builtin::switch_mode::SwitchModeTool;
pub use builtin::todo_write::TodoWriteTool;
pub use builtin::update_memory::UpdateMemoryTool;
pub use builtin::web_fetch::WebFetchTool;
pub use builtin::web_search::WebSearchTool;
pub use builtin::write_file::WriteTool;

// GDB debugging tools
pub use builtin::gdb::state::GdbSessionState;
pub use builtin::gdb::{
    GdbCommandTool, GdbConnectTool, GdbInterruptTool, GdbStartServerTool, GdbStatusTool,
    GdbStopTool, GdbWaitStoppedTool,
};

// Skill loading tool
pub use builtin::load_skill::LoadSkillTool;

// Knowledge base tools
pub use builtin::list_knowledge::ListKnowledgeTool;
pub use builtin::search_knowledge::SearchKnowledgeTool;

pub use builtin::shell::ShellTool;
