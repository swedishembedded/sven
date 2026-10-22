// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The session's subagents: their session entries, the updates they stream,
//! and the activity panel between the chat and the input that shows what
//! each one is doing (`Ctrl+w a`).
//!
//! Swedish Embedded AB implements solutions for supervising parallel
//! autonomous agents for its clients. If your team needs expertise in
//! terminal interfaces for agent runtimes then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::{collections::HashMap, sync::Arc, time::Instant};

use chrono::Utc;
use sven_frontend::SubagentActivity;
use sven_model::Message;
use sven_session_store::{ChatStatus, SessionId};
use sven_tool_api::events::SubagentUpdate;
use tokio::sync::Mutex;

use crate::{
    app::{chat_state::ChatState, session_manager::SessionEntry, App, FocusPane},
    chat::segment::ChatSegment,
};

/// The open panel's height, as a share of the screen: enough to watch a
/// handful of agents while the chat stays the main view.
const MAX_SCREEN_FIFTHS: u16 = 2;

/// The activity panel's own state.
#[derive(Debug, Default)]
pub(crate) struct ActivityPanelState {
    /// Showing a row per agent rather than the one-line summary.
    pub expanded: bool,
    /// The highlighted agent, as an index into [`App::activity_agents`].
    pub selected: usize,
    /// The first agent row drawn.
    pub scroll: usize,
}

impl SessionEntry {
    /// A session entry for a subagent task, a child of `parent_id`.
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
            activity: None,
        }
    }

    /// Fold one update from this subagent, received at `now`: into its
    /// transcript, its busy state and its activity.
    pub fn apply_subagent_update(&mut self, update: &SubagentUpdate, now: Instant) {
        let prompt = self.initial_prompt.clone();
        let chat = self.stored_chat.get_or_insert_with(|| {
            // The transcript opens with what the subagent was asked.
            let mut chat = ChatState::new();
            if let Some(prompt) = prompt {
                chat.push_segment(ChatSegment::Message(Message::user(&prompt)));
            }
            chat
        });
        super::agent_events::apply_subagent_update(chat, update);
        if let Some(activity) = &mut self.activity {
            activity.apply(update, now);
        }
        match update {
            SubagentUpdate::ToolCallStarted { name, .. } => {
                self.busy = true;
                self.current_tool = Some(name.clone());
            }
            SubagentUpdate::ToolCallFinished { .. } => self.current_tool = None,
            SubagentUpdate::Finished { .. } => {
                self.busy = false;
                self.current_tool = None;
                self.status = ChatStatus::Completed;
            }
            SubagentUpdate::Failed { .. } => {
                self.busy = false;
                self.current_tool = None;
            }
            SubagentUpdate::TokenUsage { cost_usd } => self.total_cost_usd += cost_usd,
            _ => {}
        }
    }
}

impl App {
    /// The subagents the activity panel lists: those of the session being
    /// viewed - or, while one of them is viewed, of its parent - that were
    /// started in this process, oldest first.
    pub(crate) fn activity_agents(&self) -> Vec<(&SessionId, &SubagentActivity)> {
        let active = &self.sessions.active_id;
        let root = self
            .sessions
            .get(active)
            .and_then(|e| e.parent_id.as_ref())
            .unwrap_or(active);
        self.sessions
            .children
            .get(root)
            .into_iter()
            .flatten()
            .filter_map(|id| {
                let entry = self.sessions.get(id)?;
                Some((&entry.id, entry.activity.as_ref()?))
            })
            .collect()
    }

    /// The tokens each listed subagent used, by the id of the tool call that
    /// started it - for that call's row in the chat.
    pub(crate) fn agent_tokens_by_call(&self) -> HashMap<String, u64> {
        self.activity_agents()
            .into_iter()
            .map(|(_, a)| (a.call_id.clone(), a.total_tokens()))
            .collect()
    }

