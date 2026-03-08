// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! UI overlay, focus state, and ephemeral notification toasts.

use std::time::Instant;

use ratatui::style::Color;

use crate::{
    chat::search::SearchState,
    overlay::{completion::CompletionOverlay, confirm::ConfirmModal, question::QuestionModal},
    pager::PagerOverlay,
    ui::team_picker::{TeamPickerEntry, TeamPickerState},
};

// ── FocusPane ─────────────────────────────────────────────────────────────────

/// Which pane currently holds keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPane {
    Chat,
    Input,
    /// The compact queue panel shown above the input when there are pending messages.
    Queue,
}

// ── Toast ─────────────────────────────────────────────────────────────────────

/// A brief notification shown in the bottom-right corner.
pub struct Toast {
    pub message: String,
    pub color: Color,
    pub born: Instant,
}

#[allow(dead_code)]
impl Toast {
    /// How long a toast is visible before it disappears.
    pub const LIFETIME_MS: u128 = 3000;

    pub fn new(message: impl Into<String>, color: Color) -> Self {
        Self {
            message: message.into(),
            color,
            born: Instant::now(),
        }
    }

    pub fn info(message: impl Into<String>) -> Self {
        Self::new(message, Color::Cyan)
    }

    pub fn success(message: impl Into<String>) -> Self {
        Self::new(message, Color::Green)
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self::new(message, Color::Yellow)
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::new(message, Color::Red)
    }

    pub fn is_expired(&self) -> bool {
        self.born.elapsed().as_millis() > Self::LIFETIME_MS
    }
}

// ── UiState ───────────────────────────────────────────────────────────────────

/// All UI overlay / modal / focus state.
pub(crate) struct UiState {
    pub focus: FocusPane,
    pub show_help: bool,
    pub search: SearchState,
    pub pager: Option<PagerOverlay>,
    pub completion: Option<CompletionOverlay>,
    pub question_modal: Option<QuestionModal>,
    pub confirm_modal: Option<ConfirmModal>,
    /// True after the first key of a Ctrl+w nav chord has been received.
    pub pending_nav: bool,
    /// Toast notifications (newest last). Cleaned up each frame.
    pub toasts: Vec<Toast>,
    /// Team picker overlay (shown when `show_team_picker` is true).
    pub show_team_picker: bool,
    /// Current set of team members for the picker.
    pub team_picker_entries: Vec<TeamPickerEntry>,
    /// Selection state for the team picker list.
    pub team_picker_state: TeamPickerState,
    /// Current team name (if any).
    pub team_name: Option<String>,
    /// Peer ID of the currently viewed session in the team picker.
    /// `None` = viewing the local (lead) session.
    pub active_session_peer: Option<String>,
}

#[allow(dead_code)]
impl UiState {
    pub fn new() -> Self {
        Self {
            focus: FocusPane::Input,
            show_help: false,
            search: SearchState::default(),
            pager: None,
            completion: None,
            question_modal: None,
            confirm_modal: None,
            pending_nav: false,
            toasts: Vec::new(),
            show_team_picker: false,
            team_picker_entries: Vec::new(),
            team_picker_state: TeamPickerState::default(),
            team_name: None,
            active_session_peer: None,
        }
    }

    /// Push a toast notification.
    pub fn push_toast(&mut self, toast: Toast) {
        // Limit the stack to 5 visible toasts; drop the oldest when full.
        if self.toasts.len() >= 5 {
            self.toasts.remove(0);
        }
        self.toasts.push(toast);
    }

    /// Remove expired toasts.  Call once per frame to keep the list lean.
    pub fn prune_toasts(&mut self) {
        self.toasts.retain(|t| !t.is_expired());
    }

    /// Toggle the team picker overlay.
    pub fn toggle_team_picker(&mut self) {
        self.show_team_picker = !self.show_team_picker;
    }

    /// Move selection down in the team picker.
    pub fn team_picker_next(&mut self) {
        let len = self.team_picker_entries.len();
        self.team_picker_state.select_next(len);
    }

    /// Move selection up in the team picker.
    pub fn team_picker_prev(&mut self) {
        let len = self.team_picker_entries.len();
        self.team_picker_state.select_prev(len);
    }

    /// Return the peer ID currently selected in the team picker.
    pub fn team_picker_selected_peer(&self) -> Option<&str> {
        self.team_picker_state
            .selected_peer_id(&self.team_picker_entries)
    }

    /// Cycle to the next teammate view (wraps around).
    ///
    /// Returns the peer ID of the newly active session, or `None` when cycling
    /// back to the lead.
    pub fn cycle_teammate_view_forward(&mut self) -> Option<&str> {
        if self.team_picker_entries.is_empty() {
            return None;
        }
        let current = self
            .team_picker_entries
            .iter()
            .position(|e| Some(e.peer_id.as_str()) == self.active_session_peer.as_deref())
            .map(|i| i + 1)
            .unwrap_or(0);

        let next_idx = current % self.team_picker_entries.len();
        let peer_id = self.team_picker_entries[next_idx].peer_id.clone();

        // If we landed on a "local" entry, return to lead view.
        if self.team_picker_entries[next_idx].is_local {
            self.active_session_peer = None;
            None
        } else {
            self.active_session_peer = Some(peer_id);
            self.active_session_peer.as_deref()
        }
    }

