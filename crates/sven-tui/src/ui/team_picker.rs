// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Team picker overlay — shows team members and lets the user switch the
//! active view to any teammate's session.
//!
//! Triggered by `Ctrl+a` (new `Action::OpenTeamPicker`) or `/agents`.
//! `Esc` / `Ctrl+a` again closes the overlay.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, ListItem, ListState, Widget},
};

use super::theme::{
    border_type, BAR_AGENT, BAR_TOOL, BG_ELEVATED, BORDER_DIM, BORDER_FOCUS, TEXT, TEXT_DIM,
};

// ── TeamPickerEntry ───────────────────────────────────────────────────────────

/// A single teammate shown in the picker.
#[derive(Debug, Clone)]
pub struct TeamPickerEntry {
    /// Agent name.
    pub name: String,
    /// Role string (e.g. `"reviewer"`, `"lead"`).
    pub role: String,
    /// Peer ID (base58) — used for navigation.
    pub peer_id: String,
    /// Current status.
    pub status: AgentPickerStatus,
    /// Title of the task currently being worked on, if any.
    pub current_task: Option<String>,
    /// `true` when this entry represents the local (lead) agent.
    pub is_local: bool,
}

/// Display status for a team picker entry.
///
/// `Active` is constructed immediately for the local agent.  `Idle` and
/// `Closed` are populated when real P2P `TeamEvent` updates arrive.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentPickerStatus {
    Active,
    Idle,
    Closed,
}

#[allow(dead_code)]
impl AgentPickerStatus {
    fn icon(&self) -> &'static str {
        match self {
            AgentPickerStatus::Active => "●",
            AgentPickerStatus::Idle => "○",
            AgentPickerStatus::Closed => "✗",
        }
    }

    fn color(&self) -> Color {
        match self {
            AgentPickerStatus::Active => Color::Green,
            AgentPickerStatus::Idle => Color::Yellow,
            AgentPickerStatus::Closed => TEXT_DIM,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            AgentPickerStatus::Active => "active",
            AgentPickerStatus::Idle => "idle",
            AgentPickerStatus::Closed => "closed",
        }
    }
}

// ── TeamPickerState ───────────────────────────────────────────────────────────

/// Mutable state for the team picker overlay (scroll + selection).
pub struct TeamPickerState {
    pub list_state: ListState,
}

impl Default for TeamPickerState {
    fn default() -> Self {
        let mut state = Self {
            list_state: ListState::default(),
        };
        state.list_state.select(Some(0));
        state
    }
}

impl TeamPickerState {
    pub fn select_next(&mut self, len: usize) {
        if len == 0 {
            return;
        }
        let current = self.list_state.selected().unwrap_or(0);
        self.list_state.select(Some((current + 1) % len));
    }

    pub fn select_prev(&mut self, len: usize) {
        if len == 0 {
            return;
        }
        let current = self.list_state.selected().unwrap_or(0);
        self.list_state
            .select(Some(if current == 0 { len - 1 } else { current - 1 }));
    }

    pub fn selected_peer_id<'a>(&self, entries: &'a [TeamPickerEntry]) -> Option<&'a str> {
        self.list_state
            .selected()
            .and_then(|i| entries.get(i))
            .map(|e| e.peer_id.as_str())
    }
}

// ── TeamPickerOverlay widget ──────────────────────────────────────────────────

/// Rendered team picker overlay.
pub struct TeamPickerOverlay<'a> {
    pub entries: &'a [TeamPickerEntry],
    pub state: &'a mut TeamPickerState,
    pub team_name: &'a str,
    pub ascii: bool,
}

