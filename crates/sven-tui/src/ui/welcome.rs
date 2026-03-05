// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Welcome screen — shown when the chat is empty and the agent is idle.

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use super::theme::{
    BAR_AGENT, BAR_TOOL, BORDER_DIM, BORDER_FOCUS, SEPARATOR, SE_BLUE, TEXT, TEXT_DIM,
};

/// Outer chip body — lighter blue so it reads as a distinct "shell" around the
/// darker inner squares, giving the logo depth without the yellow/blue clash.
const LOGO_OUTER: ratatui::style::Color = BORDER_FOCUS;

/// Welcome screen rendered when chat is empty and agent is idle.
pub struct WelcomeScreen<'a> {
    /// Current model name shown below the logo.
    pub model_name: &'a str,
    /// Current mode label (Research / Plan / Agent).
    pub mode_label: &'a str,
    /// Mode color style.
    pub mode_style: Style,
}

/// Compact chip logo lines styled with SE colors.
/// Each entry is (text, yellow_columns_bitmap) where columns with a 1-bit
/// are rendered in SE_YELLOW, columns with 0 in SE_BLUE or border color.
const LOGO: &[(&str, &str)] = &[
    ("    ╷    ╷    ╷    ╷   ", "border"),
    (" ╔══╧════╧════╧════╧══╗", "yellow"),
    (" ║  ╔════╗  ╔════╗    ║", "yellow_blue"),
    ("─╢  ║    ║  ║    ║    ╟─", "pin_yellow_blue"),
    ("─╢  ╚════╝  ╚════╝    ╟─", "pin_yellow_blue"),
    ("─╢                    ╟─", "pin_yellow"),
    ("─╢  ╔════╗  ╔════╗    ╟─", "pin_yellow_blue"),
    ("─╢  ║    ║  ║    ║    ╟─", "pin_yellow_blue"),
    (" ║  ╚════╝  ╚════╝    ║", "yellow_blue"),
    (" ╚══╤════╤════╤════╤══╝", "yellow"),
    ("    ╵    ╵    ╵    ╵   ", "border"),
];

impl Widget for WelcomeScreen<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 30 || area.height < 8 {
            return;
        }

        // Build all lines to center vertically.
        let mut lines: Vec<Line<'static>> = Vec::new();

        // ── Logo ──────────────────────────────────────────────────────────────
        for (text, style_hint) in LOGO {
            let line = match *style_hint {
                "border" => Line::from(vec![Span::styled(
                    text.to_string(),
                    Style::default().fg(BORDER_DIM),
                )]),
                "yellow" => Line::from(vec![Span::styled(
                    text.to_string(),
                    Style::default().fg(LOGO_OUTER),
                )]),
                "yellow_blue" => render_yellow_blue_line(text),
                "pin_yellow" => render_pin_yellow_line(text),
                "pin_yellow_blue" => render_pin_yellow_blue_line(text),
                _ => Line::from(text.to_string()),
            };
            lines.push(line);
        }

        // ── Title ─────────────────────────────────────────────────────────────
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                "sven",
                Style::default().fg(BAR_AGENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  ·  github.com/swedishembedded/sven",
                Style::default().fg(TEXT_DIM),
            ),
        ]));

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

fn render_yellow_blue_line(text: &str) -> Line<'static> {
    // Outer body in LOGO_OUTER (light blue), inner squares in SE_BLUE (dark blue).
    let mut spans = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut in_inner = false;

    while i < chars.len() {
        let ch = chars[i];
        if ch == '╔' || ch == '╚' {
            in_inner = true;
        }
        if ch == '╗' || ch == '╝' {
            spans.push(Span::styled(ch.to_string(), Style::default().fg(SE_BLUE)));
            in_inner = false;
            i += 1;
            continue;
        }
        let color = if in_inner { SE_BLUE } else { LOGO_OUTER };
        spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
        i += 1;
    }
    Line::from(spans)
}

fn render_pin_yellow_line(text: &str) -> Line<'static> {
    // Pin chars (─╢╟) are dim, body (║ and spaces) is LOGO_OUTER (light blue).
    let spans: Vec<Span<'static>> = text
        .chars()
        .map(|ch| {
            let color = match ch {
                '─' | '╢' | '╟' => BORDER_DIM,
                _ => LOGO_OUTER,
            };
            Span::styled(ch.to_string(), Style::default().fg(color))
        })
        .collect();
    Line::from(spans)
}

fn render_pin_yellow_blue_line(text: &str) -> Line<'static> {
    // Pins (─╢╟) are dim; inside inner boxes is SE_BLUE; outer body is LOGO_OUTER.
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    let mut in_inner = false;

    for ch in &chars {
        let ch = *ch;
        if ch == '╔' || ch == '╚' {
            in_inner = true;
        }
        let color = match ch {
            '─' | '╢' | '╟' => BORDER_DIM,
            c if in_inner && c != '╗' && c != '╝' => SE_BLUE,
            _ => LOGO_OUTER,
        };
        spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
        if ch == '╗' || ch == '╝' {
            in_inner = false;
        }
    }
    Line::from(spans)
}
