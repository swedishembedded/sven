// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Shared visual theme: colors, styles, border types, character-set helpers,
//! and spinner frames.

use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, BorderType, Borders},
};
use sven_config::AgentMode;

// ── Spinner ───────────────────────────────────────────────────────────────────

/// Braille spinner frame sequence (10 frames).
pub const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Return the current spinner character for `frame` (wraps around automatically).
pub(crate) fn spinner_char(frame: u8, ascii: bool) -> &'static str {
    if ascii {
        return match frame % 4 {
            0 => "-",
            1 => "\\",
            2 => "|",
            _ => "/",
        };
    }
    SPINNER_FRAMES[(frame % 10) as usize]
}

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

/// Render a compact 8-character context usage bar: `[████░░░░]`.
/// Returned as a styled String for embedding in a status line.
pub(crate) fn ctx_bar(pct: u8, ascii: bool) -> String {
    let filled = (pct as usize * 8 / 100).min(8);
    let empty = 8 - filled;
    let (fill_ch, empty_ch) = if ascii { ("#", ".") } else { ("█", "░") };
    format!("[{}{}]", fill_ch.repeat(filled), empty_ch.repeat(empty))
}

// ── Shared block builders ─────────────────────────────────────────────────────

/// Build a titled pane block with ALL borders and focus-aware style.
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

/// Build a borderless (TOP + BOTTOM only) pane block.  No left/right `│`
/// characters — terminal selection produces clean text without border chars.
pub(crate) fn open_pane_block(title: &str, focused: bool, _ascii: bool) -> Block<'static> {
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
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_type(BorderType::Plain) // '─' only, no corners
        .border_style(border_style)
}
