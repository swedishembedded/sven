// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Right-side chat list sidebar widget.
//!
//! Renders the list of all sessions with:
//! - Spinner for active/busy sessions
//! - Checkmark for completed sessions
//! - Highlighted row for the currently active session
//! - Cursor row for keyboard navigation

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    prelude::StatefulWidget,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Widget},
};
use sven_input::ChatStatus;

use crate::app::session_manager::SessionEntry;

use super::theme::{BORDER_DIM, BORDER_FOCUS, BORDER_RESIZE, SPINNER_FRAMES, TEXT, TEXT_DIM};
use super::width_utils::truncate_to_width;

/// Data for a single row in the chat list.
pub struct ChatListItem<'a> {
    /// Session title (truncated to fit).
    pub title: &'a str,
    /// Session status.
    pub status: ChatStatus,
    /// Whether the agent is currently busy for this session.
    pub busy: bool,
    /// Whether this is the currently active (displayed) session.
    pub is_active: bool,
    /// Animation frame counter for the spinner.
    pub anim_frame: u8,
    /// Tree depth: 0 = root, 1 = subagent child.
    pub depth: u16,
}

/// Right-side chat list pane widget.
///
/// Renders a scrollable list of chat sessions with status icons and highlights.
pub struct ChatListPane<'a> {
    pub items: &'a [ChatListItem<'a>],
    /// Index of the keyboard-focused row (may differ from active session).
    pub selected: usize,
    /// Whether this pane has keyboard focus.
    pub focused: bool,
    /// ASCII-only mode (no Unicode box-drawing characters).
    pub ascii: bool,
    /// Scroll offset (first visible row index).
    pub scroll_offset: usize,
    /// Whether the left border is currently being drag-resized.
    pub is_resizing: bool,
}

