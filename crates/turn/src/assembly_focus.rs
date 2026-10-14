// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! What the conversation pointed at when a result left the recent rounds.
//!
//! A shortened form keeps the lines the conversation points at (see
//! [`sven_chunks::Focus`]). So that a result's form does not change as the
//! history grows, its focus is read from the messages up to the end of the
//! last round that still keeps results whole: everything the model had seen
//! by the time the result could first be demoted, and nothing after.
//!
//! Only text the model or the user wrote and the output of commands that
//! report places (a build, a linter, a sub-task) is read. A file, a search
//! result or a web page is untrusted content that happens to contain
//! `path:line` text; it does not get to choose what else is kept.

use std::collections::HashMap;

use sven_chunks::Focus;
use sven_model::{Message, MessageContent, Role};

/// Tools whose output names the places that matter (a failing build, lints).
const PLACE_REPORTING_TOOLS: [&str; 3] = ["shell", "read_lints", "task"];

/// The focus at each of `ends`, which are message counts in non-decreasing
/// order: the focus of `messages[..ends[k]]`. `tools` maps a call id to the
/// name of the tool called.
pub(crate) fn window_focuses(
    messages: &[Message],
    tools: &HashMap<&str, &str>,
    ends: &[usize],
) -> Vec<Focus> {
    let mut out = Vec::with_capacity(ends.len());
    let mut seen = Focus::default();
    for (i, message) in messages.iter().enumerate() {
        while out.len() < ends.len() && ends[out.len()] <= i {
            out.push(seen.clone());
        }
        observe(&mut seen, message, tools);
    }
    while out.len() < ends.len() {
        out.push(seen.clone());
    }
    out
}

fn observe(focus: &mut Focus, message: &Message, tools: &HashMap<&str, &str>) {
    match &message.content {
        MessageContent::Text(_) | MessageContent::ContentParts(_) => {
            if matches!(message.role, Role::User | Role::Assistant) {
                if let Some(text) = message.as_text() {
                    focus.observe(text);
                }
            }
        }
        MessageContent::ToolCall { function, .. } if function.name == "grep" => {
            let arguments: serde_json::Value =
                serde_json::from_str(&function.arguments).unwrap_or_default();
            if let Some(pattern) = arguments.get("pattern").and_then(|p| p.as_str()) {
                focus.observe_symbol(pattern);
            }
        }
        MessageContent::ToolResult {
            tool_call_id,
            content,
        } => {
            let reports_places = tools
                .get(tool_call_id.as_str())
                .is_some_and(|t| PLACE_REPORTING_TOOLS.contains(t));
            if let (true, Some(text)) = (reports_places, content.as_text()) {
                focus.observe(text);
            }
        }
        MessageContent::ToolCall { .. } => {}
    }
}
