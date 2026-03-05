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

// ── Brand palette ─────────────────────────────────────────────────────────────

/// Main background — very dark blue-black.
pub const BG: Color = Color::Rgb(18, 18, 24);
/// Slightly elevated surface (status bar, overlays).
pub const BG_ELEVATED: Color = Color::Rgb(25, 25, 35);
/// Subtle border, unfocused elements.
pub const BORDER_DIM: Color = Color::Rgb(55, 55, 75);
/// Focused border / accent.
pub const BORDER_FOCUS: Color = Color::Rgb(100, 140, 220);
/// Default text.
pub const TEXT: Color = Color::Rgb(200, 200, 210);
/// Dimmed / secondary text.
pub const TEXT_DIM: Color = Color::Rgb(110, 110, 130);
/// Separator characters.
pub const SEPARATOR: Color = Color::Rgb(65, 65, 85);

/// User message bar — muted green.
pub const BAR_USER: Color = Color::Rgb(80, 180, 120);
/// Agent message bar — soft blue.
pub const BAR_AGENT: Color = Color::Rgb(100, 140, 220);
/// Tool call/result bar — warm amber.
pub const BAR_TOOL: Color = Color::Rgb(200, 160, 60);
/// Thinking bar — soft purple.
pub const BAR_THINKING: Color = Color::Rgb(160, 120, 200);
/// Error bar — muted red.
pub const BAR_ERROR: Color = Color::Rgb(220, 80, 80);
/// Context compaction bar — steel blue.
pub(crate) const BAR_COMPACT: Color = Color::Rgb(80, 120, 160);

/// Swedish Embedded yellow (chip body color).
pub const SE_YELLOW: Color = Color::Rgb(230, 180, 40);
/// Swedish Embedded blue (chip core color).
pub const SE_BLUE: Color = Color::Rgb(60, 120, 220);

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
        AgentMode::Research => Style::default().fg(Color::Rgb(100, 200, 130)),
        AgentMode::Plan => Style::default().fg(Color::Rgb(220, 190, 80)),
        AgentMode::Agent => Style::default().fg(Color::Rgb(180, 130, 220)),
    }
}

pub(crate) fn ctx_style(pct: u8) -> Style {
    if pct >= 90 {
        Style::default()
            .fg(Color::Rgb(220, 80, 80))
            .add_modifier(Modifier::BOLD)
    } else if pct >= 70 {
        Style::default().fg(Color::Rgb(220, 180, 60))
    } else {
        Style::default().fg(Color::Rgb(80, 180, 100))
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
        Style::default().fg(BORDER_FOCUS)
    } else {
        Style::default().fg(BORDER_DIM)
    };
    Block::default()
        .title(Span::styled(
            format!(" {title} "),
            if focused {
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(BORDER_FOCUS)
            } else {
                Style::default().fg(TEXT_DIM)
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
        Style::default().fg(BORDER_FOCUS)
    } else {
        Style::default().fg(BORDER_DIM)
    };
    Block::default()
        .title(Span::styled(
            format!(" {title} "),
            if focused {
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(BORDER_FOCUS)
            } else {
                Style::default().fg(TEXT_DIM)
            },
        ))
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_type(BorderType::Plain) // '─' only, no corners
        .border_style(border_style)
}
