// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Search bar widget — one-row inline search input shown at the bottom.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    widgets::{Paragraph, Widget},
};

/// Inline search bar rendered as a single terminal row.
pub struct SearchBar<'a> {
    pub query: &'a str,
    pub match_count: usize,
    pub current_match: usize,
}

impl Widget for SearchBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let text = if self.match_count == 0 {
            format!(
                "/{q}  (no matches)  n:next  N:prev  Esc:close",
                q = self.query
            )
        } else {
            format!(
                "/{q}  ({cur}/{total})  n:next  N:prev  Esc:close",
                q = self.query,
                cur = self.current_match + 1,
                total = self.match_count,
            )
        };
        Paragraph::new(text)
            .style(Style::default().fg(Color::Yellow).bg(Color::Black))
            .render(area, buf);
    }
}
