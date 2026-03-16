// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Lightweight markdown-to-plain-text converter for the Slint UI.
//!
//! Strips markdown formatting to produce readable plain text suitable for
//! rendering in Slint's `Text` elements. The TUI uses syntect for full
//! terminal-colour rendering; the GUI uses this simplified version since
//! Slint's text elements do not parse markdown natively.

/// Strip markdown formatting and return plain text suitable for display.
///
/// Handles the most common patterns in sven's agent output:
/// - Code blocks (``` ... ```) — kept as-is (content is preserved)
/// - Inline code (`...`) — backticks removed
/// - Headers (`#`, `##`, etc.) — `#` characters removed
/// - Bold/italic (`**`, `__`, `*`, `_`) — markers removed
/// - Horizontal rules (`---`, `***`) — replaced with a blank line
/// - List markers (`- `, `* `, `1. `) — kept as indented lines
pub fn strip_markdown(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    let mut result = String::with_capacity(text.len());
    let mut in_code_block = false;
    let mut code_fence = String::new();

    for line in text.lines() {
        let trimmed = line.trim();

        // Toggle code block state on fence markers.
        if trimmed.starts_with("```") {
            if in_code_block {
                if trimmed.starts_with(&code_fence) {
                    in_code_block = false;
                    code_fence.clear();
                    result.push('\n');
                    continue;
                }
            } else {
                in_code_block = true;
                code_fence = "```".to_string();
                result.push('\n');
                continue;
            }
        }

        if in_code_block {
            result.push_str(line);
            result.push('\n');
            continue;
        }

        // Horizontal rules → blank line
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            result.push('\n');
            continue;
        }

        // Strip leading `#` from headers
        let line = if trimmed.starts_with('#') {
            let stripped = trimmed.trim_start_matches('#').trim();
            format!("{stripped}\n")
        } else {
            format!("{line}\n")
        };

        // Strip inline markers: **bold**, *italic*, __bold__, _italic_, `code`
        let line = strip_inline_markers(&line);
        result.push_str(&line);
    }

    result.trim_end().to_string()
}

fn strip_inline_markers(text: &str) -> String {
    // Simple multi-pass stripping — not a full parser but covers common cases.
    let text = text.replace("**", "").replace("__", "").replace("~~", "");

    // Strip single * and _ only when they appear as emphasis markers,
    // not in the middle of words (e.g. snake_case).
    // This is a conservative approach: only strip leading/trailing pairs.
    let text = strip_single_marker(&text, '*');
    let text = strip_single_marker(&text, '_');

    // Strip inline code backticks
    strip_inline_code(&text)
}

fn strip_single_marker(text: &str, marker: char) -> String {
    // Replace `*word*` or `_word_` patterns — simple greedy strip.
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_emphasis = false;

    while let Some(c) = chars.next() {
        if c == marker {
            // Check if surrounded by whitespace (or start/end of text) — if so,
            // treat as a toggling marker; otherwise keep (e.g. snake_case).
            let prev_is_boundary = result
                .chars()
                .last()
                .is_none_or(|p| p.is_whitespace() || p == '\n');
            let next_is_boundary = chars
                .peek()
                .is_none_or(|n| n.is_whitespace() || *n == '\n' || *n == marker);

            if prev_is_boundary || next_is_boundary || in_emphasis {
                in_emphasis = !in_emphasis;
                // skip the marker
                continue;
            }
        }
        result.push(c);
    }

    result
}

fn strip_inline_code(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_code = false;
    let chars = text.chars().peekable();

    for c in chars {
        if c == '`' {
            in_code = !in_code;
            // Skip the backtick itself
            continue;
        }
        result.push(c);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_headers() {
        assert_eq!(strip_markdown("# Hello"), "Hello");
        assert_eq!(strip_markdown("## World"), "World");
    }

    #[test]
    fn strips_bold() {
        assert_eq!(strip_markdown("this is **bold** text"), "this is bold text");
    }

    #[test]
    fn preserves_code_blocks() {
        let input = "```\nlet x = 1;\n```";
        let out = strip_markdown(input);
        assert!(out.contains("let x = 1;"));
    }

    #[test]
    fn preserves_snake_case() {
        let input = "use snake_case names";
        assert_eq!(strip_markdown(input), input);
    }
}
