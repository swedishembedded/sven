// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Multi-session management for the TUI.
//!
//! Each session is an independent conversation with its own agent task, chat
//! state, and YAML persistence.  Sessions can run concurrently: while one chat
//! is waiting for a model response, the user can switch to another and continue
//! working there.
//!
//! # Architecture
//!
//! ```text
//! App
//!  ├── chat: ChatState          ← active session's state (mutated by agent_events)
//!  ├── agent: AgentConn         ← active session's agent connection
//!  └── sessions: SessionManager
//!       ├── active_id: SessionId
//!       ├── entries: HashMap<SessionId, SessionEntry>   ← ALL sessions
//!       ├── display_order: Vec<SessionId>               ← sidebar order
//!       └── multi_event_rx: Receiver<(SessionId, AgentEvent)>
//! ```
//!
//! All agent tasks send events to `multi_event_rx` tagged with their session ID.
//! `handle_agent_event` routes events either to `App.chat/agent` (active) or to
//! the stored `SessionEntry.chat` (background).

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use chrono::{DateTime, Utc};
use sven_core::AgentEvent;
use sven_input::{ChatDocument, ChatStatus, SessionId};
use tokio::sync::{mpsc, Mutex};

use crate::{
    agent::AgentRequest,
    app::{chat_state::ChatState, input_state::InputAttachment, queue_state::QueueState},
};

// ── SessionEntry ──────────────────────────────────────────────────────────────

/// All data associated with a single chat session.
///
/// When a session is the **active** one, `App.chat` and `App.agent` hold its
/// live state.  When the session is in the **background**, its state is stored
/// here and synced from background agent events.
pub(crate) struct SessionEntry {
    // ── Identity & metadata ───────────────────────────────────────────────────
    pub id: SessionId,
    pub title: String,
    pub status: ChatStatus,
    /// Path to the `.yaml` file backing this session, or `None` for transient sessions.
    pub yaml_path: Option<PathBuf>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,

    // ── Stored chat state (populated when session is inactive) ────────────────
    /// Stored chat segments for inactive sessions (active session uses `App.chat`).
    pub stored_chat: Option<ChatState>,

    // ── Stored input/queue state (populated when session is inactive) ─────────
    /// Saved input buffer text for this session when inactive.
    pub stored_input_buffer: Option<String>,
    /// Saved cursor position within the input buffer.
    pub stored_input_cursor: Option<usize>,
    /// Saved input attachments (images) for this session when inactive.
    pub stored_input_attachments: Option<Vec<InputAttachment>>,
    /// Saved pending-message queue for this session when inactive.
    pub stored_queue: Option<QueueState>,

    // ── Per-session model/mode state ──────────────────────────────────────────
    /// Saved model/mode state for this session (populated when session is inactive).
    /// The active session's live state is in `App.session`.
    pub session_state: Option<crate::state::SessionState>,
    /// JSONL log path for this session (None for TUI-created sessions that use YAML).
    pub jsonl_path: Option<std::path::PathBuf>,

    // ── Agent connection ──────────────────────────────────────────────────────
    /// Sender for submitting requests to this session's background agent task.
    pub agent_tx: Option<mpsc::Sender<AgentRequest>>,
    /// Shared cancel handle for the running agent turn.
    pub agent_cancel: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    /// Whether this session's agent is currently processing a turn.
    pub busy: bool,
    /// Which tool this session is currently running (if busy).
    pub current_tool: Option<String>,
    /// Context window usage for the last turn (0-100 %), relative to the
    /// usable input budget (max_tokens - max_output_tokens).
    pub context_pct: u8,
    /// Current context window size in tokens (latest turn's prompt size).
    pub total_context_tokens: u32,
    /// Context window fill percentage derived from total_context_tokens.
    pub total_context_pct: u8,
    /// Cumulative output tokens across all completed turns in this session.
    pub total_output_tokens: u32,
    /// Cache-hit rate for the last turn (0-100 %).
    pub cache_hit_pct: u8,
}