    /// Cycle to the previous teammate view.
    pub fn cycle_teammate_view_backward(&mut self) -> Option<&str> {
        if self.team_picker_entries.is_empty() {
            return None;
        }
        let len = self.team_picker_entries.len();
        let current = self
            .team_picker_entries
            .iter()
            .position(|e| Some(e.peer_id.as_str()) == self.active_session_peer.as_deref())
            .unwrap_or(0);

        let prev_idx = if current == 0 { len - 1 } else { current - 1 };
        let peer_id = self.team_picker_entries[prev_idx].peer_id.clone();

        if self.team_picker_entries[prev_idx].is_local {
            self.active_session_peer = None;
            None
        } else {
            self.active_session_peer = Some(peer_id);
            self.active_session_peer.as_deref()
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::team_picker::{AgentPickerStatus, TeamPickerEntry};

    fn entry(name: &str, peer: &str, local: bool) -> TeamPickerEntry {
        TeamPickerEntry {
            name: name.to_string(),
            role: "teammate".to_string(),
            peer_id: peer.to_string(),
            status: AgentPickerStatus::Active,
            current_task: None,
            is_local: local,
        }
    }

    fn ui_with_team() -> UiState {
        let mut ui = UiState::new();
        ui.team_picker_entries = vec![
            entry("alice", "peer-alice", true), // local lead
            entry("bob", "peer-bob", false),
            entry("carol", "peer-carol", false),
        ];
        ui
    }

    // ── toggle_team_picker ────────────────────────────────────────────────────

    #[test]
    fn toggle_team_picker_flips_visibility() {
        let mut ui = UiState::new();
        assert!(!ui.show_team_picker);
        ui.toggle_team_picker();
        assert!(ui.show_team_picker);
        ui.toggle_team_picker();
        assert!(!ui.show_team_picker);
    }

    // ── cycle_teammate_view_forward ───────────────────────────────────────────

    #[test]
    fn cycle_forward_empty_returns_none() {
        let mut ui = UiState::new();
        assert!(ui.cycle_teammate_view_forward().is_none());
    }

    #[test]
    fn cycle_forward_from_lead_goes_to_bob() {
        let mut ui = ui_with_team();
        // Start at lead (active_session_peer = None).
        let peer = ui.cycle_teammate_view_forward();
        assert_eq!(peer, Some("peer-bob"));
        assert_eq!(ui.active_session_peer.as_deref(), Some("peer-bob"));
    }

    #[test]
    fn cycle_forward_from_bob_goes_to_carol() {
        let mut ui = ui_with_team();
        ui.active_session_peer = Some("peer-bob".to_string());
        let peer = ui.cycle_teammate_view_forward();
        assert_eq!(peer, Some("peer-carol"));
    }

    #[test]
    fn cycle_forward_from_carol_wraps_to_lead() {
        let mut ui = ui_with_team();
        ui.active_session_peer = Some("peer-carol".to_string());
        let peer = ui.cycle_teammate_view_forward();
        // Wraps to alice who is_local → returns None and clears active_session_peer.
        assert!(peer.is_none());
        assert!(ui.active_session_peer.is_none());
    }

    // ── cycle_teammate_view_backward ──────────────────────────────────────────

    #[test]
    fn cycle_backward_empty_returns_none() {
        let mut ui = UiState::new();
        assert!(ui.cycle_teammate_view_backward().is_none());
    }

    #[test]
    fn cycle_backward_from_lead_goes_to_carol() {
        let mut ui = ui_with_team();
        // active_session_peer = None → "current" resolves to index 0 (alice).
        // prev of 0 = last = carol.
        let peer = ui.cycle_teammate_view_backward();
        assert_eq!(peer, Some("peer-carol"));
    }

    #[test]
    fn cycle_backward_from_bob_goes_to_lead() {
        let mut ui = ui_with_team();
        ui.active_session_peer = Some("peer-bob".to_string());
        let peer = ui.cycle_teammate_view_backward();
        // prev of bob (idx 1) = alice (idx 0) who is_local → returns None.
        assert!(peer.is_none());
        assert!(ui.active_session_peer.is_none());
    }

    // ── team_picker_next / prev ───────────────────────────────────────────────

    #[test]
    fn team_picker_next_advances_selection() {
        let mut ui = ui_with_team();
        ui.team_picker_next();
        assert_eq!(ui.team_picker_state.list_state.selected(), Some(1));
    }

    #[test]
    fn team_picker_prev_wraps_to_last() {
        let mut ui = ui_with_team();
        ui.team_picker_prev();
        assert_eq!(
            ui.team_picker_state.list_state.selected(),
            Some(ui.team_picker_entries.len() - 1)
        );
    }

    #[test]
    fn team_picker_selected_peer_returns_correct_id() {
        let mut ui = ui_with_team();
        ui.team_picker_next(); // select bob
        assert_eq!(ui.team_picker_selected_peer(), Some("peer-bob"));
    }

    // ── push_toast / prune_toasts ─────────────────────────────────────────────

    #[test]
    fn push_toast_caps_at_five() {
        let mut ui = UiState::new();
        for i in 0..7 {
            ui.push_toast(Toast::info(format!("msg {i}")));
        }
        assert_eq!(ui.toasts.len(), 5);
        // The oldest two should have been dropped; the last one should be msg 6.
        assert_eq!(ui.toasts.last().unwrap().message, "msg 6");
    }
}
