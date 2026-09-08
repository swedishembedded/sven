// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Full-screen session picker overlay for `/resume` - lists saved
//! conversations and lets the user reload one via `App::switch_session`.
//!
//! Unlike [`super::team_picker::TeamPickerOverlay`] (a small centered popup),
//! this fills the whole frame with no side borders - consistent with the
//! rest of the TUI's "no floating controls" design.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, List, ListItem, ListState, Paragraph, StatefulWidget, Widget},
};

use sven_session_store::UnifiedSessionEntry;

use super::theme::{BAR_AGENT, TEXT, TEXT_DIM};
use super::width_utils::{display_width, truncate_to_width_exact};

// ── SessionPickerState ───────────────────────────────────────────────────────

/// Mutable state for the session picker overlay (selection only - entries
/// are pre-sorted newest-first by `sven_session_store::list_all_sessions`).
pub struct SessionPickerState {
    pub list_state: ListState,
}

impl Default for SessionPickerState {
    fn default() -> Self {
        let mut state = Self {
            list_state: ListState::default(),
        };
        state.list_state.select(Some(0));
        state
    }
}

impl SessionPickerState {
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

    pub fn selected<'a>(
        &self,
        entries: &'a [UnifiedSessionEntry],
    ) -> Option<&'a UnifiedSessionEntry> {
        self.list_state.selected().and_then(|i| entries.get(i))
    }
}

// ── SessionPickerOverlay widget ───────────────────────────────────────────────

/// Rendered session picker overlay.
pub struct SessionPickerOverlay<'a> {
    pub entries: &'a [UnifiedSessionEntry],
    pub state: &'a mut SessionPickerState,
    pub ascii: bool,
}

impl Widget for SessionPickerOverlay<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let dash = if self.ascii { "-" } else { "╌" };
        let title_text =
            " RESUME  (\u{2191}\u{2193} select \u{00b7} Enter reload \u{00b7} Esc cancel) ";
        let half_w = (area.width as usize).saturating_sub(display_width(title_text)) / 2;
        let fill = dash.repeat(half_w);
        let header = Line::from(vec![
            Span::styled(fill.clone(), Style::default().fg(Color::DarkGray)),
            Span::styled(
                title_text,
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(fill, Style::default().fg(Color::DarkGray)),
        ]);
        Paragraph::new(header).render(Rect::new(area.x, area.y, area.width, 1), buf);

        let inner = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(1),
        );

        if self.entries.is_empty() {
            Paragraph::new(Line::from(Span::styled(
                "  No saved sessions yet.",
                Style::default().fg(TEXT_DIM),
            )))
            .render(inner, buf);
            return;
        }

        let title_width = (inner.width as usize).saturating_sub(28).max(10);
        let items: Vec<ListItem> = self
            .entries
            .iter()
            .map(|entry| {
                let title = truncate_to_width_exact(&entry.title, title_width);
                let when = entry.updated_at.format("%Y-%m-%d %H:%M").to_string();
                let legacy = if entry.is_legacy { "  (legacy)" } else { "" };
                ListItem::new(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(title, Style::default().fg(TEXT)),
                    Span::styled(format!("  {when}{legacy}"), Style::default().fg(TEXT_DIM)),
                ]))
            })
            .collect();

        let list = List::new(items)
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(40, 50, 70))
                    .fg(BAR_AGENT)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("\u{25b6} ");

        StatefulWidget::render(list, inner, buf, &mut self.state.list_state);
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::path::PathBuf;

    fn entry(title: &str) -> UnifiedSessionEntry {
        UnifiedSessionEntry {
            session_id: title.to_string(),
            path: PathBuf::from(format!("/sessions/{title}.json")),
            title: title.to_string(),
            status: sven_session_store::ChatStatus::Completed,
            parent_session_id: None,
            usage: None,
            updated_at: Utc::now(),
            is_legacy: false,
        }
    }

    #[test]
    fn default_state_selects_first() {
        let s = SessionPickerState::default();
        assert_eq!(s.list_state.selected(), Some(0));
    }

    #[test]
    fn select_next_wraps_around() {
        let mut s = SessionPickerState::default();
        s.select_next(2);
        assert_eq!(s.list_state.selected(), Some(1));
        s.select_next(2);
        assert_eq!(s.list_state.selected(), Some(0));
    }

    #[test]
    fn select_prev_wraps_around() {
        let mut s = SessionPickerState::default();
        s.select_prev(2);
        assert_eq!(s.list_state.selected(), Some(1));
    }

    #[test]
    fn select_on_empty_does_not_panic() {
        let mut s = SessionPickerState::default();
        s.select_next(0);
        s.select_prev(0);
    }

    #[test]
    fn selected_returns_the_entry_at_the_cursor() {
        let entries = vec![entry("first"), entry("second")];
        let mut s = SessionPickerState::default();
        s.select_next(entries.len());
        assert_eq!(
            s.selected(&entries).map(|e| e.title.as_str()),
            Some("second")
        );
    }

    #[test]
    fn render_fills_full_width_with_no_side_border_glyphs() {
        let area = Rect::new(0, 0, 60, 10);
        let mut buf = Buffer::empty(area);
        let entries = vec![entry("Fix the flaky test")];
        let mut state = SessionPickerState::default();
        SessionPickerOverlay {
            entries: &entries,
            state: &mut state,
            ascii: false,
        }
        .render(area, &mut buf);

        // Row 1 (the first list row) must not carry a left/right border glyph
        // - the picker is full-screen, not a bordered box.
        let left = buf[(0, 1)].symbol();
        let right = buf[(area.width - 1, 1)].symbol();
        assert_ne!(left, "\u{2502}");
        assert_ne!(right, "\u{2502}");
    }

    #[test]
    fn render_empty_shows_a_placeholder_not_a_blank_screen() {
        let area = Rect::new(0, 0, 60, 10);
        let mut buf = Buffer::empty(area);
        let entries: Vec<UnifiedSessionEntry> = Vec::new();
        let mut state = SessionPickerState::default();
        SessionPickerOverlay {
            entries: &entries,
            state: &mut state,
            ascii: false,
        }
        .render(area, &mut buf);

        let mut row = String::new();
        for x in 0..area.width {
            row.push_str(buf[(x, 1)].symbol());
        }
        assert!(row.contains("No saved sessions"));
    }
}
