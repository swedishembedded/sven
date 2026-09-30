// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! How a sub-agent's ACP `session/update` notifications appear to the parent:
//! as [`SubagentUpdate`]s, plus the assistant text they carry.

use agent_client_protocol::{ContentBlock, SessionUpdate, ToolCallStatus};
use serde_json::Value;
use tracing::debug;

use sven_tool_api::events::SubagentUpdate;

/// Convert one ACP [`SessionUpdate`] into zero or more [`SubagentUpdate`]s and
/// an optional text chunk to append to `final_text`.
///
/// The function is pure - callers append the returned text to their accumulator
/// explicitly, keeping side effects visible at the call site.
pub(super) fn session_update_to_subagent_updates(
    update: &SessionUpdate,
) -> (Vec<SubagentUpdate>, Option<String>) {
    let mut updates = Vec::new();
    let mut text_chunk: Option<String> = None;

    match update {
        SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
            ContentBlock::Text(t) => {
                text_chunk = Some(t.text.clone());
                updates.push(SubagentUpdate::TextDelta(t.text.clone()));
            }
            other => {
                debug!(
                    "task: dropping non-text AgentMessageChunk content variant: {:?}",
                    std::mem::discriminant(other)
                );
            }
        },
        SessionUpdate::AgentThoughtChunk(chunk) => match &chunk.content {
            ContentBlock::Text(t) => {
                updates.push(SubagentUpdate::ThinkingDelta(t.text.clone()));
            }
            other => {
                debug!(
                    "task: dropping non-text AgentThoughtChunk content variant: {:?}",
                    std::mem::discriminant(other)
                );
            }
        },
        SessionUpdate::ToolCall(tc) => {
            let id = tc.tool_call_id.to_string();
            let name = tc.title.clone();
            match tc.status {
                ToolCallStatus::InProgress => {
                    let args = tc.raw_input.clone().unwrap_or(Value::Null);
                    updates.push(SubagentUpdate::ToolCallStarted { id, name, args });
                }
                ToolCallStatus::Completed => {
                    let output = tc
                        .raw_output
                        .as_ref()
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .unwrap_or_default();
                    updates.push(SubagentUpdate::ToolCallFinished {
                        id,
                        name,
                        output,
                        is_error: false,
                    });
                }
                ToolCallStatus::Failed => {
                    let output = tc
                        .raw_output
                        .as_ref()
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .unwrap_or_default();
                    updates.push(SubagentUpdate::ToolCallFinished {
                        id,
                        name,
                        output,
                        is_error: true,
                    });
                }
                _ => {}
            }
        }
        SessionUpdate::Plan(plan) => {
            // Serialize the plan to a text delta so the parent TUI can display
            // the child's todo list without needing a separate SubagentUpdate
            // variant.  Empty plans (heartbeat pings) are silently dropped.
            if !plan.entries.is_empty() {
                let mut text = String::from("[Plan]\n");
                for entry in &plan.entries {
                    let status_icon = match entry.status {
                        agent_client_protocol::PlanEntryStatus::Completed => "✓",
                        agent_client_protocol::PlanEntryStatus::InProgress => "→",
                        _ => "○",
                    };
                    text.push_str(&format!("  {status_icon} {}\n", entry.content));
                }
                updates.push(SubagentUpdate::TextDelta(text));
            }
        }
        SessionUpdate::CurrentModeUpdate(mode_update) => {
            let mode_text = format!("[Mode: {}]\n", mode_update.current_mode_id.0);
            updates.push(SubagentUpdate::TextDelta(mode_text));
        }
        SessionUpdate::UsageUpdate(usage) => {
            if let Some(ref cost) = usage.cost {
                updates.push(SubagentUpdate::TokenUsage {
                    cost_usd: cost.amount,
                });
            }
        }
        _ => {}
    }

    (updates, text_chunk)
}
