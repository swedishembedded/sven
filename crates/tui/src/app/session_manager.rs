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

use atif::Trajectory;
use chrono::{DateTime, Utc};
use sven_machines::AgentEvent;
use sven_session_store::{ChatStatus, ChatUsage, SessionId, SvenSessionMeta, UnifiedSessionEntry};
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
    /// Path to the ATIF `.json` trajectory file backing this session (either
    /// the canonical `session_dir()/<id>.json` path or an explicit
    /// `--trace`/`--output-trace` path). `None` for a session that has never
    /// been saved yet.
    pub session_path: Option<PathBuf>,
    /// `true` iff this entry is currently backed by a legacy YAML chat file
    /// (`legacy_path`) that has not yet been re-saved in the new ATIF format.
    /// The next save clears this and writes a fresh `.json` file instead,
    /// leaving the old `.yaml` file untouched on disk (superseded, not deleted).
    pub is_legacy: bool,
    /// The original legacy `.yaml` path, kept only for reference/deletion
    /// while `is_legacy` is true; never written to again.
    pub legacy_path: Option<PathBuf>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,

    // ── Stored chat state (populated when session is inactive) ────────────────
    /// Stored chat segments for inactive sessions (active session uses `App.chat`).
    pub stored_chat: Option<ChatState>,

    /// Steps of the loaded trajectory that the chat view omits
    /// (`is_copied_context == Some(true)`, e.g. context carried over from a
    /// continued trajectory). [`Self::to_trajectory`] prepends them so an
    /// open→save round trip never deletes them from the file.
    pub copied_context_steps: Vec<atif::TraceStep>,

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
    /// usable input budget (`sven_model::budget::effective_input_budget`).
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
    /// Restore metadata from a `Trajectory` into a pre-existing entry, keeping
    /// the supplied `id` (so the session manager's `active_id` reference
    /// stays valid) and file-backing info. Used when continuing a loaded
    /// session in the TUI, both native (`is_legacy: false`) and freshly
    /// imported from a legacy YAML chat (`is_legacy: true`).
    pub fn from_trajectory_into(
        trajectory: &Trajectory,
        id: SessionId,
        session_path: Option<PathBuf>,
        is_legacy: bool,
    ) -> Self {
        let meta = SvenSessionMeta::from_trajectory(trajectory);
        let usage = trajectory
            .final_metrics
            .as_ref()
            .map(sven_session_store::final_metrics_to_chat_usage);
        let (total_input_tokens, total_output_tokens, total_cost_usd) = usage
            .map(|u| {
                (
                    u.total_input_tokens,
                    u.total_output_tokens,
                    u.total_cost_usd,
                )
            })
            .unwrap_or((0, 0, 0.0));
        let now = Utc::now();
        Self {
            id,
            parent_id: meta
                .as_ref()
                .and_then(|m| m.parent_session_id.clone())
                .map(SessionId::from_string),
            title: meta
                .as_ref()
                .map(|m| m.title.clone())
                .unwrap_or_else(|| "Untitled".to_string()),
            status: meta.as_ref().map(|m| m.status).unwrap_or_default(),
            session_path: if is_legacy {
                None
            } else {
                session_path.clone()
            },
            is_legacy,
            legacy_path: if is_legacy { session_path } else { None },
            created_at: meta.as_ref().map(|m| m.created_at).unwrap_or(now),
            updated_at: meta.as_ref().map(|m| m.updated_at).unwrap_or(now),
            stored_chat: None,
            copied_context_steps: sven_session_store::copied_context_steps(trajectory),
            stored_input_buffer: None,
            stored_input_cursor: None,
            stored_input_attachments: None,
            stored_queue: None,
            session_state: None,
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
            session_path: None,
            is_legacy: false,
            legacy_path: None,
            created_at: now,
            updated_at: now,
            stored_chat: None,
            copied_context_steps: Vec::new(),
            stored_input_buffer: None,
            stored_input_cursor: None,
            stored_input_attachments: None,
            stored_queue: None,
            session_state: None,
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
            session_path: None,
            is_legacy: false,
            legacy_path: None,
            created_at: now,
            updated_at: now,
            stored_chat: None,
            copied_context_steps: Vec::new(),
            stored_input_buffer: None,
            stored_input_cursor: None,
            stored_input_attachments: None,
            stored_queue: None,
            session_state: None,
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

    /// Build an ATIF [`Trajectory`] from this entry, the supplied chat state,
    /// and runtime display metadata.  The entry's `created_at` is preserved
    /// so repeated saves don't reset the trajectory's creation timestamp.
    pub fn to_trajectory(
        &self,
        chat: &ChatState,
        model: Option<String>,
        mode: Option<String>,
    ) -> Trajectory {
        use sven_model::Role;
        use sven_session_store::ConversationRecord;

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

        // Carry the copied-context steps the chat view omitted, or an
        // open→save round trip would permanently delete them.
        let steps = sven_session_store::conversation_records_to_steps_with_copied_context(
            &self.copied_context_steps,
            &records,
        );

        let mut agent = sven_session_store::default_agent_profile();
        if let Some(m) = &model {
            agent = agent.with_model(m.clone());
        }
        let mut trajectory = Trajectory::new(sven_session_store::ATIF_SCHEMA_VERSION, agent);
        trajectory.session_id = Some(self.id.as_str().to_string());
        trajectory.steps = steps;

        let usage = ChatUsage {
            total_input_tokens: self.total_context_tokens as u64,
            total_output_tokens: self.total_output_tokens as u64,
            total_cache_read_tokens: 0,
            total_cache_write_tokens: 0,
            total_cost_usd: self.total_cost_usd,
        };
        if !usage.is_empty() {
            trajectory.final_metrics =
                Some(sven_session_store::chat_usage_to_final_metrics(&usage));
        }

        let meta = SvenSessionMeta {
            title: self.title.clone(),
            status: self.status,
            mode,
            parent_session_id: self.parent_id.as_ref().map(|p| p.as_str().to_string()),
            created_at: self.created_at,
            updated_at: Utc::now(),
        };
        meta.apply_to_trajectory(&mut trajectory);
        trajectory
    }

    /// Apply a background agent event to this entry's stored state.
    ///
    /// Segment-producing events (ToolCallFinished, DelegateSummary, etc.) are
    /// pushed to this session's `stored_chat` so that when the user switches
    /// back, the conversation is complete. This ensures tool results and other
    /// content are never shown in the wrong chat view - they are always stored
    /// on the session that originated the event.
    pub fn apply_background_event(&mut self, event: &AgentEvent) {
        use sven_machines::AgentEvent as Ev;

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
                    let insert_pos =
                        crate::chat::segment::tool_result_insert_position(&chat.segments, call_id);
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
                    // Same formula the request gate actually enforces (see
                    // sven_model::budget) rather than a locally
                    // reimplemented, driftable copy of it.
                    if let Some(input_budget) = sven_model::budget::effective_input_budget(
                        Some(*max_tokens as u32),
                        (*max_output_tokens > 0).then_some(*max_output_tokens as u32),
                    ) {
                        let prompt = *input + *cache_read + *cache_write;
                        self.context_pct =
                            ((prompt as f64 / input_budget as f64) * 100.0).clamp(0.0, 100.0) as u8;
                    }
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

/// Build a [`SessionEntry`] from a listing row ([`UnifiedSessionEntry`]),
/// with no stored chat/agent state yet (populated lazily on first switch-to).
fn session_entry_from_unified(
    entry: UnifiedSessionEntry,
    parent_id: Option<SessionId>,
) -> SessionEntry {
    let id = SessionId::from_string(entry.session_id);
    SessionEntry {
        id,
        parent_id,
        title: entry.title,
        status: entry.status,
        session_path: if entry.is_legacy {
            None
        } else {
            Some(entry.path.clone())
        },
        is_legacy: entry.is_legacy,
        legacy_path: if entry.is_legacy {
            Some(entry.path)
        } else {
            None
        },
        created_at: entry.updated_at,
        updated_at: entry.updated_at,
        stored_chat: None,
        // Populated when the trajectory is actually loaded on switch-to.
        copied_context_steps: Vec::new(),
        stored_input_buffer: None,
        stored_input_cursor: None,
        stored_input_attachments: None,
        stored_queue: None,
        session_state: None,
        buffer_handle: None,
        initial_prompt: None,
        agent_tx: None,
        agent_cancel: Arc::new(Mutex::new(None)),
        busy: false,
        current_tool: None,
        context_pct: 0,
        total_context_tokens: entry
            .usage
            .as_ref()
            .map(|u| u.total_input_tokens as u32)
            .unwrap_or(0),
        total_context_pct: 0,
        total_output_tokens: entry
            .usage
            .as_ref()
            .map(|u| u.total_output_tokens as u32)
            .unwrap_or(0),
        total_cost_usd: entry
            .usage
            .as_ref()
            .map(|u| u.total_cost_usd)
            .unwrap_or(0.0),
        cache_hit_pct: 0,
    }
}

// ── SessionManager ────────────────────────────────────────────────────────────

/// TUI multi-session UI state - the **session manager** that owns the set of
/// [`SessionEntry`]s and tracks which one is focused in the sidebar.
///
/// # Layering note
///
/// | Type | Crate | Role |
/// |------|-------|------|
/// | [`SessionManager`] | `sven-tui` | **TUI UI state** - tree of active sessions with sidebar selection and agent-event multiplexing. |
/// | `KernelAgentSession` | `sven-bootstrap` | **Runtime state** - one fully-wired kernel session presented as an `AgentEvent` stream. |
/// | `atif::Trajectory` | `atif` (via `sven-session-store`) | **Persisted format** - the ATIF `.json` file backing each entry (`SessionEntry::session_path`). |
///
/// The sidebar is a tree: roots are in `display_order`; children are in
/// `children`. Use [`SessionManager::tree_rows`] to get a flat list for
/// rendering and keyboard navigation.
pub(crate) struct SessionManager {
    /// All session entries (active + background).
    pub entries: HashMap<SessionId, SessionEntry>,
    /// Display order for the sidebar - root session IDs only (most recent first).
    pub display_order: Vec<SessionId>,
    /// Child session IDs per parent (order = creation order).
    pub children: HashMap<SessionId, Vec<SessionId>>,
    /// The session that owns `App.chat` and `App.agent`.
    pub active_id: SessionId,
    /// Shared receiver for events from all agent tasks (tagged with session IDs).
    pub multi_event_rx: mpsc::Receiver<(SessionId, AgentEvent)>,
    /// Shared sender - cloned into forwarding tasks when spawning agents.
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
    /// each root's children (depth 1). Used for rendering and list_selected index.
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

    /// Ensure a session named by a [`UnifiedSessionEntry`] (e.g. one picked
    /// from `/resume`, which may be older than the 50 most recent sessions
    /// [`Self::load_from_disk`] preloads) is present in this manager,
    /// registering it as a root if it is not already known.
    ///
    /// Idempotent by construction: unlike calling [`Self::register`] directly,
    /// calling this twice for the same session id is a no-op on the second
    /// call. That matters because `register` does not itself dedupe -
    /// re-registering an existing child would push a duplicate id into
    /// `children[parent_id]`, and `tree_rows()` would then render it twice.
    ///
    /// Callers should call this (or otherwise confirm the id is already
    /// registered) before [`crate::App::switch_session`] - switching to an
    /// unknown id is a deliberate no-op there, not a session restore.
    pub fn ensure_registered(&mut self, unified: UnifiedSessionEntry) -> SessionId {
        let id = SessionId::from_string(unified.session_id.clone());
        if !self.entries.contains_key(&id) {
            self.register(session_entry_from_unified(unified, None));
        }
        id
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
    ///
    /// Lists BOTH native ATIF sessions and legacy YAML chats (tagged
    /// `is_legacy: true`) that have not yet been superseded by a same-id
    /// `.json` file — see [`sven_session_store::list_all_sessions`]'s doc comment
    /// for the exact legacy-visibility policy. Opening a legacy entry and
    /// saving it writes a brand new `.json` file; the original `.yaml` is
    /// left untouched on disk.
    pub fn load_from_disk(&mut self) {
        let mut entries = match sven_session_store::list_all_sessions(Some(50)) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("failed to list sessions from disk: {e}");
                return;
            }
        };
        // Sort newest first; already sorted by list_all_sessions but re-sort to be safe.
        entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        // Separate roots and children; register roots first so parents exist
        // when we add children. Orphan children (parent not loaded) become roots.
        let (roots, children): (Vec<_>, Vec<_>) = entries
            .into_iter()
            .partition(|e| e.parent_session_id.is_none());

        for entry in roots.into_iter().rev() {
            let id = SessionId::from_string(entry.session_id.clone());
            if self.entries.contains_key(&id) {
                continue;
            }
            self.register(session_entry_from_unified(entry, None));
        }

        // Register children in topological order so each parent exists before its child.
        // Iterate until all are registered; orphan children (parent not loaded) become roots.
        let mut pending: Vec<UnifiedSessionEntry> = children;
        let mut prev_len = usize::MAX;
        while prev_len != pending.len() {
            prev_len = pending.len();
            let mut remaining = Vec::new();
            for entry in pending {
                let id = SessionId::from_string(entry.session_id.clone());
                if self.entries.contains_key(&id) {
                    continue;
                }
                let parent_id = entry
                    .parent_session_id
                    .as_ref()
                    .map(|pid| SessionId::from_string(pid.clone()))
                    .filter(|pid| self.entries.contains_key(pid));

                if let Some(parent_id) = parent_id {
                    self.register(session_entry_from_unified(entry, Some(parent_id)));
                } else {
                    remaining.push(entry);
                }
            }
            pending = remaining;
        }
        // Remaining orphans: parent not in loaded set; register as roots.
        for entry in pending {
            let id = SessionId::from_string(entry.session_id.clone());
            if self.entries.contains_key(&id) {
                continue;
            }
            self.register(session_entry_from_unified(entry, None));
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

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::segment::ChatSegment;
    use sven_model::{FunctionCall, Message, MessageContent, Role};

    fn chat_with_multi_turn_tool_call_and_thinking() -> ChatState {
        let mut chat = ChatState::new();
        chat.segments = vec![
            ChatSegment::Message(Message::user("What is 2+2?")),
            ChatSegment::Thinking {
                content: "I should compute it.".to_string(),
            },
            ChatSegment::Message(Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "call_1".into(),
                    function: FunctionCall {
                        name: "calc".into(),
                        arguments: r#"{"expr":"2+2"}"#.into(),
                    },
                },
            }),
            ChatSegment::Message(Message::tool_result("call_1", "4")),
            ChatSegment::Message(Message::assistant("The answer is 4.")),
            ChatSegment::Message(Message::user("Thanks!")),
            ChatSegment::Message(Message::assistant("You're welcome!")),
        ];
        chat
    }

    #[test]
    fn to_trajectory_save_load_round_trip_preserves_thinking_and_tool_calls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-abc.json");

        let mut entry = SessionEntry::new_blank("Test session");
        entry.id = SessionId::from_string("session-abc".to_string());
        let chat = chat_with_multi_turn_tool_call_and_thinking();

        let trajectory = entry.to_trajectory(
            &chat,
            Some("anthropic/claude-3-5".to_string()),
            Some("agent".to_string()),
        );
        assert_eq!(trajectory.session_id.as_deref(), Some("session-abc"));
        atif::persist::write_trajectory_atomic(&path, &trajectory, None).unwrap();

        let loaded = sven_session_store::load_session_from(&path).unwrap();
        let records = sven_session_store::steps_to_conversation_records(&loaded.steps);
        let segments: Vec<ChatSegment> = records
            .into_iter()
            .filter_map(crate::app::construct::conversation_record_to_chat_segment)
            .collect();

        // Every original message must survive: 2 user + 2 assistant text +
        // 1 tool call + 1 tool result = 6 Message segments, plus 1 Thinking.
        let thinking_count = segments
            .iter()
            .filter(|s| matches!(s, ChatSegment::Thinking { .. }))
            .count();
        assert_eq!(
            thinking_count, 1,
            "thinking must survive the round trip: {segments:?}"
        );

        let tool_call_present = segments.iter().any(|s| {
            matches!(
                s,
                ChatSegment::Message(m)
                    if matches!(&m.content, MessageContent::ToolCall { tool_call_id, .. } if tool_call_id == "call_1")
            )
        });
        assert!(tool_call_present, "tool call must survive: {segments:?}");

        let tool_result_present = segments.iter().any(|s| {
            matches!(
                s,
                ChatSegment::Message(m)
                    if matches!(&m.content, MessageContent::ToolResult { tool_call_id, .. } if tool_call_id == "call_1")
            )
        });
        assert!(
            tool_result_present,
            "tool result must survive: {segments:?}"
        );

        let user_texts: Vec<&str> = segments
            .iter()
            .filter_map(|s| match s {
                ChatSegment::Message(m) if m.role == Role::User => m.as_text(),
                _ => None,
            })
            .collect();
        assert_eq!(user_texts, vec!["What is 2+2?", "Thanks!"]);

        let assistant_texts: Vec<&str> = segments
            .iter()
            .filter_map(|s| match s {
                ChatSegment::Message(m) if m.role == Role::Assistant => m.as_text(),
                _ => None,
            })
            .collect();
        assert_eq!(assistant_texts, vec!["The answer is 4.", "You're welcome!"]);
    }

    #[test]
    fn open_then_save_preserves_copied_context_steps() {
        // Spec: copied-context steps (is_copied_context == true, e.g. from a
        // continued trajectory) are hidden from the chat view, but an
        // open→save round trip must NOT delete them from the file.
        let mut source = atif::Trajectory::new(
            sven_session_store::ATIF_SCHEMA_VERSION,
            sven_session_store::default_agent_profile(),
        );
        let mut copied = atif::TraceStep::new(1, atif::StepOrigin::User, "carried-over context");
        copied.is_copied_context = Some(true);
        source.steps = vec![
            copied,
            atif::TraceStep::new(2, atif::StepOrigin::User, "fresh question"),
        ];

        let entry = SessionEntry::from_trajectory_into(
            &source,
            SessionId::from_string("session-copied".into()),
            None,
            false,
        );

        // Rebuild the chat view exactly as switch_session does (the view
        // omits the copied step), then save.
        let records = sven_session_store::steps_to_conversation_records(&source.steps);
        let mut chat = ChatState::new();
        chat.segments = records
            .into_iter()
            .filter_map(crate::app::construct::conversation_record_to_chat_segment)
            .collect();
        let saved = entry.to_trajectory(&chat, None, None);

        let carried: Vec<_> = saved
            .steps
            .iter()
            .filter(|s| s.is_copied_context == Some(true))
            .collect();
        assert_eq!(
            carried.len(),
            1,
            "the copied-context step must survive open→save: {:?}",
            saved.steps
        );
        assert_eq!(carried[0].message.as_text(), Some("carried-over context"));
        // The fresh step must survive too, and step ids stay contiguous.
        assert!(saved
            .steps
            .iter()
            .any(|s| s.message.as_text() == Some("fresh question")));
        for (i, s) in saved.steps.iter().enumerate() {
            assert_eq!(s.step_id, (i + 1) as u64, "contiguous step ids");
        }
    }

    #[test]
    fn to_trajectory_preserves_meta_and_usage() {
        let mut entry = SessionEntry::new_blank("My title");
        entry.id = SessionId::from_string("session-meta".to_string());
        entry.status = ChatStatus::Completed;
        entry.total_context_tokens = 100;
        entry.total_output_tokens = 50;
        entry.total_cost_usd = 0.02;
        let chat = chat_with_multi_turn_tool_call_and_thinking();

        let trajectory =
            entry.to_trajectory(&chat, Some("gpt-4o".to_string()), Some("code".to_string()));
        let meta = SvenSessionMeta::from_trajectory(&trajectory).expect("meta present");
        assert_eq!(meta.title, "My title");
        assert_eq!(meta.status, ChatStatus::Completed);
        assert_eq!(meta.mode.as_deref(), Some("code"));
        assert_eq!(trajectory.agent.model_name.as_deref(), Some("gpt-4o"));

        let metrics = trajectory.final_metrics.expect("usage present");
        assert_eq!(metrics.total_prompt_tokens, Some(100));
        assert_eq!(metrics.total_completion_tokens, Some(50));
    }

    #[test]
    fn from_trajectory_into_legacy_import_produces_working_entry() {
        use sven_session_store::chat_document::{ChatDocument, TurnRecord};

        let mut doc = ChatDocument::new("Legacy chat");
        doc.model = Some("anthropic/claude-3-5".to_string());
        doc.turns = vec![
            TurnRecord::User {
                content: "Hello from legacy".to_string(),
            },
            TurnRecord::Thinking {
                content: "thinking about it".to_string(),
            },
            TurnRecord::Assistant {
                content: "Hi there!".to_string(),
            },
        ];
        let legacy_id = doc.id.clone();

        let trajectory = sven_session_store::import_legacy_chat_document(&doc);
        // Simulate what SessionManager::load_from_disk / switch_session does:
        // a legacy entry with `session_path: None`, `legacy_path: Some(..)`.
        let entry = SessionEntry::from_trajectory_into(
            &trajectory,
            legacy_id.clone(),
            Some(PathBuf::from("/chats/legacy.yaml")),
            true,
        );

        assert!(entry.is_legacy, "must be tagged legacy");
        assert!(
            entry.session_path.is_none(),
            "legacy entries have no native session_path yet"
        );
        assert_eq!(entry.legacy_path, Some(PathBuf::from("/chats/legacy.yaml")));
        assert_eq!(entry.title, "Legacy chat");
        assert_eq!(entry.id, legacy_id);

        // Opening it (steps_to_conversation_records) must yield a working
        // chat, including the thinking block the old YAML turn carried.
        let records = sven_session_store::steps_to_conversation_records(&trajectory.steps);
        let segments: Vec<ChatSegment> = records
            .into_iter()
            .filter_map(crate::app::construct::conversation_record_to_chat_segment)
            .collect();
        assert!(segments.iter().any(
            |s| matches!(s, ChatSegment::Thinking { content } if content == "thinking about it")
        ));
        assert!(segments.iter().any(
            |s| matches!(s, ChatSegment::Message(m) if m.role == Role::User && m.as_text() == Some("Hello from legacy"))
        ));

        // The NEXT save (mirroring `resolve_session_path`) must write a fresh
        // `.json`, never touch the `.yaml` again - modeled here directly since
        // `resolve_session_path` needs a live `App`.
        let fresh_path = sven_session_store::session_path(legacy_id.as_str());
        assert!(fresh_path.extension().and_then(|e| e.to_str()) == Some("json"));
        assert_ne!(fresh_path, PathBuf::from("/chats/legacy.yaml"));
    }

    fn unified_entry(session_id: &str) -> UnifiedSessionEntry {
        UnifiedSessionEntry {
            session_id: session_id.to_string(),
            path: PathBuf::from(format!("/sessions/{session_id}.json")),
            title: "Some session".to_string(),
            status: ChatStatus::Active,
            parent_session_id: None,
            usage: None,
            updated_at: Utc::now(),
            is_legacy: false,
        }
    }

    #[test]
    fn ensure_registered_registers_an_unknown_session_as_root() {
        let (mut mgr, _initial) = SessionManager::new();
        let id = mgr.ensure_registered(unified_entry("picked-session"));
        assert!(mgr.entries.contains_key(&id));
        assert!(mgr.display_order.contains(&id));
    }

    #[test]
    fn ensure_registered_is_idempotent_for_an_already_known_session() {
        // Calling `register` twice for the same id would push a duplicate
        // into `display_order` were it not for the guard there; more
        // importantly, `ensure_registered` must not clobber the entry (e.g.
        // its live `stored_chat`) on a second call.
        let (mut mgr, _initial) = SessionManager::new();
        let id = mgr.ensure_registered(unified_entry("picked-session"));
        mgr.get_mut(&id).unwrap().stored_chat = Some(ChatState::new());
        let id_again = mgr.ensure_registered(unified_entry("picked-session"));
        assert_eq!(id, id_again);
        assert!(
            mgr.get(&id).unwrap().stored_chat.is_some(),
            "re-registering an already-known session must not overwrite it"
        );
        assert_eq!(
            mgr.display_order.iter().filter(|r| **r == id).count(),
            1,
            "must not be registered twice in display_order"
        );
    }
}
