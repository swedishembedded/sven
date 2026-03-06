// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
// SPDX-License-Identifier: Apache-2.0
//! Welcome screen — shown when the chat is empty and the agent is idle.

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Widget},
};

use super::theme::{BAR_AGENT, BAR_TOOL, SEPARATOR, TEXT, TEXT_DIM};

/// Welcome screen rendered when chat is empty and agent is idle.
pub struct WelcomeScreen<'a> {
    /// Current model name shown below the logo.
    pub model_name: &'a str,
    /// Current mode label (Research / Plan / Agent).
    pub mode_label: &'a str,
    /// Mode color style.
    pub mode_style: Style,
}

/// "sven." ASCII art logo — all five characters laid out side-by-side.
///
/// Each letter is 8 terminal columns wide; the period is 4 columns wide.
/// Letters are separated by 2-space gaps.  Total banner width: 44 columns.
///
/// Layout per row:  S(8) __ V(8) __ E(8) __ N(8) __ .(4)   (__=2 spaces)
///
/// Colour gradient: top rows are blue, bottom row turns golden.
const SVEN_LOGO: &[(&str, &str)] = &[
    (
        "░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░",
        "shade_light",
    ),
    ("▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒", "shade_med"),
    //  S          V          E          N        .
    (" ██████   ██    ██  ████████  ██    ██      ", "s_letter"),
    ("██        ██    ██  ██        ███   ██      ", "v_letter"),
    (" █████     ██  ██   ██████    ██ ██ ██      ", "e_letter"),
    ("     ██     ████    ██        ██   ███   ██ ", "n_letter"),
    ("██████       ██     ████████  ██    ██   ██ ", "dot_letter"),
    ("▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒", "shade_med"),
    (
        "░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░",
        "shade_light",
    ),
];

/// Color palette for the logo - vibrant cyberpunk-ish gradient
const COLOR_SHADE_LIGHT: ratatui::style::Color = ratatui::style::Color::Rgb(80, 90, 120);
const COLOR_SHADE_MED: ratatui::style::Color = ratatui::style::Color::Rgb(60, 70, 100);
const COLOR_S_LETTER: ratatui::style::Color = ratatui::style::Color::Rgb(100, 180, 255);
const COLOR_V_LETTER: ratatui::style::Color = ratatui::style::Color::Rgb(120, 200, 255);
const COLOR_E_LETTER: ratatui::style::Color = ratatui::style::Color::Rgb(140, 220, 255);
const COLOR_N_LETTER: ratatui::style::Color = ratatui::style::Color::Rgb(160, 240, 255);
const COLOR_DOT_LETTER: ratatui::style::Color = ratatui::style::Color::Rgb(255, 200, 100);
const COLOR_SPACE: ratatui::style::Color = ratatui::style::Color::Rgb(30, 35, 45);

impl Widget for WelcomeScreen<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 40 || area.height < 30 {
            // Fallback to simple text if terminal is too small
            render_minimal(area, buf, self.model_name, self.mode_label, self.mode_style);
            return;
        }

        // Clear the entire pane area
        Clear.render(area, buf);

        // Build all lines
        let mut lines: Vec<Line<'static>> = Vec::new();

        // ── Logo ──────────────────────────────────────────────────────────────
        for (text, style_hint) in SVEN_LOGO {
            let line = render_logo_line(*style_hint, text);
            lines.push(line);
        }

        // ── Subtitle ───────────────────────────────────────────────────────────
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "github.com/swedishembedded/sven",
            Style::default().fg(TEXT_DIM),
        )]));

        // ── Model / mode ──────────────────────────────────────────────────────
        lines.push(Line::from(vec![
            Span::styled(
                self.model_name.to_string(),
                Style::default().fg(TEXT).add_modifier(Modifier::DIM),
            ),
            Span::styled("  ", Style::default().fg(SEPARATOR)),
            Span::styled(self.mode_label.to_string(), self.mode_style),
        ]));

        // ── Hints ─────────────────────────────────────────────────────────────
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "Enter a prompt to begin",
            Style::default().fg(TEXT_DIM),
        )]));
        lines.push(Line::from(vec![
            Span::styled("/model ", Style::default().fg(BAR_TOOL)),
            Span::styled("to switch model  ", Style::default().fg(TEXT_DIM)),
            Span::styled("/mode ", Style::default().fg(BAR_TOOL)),
            Span::styled("to switch mode", Style::default().fg(TEXT_DIM)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("F1 ", Style::default().fg(BAR_TOOL)),
            Span::styled("for key bindings  ", Style::default().fg(TEXT_DIM)),
            Span::styled("F4 ", Style::default().fg(BAR_TOOL)),
            Span::styled("to cycle mode", Style::default().fg(TEXT_DIM)),
        ]));

        // ── Center vertically and horizontally ────────────────────────────────
        let content_height = lines.len() as u16;
        let y_offset = area.y + area.height.saturating_sub(content_height) / 2;

        for (i, line) in lines.into_iter().enumerate() {
            let y = y_offset + i as u16;
            if y >= area.y + area.height {
                break;
            }
            Paragraph::new(line)
                .alignment(Alignment::Center)
                .render(Rect::new(area.x, y, area.width, 1), buf);
        }
    }
}

// ── Logo rendering helpers ────────────────────────────────────────────────────

fn render_logo_line(style_hint: &str, text: &str) -> Line<'static> {
    let color = match style_hint {
        "shade_light" => COLOR_SHADE_LIGHT,
        "shade_med" => COLOR_SHADE_MED,
        "s_letter" => COLOR_S_LETTER,
        "v_letter" => COLOR_V_LETTER,
        "e_letter" => COLOR_E_LETTER,
        "n_letter" => COLOR_N_LETTER,
        "dot_letter" => COLOR_DOT_LETTER,
        "space" => COLOR_SPACE,
        _ => TEXT_DIM,
    };
    Line::from(vec![Span::styled(
        text.to_string(),
        Style::default().fg(color),
    )])
}

/// Minimal fallback for very small terminals
fn render_minimal(
    area: Rect,
    buf: &mut Buffer,
    model_name: &str,
    mode_label: &str,
    mode_style: Style,
) {
    Clear.render(area, buf);

    let lines = vec![
        Line::from(vec![Span::styled(
            "sven.",
            Style::default().fg(BAR_AGENT).add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![Span::styled(
            "github.com/swedishembedded/sven",
            Style::default().fg(TEXT_DIM),
        )]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                model_name,
                Style::default().fg(TEXT).add_modifier(Modifier::DIM),
            ),
            Span::styled("  ", Style::default().fg(SEPARATOR)),
            Span::styled(mode_label, mode_style),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Enter a prompt to begin",
            Style::default().fg(TEXT_DIM),
        )]),
    ];

    let content_height = lines.len() as u16;
    let y_offset = area.y + area.height.saturating_sub(content_height) / 2;

    for (i, line) in lines.into_iter().enumerate() {
        let y = y_offset + i as u16;
        if y >= area.y + area.height {
            break;
        }
        Paragraph::new(line)
            .alignment(Alignment::Center)
            .render(Rect::new(area.x, y, area.width, 1), buf);
    }
}
