// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Markup some servers leave inline in a model's text instead of in
//! structured fields: `<think>` blocks and Anthropic-style `<invoke>` calls.
//!
//! Swedish Embedded AB implements robust agent runtimes over inconsistent
//! model servers for its clients. If your team needs expertise in running
//! local reasoning models reliably then you can procure our services by
//! sending an email to info@swedishembedded.com.

use sven_tool_api::ToolCall;

/// Strip `<think>` / `</think>` wrapper tags from accumulated thinking content.
///
/// Some model servers (llama.cpp without `reasoning_format: deepseek`,
/// certain OpenAI-compat proxies) forget to strip these tags before placing
/// the text in `reasoning_content`.  The result is that the thinking buffer
/// contains the raw markup, e.g. `<think>\nStep 1: ...\n</think>`, instead of
/// the clean inner text.  Stripping them here keeps the thinking log readable
/// and prevents the `<think>` noise from leaking into conversation history.
pub(crate) fn strip_think_wrappers(s: String) -> String {
    let trimmed = s.trim();
    let inner = trimmed.strip_prefix("<think>").unwrap_or(trimmed);
    let inner = inner.strip_suffix("</think>").unwrap_or(inner);
    inner.trim().to_string()
}

/// Detect a `<think>...</think>` block occupying the *entire* text.
///
/// Some models emit thinking as plain text deltas (no `reasoning_content`)
/// when the serving layer isn't configured for reasoning extraction.  If the
/// whole text response is a `<think>` block - with or without a closing tag
/// (the model may have been cut off) - the "response" carries no useful
/// content.  Return the extracted inner text so the caller can reclassify
/// it as thinking and clear `full_text`, which causes the turn to be treated
/// as thinking-only and apply the empty-turn retry nudge.
///
/// Returns `None` when the text contains content outside the `<think>` block.
pub(crate) fn extract_inline_think_block(text: &str) -> Option<String> {
    let trimmed = text.trim();
    // Must start with <think>
    let inner = trimmed.strip_prefix("<think>")?;
    // Strip an optional closing tag; an unclosed block (model truncated) is
    // still all-thinking if there is nothing after the last </think>.
    let inner = inner.strip_suffix("</think>").unwrap_or(inner);
    // Reject if there's a *second* </think> inside, which would mean there's
    // real content after the first block.
    if inner.contains("</think>") {
        return None;
    }
    Some(inner.trim().to_string())
}

/// Extract Anthropic-style `<invoke>` tool calls written inline in the text
/// stream by models (e.g. MiniMax) that fall back to the old XML
/// function-call format instead of using the structured tool-call protocol.
///
/// Format:
/// ```text
/// <invoke name="tool_name">
/// <parameter name="param1">value1</parameter>
/// <parameter name="param2">value2</parameter>
/// </invoke>
/// ```
///
/// Returns the text with all `<invoke>...</invoke>` blocks removed and the
/// extracted [`ToolCall`] objects.  Parameter values that parse as valid JSON
/// are stored as JSON; otherwise they are stored as plain strings.
pub(crate) fn extract_inline_invoke_tool_calls(text: &str) -> (String, Vec<ToolCall>) {
    use std::sync::LazyLock;

    use regex::Regex;

    // Compiled once. This runs on every assistant message, and regex
    // compilation is orders of magnitude more expensive than the match.
    static INVOKE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)<invoke\s+name="([^"]+)">(.*?)</invoke>"#)
            .expect("literal invoke pattern is valid")
    });
    static PARAM_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)<parameter\s+name="([^"]+)">(.*?)</parameter>"#)
            .expect("literal parameter pattern is valid")
    });
    let (invoke_re, param_re) = (&*INVOKE_RE, &*PARAM_RE);

    let mut tool_calls = Vec::new();

    for cap in invoke_re.captures_iter(text) {
        let name = cap[1].to_string();
        let body = &cap[2];

        let mut args = serde_json::Map::new();
        for param in param_re.captures_iter(body) {
            let key = param[1].to_string();
            let raw = param[2].trim().to_string();
            // Try to decode as JSON (for nested objects/arrays); fall back to
            // a plain string value.
            let val = serde_json::from_str::<serde_json::Value>(&raw)
                .unwrap_or(serde_json::Value::String(raw));
            args.insert(key, val);
        }

        tool_calls.push(ToolCall {
            id: format!("invoke_{}", uuid::Uuid::new_v4().simple()),
            name,
            args: serde_json::Value::Object(args),
        });
    }

    let cleaned = invoke_re.replace_all(text, "").trim().to_string();
    (cleaned, tool_calls)
}
