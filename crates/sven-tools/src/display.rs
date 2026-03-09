// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Pure formatting functions for human-readable display of tool schemas.
//!
//! Produces markdown strings with no dependency on ratatui or any TUI
//! library.  Consumed by the TUI inspector (`/tools`) and usable in tests or
//! CLI output.

use std::collections::BTreeMap;

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
    if tools.is_empty() {
        return "## Tools\n\n_No tools registered._\n".to_string();
    }

    // Group by first segment (part before the first `_`).
    let mut groups: BTreeMap<String, Vec<&ToolSchema>> = BTreeMap::new();
    for tool in tools {
        let ns = tool
            .name
            .split('_')
            .next()
            .unwrap_or(&tool.name)
            .to_string();
        groups.entry(ns).or_default().push(tool);
    }

    let mut out = format!("## Tools ({} total)\n", tools.len());

    for (ns, mut group) in groups {
        group.sort_by(|a, b| a.name.cmp(&b.name));
        out.push_str(&format!("\n### {ns}\n\n"));

        for tool in group {
            out.push_str(&format!("**{}**", tool.name));
            if !tool.description.is_empty() {
                out.push_str(&format!(" — {}", tool.description.trim()));
            }
            out.push('\n');

            // Count parameters from the JSON Schema `properties` object.
            let param_count = tool
                .parameters
                .get("properties")
                .and_then(|p| p.as_object())
                .map(|o| o.len())
                .unwrap_or(0);
            if param_count > 0 {
                out.push_str(&format!("Parameters: {param_count}  \n"));
            }
            out.push('\n');
        }
    }

    out
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
        }
    }

    #[test]
    fn empty_tools_returns_placeholder() {
        let out = format_tools_list(&[]);
        assert!(out.contains("No tools registered"));
    }

    #[test]
    fn tools_grouped_by_prefix() {
        let tools = vec![
            make_tool("buf_read", "Read buffer", json!({"properties": {}})),
            make_tool("buf_grep", "Grep buffer", json!({"properties": {}})),
            make_tool("run_command", "Run a command", json!({"properties": {}})),
        ];
        let out = format_tools_list(&tools);
        assert!(out.contains("### buf"));
        assert!(out.contains("### run"));
        assert!(out.contains("**buf_read**"));
        assert!(out.contains("**buf_grep**"));
        assert!(out.contains("**run_command**"));
        assert!(out.contains("3 total"));
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
