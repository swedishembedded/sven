// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What a finished headless run's exit code says about its tool errors.

/// The exit code a finished run reports for its tool errors: 3 for a
/// workflow-file run, whose steps a pipeline may want to tell apart from a
/// clean run, and none for a single prompt - an agent's failed tool calls (a
/// test that fails before the fix, an edit whose context moved) are its
/// ordinary work, recorded in the trajectory. Which runner serves a single
/// prompt (`--output-trace` picks this one) must not change its exit code.
pub(super) fn tool_error_exit(any_tool_errors: bool, workflow_file: bool) -> Option<i32> {
    (any_tool_errors && workflow_file).then_some(super::EXIT_TOOL_WARNINGS)
}

#[cfg(test)]
mod tests {
    use super::tool_error_exit;

    #[test]
    fn only_a_workflow_file_run_reports_its_tool_errors_in_the_exit_code() {
        assert_eq!(tool_error_exit(true, true), Some(3));
        assert_eq!(tool_error_exit(true, false), None);
        assert_eq!(tool_error_exit(false, true), None);
    }
}