impl SessionEntry {
    /// Create a new `SessionEntry` from a `ChatDocument`, keeping the document's
    /// original `SessionId` so the file path derived from it stays consistent.
    pub fn from_document(doc: &ChatDocument) -> Self {
        Self {
            id: doc.id.clone(),
            title: doc.title.clone(),
            status: doc.status,
            yaml_path: Some(sven_input::chat_path(&doc.id)),
            created_at: doc.created_at,
            updated_at: doc.updated_at,
            stored_chat: None,
            stored_input_buffer: None,
            stored_input_cursor: None,
            stored_input_attachments: None,
            stored_queue: None,
            session_state: None,
            jsonl_path: None,
            agent_tx: None,
            agent_cancel: Arc::new(Mutex::new(None)),
            busy: false,
            current_tool: None,
            context_pct: 0,
            total_context_tokens: 0,
            total_context_pct: 0,
            total_output_tokens: 0,
            cache_hit_pct: 0,
        }
    }

    /// Restore metadata from a `ChatDocument` into a pre-existing entry,
    /// keeping the supplied `id` (so the session manager's active_id reference
    /// stays valid).  Used when continuing a loaded chat in the TUI.
    pub fn from_document_into(doc: &ChatDocument, id: SessionId) -> Self {
        Self {
            id,
            title: doc.title.clone(),
            status: doc.status,
            yaml_path: None, // set separately via initial_yaml_path
            created_at: doc.created_at,
            updated_at: doc.updated_at,
            stored_chat: None,
            stored_input_buffer: None,
            stored_input_cursor: None,
            stored_input_attachments: None,
            stored_queue: None,
            session_state: None,
            jsonl_path: None,
            agent_tx: None,
            agent_cancel: Arc::new(Mutex::new(None)),
            busy: false,
            current_tool: None,
            context_pct: 0,
            total_context_tokens: 0,
            total_context_pct: 0,
            total_output_tokens: 0,
            cache_hit_pct: 0,
        }
    }

    /// Create a new blank session entry (not yet backed by a file).
    pub fn new_blank(title: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: SessionId::new(),
            title: title.into(),
            status: ChatStatus::Active,
            yaml_path: None,
            created_at: now,
            updated_at: now,
            stored_chat: None,
            stored_input_buffer: None,
            stored_input_cursor: None,
            stored_input_attachments: None,
            stored_queue: None,
            session_state: None,
            jsonl_path: None,
            agent_tx: None,
            agent_cancel: Arc::new(Mutex::new(None)),
            busy: false,
            current_tool: None,
            context_pct: 0,
            total_context_tokens: 0,
            total_context_pct: 0,
            total_output_tokens: 0,
            cache_hit_pct: 0,
        }
    }

    /// Build a `ChatDocument` from this entry, the supplied chat state, and
    /// runtime display metadata.  The entry's `created_at` is preserved so
    /// repeated saves don't reset the document's creation timestamp.
    pub fn to_document(
        &self,
        chat: &ChatState,
        model: Option<String>,
        mode: Option<String>,
    ) -> ChatDocument {
        use sven_input::{records_to_turns, ConversationRecord};
        use sven_model::Role;

        let records: Vec<ConversationRecord> = chat
            .segments
            .iter()
            .filter_map(|seg| match seg {
                crate::chat::segment::ChatSegment::Message(m) => {
                    if m.role == Role::System {
                        None
                    } else {
                        Some(ConversationRecord::Message(m.clone()))
                    }
                }
                crate::chat::segment::ChatSegment::Thinking { content } => {
                    Some(ConversationRecord::Thinking {
                        content: content.clone(),
                    })
                }
                crate::chat::segment::ChatSegment::ContextCompacted {
                    tokens_before,
                    tokens_after,
                    strategy,
                    turn,
                } => Some(ConversationRecord::ContextCompacted {
                    tokens_before: *tokens_before,
                    tokens_after: *tokens_after,
                    strategy: Some(strategy.to_string()),
                    turn: Some(*turn),
                }),
                _ => None,
            })
            .collect();

        let turns = records_to_turns(&records);

        ChatDocument {
            id: self.id.clone(),
            title: self.title.clone(),
            model,
            mode,
            status: self.status,
            created_at: self.created_at,
            updated_at: Utc::now(),
            turns,
        }
    }

    /// Apply a background agent event to this entry's stored state.
    ///
    /// Only metadata (busy, current_tool) is updated — we don't rebuild
    /// `StyledLines` for inactive sessions since that's expensive and invisible.
    pub fn apply_background_event(&mut self, event: &AgentEvent) {
        use sven_core::AgentEvent as Ev;
        match event {
            Ev::TextDelta(_) | Ev::ThinkingDelta(_) => {
                self.busy = true;
            }
            Ev::TextComplete(text) => {
                if let Some(chat) = &mut self.stored_chat {
                    chat.segments
                        .push(crate::chat::segment::ChatSegment::Message(
                            sven_model::Message::assistant(text),
                        ));
                    chat.streaming_buffer.clear();
                }
                self.busy = true;
            }
            Ev::ToolCallStarted(tc) => {
                self.busy = true;
                self.current_tool = Some(tc.name.clone());
                self.updated_at = Utc::now();
            }
            Ev::ToolCallFinished { tool_name, .. } => {
                if self.current_tool.as_deref() == Some(tool_name.as_str()) {
                    self.current_tool = None;
                }
            }
            Ev::TurnComplete => {
                self.busy = false;
                self.current_tool = None;
                self.status = ChatStatus::Completed;
                self.updated_at = Utc::now();
            }
            Ev::Aborted { .. } => {
                self.busy = false;
                self.current_tool = None;
            }
            Ev::Error(_) => {
                self.busy = false;
                self.current_tool = None;
            }
            Ev::TokenUsage {
                input,
                cache_read,
                cache_write,
                max_tokens,
                max_output_tokens,
                ..
            } => {
                if *max_tokens > 0 {
                    let input_budget = max_tokens.saturating_sub(*max_output_tokens);
                    let prompt = *input + *cache_read + *cache_write;
                    self.context_pct =
                        ((prompt as f64 / input_budget as f64) * 100.0).clamp(0.0, 100.0) as u8;
                }
            }
            // Display-only events: no stored-state update needed.
            _ => {}
        }
    }
}

