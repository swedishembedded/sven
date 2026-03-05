// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Status bar widget — single top row showing model, mode, context, and
//! context-sensitive key hints.

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use sven_config::AgentMode;

use super::theme::{ctx_bar, ctx_style, mode_style, sep, spinner_char};
use crate::app::ui_state::FocusPane;

// ── StatusBar widget ──────────────────────────────────────────────────────────

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
    /// Which pane currently has keyboard focus.
    pub focus: FocusPane,
    /// Current spinner frame (0–9); incremented on each streaming event.
    pub spinner_frame: u8,
    /// Tokens streamed in the current turn (shown while busy).
    pub streaming_tokens: u32,
    /// True when editing a chat segment or queue item.
    pub in_edit: bool,
    /// True when the search bar is active.
    pub in_search: bool,
}

impl Widget for StatusBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let separator = sep(self.ascii);

        // ── Left section: busy indicator + model + mode + context ─────────────
        let busy_indicator = if self.agent_busy {
            spinner_char(self.spinner_frame, self.ascii)
        } else {
            " "
        };

        let mode_str = self.mode.to_string();
        let ctx_bar_str = ctx_bar(self.context_pct, self.ascii);

        let tool_icon = if self.ascii { "*" } else { "⚙" };
        let tool_span: Span<'static> = if let Some(t) = self.current_tool {
            Span::styled(
                format!(" {tool_icon} {t} "),
                Style::default().fg(Color::Yellow),
            )
        } else {
            Span::raw("")
        };

        let streaming_span: Span<'static> = if self.agent_busy && self.streaming_tokens > 0 {
            Span::styled(
                format!(" {}↺ {}t", separator, self.streaming_tokens),
                Style::default().fg(Color::DarkGray),
            )
        } else {
            Span::raw("")
        };

        let cache_span: Span<'static> = if self.cache_hit_pct > 0 {
            Span::styled(
                format!(" {} cache:{}%", separator, self.cache_hit_pct),
                Style::default().fg(Color::Green),
            )
        } else {
            Span::raw("")
        };

        let pending_span: Span<'static> = match (self.pending_model, self.pending_mode) {
            (Some(m), Some(pm)) => Span::styled(
                format!(" {} next: {m} [{pm}]", separator),
                Style::default().fg(Color::Magenta),
            ),
            (Some(m), None) => Span::styled(
                format!(" {} next: {m}", separator),
                Style::default().fg(Color::Magenta),
            ),
            (None, Some(pm)) => Span::styled(
                format!(" {} next: [{pm}]", separator),
                Style::default().fg(Color::Magenta),
            ),
            (None, None) => Span::raw(""),
        };

        // ── Focus badge ───────────────────────────────────────────────────────
        let (focus_label, focus_color) = match self.focus {
            FocusPane::Chat => ("CHAT", Color::LightBlue),
            FocusPane::Input => ("INPUT", Color::LightGreen),
            FocusPane::Queue => ("QUEUE", Color::LightYellow),
        };
        let focus_span = Span::styled(
            format!(" [{focus_label}] "),
            Style::default().fg(focus_color),
        );

        // ── Context-sensitive hints ────────────────────────────────────────────
        let hints: &str = if self.in_search {
            "n/N:match  Esc:close search"
        } else if self.in_edit {
            "Enter:confirm  Esc:cancel  Alt+Enter:newline"
        } else {
            match self.focus {
                FocusPane::Input => {
                    if self.agent_busy {
                        "^c:interrupt  Alt+Enter:newline  F1:help"
                    } else {
                        "Enter:send  Alt+Enter:newline  /:cmd  ^↑↓:history  ^w k:chat  F1:help"
                    }
                }
                FocusPane::Chat => {
                    "j/k:scroll  e:edit  x:remove  r:rerun  d:truncate  /:search  ^w j:input"
                }
                FocusPane::Queue => "↑↓:select  e:edit  d:del  Enter:send  Esc:close",
            }
        };

        let left_spans = vec![
            Span::styled(
                format!(" {busy_indicator} "),
                Style::default().fg(if self.agent_busy {
                    Color::Yellow
                } else {
                    Color::DarkGray
                }),
            ),
            Span::styled(
                format!("{} ", self.model_name),
                Style::default().fg(Color::LightCyan),
            ),
            Span::styled(separator, Style::default().fg(Color::DarkGray)),
            Span::styled(format!(" {mode_str} "), mode_style(self.mode)),
            Span::styled(separator, Style::default().fg(Color::DarkGray)),
            Span::styled(format!(" {ctx_bar_str} "), ctx_style(self.context_pct)),
            cache_span,
            tool_span,
            streaming_span,
            pending_span,
        ];

        let right_spans = vec![
            Span::styled(format!("  {hints}  "), Style::default().fg(Color::White)),
            focus_span,
        ];

        // Build both halves and fit them into `area.width`.
        let left_line = Line::from(left_spans.clone());
        let right_line = Line::from(right_spans.clone());

        // Render left-aligned left section.
        Paragraph::new(left_line)
            .style(Style::default().bg(Color::Black))
            .render(area, buf);

        // Render right-aligned right section on top.
        Paragraph::new(right_line)
            .style(Style::default().bg(Color::Black))
            .alignment(Alignment::Right)
            .render(area, buf);
    }
}
