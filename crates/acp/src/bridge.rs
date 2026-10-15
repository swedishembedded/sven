// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Stateless conversion helpers between sven-internal types and ACP wire types.
//!
//! Mirrors the clean bridge pattern in `mcp/src/bridge.rs`.  All
//! functions are pure (no I/O, no allocation of sessions) so they can be
//! unit-tested trivially.

use agent_client_protocol::{
    ContentBlock, ContentChunk, Cost, CurrentModeUpdate, Meta, Plan, PlanEntry, PlanEntryPriority,
    PlanEntryStatus, SessionModeId, SessionUpdate, ToolCall as AcpToolCall, ToolCallStatus,
    ToolKind, UsageUpdate,
};
use sven_hsm::UiEvent;
use sven_machines::AgentEvent;
use sven_tool_api::events::{TodoItem, TodoStatus};
use sven_vocab::{AgentMode, STEP_METRICS_EXTRA_KEY};

// ─── Mode mapping ─────────────────────────────────────────────────────────────

/// Convert a sven [`AgentMode`] to the matching ACP [`SessionModeId`].
pub fn sven_mode_to_acp_mode_id(mode: AgentMode) -> SessionModeId {
    match mode {
        AgentMode::Research => SessionModeId::new("research"),
        AgentMode::Plan => SessionModeId::new("plan"),
        AgentMode::Agent => SessionModeId::new("agent"),
        AgentMode::Chat => SessionModeId::new("chat"),
        AgentMode::Sdlc => SessionModeId::new("sdlc"),
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

        // Same as TextComplete - drop to avoid duplicating thought chunks.
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

        // ToolProgress fires while a long-running tool is executing.  Forward
        // it as an empty Plan heartbeat so the parent ACP client receives a
        // notification and its inactivity timer is reset - without causing any
        // visible change in the subagent chat view.
        AgentEvent::ToolProgress { .. } => Some(SessionUpdate::Plan(Plan::new(vec![]))),

        // A model that has produced nothing yet is, for the client, the same
        // as a tool that is still running: forward the same empty Plan
        // heartbeat so its inactivity timer is reset while the agent waits.
        AgentEvent::ModelWaiting { .. } => Some(SessionUpdate::Plan(Plan::new(vec![]))),

        // Notices about the model server are not answer text: send them as
        // thought chunks, which a client shows apart from the reply.
        AgentEvent::ServedModelChanged {
            model,
            from,
            to,
            context_window,
        } => Some(thought_notice(&sven_vocab::served_model_changed_text(
            model,
            from.as_ref(),
            to.as_ref(),
            *context_window,
        ))),
        AgentEvent::Warning(text) => Some(thought_notice(&format!("warning: {text}"))),

        // A request's token ledger rides on a UsageUpdate: the context
        // meter it carries is the request's prompt size against the window,
        // and the record itself is attached under `_meta`.
        AgentEvent::TokenUsage {
            breakdown: Some(request),
            max_tokens,
            ..
        } => {
            let prompt = request
                .reported
                .map_or(request.estimated_total, |r| r.logical_prompt());
            let mut meta = Meta::new();
            meta.insert(
                STEP_METRICS_EXTRA_KEY.to_string(),
                serde_json::to_value(request).expect("a breakdown always serializes"),
            );
            Some(SessionUpdate::UsageUpdate(
                UsageUpdate::new(u64::from(prompt), *max_tokens as u64).meta(meta),
            ))
        }

        // When the API reports cost (e.g. OpenRouter), forward to parent via UsageUpdate.
        AgentEvent::TokenUsage {
            cost_usd: Some(c), ..
        } => Some(SessionUpdate::UsageUpdate(
            UsageUpdate::new(0, 0).cost(Cost::new(*c, "USD")),
        )),

        // Transition (the kernel's per-dispatch trace event) has no ACP
        // representation; SDLC phase progress is not part of the ACP wire
        // contract.
        AgentEvent::Transition { .. } => None,

        // The remaining events have no ACP representation at this time.
        AgentEvent::TokenUsage { cost_usd: None, .. }
        | AgentEvent::ContextCompacted { .. }
        | AgentEvent::Question { .. }
        | AgentEvent::QuestionAnswer { .. }
        | AgentEvent::CollabEvent(_)
        | AgentEvent::TitleGenerated(_)
        | AgentEvent::DelegateSummary { .. }
        | AgentEvent::SubagentStarted { .. }
        | AgentEvent::SubagentEvent { .. }
        | AgentEvent::ModelChanged(_)
        | AgentEvent::PeerList(_)
        // A harness run's lifecycle is the host's concern; the ACP wire
        // carries the agent's own progress, not the harness around it.
        | AgentEvent::Harness { .. }
        // Only an SDK event stream reports its own gaps; the kernel never
        // emits it, so there is nothing to send.
        | AgentEvent::EventsDropped { .. } => None,
        // The event type is non-exhaustive; an event added later is not sent
        // until it is given a mapping here.
        _ => None,
    }
}