// ── SessionManager ────────────────────────────────────────────────────────────

/// Manages all chat sessions and the shared agent-event multiplexer.
pub(crate) struct SessionManager {
    /// All session entries (active + background).
    pub entries: HashMap<SessionId, SessionEntry>,
    /// Display order for the sidebar (most recent first).
    pub display_order: Vec<SessionId>,
    /// The session that owns `App.chat` and `App.agent`.
    pub active_id: SessionId,
    /// Shared receiver for events from all agent tasks (tagged with session IDs).
    pub multi_event_rx: mpsc::Receiver<(SessionId, AgentEvent)>,
    /// Shared sender — cloned into forwarding tasks when spawning agents.
    pub multi_event_tx: mpsc::Sender<(SessionId, AgentEvent)>,
    /// Which entry is highlighted in the sidebar (may differ from active_id).
    pub list_selected: usize,
}

impl SessionManager {
    /// Create a new `SessionManager` with a single blank active session.
    pub fn new() -> (Self, SessionEntry) {
        let (multi_tx, multi_rx) = mpsc::channel::<(SessionId, AgentEvent)>(512);
        let initial = SessionEntry::new_blank("New chat");
        let active_id = initial.id.clone();

        let mgr = Self {
            entries: HashMap::new(),
            display_order: vec![active_id.clone()],
            active_id,
            multi_event_rx: multi_rx,
            multi_event_tx: multi_tx,
            list_selected: 0,
        };
        (mgr, initial)
    }

    /// Register an entry in the manager (used when the entry is first created or loaded).
    pub fn register(&mut self, entry: SessionEntry) {
        let id = entry.id.clone();
        if !self.display_order.contains(&id) {
            self.display_order.insert(0, id.clone());
        }
        self.entries.insert(id, entry);
    }

    /// Create a new blank session, register it, and return its ID.
    pub fn create_session(&mut self, title: impl Into<String>) -> SessionId {
        let entry = SessionEntry::new_blank(title);
        let id = entry.id.clone();
        self.display_order.insert(0, id.clone());
        self.entries.insert(id.clone(), entry);
        id
    }

