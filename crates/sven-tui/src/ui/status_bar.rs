// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Status bar widget — top row showing model, mode, context, and key hints.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use sven_config::AgentMode;

use super::theme::{busy_char, ctx_style, mode_style, sep};

/// Top-of-screen status bar.
pub struct StatusBar<'a> {
    pub model_name: &'a str,
    pub mode: AgentMode,
    pub context_pct: u8,
    pub cache_hit_pct: u8,
    pub agent_busy: bool,
    pub current_tool: Option<&'a str>,
    pub pending_model: Option<&'a str>,
    pub pending_mode: Option<AgentMode>,
    pub ascii: bool,
}

impl Widget for StatusBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let separator = sep(self.ascii);
        let busy_indicator = if self.agent_busy {
            busy_char(self.ascii)
        } else {
            "  "
        };
        let mode_str = self.mode.to_string();
        let ctx_str = format!("ctx:{}%", self.context_pct);

        let tool_icon = if self.ascii { "*" } else { "⚙" };
        let tool_span: Span<'static> = if let Some(t) = self.current_tool {
            Span::styled(
                format!(" {tool_icon} {t} "),
                Style::default().fg(Color::Yellow),
            )
        } else {
            Span::raw("")
        };

        let cache_span: Span<'static> = if self.cache_hit_pct > 0 {
            Span::styled(
                format!(" {separator} cache:{}%", self.cache_hit_pct),
                Style::default().fg(Color::Green),
            )
        } else {
            Span::raw("")
        };

        let pending_span: Span<'static> = match (self.pending_model, self.pending_mode) {
            (Some(m), Some(pm)) => Span::styled(
                format!(" {separator} next: {m} [{pm}]"),
                Style::default().fg(Color::Magenta),
            ),
            (Some(m), None) => Span::styled(
                format!(" {separator} next: {m}"),
                Style::default().fg(Color::Magenta),
            ),
            (None, Some(pm)) => Span::styled(
                format!(" {separator} next: [{pm}]"),
                Style::default().fg(Color::Magenta),
            ),
            (None, None) => Span::raw(""),
        };

        let line = Line::from(vec![
            Span::styled(
                format!(" {busy_indicator}"),
                Style::default().fg(if self.agent_busy {
                    Color::Yellow
                } else {
                    Color::Gray
                }),
            ),
            Span::styled(
                format!(" {} ", self.model_name),
                Style::default().fg(Color::LightCyan),
            ),
            Span::styled(separator, Style::default().fg(Color::Gray)),
            Span::styled(format!(" {mode_str} "), mode_style(self.mode)),
            Span::styled(separator, Style::default().fg(Color::Gray)),
            Span::styled(format!(" {ctx_str} "), ctx_style(self.context_pct)),
            cache_span,
            tool_span,
            pending_span,
            Span::styled(
                "  F1:help  ^w k:↑chat  ^w j:↓input  click/e:edit  ^Enter:submit  /:search  ^T:pager  F4:mode  ^c:quit",
                Style::default().fg(Color::White),
            ),
        ]);

        Paragraph::new(line)
            .style(Style::default().bg(Color::Black))
            .render(area, buf);
    }
}
