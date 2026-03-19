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
use sven_input::{ChatDocument, ChatEntry, ChatStatus, ChatUsage, SessionId};
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
///
/// Sessions can form a tree: root sessions have `parent_id: None` and appear
/// at the top level in the Chats sidebar; subagent task sessions have
/// `parent_id: Some(parent)` and are shown as children under that parent.
pub(crate) struct SessionEntry {
    // ── Identity & metadata ───────────────────────────────────────────────────
    pub id: SessionId,
    /// Parent session ID when this is a subagent task conversation; `None` for roots.
    pub parent_id: Option<SessionId>,
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

    // ── Subagent buffer handle ────────────────────────────────────────────────
    /// Output buffer handle for subagent sessions (e.g. "buf_0001").
    /// Used to populate the chat view when switching to this subagent session.
    pub buffer_handle: Option<String>,
    /// The full prompt sent to this subagent; displayed as the first user message.
    pub initial_prompt: Option<String>,

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
    /// Cumulative cost in USD from API responses (e.g. OpenRouter usage.cost).
    pub total_cost_usd: f64,
    /// Cache-hit rate for the last turn (0-100 %).
    pub cache_hit_pct: u8,
}

impl SessionEntry {
    /// Create a new `SessionEntry` from a `ChatDocument`, keeping the document's
    /// original `SessionId` so the file path derived from it stays consistent.
    pub fn from_document(doc: &ChatDocument) -> Self {
        let (total_input_tokens, total_output_tokens, total_cost_usd) = doc
            .usage
            .as_ref()
            .map(|u| {
                (
                    u.total_input_tokens,
                    u.total_output_tokens,
                    u.total_cost_usd,
                )
            })
            .unwrap_or((0, 0, 0.0));
        Self {
            id: doc.id.clone(),
            parent_id: None,
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
            buffer_handle: None,
            initial_prompt: None,
            agent_tx: None,
            agent_cancel: Arc::new(Mutex::new(None)),
            busy: false,
            current_tool: None,
            context_pct: 0,
            total_context_tokens: total_input_tokens as u32,
            total_context_pct: 0,
            total_output_tokens: total_output_tokens as u32,
            total_cost_usd,
            cache_hit_pct: 0,
        }
    }

    /// Restore metadata from a `ChatDocument` into a pre-existing entry,
    /// keeping the supplied `id` (so the session manager's active_id reference
    /// stays valid).  Used when continuing a loaded chat in the TUI.
    pub fn from_document_into(doc: &ChatDocument, id: SessionId) -> Self {
        let (total_input_tokens, total_output_tokens, total_cost_usd) = doc
            .usage
            .as_ref()
            .map(|u| {
                (
                    u.total_input_tokens,
                    u.total_output_tokens,
                    u.total_cost_usd,
                )
            })
            .unwrap_or((0, 0, 0.0));
        Self {
            id,
            parent_id: None,
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
            buffer_handle: None,
            initial_prompt: None,
            agent_tx: None,
            agent_cancel: Arc::new(Mutex::new(None)),
            busy: false,
            current_tool: None,
            context_pct: 0,
            total_context_tokens: total_input_tokens as u32,
            total_context_pct: 0,
            total_output_tokens: total_output_tokens as u32,
            total_cost_usd,
            cache_hit_pct: 0,
        }
    }

