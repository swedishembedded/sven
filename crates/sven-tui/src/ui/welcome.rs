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

use super::theme::{BAR_AGENT, BAR_TOOL, BORDER_DIM, SEPARATOR, SE_BLUE, TEXT, TEXT_DIM};

/// Outer chip body — a noticeably lighter blue than SE_BLUE inner squares,
/// giving the logo clear two-tone depth: light shell → dark core.
const LOGO_OUTER: ratatui::style::Color = ratatui::style::Color::Rgb(110, 160, 240);

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
///
/// Every row is exactly 24 columns wide.  The 6 non-pin rows have a single
/// trailing space to match the pin rows (which have `─` extending one column
/// beyond the chip body on each side).  Equal widths ensure that
/// `Alignment::Center` positions every row at the same horizontal offset,
/// regardless of terminal width parity.
const LOGO: &[(&str, &str)] = &[
    //                         ←24 chars→
    ("    ╷    ╷    ╷    ╷    ", "border"), // trailing space
    (" ╔══╧════╧════╧════╧══╗ ", "outer"),  // trailing space
    (" ║   ╔════╗  ╔════╗   ║ ", "outer_inner"), // trailing space
    ("─╢   ║    ║  ║    ║   ╟─", "pin_outer_inner"),
    ("─╢   ╚════╝  ╚════╝   ╟─", "pin_outer_inner"),
    ("─╢                    ╟─", "pin_outer"),
    ("─╢   ╔════╗  ╔════╗   ╟─", "pin_outer_inner"),
    ("─╢   ║    ║  ║    ║   ╟─", "pin_outer_inner"),
    (" ║   ╚════╝  ╚════╝   ║ ", "outer_inner"), // trailing space
    (" ╚══╤════╤════╤════╤══╝ ", "outer"),       // trailing space
    ("    ╵    ╵    ╵    ╵    ", "border"),      // trailing space
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
                "outer" => Line::from(vec![Span::styled(
                    text.to_string(),
                    Style::default().fg(LOGO_OUTER),
                )]),
                "outer_inner" => render_outer_inner_line(text),
                "pin_outer" => render_pin_outer_line(text),
                "pin_outer_inner" => render_pin_outer_inner_line(text),
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

/// Non-pin row that contains inner boxes: outer body chars → LOGO_OUTER,
/// inner box chars (╔═╗╚╝ and content) → SE_BLUE.
fn render_outer_inner_line(text: &str) -> Line<'static> {
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

/// Pin row with no inner boxes: pin wire (─) → dim, connector (╢╟) and
/// chip body → LOGO_OUTER.
fn render_pin_outer_line(text: &str) -> Line<'static> {
    let spans: Vec<Span<'static>> = text
        .chars()
        .map(|ch| {
            let color = match ch {
                '─' => BORDER_DIM,
                _ => LOGO_OUTER,
            };
            Span::styled(ch.to_string(), Style::default().fg(color))
        })
        .collect();
    Line::from(spans)
}

/// Pin row that also contains inner boxes: pin wire (─) → dim, inner box
/// chars → SE_BLUE, everything else (outer body + connectors) → LOGO_OUTER.
fn render_pin_outer_inner_line(text: &str) -> Line<'static> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    let mut in_inner = false;

    for ch in &chars {
        let ch = *ch;
        if ch == '╔' || ch == '╚' {
            in_inner = true;
        }
        let color = match ch {
            '─' => BORDER_DIM,
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