impl Widget for ChatListPane<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        // ── Border ────────────────────────────────────────────────────────────
        let border_style = if self.is_resizing {
            Style::default().fg(BORDER_RESIZE)
        } else if self.focused {
            Style::default().fg(BORDER_FOCUS)
        } else {
            Style::default().fg(BORDER_DIM)
        };
        let block = Block::default()
            .title(Span::styled(
                " Chats ",
                if self.focused {
                    Style::default()
                        .fg(BORDER_FOCUS)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(TEXT_DIM)
                },
            ))
            .borders(Borders::LEFT | Borders::TOP | Borders::BOTTOM)
            .border_style(border_style)
            .border_type(if self.ascii {
                BorderType::Plain
            } else {
                BorderType::Rounded
            });

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height == 0 || inner.width < 3 {
            return;
        }

        // Reserve the last row for the hint when focused.
        let hint_height = if self.focused && inner.height >= 3 {
            1u16
        } else {
            0u16
        };
        let list_area = Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(hint_height),
        );

        // ── Build ListItems with pre-computed row styles ───────────────────────
        // ratatui's List sets `buf.set_style(row_area, item_style)` for every
        // item before rendering content, which fills the entire row (including
        // trailing empty cells) with the item's background — matching the
        // previous manual `buf.cell_mut` background fill.
        let max_title_cols = (inner.width as usize).saturating_sub(3); // icon + space + min indent
        let list_items: Vec<ListItem> = self
            .items
            .iter()
            .enumerate()
            .map(|(item_idx, item)| {
                let is_cursor = item_idx == self.selected;

                // ── Status icon ───────────────────────────────────────────────
                let (icon_char, icon_fg) = if item.busy {
                    let frame_idx = (item.anim_frame as usize) % SPINNER_FRAMES.len();
                    let ch = SPINNER_FRAMES[frame_idx]
                        .chars()
                        .next()
                        .unwrap_or('\u{00B7}');
                    (ch, Color::Cyan)
                } else {
                    match item.status {
                        ChatStatus::Completed => (
                            if self.ascii { 'v' } else { '\u{2713}' }, // ✓
                            Color::Rgb(100, 140, 100),
                        ),
                        ChatStatus::Archived => (
                            if self.ascii { 'a' } else { '\u{25C8}' }, // ◈
                            Color::Rgb(120, 100, 80),
                        ),
                        ChatStatus::Active => {
                            if item.is_active {
                                ('\u{25CF}', Color::Rgb(100, 180, 240)) // ●
                            } else {
                                ('\u{25CB}', TEXT_DIM) // ○
                            }
                        }
                    }
                };

                // ── Row background + text colour (tri-state: active+cursor / active / cursor / normal)
                let (bg_color, title_fg, title_mod) = if item.is_active && is_cursor {
                    (
                        Color::Rgb(35, 55, 85),
                        Color::Rgb(200, 220, 255),
                        Modifier::BOLD,
                    )
                } else if item.is_active {
                    (
                        Color::Rgb(28, 42, 65),
                        Color::Rgb(180, 200, 240),
                        Modifier::empty(),
                    )
                } else if is_cursor {
                    (Color::Rgb(40, 40, 60), TEXT, Modifier::BOLD)
                } else {
                    (Color::Reset, TEXT_DIM, Modifier::empty())
                };

                // ── Title with tree-depth indent ──────────────────────────────
                let indent = (item.depth.saturating_mul(2)) as usize;
                let avail = max_title_cols.saturating_sub(indent);
                // truncate_to_width adds ellipsis automatically when needed.
                let title = truncate_to_width(item.title, avail);
                let indent_str = " ".repeat(indent);

                let icon_span = Span::styled(icon_char.to_string(), Style::default().fg(icon_fg));
                let gap_span = Span::raw(" ");
                let indent_span = Span::raw(indent_str);
                let title_span =
                    Span::styled(title, Style::default().fg(title_fg).add_modifier(title_mod));

                ListItem::new(Line::from(vec![
                    icon_span,
                    gap_span,
                    indent_span,
                    title_span,
                ]))
                // Set the item-level background; List fills the full row with
                // this style before rendering content (see ratatui list rendering.rs).
                .style(Style::default().bg(bg_color))
            })
            .collect();

        // ── Render list via ratatui List + local ListState ────────────────────
        // We build a transient ListState here: scroll and selection are owned
        // by the app state and passed in as plain fields. List::render updates
        // the state's offset to keep the selected item visible, but since the
        // state is local the update is discarded — the app drives navigation.
        let list = List::new(list_items);
        let mut list_state = ListState::default();
        *list_state.offset_mut() = self.scroll_offset;
        list_state.select(Some(self.selected));
        StatefulWidget::render(list, list_area, buf, &mut list_state);

        // ── "New chat" hint at the bottom ─────────────────────────────────────
        if self.focused && inner.height >= 3 {
            let hint_y = inner.y + inner.height.saturating_sub(1);
            let hint = " n:new  d:del  a:arch ";
            let hint_style = Style::default().fg(TEXT_DIM);
            for (i, ch) in hint.chars().enumerate() {
                let x = inner.x + i as u16;
                if x >= inner.x + inner.width {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, hint_y)) {
                    cell.set_char(ch);
                    cell.set_style(hint_style);
                }
            }
        }
    }
}

/// Build a `ChatListItem` slice from session manager tree for rendering.
///
/// `tree_rows` is the flat list of (session_id, depth) from
/// `SessionManager::tree_rows()`. `entries` is the session map to look up
/// each entry. `active_busy` is the live busy state of the currently active
/// session; we override the active entry's busy flag to avoid ghost spinners.
pub fn build_chat_list_items<'a>(
    tree_rows: &'a [(sven_input::SessionId, u16)],
    entries: &'a std::collections::HashMap<sven_input::SessionId, SessionEntry>,
    active_id: &sven_input::SessionId,
    anim_frame: u8,
    active_busy: bool,
) -> Vec<ChatListItem<'a>> {
    tree_rows
        .iter()
        .filter_map(|(id, depth)| {
            let entry = entries.get(id)?;
            let is_active = id == active_id;
            Some(ChatListItem {
                title: entry.title.as_str(),
                status: entry.status,
                busy: if is_active { active_busy } else { entry.busy },
                is_active,
                anim_frame,
                depth: *depth,
            })
        })
        .collect()
}
