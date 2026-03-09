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
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Widget},
};
use sven_input::ChatStatus;

use crate::app::session_manager::{session_status_char, SessionEntry};

use super::theme::{BORDER_DIM, BORDER_FOCUS, BORDER_RESIZE, SPINNER_FRAMES, TEXT, TEXT_DIM};

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

        let visible_rows = inner.height as usize;
        let total = self.items.len();

        // Clamp scroll offset so selected is always visible.
        let scroll = self.scroll_offset;

        let visible_range = scroll..(scroll + visible_rows).min(total);

        for (row_idx, item_idx) in visible_range.enumerate() {
            let item = &self.items[item_idx];
            let y = inner.y + row_idx as u16;
            if y >= inner.y + inner.height {
                break;
            }

            let is_cursor = item_idx == self.selected;

            // ── Status icon ───────────────────────────────────────────────────
            let (icon_char, icon_style) = if item.busy {
                let frame_idx = (item.anim_frame as usize) % SPINNER_FRAMES.len();
                let icon = SPINNER_FRAMES[frame_idx];
                // Use the first char of the spinner frame string.
                let ch = icon.chars().next().unwrap_or('·');
                (ch, Style::default().fg(Color::Cyan))
            } else {
                match item.status {
                    ChatStatus::Completed => (
                        if self.ascii { 'v' } else { '✓' },
                        Style::default().fg(Color::Rgb(100, 140, 100)),
                    ),
                    ChatStatus::Archived => (
                        if self.ascii { 'a' } else { '◈' },
                        Style::default().fg(Color::Rgb(120, 100, 80)),
                    ),
                    ChatStatus::Active => {
                        if item.is_active {
                            ('●', Style::default().fg(Color::Rgb(100, 180, 240)))
                        } else {
                            ('○', Style::default().fg(TEXT_DIM))
                        }
                    }
                }
            };

            // ── Row background ────────────────────────────────────────────────
            let (bg_color, text_style) = if item.is_active && is_cursor {
                (
                    Color::Rgb(35, 55, 85),
                    Style::default()
                        .fg(Color::Rgb(200, 220, 255))
                        .add_modifier(Modifier::BOLD),
                )
            } else if item.is_active {
                (
                    Color::Rgb(28, 42, 65),
                    Style::default().fg(Color::Rgb(180, 200, 240)),
                )
            } else if is_cursor {
                (
                    Color::Rgb(40, 40, 60),
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )
            } else {
                (Color::Reset, Style::default().fg(TEXT_DIM))
            };

            // Fill background for the full row width.
            for x in 0..inner.width {
                let cell = buf.cell_mut((inner.x + x, y));
                if let Some(c) = cell {
                    c.set_bg(bg_color);
                }
            }

            // ── Icon ──────────────────────────────────────────────────────────
            let icon_x = inner.x;
            if let Some(cell) = buf.cell_mut((icon_x, y)) {
                cell.set_char(icon_char);
                cell.set_style(icon_style.bg(bg_color));
            }

            // ── Title (truncated) ─────────────────────────────────────────────
            let title_x = inner.x + 2;
            let max_title_width = inner.width.saturating_sub(2) as usize;
            let title_chars: Vec<char> = item.title.chars().collect();
            let display_len = title_chars.len().min(max_title_width);
            let truncated: String = title_chars[..display_len].iter().collect();

            // Check if we need the ellipsis.
            let (display_title, needs_ellipsis) =
                if title_chars.len() > max_title_width && max_title_width > 1 {
                    let t: String = title_chars[..max_title_width - 1].iter().collect();
                    (t, true)
                } else {
                    (truncated, false)
                };

            let mut col_offset = 0u16;
            for ch in display_title.chars() {
                if let Some(cell) = buf.cell_mut((title_x + col_offset, y)) {
                    cell.set_char(ch);
                    cell.set_style(text_style.bg(bg_color));
                }
                col_offset += 1;
            }
            if needs_ellipsis {
                let ellipsis_x = title_x + col_offset;
                if let Some(cell) = buf.cell_mut((ellipsis_x, y)) {
                    cell.set_char(if self.ascii { '.' } else { '…' });
                    cell.set_style(Style::default().fg(TEXT_DIM).bg(bg_color));
                }
            }
        }

        // ── "New chat" hint at the bottom ─────────────────────────────────────
        let hint_y = inner.y + inner.height.saturating_sub(1);
        if self.focused && inner.height >= 3 {
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

/// Build a `ChatListItem` slice from session manager data for rendering.
pub fn build_chat_list_items<'a>(
    entries: impl Iterator<Item = &'a SessionEntry>,
    active_id: &sven_input::SessionId,
    anim_frame: u8,
) -> Vec<ChatListItem<'a>> {
    entries
        .map(|entry| {
            let is_active = &entry.id == active_id;
            let status_ch = session_status_char(entry, is_active, anim_frame);
            let _ = status_ch; // used for rendering above
            ChatListItem {
                title: &entry.title,
                status: entry.status,
                busy: entry.busy,
                is_active,
                anim_frame,
            }
        })
        .collect()
}
