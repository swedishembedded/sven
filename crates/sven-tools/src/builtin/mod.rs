// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
pub mod ask_question;
pub mod delete_file;
pub mod edit_file;
pub mod glob_file_search;
pub mod grep;
pub mod list_dir;
pub mod load_skill;
pub mod read_file;
pub mod read_image;
pub mod read_lints;
pub mod run_terminal_command;
pub mod search_codebase;
pub mod switch_mode;
pub mod todo_write;
pub mod update_memory;
pub mod web_fetch;
pub mod web_search;
pub mod write_file;

pub mod gdb;

// Legacy modules kept for backwards compatibility
pub mod glob;
pub mod shell;

// ─── OutputCategory contract tests ───────────────────────────────────────────
//
// Each builtin tool that overrides `output_category()` is verified here so
// that renames or copy-paste errors are caught at compile time with a clear
// failure message.  Tools that intentionally use the default (Generic) are
// also listed so that adding an override never silently goes un-reviewed.
#[cfg(test)]
mod output_category_tests {
    use std::sync::Arc;
    use sven_config::GdbConfig;
    use tokio::sync::Mutex;

    use super::gdb::state::GdbSessionState;
    use crate::tool::OutputCategory;
    use crate::Tool;

    fn gdb_state() -> Arc<Mutex<GdbSessionState>> {
        Arc::new(Mutex::new(GdbSessionState::default()))
    }

    // ── HeadTail tools (terminal / process output) ────────────────────────────

    #[test]
    fn shell_tool_is_headtail() {
        let t = super::shell::ShellTool { timeout_secs: 30 };
        assert_eq!(t.output_category(), OutputCategory::HeadTail);
    }

    #[test]
    fn run_terminal_command_is_headtail() {
        let t = super::run_terminal_command::RunTerminalCommandTool { timeout_secs: 30 };
        assert_eq!(t.output_category(), OutputCategory::HeadTail);
    }

    #[test]
    fn gdb_command_is_headtail() {
        let t = super::gdb::command::GdbCommandTool::new(gdb_state(), GdbConfig::default());
        assert_eq!(t.output_category(), OutputCategory::HeadTail);
    }

    #[test]
    fn gdb_wait_stopped_is_headtail() {
        let t = super::gdb::wait_stopped::GdbWaitStoppedTool::new(gdb_state());
        assert_eq!(t.output_category(), OutputCategory::HeadTail);
    }

    #[test]
    fn gdb_interrupt_is_headtail() {
        let t = super::gdb::interrupt::GdbInterruptTool::new(gdb_state());
        assert_eq!(t.output_category(), OutputCategory::HeadTail);
    }

    // ── MatchList tools (ordered result sets) ────────────────────────────────

    #[test]
    fn grep_tool_is_matchlist() {
        let t = super::grep::GrepTool;
        assert_eq!(t.output_category(), OutputCategory::MatchList);
    }

    #[test]
    fn search_codebase_is_matchlist() {
        let t = super::search_codebase::SearchCodebaseTool;
        assert_eq!(t.output_category(), OutputCategory::MatchList);
    }

    #[test]
    fn read_lints_is_matchlist() {
        let t = super::read_lints::ReadLintsTool;
        assert_eq!(t.output_category(), OutputCategory::MatchList);
    }

    // ── FileContent tools (file reads) ────────────────────────────────────────

    #[test]
    fn read_file_is_filecontent() {
        let t = super::read_file::ReadFileTool;
        assert_eq!(t.output_category(), OutputCategory::FileContent);
    }

    // ── Generic tools (no override — hard truncation) ─────────────────────────

    #[test]
    fn write_tool_is_generic() {
        let t = super::write_file::WriteTool;
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }

    #[test]
    fn list_dir_is_generic() {
        let t = super::list_dir::ListDirTool;
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }

    #[test]
    fn edit_file_is_generic() {
        let t = super::edit_file::EditFileTool;
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }

    #[test]
    fn delete_file_is_generic() {
        let t = super::delete_file::DeleteFileTool;
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }

    #[test]
    fn web_fetch_is_generic() {
        let t = super::web_fetch::WebFetchTool;
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }

    #[test]
    fn web_search_is_generic() {
        let t = super::web_search::WebSearchTool { api_key: None };
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }

    #[test]
    fn glob_tool_is_generic() {
        let t = super::glob::GlobTool;
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }
}
