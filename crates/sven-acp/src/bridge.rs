// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Stateless conversion helpers between sven-internal types and ACP wire types.
//!
//! Mirrors the clean bridge pattern in `sven-mcp/src/bridge.rs`.  All
//! functions are pure (no I/O, no allocation of sessions) so they can be
//! unit-tested trivially.

use agent_client_protocol::{
    ContentBlock, ContentChunk, CurrentModeUpdate, Plan, PlanEntry, PlanEntryPriority,
    PlanEntryStatus, SessionModeId, SessionUpdate, ToolCall as AcpToolCall, ToolCallStatus,
    ToolKind,
};
use sven_config::AgentMode;
use sven_core::AgentEvent;
use sven_tools::events::{TodoItem, TodoStatus};

// ─── Mode mapping ─────────────────────────────────────────────────────────────

/// Convert a sven [`AgentMode`] to the matching ACP [`SessionModeId`].
pub fn sven_mode_to_acp_mode_id(mode: AgentMode) -> SessionModeId {
    match mode {
        AgentMode::Research => SessionModeId::new("research"),
        AgentMode::Plan => SessionModeId::new("plan"),
        AgentMode::Agent => SessionModeId::new("agent"),
    }
}

/// Convert an ACP [`SessionModeId`] back to a sven [`AgentMode`].
///
/// Unknown mode IDs fall back to [`AgentMode::Agent`].
pub fn acp_mode_id_to_sven_mode(mode_id: &SessionModeId) -> AgentMode {
    match mode_id.0.as_ref() {
        "research" => AgentMode::Research,
        "plan" => AgentMode::Plan,
        _ => AgentMode::Agent,
    }
}

// ─── Event bridge ─────────────────────────────────────────────────────────────

/// Map one [`AgentEvent`] to zero or one ACP [`SessionUpdate`] notifications.
///
/// Returns `None` for events that have no ACP equivalent (e.g. raw token
/// budget bookkeeping) so the caller can skip the `session/update` send.
pub fn agent_event_to_session_update(event: &AgentEvent) -> Option<SessionUpdate> {
    match event {
        AgentEvent::TextDelta(text) => Some(SessionUpdate::AgentMessageChunk(ContentChunk::new(
            ContentBlock::from(text.as_str()),
        ))),

        // TextComplete carries the full accumulated text (not a new delta), so
        // forwarding it would duplicate everything already sent via TextDelta.
        AgentEvent::TextComplete(_) => None,

        AgentEvent::ThinkingDelta(text) => Some(SessionUpdate::AgentThoughtChunk(
            ContentChunk::new(ContentBlock::from(text.as_str())),
        )),

        // Same as TextComplete – drop to avoid duplicating thought chunks.
        AgentEvent::ThinkingComplete(_) => None,

        AgentEvent::ToolCallStarted(tc) => {
            let acp_tc = AcpToolCall::new(tc.id.clone(), tc.name.clone())
                .kind(tool_name_to_kind(&tc.name))
                .status(ToolCallStatus::InProgress)
                .raw_input(tc.args.clone());
            Some(SessionUpdate::ToolCall(acp_tc))
        }

        AgentEvent::ToolCallFinished {
            call_id,
            tool_name,
            output,
            is_error,
        } => {
            let status = if *is_error {
                ToolCallStatus::Failed
            } else {
                ToolCallStatus::Completed
            };
            let raw_output = serde_json::Value::String(output.clone());
            let acp_tc = AcpToolCall::new(call_id.clone(), tool_name.clone())
                .kind(tool_name_to_kind(tool_name))
                .status(status)
                .raw_output(raw_output);
            Some(SessionUpdate::ToolCall(acp_tc))
        }

        AgentEvent::TodoUpdate(todos) => {
            let entries = todos.iter().map(todo_item_to_plan_entry).collect();
            Some(SessionUpdate::Plan(Plan::new(entries)))
        }

        AgentEvent::ModeChanged(mode) => {
            let mode_id = sven_mode_to_acp_mode_id(*mode);
            Some(SessionUpdate::CurrentModeUpdate(CurrentModeUpdate::new(
                mode_id,
            )))
        }

        AgentEvent::Error(msg) => Some(SessionUpdate::AgentMessageChunk(ContentChunk::new(
            ContentBlock::from(format!("[error] {msg}").as_str()),
        ))),

        // These events signal prompt completion; the caller handles them separately.
        AgentEvent::TurnComplete | AgentEvent::Aborted { .. } => None,

        // The remaining events have no ACP representation at this time.
        AgentEvent::TokenUsage { .. }
        | AgentEvent::ContextCompacted { .. }
        | AgentEvent::ToolProgress { .. }
        | AgentEvent::Question { .. }
        | AgentEvent::QuestionAnswer { .. }
        | AgentEvent::CollabEvent(_)
        | AgentEvent::TitleGenerated(_)
        | AgentEvent::DelegateSummary { .. } => None,
    }
}

// ─── Tool-kind heuristic ──────────────────────────────────────────────────────

fn tool_name_to_kind(name: &str) -> ToolKind {
    match name {
        "read_file" | "read_image" | "list_dir" | "find_file" | "buf_read" => ToolKind::Read,
        "write" | "edit_file" | "update_memory" => ToolKind::Edit,
        "delete_file" => ToolKind::Delete,
        "grep" | "search_codebase" | "buf_grep" | "context_grep" => ToolKind::Search,
        "run_terminal_command" | "shell" | "task" => ToolKind::Execute,
        "web_fetch" | "web_search" => ToolKind::Fetch,
        "switch_mode" => ToolKind::SwitchMode,
        _ => ToolKind::Other,
    }
}

// ─── Plan helpers ─────────────────────────────────────────────────────────────

fn todo_item_to_plan_entry(item: &TodoItem) -> PlanEntry {
    let status = match item.status {
        TodoStatus::InProgress => PlanEntryStatus::InProgress,
        TodoStatus::Completed => PlanEntryStatus::Completed,
        TodoStatus::Pending | TodoStatus::Cancelled => PlanEntryStatus::Pending,
    };
    PlanEntry::new(&item.content, PlanEntryPriority::Medium, status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_agent_mode() {
        for mode in [AgentMode::Research, AgentMode::Plan, AgentMode::Agent] {
            let id = sven_mode_to_acp_mode_id(mode);
            assert_eq!(acp_mode_id_to_sven_mode(&id), mode);
        }
    }

    #[test]
    fn text_delta_maps_to_agent_message_chunk() {
        let ev = AgentEvent::TextDelta("hello".into());
        assert!(matches!(
            agent_event_to_session_update(&ev),
            Some(SessionUpdate::AgentMessageChunk(_))
        ));
    }

    #[test]
    fn thinking_delta_maps_to_thought_chunk() {
        let ev = AgentEvent::ThinkingDelta("thinking".into());
        assert!(matches!(
            agent_event_to_session_update(&ev),
            Some(SessionUpdate::AgentThoughtChunk(_))
        ));
    }

    #[test]
    fn turn_complete_returns_none() {
        assert!(agent_event_to_session_update(&AgentEvent::TurnComplete).is_none());
    }
}
