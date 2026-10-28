// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Command execution: the free-form `shell` tool and the harness-only gate
//! command tool.
pub mod gate;
pub mod shell;

pub use gate::{GateCommandTool, TrajectoryHook};
pub use shell::ShellTool;

// ─── OutputCategory contract tests ───────────────────────────────────────────
//
// Pins each tool's declared `OutputCategory`: the executor's truncation
// strategy depends on it, so a silent change would change what the model
// sees of every oversized result.
#[cfg(test)]
mod output_category_tests {
    use sven_tool_api::tool::{OutputCategory, Tool};

    #[test]
    fn shell_tool_is_bounded() {
        let t = super::ShellTool {
            timeout_secs: 30,
            ..Default::default()
        };
        assert_eq!(t.output_category(), OutputCategory::Bounded);
    }
}
