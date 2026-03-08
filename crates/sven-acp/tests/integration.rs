// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Integration tests for the sven ACP server.
//!
//! These tests drive the ACP layer directly at the bridge / agent level
//! without requiring a running sven node or real LLM provider.

use sven_acp::bridge::{acp_mode_id_to_sven_mode, agent_event_to_session_update, sven_mode_to_acp_mode_id};
use agent_client_protocol::SessionUpdate;
use sven_config::AgentMode;
use sven_core::AgentEvent;

// ─── Bridge unit tests ────────────────────────────────────────────────────────

#[test]
fn agent_mode_round_trip() {
    for mode in [AgentMode::Agent, AgentMode::Plan, AgentMode::Research] {
        let id = sven_mode_to_acp_mode_id(mode);
        assert_eq!(acp_mode_id_to_sven_mode(&id), mode, "round-trip failed for {mode:?}");
    }
}

#[test]
fn unknown_mode_falls_back_to_agent() {
    use agent_client_protocol::SessionModeId;
    let unknown = SessionModeId::new("totally-unknown");
    assert_eq!(acp_mode_id_to_sven_mode(&unknown), AgentMode::Agent);
}

#[test]
fn text_delta_becomes_agent_message_chunk() {
    let ev = AgentEvent::TextDelta("hello world".into());
    assert!(matches!(
        agent_event_to_session_update(&ev),
        Some(SessionUpdate::AgentMessageChunk(_))
    ));
}

#[test]
fn thinking_delta_becomes_thought_chunk() {
    let ev = AgentEvent::ThinkingDelta("let me think".into());
    assert!(matches!(
        agent_event_to_session_update(&ev),
        Some(SessionUpdate::AgentThoughtChunk(_))
    ));
}

#[test]
fn text_complete_has_no_update_to_avoid_double_send() {
    // TextComplete carries the full accumulated text, not a fresh delta.
    // Forwarding it would duplicate all content already sent via TextDelta.
    let ev = AgentEvent::TextComplete("full response".into());
    assert!(agent_event_to_session_update(&ev).is_none());
}

#[test]
fn thinking_complete_has_no_update_to_avoid_double_send() {
    let ev = AgentEvent::ThinkingComplete("full thought".into());
    assert!(agent_event_to_session_update(&ev).is_none());
}

#[test]
fn turn_complete_has_no_update() {
    assert!(agent_event_to_session_update(&AgentEvent::TurnComplete).is_none());
}

#[test]
fn aborted_has_no_update() {
    assert!(agent_event_to_session_update(&AgentEvent::Aborted {
        partial_text: String::new()
    })
    .is_none());
}

#[test]
fn error_becomes_agent_message_chunk() {
    let ev = AgentEvent::Error("something went wrong".into());
    let update = agent_event_to_session_update(&ev);
    assert!(matches!(update, Some(SessionUpdate::AgentMessageChunk(_))));
}

#[test]
fn todo_update_becomes_plan() {
    use sven_tools::events::{TodoItem, TodoStatus};
    let todos = vec![
        TodoItem {
            id: "t1".into(),
            content: "Write tests".into(),
            status: TodoStatus::InProgress,
        },
        TodoItem {
            id: "t2".into(),
            content: "Ship it".into(),
            status: TodoStatus::Pending,
        },
    ];
    let ev = AgentEvent::TodoUpdate(todos);
    assert!(matches!(agent_event_to_session_update(&ev), Some(SessionUpdate::Plan(_))));
}

#[test]
fn mode_changed_becomes_current_mode_update() {
    let ev = AgentEvent::ModeChanged(AgentMode::Research);
    assert!(matches!(
        agent_event_to_session_update(&ev),
        Some(SessionUpdate::CurrentModeUpdate(_))
    ));
}

#[test]
fn tool_call_started_becomes_tool_call() {
    use sven_tools::ToolCall;
    let tc = ToolCall {
        id: "call-1".into(),
        name: "read_file".into(),
        args: serde_json::json!({ "path": "src/main.rs" }),
    };
    let ev = AgentEvent::ToolCallStarted(tc);
    assert!(matches!(
        agent_event_to_session_update(&ev),
        Some(SessionUpdate::ToolCall(_))
    ));
}

#[test]
fn tool_call_finished_becomes_tool_call() {
    let ev = AgentEvent::ToolCallFinished {
        call_id: "call-1".into(),
        tool_name: "write".into(),
        output: "ok".into(),
        is_error: false,
    };
    assert!(matches!(
        agent_event_to_session_update(&ev),
        Some(SessionUpdate::ToolCall(_))
    ));
}