fn thought_notice(text: &str) -> SessionUpdate {
    SessionUpdate::AgentThoughtChunk(ContentChunk::new(ContentBlock::from(
        format!("[sven] {text}\n").as_str(),
    )))
}

// ─── UiEvent → SessionUpdate bridge (kernel path) ────────────────────────────

/// Map one kernel [`UiEvent`] to zero or one ACP [`SessionUpdate`] notifications.
///
/// `UiEvent` and `AgentEvent` are both re-exports of the same `SessionEvent`
/// type (`sven-vocab`), so this delegates to
/// [`agent_event_to_session_update`] and a kernel-driven session reports
/// every event, including its mode, to the IDE exactly as the agent path does.
pub fn ui_event_to_session_update(ev: &UiEvent) -> Option<SessionUpdate> {
    agent_event_to_session_update(ev)
}

// ─── Tool-kind heuristic ──────────────────────────────────────────────────────

fn tool_name_to_kind(name: &str) -> ToolKind {
    match name {
        "read_file" | "find_file" | "buf_read" => ToolKind::Read,
        "write" | "edit_file" | "update_memory" => ToolKind::Edit,
        "grep" | "buf_grep" | "context_grep" => ToolKind::Search,
        "shell" | "task" => ToolKind::Execute,
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
    fn a_request_ledger_rides_a_usage_update_with_the_record_under_meta() {
        let request = sven_vocab::RequestBreakdown {
            user: 40,
            estimated_total: 40,
            ..Default::default()
        };
        let ev = AgentEvent::TokenUsage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_read_total: 0,
            cache_write_total: 0,
            max_tokens: 1000,
            max_output_tokens: 0,
            cost_usd: None,
            breakdown: Some(Box::new(request.clone())),
        };
        let Some(SessionUpdate::UsageUpdate(update)) = agent_event_to_session_update(&ev) else {
            panic!("expected a UsageUpdate");
        };
        assert_eq!((update.used, update.size), (40, 1000));
        let record = update
            .meta
            .as_ref()
            .and_then(|m| m.get(STEP_METRICS_EXTRA_KEY))
            .expect("the record is attached under _meta");
        assert_eq!(
            serde_json::from_value::<sven_vocab::RequestBreakdown>(record.clone()).unwrap(),
            request
        );
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
    fn model_waiting_maps_to_a_heartbeat_plan() {
        let ev = AgentEvent::ModelWaiting {
            elapsed_secs: 30,
            phase: sven_vocab::ModelWaitPhase::AwaitingFirstToken,
        };
        assert!(matches!(
            agent_event_to_session_update(&ev),
            Some(SessionUpdate::Plan(_))
        ));
    }

    #[test]
    fn server_notices_map_to_thought_chunks() {
        for ev in [
            AgentEvent::Warning("no tools".into()),
            AgentEvent::ServedModelChanged {
                model: "m".into(),
                from: None,
                to: None,
                context_window: None,
            },
        ] {
            assert!(matches!(
                agent_event_to_session_update(&ev),
                Some(SessionUpdate::AgentThoughtChunk(_))
            ));
        }
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