impl Widget for TeamPickerOverlay<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let width = 72u16.min(area.width.saturating_sub(4));
        let max_entries = self.entries.len().max(3) as u16;
        let height = (max_entries + 6).min(area.height.saturating_sub(2));

        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let popup_area = Rect::new(x, y, width, height);

        Clear.render(popup_area, buf);

        let bt = border_type(self.ascii);
        let block = Block::default()
            .title(Span::styled(
                format!(
                    "  Team: {}  (↑↓ select · Enter switch · Esc close)  ",
                    self.team_name
                ),
                Style::default().fg(BAR_AGENT).add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_type(bt)
            .border_style(Style::default().fg(BORDER_FOCUS))
            .style(Style::default().bg(BG_ELEVATED));

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        if self.entries.is_empty() {
            let no_team_line = Line::from(vec![Span::styled(
                "  No team members. Use create_team to start a team.",
                Style::default().fg(TEXT_DIM),
            )]);
            ratatui::widgets::Paragraph::new(no_team_line)
                .style(Style::default().bg(BG_ELEVATED))
                .render(inner, buf);
            return;
        }

        let items: Vec<ListItem> = self
            .entries
            .iter()
            .map(|e| {
                let status_span = Span::styled(
                    format!("{} ", e.status.icon()),
                    Style::default().fg(e.status.color()),
                );
                let name_span = Span::styled(
                    e.name.clone(),
                    Style::default()
                        .fg(if e.is_local { BAR_TOOL } else { TEXT })
                        .add_modifier(if e.is_local {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                );
                let role_span =
                    Span::styled(format!(" [{}]", e.role), Style::default().fg(BORDER_DIM));
                let task_hint = if let Some(t) = &e.current_task {
                    let preview: String = t.chars().take(32).collect();
                    Span::styled(format!("  — {preview}"), Style::default().fg(TEXT_DIM))
                } else {
                    Span::raw("")
                };

                ListItem::new(Line::from(vec![
                    Span::raw("  "),
                    status_span,
                    name_span,
                    role_span,
                    task_hint,
                ]))
            })
            .collect();

        let list = ratatui::widgets::List::new(items)
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(40, 50, 70))
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ")
            .style(Style::default().bg(BG_ELEVATED));

        ratatui::widgets::StatefulWidget::render(list, inner, buf, &mut self.state.list_state);
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(names: &[&str]) -> Vec<TeamPickerEntry> {
        names
            .iter()
            .enumerate()
            .map(|(i, &n)| TeamPickerEntry {
                name: n.to_string(),
                role: "teammate".to_string(),
                peer_id: format!("peer-{n}"),
                status: AgentPickerStatus::Active,
                current_task: None,
                is_local: i == 0,
            })
            .collect()
    }

    #[test]
    fn default_state_selects_first() {
        let s = TeamPickerState::default();
        assert_eq!(s.list_state.selected(), Some(0));
    }

    #[test]
    fn select_next_advances() {
        let mut s = TeamPickerState::default();
        s.select_next(3);
        assert_eq!(s.list_state.selected(), Some(1));
        s.select_next(3);
        assert_eq!(s.list_state.selected(), Some(2));
    }

    #[test]
    fn select_next_wraps_around() {
        let mut s = TeamPickerState::default();
        s.select_next(3);
        s.select_next(3);
        s.select_next(3); // 0 → 1 → 2 → 0
        assert_eq!(s.list_state.selected(), Some(0));
    }

    #[test]
    fn select_prev_wraps_around() {
        let mut s = TeamPickerState::default(); // starts at 0
        s.select_prev(3); // should wrap to 2
        assert_eq!(s.list_state.selected(), Some(2));
    }

    #[test]
    fn select_next_noop_on_empty() {
        let mut s = TeamPickerState::default();
        s.select_next(0); // should not panic
    }

    #[test]
    fn select_prev_noop_on_empty() {
        let mut s = TeamPickerState::default();
        s.select_prev(0); // should not panic
    }

    #[test]
    fn selected_peer_id_returns_correct_entry() {
        let es = entries(&["alice", "bob", "carol"]);
        let mut s = TeamPickerState::default();
        s.select_next(es.len()); // move to bob
        assert_eq!(s.selected_peer_id(&es), Some("peer-bob"));
    }

    #[test]
    fn selected_peer_id_none_on_empty_entries() {
        let s = TeamPickerState::default();
        assert_eq!(s.selected_peer_id(&[]), None);
    }

    #[test]
    fn agent_picker_status_icons_are_distinct() {
        let icons: Vec<&str> = [
            AgentPickerStatus::Active,
            AgentPickerStatus::Idle,
            AgentPickerStatus::Closed,
        ]
        .iter()
        .map(|s| s.icon())
        .collect();
        // All three icons must be different.
        assert_eq!(icons.len(), 3);
        assert_ne!(icons[0], icons[1]);
        assert_ne!(icons[1], icons[2]);
        assert_ne!(icons[0], icons[2]);
    }
}