    /// Rows the activity panel needs on a screen `screen_height` tall: none
    /// without agents, one for the summary, or a row per agent plus its
    /// border - at most two fifths of the screen.
    pub(crate) fn activity_pane_height(&self, screen_height: u16) -> u16 {
        let agents = self.activity_agents().len();
        if agents == 0 {
            0
        } else if !self.ui.activity.expanded {
            1
        } else {
            let wanted = u16::try_from(agents).unwrap_or(u16::MAX).saturating_add(2);
            wanted.min((screen_height * MAX_SCREEN_FIFTHS / 5).max(3))
        }
    }

    /// `Ctrl+w j` / `Ctrl+w k`: move focus to the next pane down (or up)
    /// among those on screen - the chat, the activity panel, the queue
    /// panel, the input. At either end focus stays.
    pub(crate) fn focus_neighbour_pane(&mut self, down: bool) {
        let mut panes = vec![FocusPane::Chat];
        if !self.activity_agents().is_empty() {
            panes.push(FocusPane::Activity);
        }
        if !self.queue.messages.is_empty() {
            panes.push(FocusPane::Queue);
        }
        panes.push(FocusPane::Input);
        let at = panes.iter().position(|p| *p == self.ui.focus).unwrap_or(0);
        let to = if down {
            (at + 1).min(panes.len() - 1)
        } else {
            at.saturating_sub(1)
        };
        match panes[to] {
            FocusPane::Queue if self.queue.selected.is_none() => self.queue.selected = Some(0),
            FocusPane::Chat if self.ui.focus != FocusPane::Chat => self.recompute_focused_segment(),
            _ => {}
        }
        self.ui.focus = panes[to];
    }

    /// `Ctrl+w a`: put focus on the activity panel, if there is one.
    pub(crate) fn focus_activity(&mut self) {
        if !self.activity_agents().is_empty() {
            self.ui.focus = FocusPane::Activity;
        }
    }

    /// Open the panel to a row per agent, or fold it back to its summary.
    pub(crate) fn toggle_activity(&mut self) {
        self.ui.activity.expanded = !self.ui.activity.expanded;
    }

    /// Move the highlight `down` or up one agent, keeping it in view.
    pub(crate) fn step_activity(&mut self, down: bool) {
        let n = self.activity_agents().len();
        if n == 0 {
            return;
        }
        let panel = &mut self.ui.activity;
        panel.selected = if down {
            (panel.selected + 1).min(n - 1)
        } else {
            panel.selected.saturating_sub(1)
        };
        let rows = usize::from(self.layout.activity_pane.height.saturating_sub(2)).max(1);
        let panel = &mut self.ui.activity;
        if panel.selected < panel.scroll {
            panel.scroll = panel.selected;
        } else if panel.selected >= panel.scroll + rows {
            panel.scroll = panel.selected + 1 - rows;
        }
    }

    /// Enter in the panel: open a folded panel; in an open one, show the
    /// highlighted agent's session (its transcript, live).
    pub(crate) async fn activate_activity(&mut self) {
        if !self.ui.activity.expanded {
            self.ui.activity.expanded = true;
            return;
        }
        let selected = self
            .activity_agents()
            .get(self.ui.activity.selected)
            .map(|(id, _)| (*id).clone());
        if let Some(id) = selected {
            self.switch_session(id).await;
            self.ui.focus = FocusPane::Chat;
        }
    }

    /// A click on the panel: its summary line (or top border) opens or folds
    /// it; an agent's row shows that agent's session.
    pub(crate) async fn click_activity(&mut self, row: u16) {
        self.ui.focus = FocusPane::Activity;
        if row == 0 || !self.ui.activity.expanded {
            self.toggle_activity();
            return;
        }
        let index = self.ui.activity.scroll + usize::from(row - 1);
        if index < self.activity_agents().len() {
            self.ui.activity.selected = index;
            self.activate_activity().await;
        }
    }

    /// Esc in the panel: fold it and give focus back to the input.
    pub(crate) fn leave_activity(&mut self) {
        self.ui.activity.expanded = false;
        self.ui.focus = FocusPane::Input;
    }
}
