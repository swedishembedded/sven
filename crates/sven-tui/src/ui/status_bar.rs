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
    /// Context window fill percentage derived from total_context_tokens.
    pub total_context_pct: u8,
    /// Current context window size in tokens (latest turn's prompt size).
    pub total_context_tokens: u32,
    /// Cumulative output tokens across all completed turns in this session.
    pub total_output_tokens: u32,
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
    /// Live approximate output token count while generating (chars/4).
    /// Zero once the provider's exact output count has been received.
    pub streaming_tokens: u32,
    /// True when editing a chat segment or queue item.
    pub in_edit: bool,
    /// True when the search bar is active.
    pub in_search: bool,
    // ── Team info (all `None` when not in a team) ──────────────────────────
    /// Active team name (e.g. `"auth-refactor"`).
    pub team_name: Option<&'a str>,
    /// `"lead"` or the local agent's role in the team.
    pub team_role: Option<&'a str>,
    /// Number of active teammates (excluding the local agent).
    pub team_active_count: u8,
    /// `completed/total` task progress, e.g. `(3, 7)`.
    pub task_progress: Option<(usize, usize)>,
    /// Name of the teammate whose session is currently being viewed.
    /// `None` = viewing the local session.
    pub viewing_teammate: Option<&'a str>,
}

/// Format a token count compactly: raw below 1000, "Xk" below 1M, "X.XM" above.
fn fmt_tokens(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f32 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        format!("{n}")
    }
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
        // Use cumulative context percentage for the bar display.
        let display_ctx_pct = if self.total_context_tokens > 0 {
            self.total_context_pct
        } else {
            self.context_pct
        };
        let ctx_bar_str = ctx_bar(display_ctx_pct, self.ascii);
        let ctx_pct_str = format!(" {}%", display_ctx_pct);

        // Tool in progress — only shown when a tool is actually running.
        let tool_sym = if self.ascii { "*" } else { "⚙" };
        let tool_span: Span<'static> = if let Some(t) = self.current_tool {
            Span::styled(format!("  {tool_sym} {t}"), Style::default().fg(BAR_TOOL))
        } else {
            Span::raw("")
        };

        // Token counts: "in: 32k out: 1.2k"
        // Use exact provider-reported values.  While the model is generating and
        // the provider hasn't yet sent the output count, fall back to the live
        // streaming approximation (↑Xt). Show cumulative session totals.
        let token_span: Span<'static> = if self.total_context_tokens > 0 {
            let in_str = fmt_tokens(self.total_context_tokens);
            let out_str =
                if self.agent_busy && self.current_tool.is_none() && self.streaming_tokens > 0 {
                    // Exact output count not yet received; show live estimate.
                    format!("↑{}t", self.streaming_tokens)
                } else if self.total_output_tokens > 0 {
                    fmt_tokens(self.total_output_tokens)
                } else {
                    String::new()
                };
            // Cache hit is only relevant for the last turn, not cumulative.
            // Show it only when we have a value from the last turn.
            let cache_str = if self.cache_hit_pct > 0 {
                format!(" cache hit: {}%", self.cache_hit_pct)
            } else {
                String::new()
            };
            let label = if out_str.is_empty() {
                format!("  in: {in_str}{cache_str}")
            } else {
                format!("  in: {in_str} out: {out_str}{cache_str}")
            };
            Span::styled(label, Style::default().fg(TEXT_DIM))
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
                FocusPane::Chat => "j/k scroll · y copy · e edit · r rerun · x del · / search",
                FocusPane::Queue => "↑↓ select · Enter send · Esc close",
                FocusPane::ChatList => "j/k nav · Enter switch · n new · d del · ^b hide",
            }
        };

        // ── Team info ─────────────────────────────────────────────────────────
        // Shows: "⬡ auth-refactor [lead] 3/7 tasks | viewing: security-reviewer"
        let team_span: Span<'static> = if let Some(team) = self.team_name {
            let role_part = self
                .team_role
                .map(|r| format!(" [{r}]"))
                .unwrap_or_default();
            let tasks_part = if let Some((done, total)) = self.task_progress {
                format!(" {done}/{total}t")
            } else {
                String::new()
            };
            let active_part = if self.team_active_count > 0 {
                format!(" {}●", self.team_active_count)
            } else {
                String::new()
            };
            let viewing_part = if let Some(name) = self.viewing_teammate {
                format!(" → {name}")
            } else {
                String::new()
            };
            Span::styled(
                format!("  ⬡ {team}{role_part}{tasks_part}{active_part}{viewing_part}"),
                Style::default().fg(SE_YELLOW),
            )
        } else {
            Span::raw("")
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
            Span::styled(ctx_bar_str.to_string(), ctx_style(self.context_pct)),
            Span::styled(ctx_pct_str, ctx_style(self.context_pct)),
            token_span,
            tool_span,
            pending_span,
            team_span,
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
