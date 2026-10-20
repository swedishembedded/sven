// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The tools' side channel, put on the observation plane.

use std::sync::Arc;

use sven_executors::ThreadStore;
use sven_hsm::{ObservationSink, UiEvent};
use sven_tool_api::events::ToolEvent;
use tokio::sync::mpsc;

/// Forward [`ToolEvent`]s onto the kernel's [`ObservationSink`] as the
/// matching [`UiEvent`], for as long as `rx`'s sender half is alive.
///
/// # Why this exists
///
/// Tools report side-band state changes through
/// `sven_tool_api::events::ToolEvent` — `TodoTool` (todo list updates),
/// `TaskTool` (`SubagentStarted`/`SubagentEvent`), `SystemTool`
/// (`ModeChanged`). `TurnExecutor` only reads the kernel's own `UiEvent`
/// observation plane, so without this forwarder those events would reach no
/// surface (TUI, CI, ACP, SDK) — `RuntimeBuilder` is the one assembly point
/// every one of them goes through.
///
/// This function re-threads the side channel onto the observation plane,
/// mapping each `ToolEvent` onto the `UiEvent` variant that reports the same
/// fact (`AgentEvent`/`UiEvent` are the same `sven_vocab::SessionEvent`
/// type, so a `ToolEvent` and the equivalent `AgentEvent` produce the same
/// value).
///
/// `ToolEvent::McpServerAdded`/`McpServerRemoved` are deliberately **not**
/// forwarded — they are registry mutations (add/remove a tool from the live
/// `ToolRegistry`), not renderable observations, and have no `UiEvent`
/// counterpart.
pub(crate) fn spawn_tool_event_forwarder(
    mut rx: mpsc::Receiver<ToolEvent>,
    obs: ObservationSink,
    store: Arc<std::sync::Mutex<ThreadStore>>,
) {
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let ui_event = match event {
                ToolEvent::TodoUpdate(items) => {
                    // The list is also state outside the conversation: a
                    // compaction carries it through verbatim.
                    if let Ok(mut store) = store.lock() {
                        store.set_todos(items.clone());
                    }
                    UiEvent::TodoUpdate(items)
                }
                ToolEvent::ModeChanged(mode) => UiEvent::ModeChanged(mode),
                ToolEvent::ModelChanged(m) => UiEvent::ModelChanged(m),
                ToolEvent::Progress { call_id, message } => {
                    UiEvent::ToolProgress { call_id, message }
                }
                ToolEvent::DelegateSummary {
                    to_name,
                    task_title,
                    duration_ms,
                    status,
                    result_preview,
                } => UiEvent::DelegateSummary {
                    to_name,
                    task_title,
                    duration_ms,
                    status,
                    result_preview,
                },
                ToolEvent::SubagentStarted {
                    call_id,
                    handle_id,
                    description,
                    prompt,
                } => UiEvent::SubagentStarted {
                    call_id,
                    handle_id,
                    description,
                    prompt,
                },
                ToolEvent::SubagentEvent {
                    call_id,
                    handle_id,
                    update,
                } => UiEvent::SubagentEvent {
                    call_id,
                    handle_id,
                    update,
                },
                ToolEvent::Notice(text) => UiEvent::Warning(text),
                // No `UiEvent` counterpart — see the doc comment above.
                ToolEvent::McpServerAdded(_) | ToolEvent::McpServerRemoved(_) => continue,
            };
            obs.emit(ui_event);
        }
    });
}
