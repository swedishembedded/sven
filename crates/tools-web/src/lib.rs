// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Information-gathering tools: internet fetch/search (`web_fetch`,
//! `web_search`) and read-only codebase diagnostics that shell out to an
//! external binary (`grep`, `search_codebase`, and `read_lints`, which run
//! ripgrep/grep, ripgrep, and `cargo clippy`/`eslint`/`ruff` respectively).
//!
//! Split out of `sven-tools`'s `builtin/web/`, `builtin/search/{grep,search_codebase}.rs`,
//! and `builtin/system/read_lints.rs` (5.5 of the refactor plan's god-crate
//! splits). None of the five files has a code-level cross-ref to any other
//! `builtin/` subdirectory or to each other beyond the kernel-tier `Tool`
//! vocabulary, so this grouping is the plan's original conceptual pairing
//! ("web+search to web"), not a forced coupling -- confirmed by checking
//! before committing to it, per this refactor's verify-first discipline.
//! `search_knowledge.rs`, which *did* live in `builtin/search/` alongside
//! `grep.rs`/`search_codebase.rs`, decoupled from that directory grouping
//! into `sven-tools-ctx` instead, on real `SharedKnowledge` coupling with
//! `list_knowledge.rs` -- see that crate's doc comment. `read_lints.rs`
//! joins this crate rather than `sven-tools-agent` because it shares the
//! same shape as `grep`/`search_codebase`: a read-only, subprocess-backed
//! diagnostic tool with no state and no self-modification of the agent.
pub mod grep;
mod provenance;
pub mod read_lints;
pub mod search_codebase;
pub mod web_fetch;
pub mod web_search;

pub use grep::GrepTool;
pub use read_lints::ReadLintsTool;
pub use search_codebase::SearchCodebaseTool;
pub use web_fetch::WebFetchTool;
pub use web_search::WebSearchTool;

// ─── OutputCategory contract tests ───────────────────────────────────────────
//
// Moved from sven-tools's builtin/mod.rs::output_category_tests along with
// these tools themselves -- see that module's comment for why this contract
// is pinned per-tool at compile time.
#[cfg(test)]
mod output_category_tests {
    use sven_tool_api::tool::{OutputCategory, Tool};

    #[test]
    fn grep_tool_is_matchlist() {
        let t = super::GrepTool;
        assert_eq!(t.output_category(), OutputCategory::MatchList);
    }

    #[test]
    fn search_codebase_is_matchlist() {
        let t = super::SearchCodebaseTool;
        assert_eq!(t.output_category(), OutputCategory::MatchList);
    }

    #[test]
    fn read_lints_is_matchlist() {
        let t = super::ReadLintsTool;
        assert_eq!(t.output_category(), OutputCategory::MatchList);
    }

    #[test]
    fn web_fetch_is_generic() {
        let t = super::WebFetchTool::default();
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }

    #[test]
    fn web_search_is_generic() {
        let t = super::WebSearchTool { api_key: None };
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }
}
