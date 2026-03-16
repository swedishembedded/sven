// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Shared tool-call view data extraction.
//!
//! Extracts structured display data from tool call arguments and optional
//! `ToolDisplay` metadata so both the TUI and GUI can render tool calls
//! consistently without duplicating extraction logic.

use serde_json::Value;
use sven_tools::ToolDisplay;

/// Structured view data for a tool call, suitable for rendering in any frontend.
#[derive(Debug, Clone)]
pub struct ToolViewData {
    /// Short icon symbol (e.g. `"▶"`, `"$"`, `"⊞"`).
    pub icon: String,
    /// Human-readable display name (e.g. `"Shell"`, `"Read"`).
    pub display_name: String,
    /// Category tag for colour theming (`"file"`, `"shell"`, `"search"`, etc.).
    pub category: String,
    /// One-line collapsed summary string (the most important argument value).
    pub summary: String,
    /// Ordered key-value pairs for the expanded view.
    pub fields: Vec<(String, String)>,
}

/// Extract display data from a tool call.
///
/// When `display` is provided (from the tool's `ToolDisplay` implementation),
/// it takes precedence for icon, display_name, category, and summary.
/// Fields are always extracted from the raw JSON arguments.
pub fn extract_tool_view(
    tool_name: &str,
    args: &Value,
    display: Option<&dyn ToolDisplay>,
) -> ToolViewData {
    let (icon, display_name, category, summary) = if let Some(d) = display {
        (
            d.icon().to_string(),
            d.display_name().to_string(),
            d.category().to_string(),
            d.collapsed_summary(args),
        )
    } else {
        (
            sven_tools::tool_icon(tool_name).to_string(),
            tool_name.to_string(),
            sven_tools::tool_category(tool_name).to_string(),
            sven_tools::tool_smart_summary(tool_name, args),
        )
    };

    let fields = extract_fields(args);

    ToolViewData {
        icon,
        display_name,
        category,
        summary,
        fields,
    }
}

/// Extract key-value pairs from tool call JSON arguments for the expanded view.
///
/// Applies category-specific ordering to highlight the most important fields
/// first. Skips empty or null values to keep the view concise.
fn extract_fields(args: &Value) -> Vec<(String, String)> {
    let Some(obj) = args.as_object() else {
        return vec![];
    };

    // Prioritised key order: important fields first regardless of JSON order.
    const PRIORITY_KEYS: &[&str] = &[
        "description",
        "command",
        "shell_command",
        "path",
        "pattern",
        "url",
        "search_term",
        "query",
        "old_string",
        "new_string",
        "contents",
        "glob_pattern",
        "target_directory",
        "working_directory",
    ];

    let mut result: Vec<(String, String)> = Vec::new();

    // Add priority keys first if they exist in the object.
    for key in PRIORITY_KEYS {
        if let Some(val) = obj.get(*key) {
            if let Some(s) = format_field_value(val) {
                result.push((key.to_string(), s));
            }
        }
    }

    // Append remaining keys in their original order.
    for (k, v) in obj.iter() {
        if PRIORITY_KEYS.contains(&k.as_str()) {
            continue;
        }
        if let Some(s) = format_field_value(v) {
            result.push((k.clone(), s));
        }
    }

    result
}

/// Format a JSON value as a compact string for display.
///
/// Returns `None` for null values and empty strings/arrays to keep the
/// expanded view clean.
fn format_field_value(val: &Value) -> Option<String> {
    match val {
        Value::Null => None,
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        Value::Array(arr) if arr.is_empty() => None,
        Value::Array(arr) => {
            let items: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            if items.is_empty() {
                Some(format!("[{} items]", arr.len()))
            } else {
                Some(items.join(", "))
            }
        }
        Value::Object(obj) => {
            if obj.is_empty() {
                None
            } else {
                Some(format!("{{…{} fields}}", obj.len()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_shell_tool_prioritises_description() {
        let args = json!({
            "command": "cargo build",
            "description": "Build the project"
        });
        let view = extract_tool_view("shell", &args, None);
        assert_eq!(view.summary, "Build the project");
        assert_eq!(view.fields[0].0, "description");
        assert_eq!(view.fields[1].0, "command");
    }

    #[test]
    fn extract_fields_skips_empty_strings() {
        let args = json!({ "path": "", "pattern": "foo" });
        let fields = extract_fields(&args);
        assert!(!fields.iter().any(|(k, _)| k == "path"));
        assert!(fields.iter().any(|(k, _)| k == "pattern"));
    }

    #[test]
    fn extract_fields_skips_nulls() {
        let args = json!({ "path": null, "command": "ls" });
        let fields = extract_fields(&args);
        assert!(!fields.iter().any(|(k, _)| k == "path"));
        assert!(fields.iter().any(|(k, _)| k == "command"));
    }

    #[test]
    fn extract_returns_category_from_fallback() {
        let args = json!({ "path": "/data/foo.rs" });
        let view = extract_tool_view("read_file", &args, None);
        assert_eq!(view.category, "file");
        assert_eq!(view.icon, "▶");
    }
}
