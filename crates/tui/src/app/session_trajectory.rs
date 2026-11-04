// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A session entry as a saved trajectory, and back: what the TUI writes for
//! a session and what it reads when the session is opened again.

use std::path::PathBuf;
use std::sync::Arc;

use atif::Trajectory;
use chrono::Utc;
use sven_session_store::{ChatUsage, SessionId, SvenSessionMeta};
use tokio::sync::Mutex;

use crate::app::{chat_state::ChatState, session_manager::SessionEntry};

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
            activity: None,
            goal: sven_session_store::read_goal(trajectory),
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
            .segments()
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
                    ..
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
        sven_session_store::write_goal(&mut trajectory, self.goal.as_ref());
        trajectory
    }
}
