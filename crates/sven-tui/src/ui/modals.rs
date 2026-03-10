// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Modal dialog widgets: confirmation modal and question-answer modal.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use super::theme::border_type;
use super::width_utils::{display_width, truncate_to_width_exact};

// ── ConfirmModal widget ───────────────────────────────────────────────────────

/// Generic confirmation / info modal.
///
/// Shows a title, message body, and one or two buttons. The focused button
/// is highlighted in cyan; the other is dimmed.
///
/// Returns the bounding rectangles of the confirm and cancel buttons via
/// `confirm_rect` / `cancel_rect` after rendering (for mouse-click detection).
pub struct ConfirmModalView<'a> {
    pub title: &'a str,
    pub message: &'a str,
    pub confirm_label: &'a str,
    pub cancel_label: &'a str,
    pub focused_button: usize,
    pub has_action: bool,
    pub ascii: bool,
    /// Border (and title) color for the modal.
    pub border_color: Color,
}

impl Widget for ConfirmModalView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let bt = border_type(self.ascii);

        let max_line_chars = self.message.lines().map(display_width).max().unwrap_or(0);
        let modal_w = (max_line_chars as u16 + 4)
            .clamp(40, 60)
            .min(area.width.saturating_sub(4));
        let message_lines = self.message.lines().count() as u16;
        let modal_h = (1 + message_lines + 1 + 1 + 1 + 2)
            .max(7)
            .min(area.height.saturating_sub(2));
        let x = area.width.saturating_sub(modal_w) / 2;
        let y = area.height.saturating_sub(modal_h) / 2;
        let modal_area = Rect::new(x, y, modal_w, modal_h);

        Clear.render(modal_area, buf);

        let block = Block::default()
            .title(Span::styled(
                format!(" {} ", self.title),
                Style::default()
                    .fg(self.border_color)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_type(bt)
            .border_style(Style::default().fg(self.border_color))
            .style(Style::default().bg(Color::Black));

        let inner = block.inner(modal_area);
        block.render(modal_area, buf);

        let mut lines: Vec<Line> = vec![Line::from("")];
        for msg_line in self.message.lines() {
            lines.push(Line::from(Span::styled(
                msg_line.to_owned(),
                Style::default().fg(Color::White),
            )));
        }
        lines.push(Line::from(""));
        let hint = if self.has_action {
            "←/→: move  Enter: confirm  Esc: cancel"
        } else {
            "Enter / Esc: close"
        };
        lines.push(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        )));
        Paragraph::new(lines).render(inner, buf);

        let btn_row_y = inner.y + inner.height.saturating_sub(1);

        if !self.has_action {
            let lbl = self.cancel_label;
            let bw = display_width(lbl) as u16 + 2;
            let bx = inner.x + (inner.width.saturating_sub(bw)) / 2;
            Paragraph::new(Line::from(Span::styled(
                format!("[{lbl}]"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )))
            .render(Rect::new(bx, btn_row_y, bw, 1), buf);
            return;
        }

        let cw = display_width(self.confirm_label) as u16 + 2;
        let xw = display_width(self.cancel_label) as u16 + 2;
        let gap: u16 = 4;
        let total = cw + gap + xw;
        let bx = inner.x + inner.width.saturating_sub(total) / 2;

        let focused_style = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::REVERSED);
        let unfocused_style = Style::default().fg(Color::DarkGray);

        let (cs, xs) = if self.focused_button == 0 {
            (focused_style, unfocused_style)
        } else {
            (unfocused_style, focused_style)
        };

        Paragraph::new(Line::from(Span::styled(
            format!("[{}]", self.confirm_label),
            cs,
        )))
        .render(Rect::new(bx, btn_row_y, cw, 1), buf);
        Paragraph::new(Line::from(Span::styled(
            format!("[{}]", self.cancel_label),
            xs,
        )))
        .render(Rect::new(bx + cw + gap, btn_row_y, xw, 1), buf);
    }
}

// ── QuestionModal widget ──────────────────────────────────────────────────────

/// Agent question-answer modal with keyboard navigation and optional free-text
/// "Other" row.
#[allow(clippy::too_many_arguments)]
pub struct QuestionModalView<'a> {
    pub questions: &'a [sven_tools::Question],
    pub current_q: usize,
    pub selected_options: &'a [usize],
    pub other_selected: bool,
    pub other_input: &'a str,
    pub other_cursor: usize,
    pub focused_option: usize,
    pub ascii: bool,
}

/// Computed cursor position for the "Other" text field within the question modal.
pub struct QuestionModalCursor {
    pub pos: Option<(u16, u16)>,
}

