// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//!
//! Default MCP-safe tool registry for the sven MCP server.
//!
//! Not every sven tool makes sense to expose via MCP.  Tools that require a
//! live TUI session (`ask_question`, `switch_mode`, `todo_write`), tools that
//! modify internal agent state (`update_memory`), and tools that need the P2P
//! stack (`delegate`, `list_peers`) are intentionally omitted.
//!
//! The tools registered here are stateless from the MCP client's perspective
//! and work without any running sven node or TUI.

use sven_tools::{
    DeleteFileTool, EditFileTool, FindFileTool, GrepTool, ListDirTool, ReadFileTool, ReadImageTool,
    ReadLintsTool, RunTerminalCommandTool, SearchCodebaseTool, ShellTool, ToolRegistry,
    WebFetchTool, WebSearchTool, WriteTool,
};

/// Tool names included in the default MCP-safe set.
///
/// These names correspond exactly to the values returned by each tool's
/// `Tool::name()` implementation.  Clients can use this list to discover
/// what `sven mcp serve` exposes by default.
pub const DEFAULT_TOOL_NAMES: &[&str] = &[
    "delete_file",
    "edit_file",
    "find_file",
    "grep",
    "list_dir",
    "read_file",
    "read_image",
    "read_lints",
    "run_terminal_command",
    "search_codebase",
    "shell",
    "web_fetch",
    "web_search",
    "write_file",
];

/// Build a [`ToolRegistry`] populated with the default MCP-safe tool set.
///
/// `web_search_api_key` is forwarded to [`WebSearchTool`].  When `None` the
/// web_search tool is still registered but will return an error if invoked
/// without a Brave API key configured via the `BRAVE_API_KEY` environment
/// variable.
///
/// `allowed_names` is an optional comma-separated list of tool names to
/// include.  Pass `"all"` (or `None`) to include all default tools.
/// Any name not in [`DEFAULT_TOOL_NAMES`] is silently ignored — this guards
/// against clients accidentally requesting internal tools that were never
/// registered.
pub fn build_mcp_registry(
    web_search_api_key: Option<String>,
    allowed_names: Option<&str>,
) -> ToolRegistry {
    let filter: Option<std::collections::HashSet<&str>> = match allowed_names {
        None | Some("all") => None,
        Some(list) => Some(list.split(',').map(|s| s.trim()).collect()),
    };

    let allow = |name: &str| -> bool {
        match &filter {
            None => true,
            Some(set) => set.contains(name),
        }
    };

    let mut reg = ToolRegistry::new();

    if allow("delete_file") {
        reg.register(DeleteFileTool);
    }
    if allow("edit_file") {
        reg.register(EditFileTool);
    }
    if allow("find_file") {
        reg.register(FindFileTool);
    }
    if allow("grep") {
        reg.register(GrepTool);
    }
    if allow("list_dir") {
        reg.register(ListDirTool);
    }
    if allow("read_file") {
        reg.register(ReadFileTool);
    }
    if allow("read_image") {
        reg.register(ReadImageTool);
    }
    if allow("read_lints") {
        reg.register(ReadLintsTool);
    }
    if allow("run_terminal_command") {
        reg.register(RunTerminalCommandTool::default());
    }
    if allow("search_codebase") {
        reg.register(SearchCodebaseTool);
    }
    if allow("shell") {
        reg.register(ShellTool::default());
    }
    if allow("web_fetch") {
        reg.register(WebFetchTool);
    }
    if allow("web_search") {
        reg.register(WebSearchTool {
            api_key: web_search_api_key,
        });
    }
    if allow("write_file") {
        reg.register(WriteTool);
    }

    reg
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_contains_all_default_tools() {
        let reg = build_mcp_registry(None, None);
        let names = reg.names();
        for expected in DEFAULT_TOOL_NAMES {
            assert!(
                names.iter().any(|n| n == expected),
                "expected tool {expected:?} in default registry, got: {names:?}"
            );
        }
    }

    #[test]
    fn all_keyword_includes_all_default_tools() {
        let reg = build_mcp_registry(None, Some("all"));
        let names = reg.names();
        assert_eq!(names.len(), DEFAULT_TOOL_NAMES.len());
    }

    #[test]
    fn allowed_names_filter_restricts_tools() {
        let reg = build_mcp_registry(None, Some("read_file,write_file"));
        let mut names = reg.names();
        names.sort();
        assert_eq!(names, vec!["read_file", "write_file"]);
    }

    #[test]
    fn single_tool_allowed() {
        let reg = build_mcp_registry(None, Some("grep"));
        assert_eq!(reg.names().len(), 1);
        assert!(reg.get("grep").is_some());
    }

    #[test]
    fn unknown_tool_name_in_filter_is_ignored() {
        let reg = build_mcp_registry(None, Some("read_file,nonexistent_tool"));
        let names = reg.names();
        assert_eq!(names.len(), 1);
        assert!(reg.get("read_file").is_some());
    }

    #[test]
    fn whitespace_around_tool_names_is_trimmed() {
        let reg = build_mcp_registry(None, Some(" read_file , write_file "));
        let mut names = reg.names();
        names.sort();
        assert_eq!(names, vec!["read_file", "write_file"]);
    }

    #[test]
    fn web_search_registered_with_api_key() {
        let reg = build_mcp_registry(Some("test_key".to_string()), Some("web_search"));
        assert!(reg.get("web_search").is_some());
    }

    #[test]
    fn default_tool_names_constant_is_sorted() {
        let mut sorted = DEFAULT_TOOL_NAMES.to_vec();
        sorted.sort_unstable();
        assert_eq!(
            DEFAULT_TOOL_NAMES,
            sorted.as_slice(),
            "DEFAULT_TOOL_NAMES should be sorted for deterministic output"
        );
    }
}
