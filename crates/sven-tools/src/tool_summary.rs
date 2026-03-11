// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Tool-call summary helpers: generate compact one-line descriptions of tool
//! calls for the TUI collapsed view without depending on any rendering library.

/// Return the last `n` non-empty path components joined by `/`.
///
/// `/data/agents/sven/crates/sven-tui/src/chat/markdown.rs`  →  `chat/markdown.rs`
pub fn shorten_path(path: &str, n: usize) -> String {
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() <= n {
        return path.trim_start_matches('/').to_string();
    }
    parts[parts.len() - n..].join("/")
}

/// Build a human-readable one-line description of a tool call for the collapsed
/// tier-0 display.  Returns just the *value* that is meaningful for each tool,
/// without the redundant `key=` prefix.
///
/// Examples:
/// - `read_file {"path":"/data/foo/bar.rs"}` → `foo/bar.rs`
/// - `shell {"command":"cargo build --release"}` → `cargo build --release`
/// - `grep {"pattern":"foo","path":"/data/baz"}` → `foo  baz`
pub fn tool_smart_summary(name: &str, args: &serde_json::Value) -> String {
    let str_field = |key: &str| -> Option<String> {
        args.get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };

    match name {
        // File operations — show last 2 path components for context
        "read_file" | "write_file" | "str_replace_editor" | "str_replace" | "delete_file"
        | "Read" | "Write" | "StrReplace" | "Delete" | "EditNotebook" => str_field("path")
            .map(|p| shorten_path(&p, 2))
            .unwrap_or_default(),

        // Shell — prefer "description" (intent) for the summary; fall back to the command.
        // Support both "shell_command" and "command" parameter names.
        "shell" | "bash" | "Shell" | "run_terminal_command" | "RunTerminalCommand" => {
            str_field("description")
                .or_else(|| str_field("shell_command"))
                .or_else(|| str_field("command"))
                .map(|c| truncate_summary(&c, 80))
                .unwrap_or_default()
        }

        // Search — pattern + optional short path
        "grep" | "search" | "Grep" => {
            let pattern = str_field("pattern").unwrap_or_default();
            let path = str_field("path")
                .or_else(|| str_field("target_directory"))
                .unwrap_or_default();
            let path_short = if path.is_empty() {
                String::new()
            } else {
                format!("  {}", shorten_path(&path, 2))
            };
            truncate_summary(&format!("{pattern}{path_short}"), 80)
        }

        // Glob — show the pattern
        "glob" | "Glob" => str_field("glob_pattern")
            .or_else(|| str_field("pattern"))
            .map(|p| truncate_summary(&p, 80))
            .unwrap_or_default(),

        // Web operations
        "web_search" | "WebSearch" => str_field("search_term")
            .or_else(|| str_field("query"))
            .map(|q| truncate_summary(&q, 80))
            .unwrap_or_default(),
        "web_fetch" | "WebFetch" => str_field("url")
            .map(|u| {
                let stripped = u
                    .trim_start_matches("https://")
                    .trim_start_matches("http://");
                truncate_summary(stripped, 80)
            })
            .unwrap_or_default(),

        // Semantic / AI search
        "semantic_search" | "SemanticSearch" => str_field("query")
            .map(|q| truncate_summary(&q, 80))
            .unwrap_or_default(),

        // Todo management
        "todo" => String::new(),

        // Lints
        "ReadLints" | "read_lints" => String::new(),

        // List directory
        "list_dir" | "ListDir" => str_field("path")
            .map(|p| shorten_path(&p, 2))
            .unwrap_or_default(),

        // Memory operations
        "memory" | "Memory" | "update_memory" | "UpdateMemory" => String::new(),

        // Internal buffer/editor tools — opaque IDs add no value
        _ if name.starts_with("buf_") || name.starts_with("nvim_") => String::new(),

        // Generic fallback: first string value, no key name
        _ => {
            if let serde_json::Value::Object(map) = args {
                for (_, val) in map.iter() {
                    if let serde_json::Value::String(s) = val {
                        return truncate_summary(s, 80);
                    }
                }
            }
            String::new()
        }
    }
}

/// Icon for a tool category.
pub fn tool_icon(_name: &str) -> &'static str {
    "▶"
}

/// Category tag for a tool name (used by TUI for colour theming).
pub fn tool_category(name: &str) -> &'static str {
    match name {
        "read_file" | "Read" | "write_file" | "Write" | "str_replace" | "StrReplace"
        | "str_replace_editor" | "edit_file" | "delete_file" | "Delete" | "EditNotebook"
        | "find_file" | "FindFile" | "list_dir" | "ListDir" => "file",
        "shell" | "bash" | "Shell" | "run_terminal_command" | "RunTerminalCommand" => "shell",
        "grep" | "Grep" | "glob" | "Glob" | "search_codebase" | "SemanticSearch"
        | "semantic_search" | "search_knowledge" | "SearchKnowledge" => "search",
        "web_search" | "WebSearch" | "web_fetch" | "WebFetch" => "web",
        "todo" | "read_lints" | "ReadLints" | "ask_question" | "AskQuestion" | "switch_mode"
        | "SwitchMode" | "load_skill" | "LoadSkill" | "memory" | "Memory" | "update_memory"
        | "UpdateMemory" | "system" | "System" | "skill" | "Skill" => "system",
        _ if name.starts_with("gdb") || name.starts_with("Gdb") => "agent",
        _ if name.starts_with("buf_") => "system",
        _ => "",
    }
}

/// Simple string truncation for summaries (unicode-agnostic fast path using chars).
///
/// This does not depend on unicode-width; sven-tui will apply proper
/// display-width truncation when rendering.
fn truncate_summary(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else {
        let t: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summary_read_file() {
        let args = json!({"path": "/data/foo/bar.rs"});
        assert_eq!(tool_smart_summary("read_file", &args), "foo/bar.rs");
    }

    #[test]
    fn summary_shell() {
        // "command" key (legacy)
        let args = json!({"command": "cargo build"});
        assert_eq!(tool_smart_summary("shell", &args), "cargo build");
    }

    #[test]
    fn summary_shell_command_key() {
        // "shell_command" key (ShellTool)
        let args = json!({"shell_command": "cargo test"});
        assert_eq!(tool_smart_summary("shell", &args), "cargo test");
    }

    #[test]
    fn summary_shell_description_priority() {
        // "description" key takes priority over shell_command for collapsed display
        let args = json!({"description": "Run tests", "shell_command": "cargo test --all"});
        assert_eq!(tool_smart_summary("shell", &args), "Run tests");
    }

    #[test]
    fn summary_grep() {
        let args = json!({"pattern": "fn main", "path": "/data/src"});
        let s = tool_smart_summary("grep", &args);
        assert!(s.contains("fn main"));
        assert!(s.contains("src"));
    }

    #[test]
    fn shorten_path_basic() {
        assert_eq!(
            shorten_path("/data/agents/sven/src/main.rs", 2),
            "src/main.rs"
        );
        assert_eq!(
            shorten_path("/data/agents/sven/src/main.rs", 3),
            "sven/src/main.rs"
        );
    }

    #[test]
    fn tool_icon_shell() {
        assert_eq!(tool_icon("Shell"), "▶");
    }
}
