// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Multi-session lifecycle: creating a new chat, switching the active chat,
//! snapshotting the active chat into its `SessionEntry` before it stops being
//! active, and (re)spawning the per-session agent task.

use std::sync::Arc;

use sven_config::AgentMode;
use sven_machines::AgentEvent;
use tokio::sync::mpsc;

use crate::{
    agent::AgentRequest,
    app::{
        agent_conn::AgentConn, chat_state::ChatState,
        construct::conversation_record_to_chat_segment, input_state::EditState,
        queue_state::QueueState, session_manager::SessionEntry, ui_state::FocusPane, App,
    },
    chat::segment::ChatSegment,
};

impl App {
    /// Create a new chat session, make it active, and clear the current chat state.
    pub(crate) async fn new_session(&mut self) {
        // Snapshot active chat into its session entry before creating the new one.
        self.save_active_to_session_entry();

        // Create the session entry and make it active.
        let new_id = self.sessions.create_session("New chat");
        self.sessions.active_id = new_id.clone();

        // Reset live chat/agent/input/queue/edit state for the new empty session.
        self.chat = ChatState::new();
        self.agent = AgentConn::new();
        self.session = crate::state::SessionState::new(self.config.model.clone(), AgentMode::Agent);
        self.input = crate::app::input_state::InputState::new();
        // Clear any in-flight queue and edit state from the previous session so
        // queued messages don't bleed into the new session's agent.
        self.queue = QueueState::new();
        self.edit = EditState::new();

        // Spawn an agent task; sets self.agent.tx and the entry's agent_tx/cancel.
        self.spawn_agent_for_session(&new_id).await;

        self.chat_title = "New chat".to_string();
        // Deferred: resolved lazily on first save (see `save_history_async`),
        // matching every other transient session.
        self.session_path = None;

        self.sessions.promote_to_top(&new_id);
        self.sessions.sync_list_selection_to_active();
        self.rerender_chat().await;
        self.ui.focus = FocusPane::Input;
    }

