// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Command-execution tools: the free-form `shell` tool and the
//! `run_terminal_command` tool used for long-running/background subprocesses.
//!
//! Split out of `sven-tools`'s `builtin/shell/` + `builtin/terminal/` (5.3 of
//! the refactor plan's god-crate splits). Confirmed real coupling: `terminal`
//! imports `shell::head_tail_truncate`, so the two subtrees move together as
//! one crate rather than splitting further.
pub mod shell;
pub mod terminal;

pub use shell::ShellTool;
pub use terminal::RunTerminalCommandTool;

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

    #[test]
    fn run_terminal_command_is_headtail() {
        let t = super::terminal::run_terminal_command::RunTerminalCommandTool { timeout_secs: 30 };
        assert_eq!(t.output_category(), OutputCategory::HeadTail);
    }
}
