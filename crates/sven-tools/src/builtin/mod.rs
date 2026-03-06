// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>

// SPDX-License-Identifier: Apache-2.0
pub mod context;
pub mod file;
pub mod gdb;
pub mod knowledge;
pub mod search;
pub mod shell;
pub mod system;
pub mod terminal;
pub mod web;

// Legacy re-exports for backward compatibility during transition
// These modules still exist at root level for now but will be deprecated
pub mod read_image;

// ─── OutputCategory contract tests ───────────────────────────────────────────

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
        let t = super::shell::shell::ShellTool { timeout_secs: 30 };
        assert_eq!(t.output_category(), OutputCategory::HeadTail);
    }

    #[test]
    fn run_terminal_command_is_headtail() {
        let t = super::terminal::run_terminal_command::RunTerminalCommandTool { timeout_secs: 30 };
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
        let t = super::search::grep::GrepTool;
        assert_eq!(t.output_category(), OutputCategory::MatchList);
    }

    #[test]
    fn search_codebase_is_matchlist() {
        let t = super::search::search_codebase::SearchCodebaseTool;
        assert_eq!(t.output_category(), OutputCategory::MatchList);
    }

    #[test]
    fn read_lints_is_matchlist() {
        let t = super::system::read_lints::ReadLintsTool;
        assert_eq!(t.output_category(), OutputCategory::MatchList);
    }

    // ── FileContent tools (file reads) ────────────────────────────────────────

    #[test]
    fn read_file_is_filecontent() {
        let t = super::file::read_file::ReadFileTool;
        assert_eq!(t.output_category(), OutputCategory::FileContent);
    }

    // ── Generic tools (no override — hard truncation) ─────────────────────────

    #[test]
    fn write_tool_is_generic() {
        let t = super::file::write_file::WriteTool;
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }

    #[test]
    fn list_dir_is_generic() {
        let t = super::system::list_dir::ListDirTool;
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }

    #[test]
    fn edit_file_is_generic() {
        let t = super::file::edit_file::EditFileTool;
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }

    #[test]
    fn delete_file_is_generic() {
        let t = super::file::delete_file::DeleteFileTool;
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }

    #[test]
    fn web_fetch_is_generic() {
        let t = super::web::web_fetch::WebFetchTool;
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }

    #[test]
    fn web_search_is_generic() {
        let t = super::web::web_search::WebSearchTool { api_key: None };
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }

    #[test]
    fn find_file_tool_is_generic() {
        let t = super::file::find_file::FindFileTool;
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }

    // ── Knowledge tools ───────────────────────────────────────────────────────

    #[test]
    fn list_knowledge_is_matchlist() {
        let t = super::knowledge::list_knowledge::ListKnowledgeTool {
            knowledge: sven_runtime::SharedKnowledge::empty(),
        };
        assert_eq!(t.output_category(), OutputCategory::MatchList);
    }

    #[test]
    fn search_knowledge_is_matchlist() {
        let t = super::search::search_knowledge::SearchKnowledgeTool {
            knowledge: sven_runtime::SharedKnowledge::empty(),
        };
        assert_eq!(t.output_category(), OutputCategory::MatchList);
    }
}
