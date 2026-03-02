// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Shared visual theme: colors, styles, border types, and character-set helpers.

use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, BorderType, Borders},
};
use sven_config::AgentMode;

// ── Character-set helpers ─────────────────────────────────────────────────────

pub(crate) fn border_type(ascii: bool) -> BorderType {
    if ascii {
        BorderType::Plain
    } else {
        BorderType::Rounded
    }
}

pub(crate) fn sep(ascii: bool) -> &'static str {
    if ascii {
        "|"
    } else {
        "│"
    }
}

pub(crate) fn busy_char(ascii: bool) -> &'static str {
    if ascii {
        "* "
    } else {
        "⠿ "
    }
}

// ── Markdown / chat rendering character helpers ───────────────────────────────

pub(crate) fn md_rule_char(ascii: bool) -> char {
    if ascii {
        '-'
    } else {
        '─'
    }
}

pub(crate) fn md_blockquote(ascii: bool) -> &'static str {
    if ascii {
        "> "
    } else {
        "▌ "
    }
}

pub(crate) fn md_bullet(ascii: bool) -> &'static str {
    if ascii {
        "- "
    } else {
        "• "
    }
}

// ── Style helpers ─────────────────────────────────────────────────────────────

pub(crate) fn mode_style(mode: AgentMode) -> Style {
    match mode {
        AgentMode::Research => Style::default().fg(Color::LightGreen),
        AgentMode::Plan => Style::default().fg(Color::LightYellow),
        AgentMode::Agent => Style::default().fg(Color::LightMagenta),
    }
}

pub(crate) fn ctx_style(pct: u8) -> Style {
    if pct >= 90 {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if pct >= 70 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Green)
    }
}

// ── Shared block builder ──────────────────────────────────────────────────────

/// Build a titled pane block with focus-aware border and title styling.
pub(crate) fn pane_block(title: &str, focused: bool, ascii: bool) -> Block<'static> {
    let border_style = if focused {
        Style::default().fg(Color::LightBlue)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Block::default()
        .title(Span::styled(
            format!(" {title} "),
            if focused {
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::LightBlue)
            } else {
                Style::default().fg(Color::Gray)
            },
        ))
        .borders(Borders::ALL)
        .border_type(border_type(ascii))
        .border_style(border_style)
}
