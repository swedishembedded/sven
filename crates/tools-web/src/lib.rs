// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Information-gathering tools: internet fetch/search (`web_fetch`,
//! `web_search`) and read-only codebase diagnostics that shell out to an
//! external binary (`grep`, which runs ripgrep, and `read_lints`, which runs
//! `cargo check`, `tsc` or `ruff`).
//!
//! None of these tools references another beyond the kernel-tier `Tool`
//! vocabulary; they are grouped as stateless, read-only information
//! gathering that never modifies the agent itself.
mod config;
pub mod grep;
mod provenance;
pub mod read_lints;
pub mod web_fetch;
pub mod web_search;

pub use config::{LintsConfig, WebConfig, WebSearchConfig};
pub use grep::GrepTool;
pub use read_lints::ReadLintsTool;
pub use web_fetch::WebFetchTool;
pub use web_search::WebSearchTool;

// ─── OutputCategory contract tests ───────────────────────────────────────────
//
// Pins each tool's declared `OutputCategory`: the executor's truncation
// strategy depends on it, so a silent change would change what the model
// sees of every oversized result.
#[cfg(test)]
mod output_category_tests {
    use sven_tool_api::tool::{OutputCategory, Tool};

    #[test]
    fn grep_tool_is_matchlist() {
        let t = super::GrepTool::default();
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