    /// Create a new blank session entry (not yet backed by a file).
    pub fn new_blank(title: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: SessionId::new(),
            parent_id: None,
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
            buffer_handle: None,
            initial_prompt: None,
            agent_tx: None,
            agent_cancel: Arc::new(Mutex::new(None)),
            busy: false,
            current_tool: None,
            context_pct: 0,
            total_context_tokens: 0,
            total_context_pct: 0,
            total_output_tokens: 0,
            total_cost_usd: 0.0,
            cache_hit_pct: 0,
        }
    }

    /// Create a new session entry for a subagent task (child of another session).
    pub fn new_subagent(
        title: impl Into<String>,
        parent_id: SessionId,
        buffer_handle: Option<String>,
        prompt: String,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: SessionId::new(),
            parent_id: Some(parent_id),
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
            buffer_handle,
            initial_prompt: Some(prompt),
            agent_tx: None,
            agent_cancel: Arc::new(Mutex::new(None)),
            busy: false,
            current_tool: None,
            context_pct: 0,
            total_context_tokens: 0,
            total_context_pct: 0,
            total_output_tokens: 0,
            total_cost_usd: 0.0,
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

        let usage = {
            let u = ChatUsage {
                total_input_tokens: self.total_context_tokens as u64,
                total_output_tokens: self.total_output_tokens as u64,
                total_cache_read_tokens: 0,
                total_cache_write_tokens: 0,
                total_cost_usd: self.total_cost_usd,
            };
            if u.is_empty() {
                None
            } else {
                Some(u)
            }
        };
        ChatDocument {
            id: self.id.clone(),
            title: self.title.clone(),
            model,
            mode,
            status: self.status,
            created_at: self.created_at,
            updated_at: Utc::now(),
            parent_id: self.parent_id.clone(),
            usage,
            turns,
        }
    }

    /// Apply a background agent event to this entry's stored state.
    ///
    /// Segment-producing events (ToolCallFinished, DelegateSummary, etc.) are
    /// pushed to this session's `stored_chat` so that when the user switches
    /// back, the conversation is complete. This ensures tool results and other
    /// content are never shown in the wrong chat view — they are always stored
    /// on the session that originated the event.
    pub fn apply_background_event(&mut self, event: &AgentEvent) {
        use sven_core::AgentEvent as Ev;
        use sven_model::MessageContent;

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
            Ev::ToolCallFinished {
                call_id,
                tool_name,
                output,
                is_error,
                ..
            } => {
                if self.current_tool.as_deref() == Some(tool_name.as_str()) {
                    self.current_tool = None;
                }
                if let Some(chat) = &mut self.stored_chat {
                    let output_with_error = if *is_error {
                        format!("error: {output}")
                    } else {
                        output.clone()
                    };
                    let result_seg = crate::chat::segment::ChatSegment::Message(
                        sven_model::Message::tool_result(call_id, &output_with_error),
                    );
                    let insert_pos = chat
                        .segments
                        .iter()
                        .rposition(|seg| {
                            if let crate::chat::segment::ChatSegment::Message(m) = seg {
                                if let MessageContent::ToolCall { tool_call_id, .. } = &m.content {
                                    return tool_call_id == call_id;
                                }
                            }
                            false
                        })
                        .map(|call_idx| call_idx + 1);
                    if let Some(pos) = insert_pos {
                        let shifted: std::collections::HashMap<usize, u8> = chat
                            .expand_level
                            .drain()
                            .map(|(i, v)| (if i >= pos { i + 1 } else { i }, v))
                            .collect();
                        chat.expand_level = shifted;
                        chat.segments.insert(pos, result_seg);
                    } else {
                        chat.segments.push(result_seg);
                    }
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
            Ev::Error(msg) => {
                self.busy = false;
                self.current_tool = None;
                if let Some(chat) = &mut self.stored_chat {
                    chat.segments
                        .push(crate::chat::segment::ChatSegment::Error(msg.clone()));
                }
            }
            Ev::TokenUsage {
                input,
                output,
                cache_read,
                cache_write,
                max_tokens,
                max_output_tokens,
                cost_usd,
                ..
            } => {
                if *max_tokens > 0 {
                    let input_budget = max_tokens.saturating_sub(*max_output_tokens);
                    let prompt = *input + *cache_read + *cache_write;
                    self.context_pct =
                        ((prompt as f64 / input_budget as f64) * 100.0).clamp(0.0, 100.0) as u8;
                }
                if *output > 0 {
                    self.total_output_tokens = self.total_output_tokens.saturating_add(*output);
                }
                if let Some(c) = cost_usd {
                    self.total_cost_usd += c;
                }
            }
            Ev::ContextCompacted {
                tokens_before,
                tokens_after,
                strategy,
                turn,
            } => {
                if let Some(chat) = &mut self.stored_chat {
                    chat.segments
                        .push(crate::chat::segment::ChatSegment::ContextCompacted {
                            tokens_before: *tokens_before,
                            tokens_after: *tokens_after,
                            strategy: strategy.clone(),
                            turn: *turn,
                        });
                }
            }
            Ev::ThinkingComplete(content) => {
                if let Some(chat) = &mut self.stored_chat {
                    chat.segments
                        .push(crate::chat::segment::ChatSegment::Thinking {
                            content: content.clone(),
                        });
                }
            }
            Ev::CollabEvent(ev) => {
                if let Some(chat) = &mut self.stored_chat {
                    chat.segments
                        .push(crate::chat::segment::ChatSegment::CollabEvent(ev.clone()));
                }
            }
            Ev::DelegateSummary {
                to_name,
                task_title,
                duration_ms,
                status,
                result_preview,
            } => {
                if let Some(chat) = &mut self.stored_chat {
                    chat.segments
                        .push(crate::chat::segment::ChatSegment::DelegateSummary {
                            to_name: to_name.clone(),
                            task_title: task_title.clone(),
                            duration_ms: *duration_ms,
                            status: status.clone(),
                            result_preview: result_preview.clone(),
                            expanded: false,
                            inner: vec![],
                        });
                }
            }
            // Display-only or metadata-only: no stored segment.
            _ => {}
        }
    }
}

