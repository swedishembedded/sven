// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Command execution: the free-form `shell` tool.
pub mod shell;

pub use shell::ShellTool;

// ─── OutputCategory contract tests ───────────────────────────────────────────
//
// Moved from sven-tools's builtin/mod.rs::output_category_tests along with
// these tools themselves -- see that module's comment for why this contract
// is pinned per-tool at compile time.
#[cfg(test)]
mod output_category_tests {
    use sven_tool_api::tool::{OutputCategory, Tool};

    #[test]
    fn shell_tool_is_headtail() {
        let t = super::ShellTool { timeout_secs: 30 };
        assert_eq!(t.output_category(), OutputCategory::HeadTail);
    }
}
