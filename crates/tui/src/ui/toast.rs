// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Toast notification stack - brief ephemeral messages in the bottom-right corner.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Widget},
};

use crate::app::ui_state::Toast;

use super::width_utils::{display_width, truncate_to_width, truncate_to_width_exact};

/// Renders up to 4 toasts stacked above the bottom-right corner of `area`,
/// each wrapped onto as many as [`MAX_LINES`] lines, so an error's reason is
/// read whole rather than cut at the box's edge.
pub struct ToastStack<'a> {
    pub toasts: &'a [Toast],
    pub ascii: bool,
}

/// The most lines one toast takes; text beyond them ends in an ellipsis.
const MAX_LINES: usize = 4;
/// The widest a toast box grows, borders and icon included.
const MAX_WIDTH: u16 = 60;
/// The icon and its space in front of the first line.
const PREFIX: usize = 2;

/// `text` wrapped at word boundaries into lines of at most `cols` columns, at
/// most [`MAX_LINES`] of them; a word longer than a line is cut, and text left
/// over ends the last line in an ellipsis.
fn wrap(text: &str, cols: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_string()
        } else {
            format!("{line} {word}")
        };
        if display_width(&candidate) <= cols {
            line = candidate;
            continue;
        }
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        line = truncate_to_width_exact(word, cols);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.len() > MAX_LINES {
        let mut last = lines[MAX_LINES - 1].clone();
        last.push_str(" ...");
        lines.truncate(MAX_LINES - 1);
        lines.push(truncate_to_width(&last, cols));
    }
    lines
}

impl Widget for ToastStack<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let visible: Vec<&Toast> = self
            .toasts
            .iter()
            .filter(|t| !t.is_expired())
            .take(4)
            .collect();

        if visible.is_empty() {
            return;
        }

        let max_len = visible
            .iter()
            .map(|t| display_width(&t.message))
            .max()
            .unwrap_or(10);
        // +5 for icon prefix + padding
        let width = (max_len as u16 + 5)
            .clamp(18, MAX_WIDTH)
            .min(area.width.saturating_sub(2));
        let x = area.x + area.width.saturating_sub(width);
        let text_cols = (width as usize).saturating_sub(PREFIX + 1);

        // Render from the bottom up so newest toast is at the bottom.
        let mut row = area.y + area.height.saturating_sub(1);
        'toasts: for toast in visible.iter().rev() {
            let fg = toast.color;
            // What kind of notice it is, not only its colour.
            let check = match (fg, self.ascii) {
                (Color::Red, false) => "\u{2717} ", // ✗
                (Color::Red, true) => "x ",
                (Color::Yellow, _) => "! ",
                (Color::Green, false) => "\u{2713} ", // ✓
                (Color::Green, true) => "* ",
                (_, false) => "\u{2022} ", // •
                (_, true) => "- ",
            };
            let lines = wrap(&toast.message, text_cols);
            // Bottom line first, so the toast reads top to bottom.
            for (index, text) in lines.iter().enumerate().rev() {
                if row < area.y || row > area.y + area.height {
                    break 'toasts;
                }
                let lead = if index == 0 { check } else { "  " };
                let toast_area = Rect::new(x, row, width, 1);
                Clear.render(toast_area, buf);
                Paragraph::new(Line::from(Span::styled(
                    format!("{lead}{text}"),
                    Style::default().fg(fg).add_modifier(Modifier::BOLD),
                )))
                .render(toast_area, buf);
                if row == area.y {
                    break 'toasts;
                }
                row -= 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(toasts: &[Toast], width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        ToastStack {
            toasts,
            ascii: true,
        }
        .render(area, &mut buf);
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn a_long_error_is_shown_on_several_lines() {
        let reason = "github was not added to your account: it does not let sven register itself, ask your administrator";
        let rows = rendered(&[Toast::error(reason)], 80, 10);
        let shown: Vec<&String> = rows.iter().filter(|r| !r.is_empty()).collect();
        assert!(shown.len() > 1, "{rows:?}");
        let joined = shown
            .iter()
            .map(|r| r.trim_start_matches("x ").trim())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(joined, reason, "every word, in order: {rows:?}");
        assert!(shown[0].starts_with("x "), "the icon leads the first line");
    }

    #[test]
    fn text_beyond_the_most_lines_ends_in_an_ellipsis() {
        let lines = wrap(&"word ".repeat(100), 20);
        assert_eq!(lines.len(), MAX_LINES);
        assert!(lines.last().unwrap().ends_with("..."), "{lines:?}");
    }
}
