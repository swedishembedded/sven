// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Pure formatting functions for human-readable display of tool schemas.
//!
//! Produces markdown strings with no dependency on ratatui or any TUI
//! library.  Consumed by the TUI inspector (`/tools`) and usable in tests or
//! CLI output.

use crate::registry::ToolSchema;

// ── Tools ─────────────────────────────────────────────────────────────────────

/// Format a slice of [`ToolSchema`] as a grouped markdown list.
///
/// Tools are grouped by the first segment of their `name` (the part before
/// the first `_`).  Within each group they are sorted alphabetically.  Each
/// entry shows the tool name, description, and the number of parameters
/// described by its JSON Schema.
///
/// # Example output
///
/// ```text
/// ## Tools (42 total)
///
/// ### buf
///
/// **buf_grep** — Search output buffer contents with a regex pattern
/// Parameters: 3
///
/// **buf_read** — Read lines from an output buffer
/// Parameters: 3
/// ```
pub fn format_tools_list(tools: &[ToolSchema]) -> String {
    let core: Vec<&ToolSchema> = tools.iter().filter(|t| !t.is_mcp).collect();
    let mcp: Vec<&ToolSchema> = tools.iter().filter(|t| t.is_mcp).collect();

    let mut out = format!("## Tools ({} total)\n\n", tools.len());

    // ── Core tools ────────────────────────────────────────────────────────────
    if !core.is_empty() {
        out.push_str(&format!("### Built-in tools ({})\n\n", core.len()));
        out.push_str(&format_tool_entries(&core));
    }

    // ── MCP tools grouped by server name ─────────────────────────────────────
    if !mcp.is_empty() {
        // Group by server prefix (before the first `-`).
        let mut by_server: std::collections::BTreeMap<String, Vec<&ToolSchema>> =
            std::collections::BTreeMap::new();
        for t in &mcp {
            let server = t.name.split('-').next().unwrap_or(&t.name).to_string();
            by_server.entry(server).or_default().push(t);
        }
        for (server, group) in &by_server {
            out.push_str(&format!("### MCP: {} ({} tools)\n\n", server, group.len()));
            out.push_str(&format_tool_entries(group));
        }
    }

    if tools.is_empty() {
        return "## Tools\n\nNo tools registered.\n".to_string();
    }

    out
}

fn format_tool_entry(tool: &ToolSchema) -> String {
    let mut entry = format!("**{}**", tool.name);
    if !tool.description.is_empty() {
        let first_line = tool
            .description
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .replace('|', "\\|");
        entry.push_str(&format!(" — {first_line}"));
    }
    entry.push('\n');
    let param_count = tool
        .parameters
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|o| o.len())
        .unwrap_or(0);
    if param_count > 0 {
        entry.push_str(&format!("Parameters: {param_count}  \n"));
    }
    entry.push('\n');
    entry
}

fn format_tool_entries(tools: &[&ToolSchema]) -> String {
    tools.iter().map(|t| format_tool_entry(t)).collect()
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn make_tool(name: &str, description: &str, params: serde_json::Value) -> ToolSchema {
        ToolSchema {
            name: name.to_string(),
            description: description.to_string(),
            parameters: params,
            is_mcp: false,
        }
    }

    #[test]
    fn empty_tools_returns_placeholder() {
        let out = format_tools_list(&[]);
        assert!(out.contains("No tools registered"));
    }

    #[test]
    fn tools_listed_in_builtin_section() {
        let tools = vec![
            make_tool("buf_read", "Read buffer", json!({"properties": {}})),
            make_tool("buf_grep", "Grep buffer", json!({"properties": {}})),
            make_tool("run_command", "Run a command", json!({"properties": {}})),
        ];
        let out = format_tools_list(&tools);
        assert!(out.contains("Built-in tools (3)"));
        assert!(out.contains("**buf_read**"));
        assert!(out.contains("**buf_grep**"));
        assert!(out.contains("**run_command**"));
        assert!(out.contains("3 total"));
    }

    #[test]
    fn mcp_tools_grouped_by_server() {
        let tools = vec![
            make_tool("read_file", "Read a file", json!({})),
            ToolSchema {
                name: "github-create_issue".to_string(),
                description: "Create a GitHub issue".to_string(),
                parameters: json!({}),
                is_mcp: true,
            },
            ToolSchema {
                name: "github-list_repos".to_string(),
                description: "List GitHub repos".to_string(),
                parameters: json!({}),
                is_mcp: true,
            },
        ];
        let out = format_tools_list(&tools);
        assert!(out.contains("MCP: github (2 tools)"));
        assert!(out.contains("**github-create_issue**"));
        assert!(out.contains("**github-list_repos**"));
    }

    #[test]
    fn tool_with_parameters_shows_count() {
        let tool = make_tool(
            "read_file",
            "Read a file",
            json!({"properties": {"path": {}, "offset": {}, "limit": {}}}),
        );
        let out = format_tools_list(&[tool]);
        assert!(out.contains("Parameters: 3"));
    }

    #[test]
    fn tool_path_in_output() {
        let tool = make_tool("write_file", "Write contents to file", json!({}));
        let out = format_tools_list(&[tool]);
        assert!(out.contains("**write_file**"));
        assert!(out.contains("Write contents to file"));
    }
}
