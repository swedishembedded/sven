// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A tool call in words, for a server that shows the conversation.
//!
//! Every word comes from what the model already wrote: the tool's own
//! one-line summary of its arguments (`sven_tool_api::tool_smart_summary`,
//! the same the terminal shows), and its name made readable. Nothing asks the
//! model to describe its own calls, so describing them costs no tokens.

use serde_json::Value;

/// What the call did, its one-line title, and its category.
#[derive(Clone)]
pub(super) struct Described {
    pub action: String,
    pub title: String,
    pub category: String,
}

/// A tool name as words: `read_file` is "Read file", an MCP tool's
/// `github__create_issue` is "Github: create issue".
fn readable(name: &str) -> String {
    let words = name.replace("__", ": ").replace(['_', '-'], " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

pub(super) fn describe(name: &str, args: &Value) -> Described {
    let category = sven_tool_api::tool_category(name).to_string();
    // A command shows itself: what runs is what a reader wants to see.
    let command = (category == "shell")
        .then(|| {
            ["shell_command", "command"]
                .iter()
                .find_map(|key| args.get(*key).and_then(Value::as_str))
                .map(|command| command.lines().next().unwrap_or_default().to_string())
        })
        .flatten();
    let summary = command.unwrap_or_else(|| sven_tool_api::tool_smart_summary(name, args));
    let action = readable(name);
    Described {
        title: if summary.is_empty() {
            action.clone()
        } else {
            summary
        },
        action,
        category,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_command_is_shown_as_it_runs_never_as_the_models_paraphrase() {
        let described = describe(
            "shell",
            &json!({"description": "List files", "shell_command": "ls -la\necho done"}),
        );
        assert_eq!(described.title, "ls -la");
        assert_eq!(described.category, "shell");
    }

    #[test]
    fn a_known_tool_says_what_it_touched() {
        let described = describe("read_file", &json!({"path": "/work/project/src/main.rs"}));
        assert_eq!(described.action, "Read file");
        assert!(described.title.ends_with("main.rs"), "{}", described.title);
    }

    #[test]
    fn a_tool_sven_does_not_know_still_reads_as_words() {
        let described = describe(
            "github__create_issue",
            &json!({"repository": 7, "title": "Fix the build"}),
        );
        assert_eq!(described.action, "Github: create issue");
        assert_eq!(described.title, "Fix the build");

        let bare = describe("refresh_cache", &json!({}));
        assert_eq!(bare.title, "Refresh cache");
    }
}
