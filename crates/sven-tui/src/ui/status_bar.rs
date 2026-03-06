// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Status bar widget — single top row showing model, mode, context, and
//! context-sensitive key hints.

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use sven_config::AgentMode;

use super::theme::{
    ctx_bar, ctx_style, mode_style, sep, spinner_char, BAR_AGENT, BAR_THINKING, BAR_TOOL,
    BG_ELEVATED, BORDER_DIM, SE_YELLOW, TEXT_DIM,
};
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

        // ── Brand mark ────────────────────────────────────────────────────────
        let brand = Span::styled(
            " ⬡ sven ",
            Style::default().fg(SE_YELLOW).add_modifier(Modifier::BOLD),
        );

        // ── Busy / spinner ────────────────────────────────────────────────────
        let busy_indicator = if self.agent_busy {
            spinner_char(self.spinner_frame, self.ascii)
        } else {
            " "
        };

        let mode_str = self.mode.to_string();
        let ctx_bar_str = ctx_bar(self.context_pct, self.ascii);
        let ctx_pct_str = format!(" {}%", self.context_pct);

        // Tool in progress — only shown when a tool is actually running.
        let tool_sym = if self.ascii { "*" } else { "⚙" };
        let tool_span: Span<'static> = if let Some(t) = self.current_tool {
            Span::styled(format!("  {tool_sym} {t}"), Style::default().fg(BAR_TOOL))
        } else {
            Span::raw("")
        };

        // Token counter — only shown while streaming non-tool output.
        let streaming_span: Span<'static> =
            if self.agent_busy && self.streaming_tokens > 0 && self.current_tool.is_none() {
                Span::styled(
                    format!("  {}t", self.streaming_tokens),
                    Style::default().fg(TEXT_DIM),
                )
            } else {
                Span::raw("")
            };

        // Cache hit rate — shown in green when >0.
        let cache_span: Span<'static> = if self.cache_hit_pct > 0 && !self.agent_busy {
            Span::styled(
                format!("  cache hit {}%", self.cache_hit_pct),
                Style::default().fg(Color::Rgb(80, 180, 100)),
            )
        } else {
            Span::raw("")
        };

        // Staged model/mode override notification.
        let pending_span: Span<'static> = match (self.pending_model, self.pending_mode) {
            (Some(m), Some(pm)) => Span::styled(
                format!("  next: {m} [{pm}]"),
                Style::default().fg(Color::Rgb(180, 100, 220)),
            ),
            (Some(m), None) => Span::styled(
                format!("  next: {m}"),
                Style::default().fg(Color::Rgb(180, 100, 220)),
            ),
            (None, Some(pm)) => Span::styled(
                format!("  next: [{pm}]"),
                Style::default().fg(Color::Rgb(180, 100, 220)),
            ),
            (None, None) => Span::raw(""),
        };

        // ── Context-sensitive hint (right side) ───────────────────────────────
        // Show only the most relevant hint for the current state.
        let hint: &str = if self.in_search {
            "n/N match · Esc close"
        } else if self.in_edit {
            "Enter confirm · Esc cancel"
        } else {
            match self.focus {
                FocusPane::Input => {
                    if self.agent_busy {
                        "^c interrupt"
                    } else {
                        "Enter send · / cmd · F1 help"
                    }
                }
                FocusPane::Chat => "j/k scroll · e edit · y copy · x del · / search",
                FocusPane::Queue => "↑↓ select · Enter send · Esc close",
            }
        };

        let left_spans = vec![
            brand,
            Span::styled(separator, Style::default().fg(BORDER_DIM)),
            Span::styled(
                format!(" {busy_indicator} "),
                Style::default().fg(if self.agent_busy {
                    BAR_THINKING
                } else {
                    TEXT_DIM
                }),
            ),
            Span::styled(self.model_name.to_string(), Style::default().fg(BAR_AGENT)),
            Span::styled(separator, Style::default().fg(BORDER_DIM)),
            Span::styled(format!(" {mode_str} "), mode_style(self.mode)),
            Span::styled(separator, Style::default().fg(BORDER_DIM)),
            Span::styled(" ctx ", Style::default().fg(TEXT_DIM)),
            Span::styled(format!("{ctx_bar_str}"), ctx_style(self.context_pct)),
            Span::styled(ctx_pct_str, ctx_style(self.context_pct)),
            cache_span,
            tool_span,
            streaming_span,
            pending_span,
        ];

        let right_spans = vec![Span::styled(
            format!("  {hint}  "),
            Style::default().fg(TEXT_DIM),
        )];

        // Render left-aligned left section.
        Paragraph::new(Line::from(left_spans))
            .style(Style::default().bg(BG_ELEVATED))
            .render(area, buf);

        // Render right-aligned right section on top.
        Paragraph::new(Line::from(right_spans))
            .style(Style::default().bg(BG_ELEVATED))
            .alignment(Alignment::Right)
            .render(area, buf);
    }
}
