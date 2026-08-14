// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>

// SPDX-License-Identifier: Apache-2.0
pub mod context;
pub mod knowledge;
pub mod search;
pub mod system;
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
    use crate::tool::OutputCategory;
    use crate::Tool;

    // The GDB and shell/terminal HeadTail tools' equivalents of this contract
    // test moved with them into sven-tools-gdb and sven-tools-exec (5.7 and
    // 5.3 of the refactor plan's god-crate splits).

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

    // ── FileContent / Generic file tools ──────────────────────────────────────
    //
    // read_file/write_file/edit_file/delete_file/find_file's equivalents of
    // this contract test moved with them into sven-tools-fs (5.2 of the
    // refactor plan's god-crate splits).

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

    // ── Knowledge tools ───────────────────────────────────────────────────────

    #[test]
    fn list_knowledge_is_matchlist() {
        let t = super::knowledge::list_knowledge::ListKnowledgeTool {
            knowledge: sven_workspace::SharedKnowledge::empty(),
        };
        assert_eq!(t.output_category(), OutputCategory::MatchList);
    }

    #[test]
    fn search_knowledge_is_matchlist() {
        let t = super::search::search_knowledge::SearchKnowledgeTool {
            knowledge: sven_workspace::SharedKnowledge::empty(),
        };
        assert_eq!(t.output_category(), OutputCategory::MatchList);
    }
}