// ── SessionManager ────────────────────────────────────────────────────────────

/// TUI multi-session UI state — the **session manager** that owns the set of active
/// [`sven_core::Session`]s and tracks which one is focused in the sidebar.
///
/// # Layering note
///
/// | Type | Crate | Role |
/// |------|-------|------|
/// | [`SessionManager`] | `sven-tui` | **TUI UI state** — tree of active sessions with sidebar selection and agent-event multiplexing. |
/// | `sven_core::Session` | `sven-core` | **Runtime state** — one live agent session with mutable message history and token accounting. |
/// | `ConversationFile` | `sven-input` | **Persisted format** — a parsed `.md`/`.jsonl` snapshot used to seed session history on load. |
///
/// The sidebar is a tree: roots are in `display_order`; children are in
/// `children`. Use [`SessionManager::tree_rows`] to get a flat list for
/// rendering and keyboard navigation.
pub(crate) struct SessionManager {
    /// All session entries (active + background).
    pub entries: HashMap<SessionId, SessionEntry>,
    /// Display order for the sidebar — root session IDs only (most recent first).
    pub display_order: Vec<SessionId>,
    /// Child session IDs per parent (order = creation order).
    pub children: HashMap<SessionId, Vec<SessionId>>,
    /// The session that owns `App.chat` and `App.agent`.
    pub active_id: SessionId,
    /// Shared receiver for events from all agent tasks (tagged with session IDs).
    pub multi_event_rx: mpsc::Receiver<(SessionId, AgentEvent)>,
    /// Shared sender — cloned into forwarding tasks when spawning agents.
    pub multi_event_tx: mpsc::Sender<(SessionId, AgentEvent)>,
    /// Which row is highlighted in the sidebar (index into tree_rows(); may differ from active_id).
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
            children: HashMap::new(),
            active_id,
            multi_event_rx: multi_rx,
            multi_event_tx: multi_tx,
            list_selected: 0,
        };
        (mgr, initial)
    }

    /// Flat list of (session_id, depth) for sidebar: roots first (depth 0), then
    /// each root’s children (depth 1). Used for rendering and list_selected index.
    pub fn tree_rows(&self) -> Vec<(SessionId, u16)> {
        let mut rows = Vec::new();
        for root_id in &self.display_order {
            if self.entries.contains_key(root_id) {
                rows.push((root_id.clone(), 0));
                if let Some(ids) = self.children.get(root_id) {
                    for child_id in ids {
                        if self.entries.contains_key(child_id) {
                            rows.push((child_id.clone(), 1));
                        }
                    }
                }
            }
        }
        rows
    }

    /// Register an entry in the manager (used when the entry is first created or loaded).
    /// Root entries (parent_id None) are added to display_order; child entries are not.
    pub fn register(&mut self, entry: SessionEntry) {
        let id = entry.id.clone();
        let parent_id = entry.parent_id.clone();
        if let Some(pid) = &parent_id {
            self.children
                .entry(pid.clone())
                .or_default()
                .push(id.clone());
        } else if !self.display_order.contains(&id) {
            self.display_order.insert(0, id.clone());
        }
        self.entries.insert(id, entry);
    }

    /// Add a child session under the given parent (e.g. subagent task). Does not
    /// add the child to display_order.
    pub fn add_child_session(&mut self, parent_id: SessionId, entry: SessionEntry) {
        let id = entry.id.clone();
        self.children.entry(parent_id).or_default().push(id.clone());
        self.entries.insert(id, entry);
    }

    /// Create a new blank session, register it as a root, and return its ID.
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
    /// pushed down), sorted by updated_at descending. Subagent sessions
    /// (with parent_id) are restored as children under their parent.
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

        // Separate roots and children; register roots first so parents exist
        // when we add children. Orphan children (parent not loaded) become roots.
        let (roots, children): (Vec<_>, Vec<_>) =
            entries.into_iter().partition(|e| e.parent_id.is_none());

        for chat_entry in roots.into_iter().rev() {
            let id = chat_entry.id.clone();
            if self.entries.contains_key(&id) {
                continue;
            }
            let session_entry = SessionEntry {
                id: id.clone(),
                parent_id: None,
                title: chat_entry.title,
                status: chat_entry.status,
                yaml_path: Some(chat_entry.path),
                created_at: chat_entry.updated_at,
                updated_at: chat_entry.updated_at,
                stored_chat: None,
                stored_input_buffer: None,
                stored_input_cursor: None,
                stored_input_attachments: None,
                stored_queue: None,
                session_state: None,
                jsonl_path: None,
                buffer_handle: None,
                initial_prompt: None,
                agent_tx: None,
                agent_cancel: Arc::new(Mutex::new(None)),
                busy: false,
                current_tool: None,
                context_pct: 0,
                total_context_tokens: 0,
                total_context_pct: 0,
                total_output_tokens: 0,
                total_cost_usd: 0.0,
                cache_hit_pct: 0,
            };
            self.register(session_entry);
        }

        // Register children in topological order so each parent exists before its child.
        // Iterate until all are registered; orphan children (parent not loaded) become roots.
        let mut pending: Vec<ChatEntry> = children;
        let mut prev_len = usize::MAX;
        while prev_len != pending.len() {
            prev_len = pending.len();
            let mut remaining = Vec::new();
            for chat_entry in pending {
                let id = chat_entry.id.clone();
                if self.entries.contains_key(&id) {
                    continue;
                }
                let parent_id = chat_entry
                    .parent_id
                    .as_ref()
                    .filter(|pid| self.entries.contains_key(pid))
                    .cloned();

                if parent_id.is_some() {
                    let session_entry = SessionEntry {
                        id: id.clone(),
                        parent_id: parent_id.clone(),
                        title: chat_entry.title,
                        status: chat_entry.status,
                        yaml_path: Some(chat_entry.path),
                        created_at: chat_entry.updated_at,
                        updated_at: chat_entry.updated_at,
                        stored_chat: None,
                        stored_input_buffer: None,
                        stored_input_cursor: None,
                        stored_input_attachments: None,
                        stored_queue: None,
                        session_state: None,
                        jsonl_path: None,
                        buffer_handle: None,
                        initial_prompt: None,
                        agent_tx: None,
                        agent_cancel: Arc::new(Mutex::new(None)),
                        busy: false,
                        current_tool: None,
                        context_pct: 0,
                        total_context_tokens: 0,
                        total_context_pct: 0,
                        total_output_tokens: 0,
                        total_cost_usd: 0.0,
                        cache_hit_pct: 0,
                    };
                    self.register(session_entry);
                } else {
                    remaining.push(chat_entry);
                }
            }
            pending = remaining;
        }
        // Remaining orphans: parent not in loaded set; register as roots.
        for chat_entry in pending {
            let id = chat_entry.id.clone();
            if self.entries.contains_key(&id) {
                continue;
            }
            let session_entry = SessionEntry {
                id: id.clone(),
                parent_id: None,
                title: chat_entry.title,
                status: chat_entry.status,
                yaml_path: Some(chat_entry.path),
                created_at: chat_entry.updated_at,
                updated_at: chat_entry.updated_at,
                stored_chat: None,
                stored_input_buffer: None,
                stored_input_cursor: None,
                stored_input_attachments: None,
                stored_queue: None,
                session_state: None,
                jsonl_path: None,
                buffer_handle: None,
                initial_prompt: None,
                agent_tx: None,
                agent_cancel: Arc::new(Mutex::new(None)),
                busy: false,
                current_tool: None,
                context_pct: 0,
                total_context_tokens: 0,
                total_context_pct: 0,
                total_output_tokens: 0,
                total_cost_usd: 0.0,
                cache_hit_pct: 0,
            };
            self.register(session_entry);
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

    /// Total cost in USD for the given session including all subagent descendants.
    pub fn total_cost_including_children(&self, id: &SessionId) -> f64 {
        let mut total = self
            .entries
            .get(id)
            .map(|e| e.total_cost_usd)
            .unwrap_or(0.0);
        if let Some(child_ids) = self.children.get(id) {
            for cid in child_ids {
                total += self.total_cost_including_children(cid);
            }
        }
        total
    }

    /// True if any background session's agent task is currently busy.
    pub fn any_background_busy(&self) -> bool {
        self.entries
            .values()
            .any(|e| e.id != self.active_id && e.busy)
    }

    /// Select the previous row in the sidebar (tree order).
    pub fn select_prev(&mut self) {
        if self.list_selected > 0 {
            self.list_selected -= 1;
        }
    }

    /// Select the next row in the sidebar (tree order).
    pub fn select_next(&mut self) {
        let rows = self.tree_rows();
        if !rows.is_empty() && self.list_selected < rows.len() - 1 {
            self.list_selected += 1;
        }
    }

    /// Set `list_selected` to the index of the active session in the sidebar.
    pub fn sync_list_selection_to_active(&mut self) {
        let rows = self.tree_rows();
        if let Some(idx) = rows.iter().position(|(id, _)| id == &self.active_id) {
            self.list_selected = idx;
        }
    }

    /// Move the given session to the top of the display order (after activation).
    /// Only affects roots; children stay under their parent.
    pub fn promote_to_top(&mut self, id: &SessionId) {
        if self
            .entries
            .get(id)
            .and_then(|e| e.parent_id.as_ref())
            .is_none()
        {
            self.display_order.retain(|x| x != id);
            self.display_order.insert(0, id.clone());
        }
        self.sync_list_selection_to_active();
    }

    /// Mark a session as archived (but keep it in memory).
    pub fn archive(&mut self, id: &SessionId) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.status = ChatStatus::Archived;
        }
    }

    /// Remove a session from the manager and delete its YAML file.
    /// If the session has children, they are removed too (but they have no YAML).
    pub fn delete(&mut self, id: &SessionId) -> bool {
        if *id == self.active_id {
            return false; // can't delete the active session
        }
        if let Some(entry) = self.entries.remove(id) {
            if entry.parent_id.is_some() {
                if let Some(pid) = &entry.parent_id {
                    if let Some(sibs) = self.children.get_mut(pid) {
                        sibs.retain(|x| x != id);
                    }
                }
            } else {
                self.display_order.retain(|x| x != id);
            }
            // Remove any children (collect first so we don't hold refs during delete).
            let child_ids: Vec<SessionId> = self.children.remove(id).unwrap_or_default();
            for cid in child_ids {
                let _ = self.delete(&cid);
            }
            let rows = self.tree_rows();
            if !rows.is_empty() && self.list_selected >= rows.len() {
                self.list_selected = rows.len() - 1;
            }
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

    /// Find the first session entry whose `buffer_handle` matches `handle`.
    /// Used to route `SubagentEvent` updates to the correct child session.
    pub fn find_by_buffer_handle(&mut self, handle: &str) -> Option<&mut SessionEntry> {
        self.entries
            .values_mut()
            .find(|e| e.buffer_handle.as_deref() == Some(handle))
    }

    /// Update the title of a session.
    pub fn set_title(&mut self, id: &SessionId, title: String) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.title = title;
            entry.updated_at = Utc::now();
        }
    }
}