    /// Switch to a different session.
    pub(crate) async fn switch_session(&mut self, target_id: sven_session_store::SessionId) {
        // Save active state.
        self.save_active_to_session_entry();

        // Swap in the target session's stored state.
        let target_chat = self
            .sessions
            .get_mut(&target_id)
            .and_then(|e| e.stored_chat.take());

        // If the target session has no stored chat, try to load from disk -
        // either its native ATIF session file, or (for a not-yet-migrated
        // session) a legacy YAML chat via the read-only importer. Also
        // refresh session entry metadata (title, status, created_at) from
        // the full trajectory - load_from_disk only has header approximations.
        let target_chat = target_chat.or_else(|| {
            let entry = self.sessions.get(&target_id)?;
            let trajectory = if let Some(path) = entry.session_path.clone() {
                sven_session_store::load_session_from(&path).ok()
            } else if let Some(legacy_path) = entry.legacy_path.clone() {
                let doc = sven_session_store::load_chat_from(&legacy_path).ok()?;
                Some(sven_session_store::import_legacy_chat_document(&doc))
            } else {
                None
            }?;

            let segments: Vec<crate::chat::segment::ChatSegment> =
                sven_session_store::steps_to_conversation_records(&trajectory.steps)
                    .into_iter()
                    .filter_map(conversation_record_to_chat_segment)
                    .collect();

            // Refresh entry metadata from the full trajectory.
            if let Some(entry) = self.sessions.get_mut(&target_id) {
                let is_legacy = entry.is_legacy;
                let legacy_path = entry.legacy_path.clone();
                let refreshed = SessionEntry::from_trajectory_into(
                    &trajectory,
                    target_id.clone(),
                    None,
                    is_legacy,
                );
                entry.title = refreshed.title;
                entry.status = refreshed.status;
                entry.created_at = refreshed.created_at;
                entry.updated_at = refreshed.updated_at;
                entry.legacy_path = legacy_path;
                // The chat view omits copied-context steps: carry them on the
                // entry so the next save doesn't delete them from the file.
                entry.copied_context_steps = refreshed.copied_context_steps;
                // Restore persisted usage only when the entry has no live data yet
                // (i.e. this session has never been active in this process run).
                if entry.total_output_tokens == 0 && entry.total_cost_usd == 0.0 {
                    entry.total_context_tokens = refreshed.total_context_tokens;
                    entry.total_output_tokens = refreshed.total_output_tokens;
                    entry.total_cost_usd = refreshed.total_cost_usd;
                }
            }
            let mut chat = ChatState::new();
            chat.segments = segments;
            Some(chat)
        });

        // Subagent sessions get their stored_chat populated via SubagentEvent updates
        // (ACP-based subagents).  As a fallback for legacy subagent sessions that
        // pre-date the ACP rewrite, read the raw buffer content.  This path should
        // rarely be needed in practice.
        let target_chat = if target_chat.is_none() {
            let buffer_handle = self
                .sessions
                .get(&target_id)
                .and_then(|e| e.buffer_handle.clone());
            if let Some(handle) = buffer_handle {
                let store = self.buffer_store.lock().await;
                let content = store.read_all(&handle).unwrap_or_default();
                drop(store);
                if !content.is_empty() {
                    use sven_model::{Message, MessageContent, Role};
                    let seg = ChatSegment::Message(Message {
                        role: Role::Assistant,
                        content: MessageContent::Text(content),
                    });
                    let mut chat = ChatState::new();
                    chat.segments = vec![seg];
                    Some(chat)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            target_chat
        };

        let old_chat = std::mem::replace(&mut self.chat, target_chat.unwrap_or_default());
        let _ = old_chat; // already saved to session entry

        // Restore per-session model/mode state.
        let target_session_state = self
            .sessions
            .get_mut(&target_id)
            .and_then(|e| e.session_state.take())
            .unwrap_or_else(|| {
                crate::state::SessionState::new(self.config.model.clone(), AgentMode::Agent)
            });
        self.session = target_session_state;

        // Update active session ID.
        self.sessions.active_id = target_id.clone();

        // Swap agent tx (we keep background tasks running; just update which tx we use).
        let target_tx = self
            .sessions
            .get(&target_id)
            .and_then(|e| e.agent_tx.clone());
        let target_cancel = self
            .sessions
            .get(&target_id)
            .map(|e| e.agent_cancel.clone())
            .unwrap_or_else(|| Arc::new(tokio::sync::Mutex::new(None)));
        let target_busy = self
            .sessions
            .get(&target_id)
            .map(|e| e.busy)
            .unwrap_or(false);

        // If the target session has no agent yet, spawn one.
        if target_tx.is_none() {
            self.spawn_agent_for_active_session().await;
            // New agent is idle; clear busy state so the chat list doesn't show a
            // spinner on the session we just switched to (avoids ghost spinner when
            // clicking list items while another session's change was in progress).
            // Still restore token metrics from the target session.
            self.agent.busy = false;
            self.agent.current_tool = None;
            if let Some(entry) = self.sessions.get(&target_id) {
                self.agent.total_context_tokens = entry.total_context_tokens;
                self.agent.total_context_pct = entry.total_context_pct;
                self.agent.total_output_tokens = entry.total_output_tokens;
                self.agent.total_cost_usd = entry.total_cost_usd;
                self.agent.cache_hit_pct = entry.cache_hit_pct;
                self.agent.context_pct = entry.context_pct;
                self.agent.current_tool = entry.current_tool.clone();
            }
        } else {
            self.agent.tx = target_tx;
            self.agent.cancel = target_cancel;
            self.agent.busy = target_busy;
            // Restore token-related fields from the target session.
            if let Some(entry) = self.sessions.get(&target_id) {
                self.agent.total_context_tokens = entry.total_context_tokens;
                self.agent.total_context_pct = entry.total_context_pct;
                self.agent.total_output_tokens = entry.total_output_tokens;
                self.agent.total_cost_usd = entry.total_cost_usd;
                self.agent.cache_hit_pct = entry.cache_hit_pct;
            }
        }

        // Update chat title and session path.
        self.chat_title = self
            .sessions
            .get(&target_id)
            .map(|e| e.title.clone())
            .unwrap_or_else(|| "Chat".to_string());
        self.session_path = self
            .sessions
            .get(&target_id)
            .and_then(|e| e.session_path.clone());

        // Cancel any in-progress inline edit so stale edit state doesn't bleed
        // into the newly active session.
        self.edit.clear();

        // Restore the target session's input buffer and queue (or reset to empty).
        let (target_input_buffer, target_input_cursor, target_input_attachments, target_queue) =
            if let Some(entry) = self.sessions.get_mut(&target_id) {
                (
                    entry.stored_input_buffer.take().unwrap_or_default(),
                    entry.stored_input_cursor.take().unwrap_or(0),
                    entry.stored_input_attachments.take().unwrap_or_default(),
                    entry.stored_queue.take().unwrap_or_else(QueueState::new),
                )
            } else {
                (String::new(), 0, Vec::new(), QueueState::new())
            };
        self.input.buffer = target_input_buffer;
        self.input.cursor = target_input_cursor;
        self.input.scroll_offset = 0;
        self.input.attachments = target_input_attachments;
        self.queue = target_queue;

        self.sessions.sync_list_selection_to_active();
        self.rerender_chat().await;
        self.scroll_to_bottom();
    }

    /// Snapshot the current active session's chat state into its SessionEntry.
    ///
    /// For idle sessions that have a `session_path` (i.e. their content is
    /// already persisted on disk), the snapshot is stored but then immediately
    /// evicted (`stored_chat` cleared) to free memory.  Busy background
    /// sessions keep their `stored_chat` so incoming agent event segments are
    /// not lost.
    fn save_active_to_session_entry(&mut self) {
        let active_id = self.sessions.active_id.clone();
        if let Some(entry) = self.sessions.get_mut(&active_id) {
            entry.stored_chat = Some(self.chat.clone());
            entry.stored_input_buffer = Some(self.input.buffer.clone());
            entry.stored_input_cursor = Some(self.input.cursor);
            entry.stored_input_attachments = Some(self.input.attachments.clone());
            entry.stored_queue = Some(self.queue.clone());
            entry.session_state = Some(self.session.clone());
            entry.session_path = self.session_path.clone();
            entry.busy = self.agent.busy;
            entry.current_tool = self.agent.current_tool.clone();
            entry.title = self.chat_title.clone();
            entry.context_pct = self.agent.context_pct;
            entry.total_context_tokens = self.agent.total_context_tokens;
            entry.total_context_pct = self.agent.total_context_pct;
            entry.total_output_tokens = self.agent.total_output_tokens;
            entry.total_cost_usd = self.agent.total_cost_usd;
            entry.cache_hit_pct = self.agent.cache_hit_pct;
            entry.updated_at = chrono::Utc::now();

            // Evict stored_chat for idle sessions that have a backing file.
            // Busy sessions must keep their chat state in memory so that
            // background agent events can still push segments into it.
            let has_disk_backing = entry
                .session_path
                .as_ref()
                .map(|p| p.exists())
                .unwrap_or(false);
            if !entry.busy && has_disk_backing {
                entry.stored_chat = None;
            }
        }
    }

    /// Spawn a new local agent task for the currently active session.
    async fn spawn_agent_for_active_session(&mut self) {
        let id = self.sessions.active_id.clone();
        self.spawn_agent_for_session(&id).await;
    }

    /// Spawn a new local agent task for the given session ID, updating
    /// `self.agent` (if it's the active session) and the entry's `agent_tx`.
    async fn spawn_agent_for_session(&mut self, id: &sven_session_store::SessionId) {
        let (submit_tx, submit_rx) = mpsc::channel::<AgentRequest>(64);
        let (evt_tx, evt_rx) = mpsc::channel::<AgentEvent>(512);
        // Reuse the shared question sender so all sessions route questions
        // through the single question_rx in run().  Fall back to a disconnected
        // channel only in tests where run() was never called.
        let question_tx = self.question_tx.clone().unwrap_or_else(|| {
            let (tx, _) = mpsc::channel::<sven_tools_agent::QuestionRequest>(4);
            tx
        });
        let cancel = Arc::new(tokio::sync::Mutex::new(None));

        if *id == self.sessions.active_id {
            self.agent.tx = Some(submit_tx.clone());
            self.agent.cancel = cancel.clone();
        }
        if let Some(entry) = self.sessions.get_mut(id) {
            entry.agent_tx = Some(submit_tx);
            entry.agent_cancel = cancel.clone();
        }

        // Forwarding task.
        let mux_tx = self.sessions.multi_event_tx.clone();
        let session_id = id.clone();
        tokio::spawn(async move {
            let mut rx = evt_rx;
            while let Some(event) = rx.recv().await {
                if mux_tx.send((session_id.clone(), event)).await.is_err() {
                    break;
                }
            }
        });

        let cfg = self.config.clone();
        let mode = self.session.mode;
        let startup_model_cfg = self.session.model_cfg.clone();
        let shared_skills = self.shared_skills.clone();
        let shared_agents = self.shared_agents.clone();

        tokio::spawn(crate::agent::kernel_session_task(
            cfg,
            startup_model_cfg,
            mode,
            submit_rx,
            evt_tx,
            question_tx,
            cancel,
            shared_skills,
            shared_agents,
            None, // mcp_manager_tx - not needed for sub-session restarts
        ));
    }
}