    /// Load sessions from disk into the manager (without making any active).
    ///
    /// Sessions are inserted at the end of the display order (older entries
    /// pushed down), sorted by updated_at descending.
    pub fn load_from_disk(&mut self) {
        let mut entries = match sven_input::list_chats(Some(50)) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("failed to list chats from disk: {e}");
                return;
            }
        };
        // Sort newest first; already sorted by list_chats but re-sort to be safe.
        entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        for chat_entry in entries {
            let id = chat_entry.id.clone();
            // Don't overwrite the active session or any already-registered entry.
            if self.entries.contains_key(&id) {
                continue;
            }
            let session_entry = SessionEntry {
                id: id.clone(),
                title: chat_entry.title,
                status: chat_entry.status,
                yaml_path: Some(chat_entry.path),
                created_at: chat_entry.updated_at, // best available approximation
                updated_at: chat_entry.updated_at,
                stored_chat: None, // lazy-loaded when activated
                stored_input_buffer: None,
                stored_input_cursor: None,
                stored_input_attachments: None,
                stored_queue: None,
                session_state: None,
                jsonl_path: None,
                agent_tx: None,
                agent_cancel: Arc::new(Mutex::new(None)),
                busy: false,
                current_tool: None,
                context_pct: 0,
                total_context_tokens: 0,
                total_context_pct: 0,
                total_output_tokens: 0,
                cache_hit_pct: 0,
            };
            self.display_order.push(id.clone());
            self.entries.insert(id, session_entry);
        }
    }

    /// Get an immutable reference to a session entry.
    pub fn get(&self, id: &SessionId) -> Option<&SessionEntry> {
        self.entries.get(id)
    }

    /// Get a mutable reference to a session entry.
    pub fn get_mut(&mut self, id: &SessionId) -> Option<&mut SessionEntry> {
        self.entries.get_mut(id)
    }

    /// True if any background session's agent task is currently busy.
    pub fn any_background_busy(&self) -> bool {
        self.entries
            .values()
            .any(|e| e.id != self.active_id && e.busy)
    }

    /// Select the previous (older) entry in the sidebar.
    pub fn select_prev(&mut self) {
        if self.list_selected > 0 {
            self.list_selected -= 1;
        }
    }

    /// Select the next (newer) entry in the sidebar.
    pub fn select_next(&mut self) {
        if !self.display_order.is_empty() && self.list_selected < self.display_order.len() - 1 {
            self.list_selected += 1;
        }
    }

    /// Set `list_selected` to the index of the active session in the sidebar.
    pub fn sync_list_selection_to_active(&mut self) {
        if let Some(idx) = self
            .display_order
            .iter()
            .position(|id| id == &self.active_id)
        {
            self.list_selected = idx;
        }
    }

    /// Move the given session to the top of the display order (after activation).
    pub fn promote_to_top(&mut self, id: &SessionId) {
        self.display_order.retain(|x| x != id);
        self.display_order.insert(0, id.clone());
        self.sync_list_selection_to_active();
    }

    /// Mark a session as archived (but keep it in memory).
    pub fn archive(&mut self, id: &SessionId) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.status = ChatStatus::Archived;
        }
    }

    /// Remove a session from the manager and delete its YAML file.
    pub fn delete(&mut self, id: &SessionId) -> bool {
        if *id == self.active_id {
            return false; // can't delete the active session
        }
        if let Some(entry) = self.entries.remove(id) {
            self.display_order.retain(|x| x != id);
            // Clamp list_selected after removal.
            if self.list_selected >= self.display_order.len() && !self.display_order.is_empty() {
                self.list_selected = self.display_order.len() - 1;
            }
            // Delete the YAML file.
            if let Some(path) = entry.yaml_path {
                if let Err(e) = std::fs::remove_file(&path) {
                    tracing::warn!(path = %path.display(), "failed to delete chat file: {e}");
                }
            }
            true
        } else {
            false
        }
    }

    /// Update the title of a session.
    pub fn set_title(&mut self, id: &SessionId, title: String) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.title = title;
            entry.updated_at = Utc::now();
        }
    }
}