impl QuestionModalView<'_> {
    /// Render and return the cursor position for the "Other" text field.
    pub fn render_with_cursor(self, area: Rect, buf: &mut Buffer) -> QuestionModalCursor {
        let bt = border_type(self.ascii);

        let modal_w = (area.width.saturating_sub(8)).clamp(20, 80);
        let current_question = self.questions.get(self.current_q);
        let content_rows = if let Some(q) = current_question {
            1 + 1 + q.options.len() as u16 + 1 + 1 + 2
        } else {
            8
        };
        let modal_h = (content_rows + 2)
            .min(area.height.saturating_sub(2))
            .max(10);
        let x = area.width.saturating_sub(modal_w) / 2;
        let y = area.height.saturating_sub(modal_h) / 2;
        let modal_area = Rect::new(x, y, modal_w, modal_h);

        Clear.render(modal_area, buf);

        let has_prev = self.current_q > 0;
        let block = Block::default()
            .title(Span::styled(
                " Questions from agent ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_type(bt)
            .border_style(Style::default().fg(Color::Yellow))
            .style(Style::default().bg(Color::Black));

        let inner = block.inner(modal_area);
        block.render(modal_area, buf);

        let mut cursor_pos: Option<(u16, u16)> = None;

        if let Some(q) = current_question {
            let mut lines: Vec<Line> = Vec::new();

            lines.push(Line::from(Span::styled(
                format!(
                    "Q{}/{}: {}",
                    self.current_q + 1,
                    self.questions.len(),
                    q.prompt
                ),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));

            let (checkbox, checked, focus_arrow) = if self.ascii {
                if q.allow_multiple {
                    ("[ ]", "[x]", "> ")
                } else {
                    ("( )", "(*)", "> ")
                }
            } else if q.allow_multiple {
                ("☐", "☑", "▶ ")
            } else {
                ("○", "●", "▶ ")
            };

            for (i, opt) in q.options.iter().enumerate() {
                let is_selected = self.selected_options.contains(&i);
                let is_focused = self.focused_option == i && !self.other_selected;
                let indicator = if is_selected { checked } else { checkbox };
                let focus_prefix = if is_focused { focus_arrow } else { "  " };
                let text_style = if is_selected {
                    Style::default().fg(Color::Green)
                } else if is_focused {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default().fg(Color::White)
                };
                lines.push(Line::from(vec![
                    Span::styled(focus_prefix, Style::default().fg(Color::Cyan)),
                    Span::styled(format!("{} ", indicator), text_style),
                    Span::styled(format!("{}. {}", i + 1, opt), text_style),
                ]));
            }

            let other_focused = self.focused_option == q.options.len() && !self.other_selected;
            let other_indicator = if self.other_selected {
                checked
            } else {
                checkbox
            };
            let other_prefix = if other_focused { focus_arrow } else { "  " };
            let other_label_style = if self.other_selected {
                Style::default().fg(Color::Green)
            } else if other_focused {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::White)
            };

            if self.other_selected {
                let before = &self.other_input[..self.other_cursor.min(self.other_input.len())];
                let after = &self.other_input[self.other_cursor.min(self.other_input.len())..];
                let mut row_spans = vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(format!("{} ", other_indicator), other_label_style),
                    Span::styled(
                        format!("{}. Other: ", q.options.len() + 1),
                        other_label_style,
                    ),
                    Span::styled(before.to_owned(), Style::default().fg(Color::White)),
                ];
                if !after.is_empty() {
                    row_spans.push(Span::styled(
                        after.to_owned(),
                        Style::default().fg(Color::White),
                    ));
                }
                lines.push(Line::from(row_spans));

                // Compute cursor position for the Other text field.
                let other_row_y = inner.y + 2 + q.options.len() as u16;
                use unicode_width::UnicodeWidthChar;
                let indicator_w: usize = other_indicator
                    .chars()
                    .map(|c| c.width().unwrap_or(1))
                    .sum();
                let number_part = format!("{}. Other: ", q.options.len() + 1);
                let prefix_cols = 2 + indicator_w + 1 + display_width(&number_part);
                let text_before =
                    &self.other_input[..self.other_cursor.min(self.other_input.len())];
                let cursor_col_offset: usize =
                    text_before.chars().map(|c| c.width().unwrap_or(1)).sum();
                let cursor_x = (inner.x + prefix_cols as u16 + cursor_col_offset as u16)
                    .min(inner.x + inner.width.saturating_sub(1));
                cursor_pos = Some((cursor_x, other_row_y));
            } else {
                let other_text = if self.other_input.trim().is_empty() {
                    format!("{}. Other (type a custom answer)", q.options.len() + 1)
                } else {
                    const MAX_PREVIEW: usize = 35;
                    if display_width(self.other_input) > MAX_PREVIEW {
                        format!(
                            "{}. Other: {}…",
                            q.options.len() + 1,
                            truncate_to_width_exact(self.other_input, MAX_PREVIEW)
                        )
                    } else {
                        format!("{}. Other: {}", q.options.len() + 1, self.other_input)
                    }
                };
                let display_indicator = if !self.other_input.trim().is_empty() {
                    checked
                } else {
                    other_indicator
                };
                let display_style = if !self.other_input.trim().is_empty() && !other_focused {
                    Style::default().fg(Color::Green)
                } else {
                    other_label_style
                };
                lines.push(Line::from(vec![
                    Span::styled(other_prefix, Style::default().fg(Color::Cyan)),
                    Span::styled(format!("{} ", display_indicator), display_style),
                    Span::styled(other_text, display_style),
                ]));
            }

            lines.push(Line::from(""));

            let nav_hint = if self.other_selected {
                "Type your answer  Enter: accept  Esc: cancel edit"
            } else if !self.other_input.trim().is_empty() && self.focused_option == q.options.len()
            {
                "↑/↓: move  Enter: submit  Space: re-edit"
            } else if q.allow_multiple {
                "↑/↓: move  Space: toggle  Enter: select/submit"
            } else {
                "↑/↓: move  Space: select  Enter: select/submit"
            };
            lines.push(Line::from(Span::styled(
                nav_hint,
                Style::default().fg(Color::DarkGray),
            )));

            let back_hint = if has_prev {
                "↑ at first option: go back   Esc: cancel"
            } else {
                "Esc: cancel"
            };
            lines.push(Line::from(Span::styled(
                back_hint,
                Style::default().fg(Color::DarkGray),
            )));

            Paragraph::new(lines).render(inner, buf);
        }

        QuestionModalCursor { pos: cursor_pos }
    }
}

impl Widget for QuestionModalView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.render_with_cursor(area, buf);
    }
}
