// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use sven_config::AgentMode;

use crate::markdown::StyledLines;
use crate::pager::{highlight_match_in_line, tint_match_line};

// ── Character sets ────────────────────────────────────────────────────────────

fn sep(ascii: bool) -> &'static str {
    if ascii { "|" } else { "│" }
}
fn busy_char(ascii: bool) -> &'static str {
    if ascii { "* " } else { "⠿ " }
}
fn rule_char(ascii: bool) -> char {
    if ascii { '-' } else { '─' }
}
fn blockquote_prefix(ascii: bool) -> &'static str {
    if ascii { "> " } else { "▌ " }
}
fn bullet(ascii: bool) -> &'static str {
    if ascii { "- " } else { "• " }
}
fn border_type(ascii: bool) -> BorderType {
    if ascii { BorderType::Plain } else { BorderType::Rounded }
}

// ── Draw functions ────────────────────────────────────────────────────────────

/// Draw the status bar at the top.
#[allow(clippy::too_many_arguments)]
pub fn draw_status(
    frame: &mut Frame,
    area: Rect,
    model_name: &str,
    mode: AgentMode,
    context_pct: u8,
    cache_hit_pct: u8,
    agent_busy: bool,
    current_tool: Option<&str>,
    ascii: bool,
) {
    let busy_indicator = if agent_busy { busy_char(ascii) } else { "  " };
    let mode_str = mode.to_string();
    let ctx_str = format!("ctx:{context_pct}%");
    let separator = sep(ascii);

    let tool_span: Span<'static> = if let Some(t) = current_tool {
        Span::styled(
            format!(" ⚙ {t} "),
            Style::default().fg(Color::Yellow),
        )
    } else {
        Span::raw("")
    };

    // Show cache hit rate only when the provider is reporting cached tokens.
    let cache_span: Span<'static> = if cache_hit_pct > 0 {
        Span::styled(
            format!(" {separator} cache:{cache_hit_pct}%"),
            Style::default().fg(Color::Green),
        )
    } else {
        Span::raw("")
    };

    let line = Line::from(vec![
        Span::styled(
            format!(" {busy_indicator}"),
            Style::default().fg(if agent_busy { Color::Yellow } else { Color::DarkGray }),
        ),
        Span::styled(format!(" {model_name} "), Style::default().fg(Color::LightCyan)),
        Span::styled(separator, Style::default().fg(Color::DarkGray)),
        Span::styled(format!(" {mode_str} "), mode_style(mode)),
        Span::styled(separator, Style::default().fg(Color::DarkGray)),
        Span::styled(format!(" {ctx_str} "), ctx_style(context_pct)),
        cache_span,
        tool_span,
        Span::styled(
            "  F1:help  ^w k:↑chat  ^w j:↓input  click/e:edit  ^Enter:submit  /:search  ^T:pager  F4:mode  ^c:quit",
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    let para = Paragraph::new(line).style(Style::default().bg(Color::DarkGray));
    frame.render_widget(para, area);
}

/// Draw the chat / markdown scroll pane with optional search highlighting.
#[allow(clippy::too_many_arguments)]
pub fn draw_chat(
    frame: &mut Frame,
    area: Rect,
    lines: &StyledLines,
    scroll_offset: u16,
    focused: bool,
    ascii: bool,
    search_query: &str,
    search_matches: &[usize],
    search_current: usize,
    search_regex: Option<&regex::Regex>,
    nvim_cursor: Option<(u16, u16)>,
) {
    let block = pane_block("Chat", focused, ascii);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Build a set for O(1) per-line match lookup instead of scanning the Vec.
    let match_set: std::collections::HashSet<usize> =
        search_matches.iter().copied().collect();
    let current_match_line = search_matches.get(search_current).copied();

    let visible: Vec<Line<'static>> = lines
        .iter()
        .enumerate()
        .skip(scroll_offset as usize)
        .take(inner.height as usize)
        .map(|(i, line)| {
            let is_current = !search_query.is_empty()
                && current_match_line == Some(i);
            let is_other = !search_query.is_empty()
                && !is_current
                && match_set.contains(&i);
            if is_current {
                highlight_match_in_line(line.clone(), search_query, search_regex)
            } else if is_other {
                tint_match_line(line.clone())
            } else {
                line.clone()
            }
        })
        .collect();

    // When Neovim is the content source (nvim_cursor is Some), the lines are
    // already grid rows of exactly bridge.width columns — never rewrap them.
    // Ratatui's `Wrap` can miscount unicode wide-char display widths and add
    // an unexpected extra visual row, which shifts every subsequent row down
    // by 1 and clips the bottom of the grid from view.
    //
    // For the non-Neovim fallback (chat_lines from markdown renderer), we
    // keep wrapping on so that unusually long words are not hard-truncated.
    let para = if nvim_cursor.is_some() {
        Paragraph::new(visible)
    } else {
        Paragraph::new(visible).wrap(Wrap { trim: false })
    };
    frame.render_widget(para, inner);

    // Draw Neovim cursor if provided and focused
    if focused {
        if let Some((cursor_row, cursor_col)) = nvim_cursor {
            // cursor_row is the 0-indexed row in the Neovim grid; scroll_offset
            // is 0 when Neovim owns the viewport, so visible_row == cursor_row.
            if let Some(visible_row) = cursor_row.checked_sub(scroll_offset) {
                if (visible_row as usize) < inner.height as usize {
                    frame.set_cursor_position((
                        inner.x + cursor_col.min(inner.width.saturating_sub(1)),
                        inner.y + visible_row,
                    ));
                }
            }
        }
    }
}

/// Draw the input box at the bottom.
#[allow(clippy::too_many_arguments)]
pub fn draw_input(
    frame: &mut Frame,
    area: Rect,
    content: &str,
    cursor_pos: usize,
    focused: bool,
    queued_steps: usize,
    ascii: bool,
    edit_mode: bool,
) {
    let title = if edit_mode {
        "Edit  [Enter:confirm  Esc:cancel]".into()
    } else if queued_steps > 0 {
        format!("Input  [{queued_steps} queued]")
    } else {
        "Input  [Enter:send  Shift+Enter:newline  ^w k:↑chat]".into()
    };

    let block = pane_block(&title, focused, ascii);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let para = Paragraph::new(content).wrap(Wrap { trim: false });
    frame.render_widget(para, inner);

    if focused {
        let col = (cursor_pos % inner.width as usize) as u16;
        let row = (cursor_pos / inner.width as usize) as u16;
        frame.set_cursor_position((inner.x + col, inner.y + row));
    }
}

/// Draw the inline search bar.
pub fn draw_search(
    frame: &mut Frame,
    area: Rect,
    query: &str,
    match_count: usize,
    current_match: usize,
) {
    let text = if match_count == 0 {
        format!("/{query}  (no matches)  n:next  N:prev  Esc:close")
    } else {
        format!(
            "/{query}  ({}/{match_count})  n:next  N:prev  Esc:close",
            current_match + 1,
        )
    };
    let para = Paragraph::new(text)
        .style(Style::default().fg(Color::Yellow).bg(Color::Black));
    frame.render_widget(para, area);
}

/// Draw the help overlay.
pub fn draw_help(frame: &mut Frame, ascii: bool) {
    let area = frame.area();
    let bt = border_type(ascii);

    let help_text = vec![
        Line::from(Span::styled(
            "  Sven Key Bindings",
            Style::default().add_modifier(Modifier::BOLD).fg(Color::LightBlue),
        )),
        Line::default(),
        Line::from(" ^w k     Focus chat pane"),
        Line::from(" ^w j     Focus input pane"),
        Line::from(" j/k/J/K  Scroll chat down/up"),
        Line::from(" ^u/^d    Half-page up/down"),
        Line::from(" g / G    Jump to top/bottom"),
        Line::from(" e        Edit message at top of chat view"),
        Line::from(" click    Click any message to edit it (ratatui mode)"),
        Line::from("           Live preview as you type; Enter submits"),
        Line::from("           Submitting discards later conversation"),
        Line::from("           Esc to cancel and restore original"),
        Line::from(" click    Toggle tool call / thought collapse"),
        Line::from("           (click again to expand)"),
        Line::from(" ^T       Open full-screen pager"),
        Line::from(" /        Open search bar"),
        Line::from(" n / N    Next/prev search match"),
        Line::from(" Enter    Submit input (confirm edit in edit mode)"),
        Line::from(" S+Enter  Insert newline (^J if S+Enter not available)"),
        Line::from(" F4       Cycle agent mode"),
        Line::from(" ^c       Interrupt agent / quit"),
        Line::from(" F1       Toggle this help"),
        Line::default(),
        Line::from(Span::styled(
            " Press any key to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let width = 60u16.min(area.width);
    let height = (help_text.len() as u16 + 2).min(area.height);
    let x = area.width.saturating_sub(width) / 2;
    let y = area.height.saturating_sub(height) / 2;
    let overlay = Rect::new(x, y, width, height);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(bt)
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(overlay);
    frame.render_widget(block, overlay);
    frame.render_widget(Paragraph::new(help_text), inner);
}

/// Draw the ask-question modal.
///
/// Shows all questions at once, with the current one highlighted.
/// The answer input field is shown at the bottom.
#[allow(clippy::too_many_arguments)]
pub fn draw_question_modal(
    frame: &mut Frame,
    questions: &[sven_tools::Question],
    current_q: usize,
    selected_options: &[usize],
    other_selected: bool,
    other_input: &str,
    other_cursor: usize,
    ascii: bool,
) {
    let area = frame.area();
    let bt = border_type(ascii);

    // Modal width: up to 80 columns, leaving 4 cols margin each side.
    let modal_w = (area.width.saturating_sub(8)).clamp(20, 80);

    // Calculate rows needed: question prompt + options + "Other" line + hint
    let current_question = questions.get(current_q);
    let content_rows = if let Some(q) = current_question {
        // 1 for prompt + options count + 1 for "Other" + 1 blank + 1 hint
        1 + q.options.len() as u16 + 3
    } else {
        5
    };

    let modal_h = (content_rows + 2).min(area.height.saturating_sub(2)).max(10);
    let x = area.width.saturating_sub(modal_w) / 2;
    let y = area.height.saturating_sub(modal_h) / 2;
    let modal_area = Rect::new(x, y, modal_w, modal_h);

    // Clear background
    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .title(Span::styled(
            " Questions from agent ",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(bt)
        .border_style(Style::default().fg(Color::Yellow))
        .style(Style::default().bg(Color::Black));

    let inner = block.inner(modal_area);
    frame.render_widget(block, modal_area);

    if let Some(q) = current_question {
        let mut lines: Vec<Line> = Vec::new();

        // Question prompt
        lines.push(Line::from(Span::styled(
            format!("Q{}/{}: {}", current_q + 1, questions.len(), q.prompt),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from("")); // blank

        // Options with checkbox/radio indicators
        let checkbox = if q.allow_multiple { "☐" } else { "○" };
        let checked = if q.allow_multiple { "☑" } else { "●" };
        
        for (i, opt) in q.options.iter().enumerate() {
            let is_selected = selected_options.contains(&i);
            let indicator = if is_selected { checked } else { checkbox };
            let style = if is_selected {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::White)
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("{} ", indicator), style),
                Span::styled(format!("{}. {}", i + 1, opt), style),
            ]));
        }

        // "Other" option
        let other_indicator = if other_selected { checked } else { checkbox };
        let other_style = if other_selected {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{} ", other_indicator), other_style),
            Span::styled(format!("{}. Other: ", q.options.len() + 1), other_style),
            Span::styled(other_input, Style::default().fg(Color::Cyan)),
        ]));

        lines.push(Line::from("")); // blank

        // Hint
        let hint = if q.allow_multiple {
            "1-9: toggle   O: Other   Enter: submit   Esc: cancel"
        } else {
            "1-9: select   O: Other   Enter: submit   Esc: cancel"
        };
        lines.push(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        )));

        frame.render_widget(
            Paragraph::new(lines),
            inner,
        );

        // Cursor for "Other" input when selected
        if other_selected && !other_input.is_empty() {
            // Position cursor after "Other: " text
            let other_line_y = inner.y + 2 + q.options.len() as u16 + 1;
            let other_prefix_len = format!("  {} {}. Other: ", checkbox, q.options.len() + 1).len();
            let cursor_x = inner.x + other_prefix_len as u16 + other_cursor as u16;
            if cursor_x < inner.x + inner.width {
                frame.set_cursor_position((cursor_x, other_line_y));
            }
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

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

pub(crate) fn md_rule_char(ascii: bool) -> char { rule_char(ascii) }
pub(crate) fn md_blockquote(ascii: bool) -> &'static str { blockquote_prefix(ascii) }
pub(crate) fn md_bullet(ascii: bool) -> &'static str { bullet(ascii) }

fn mode_style(mode: AgentMode) -> Style {
    match mode {
        AgentMode::Research => Style::default().fg(Color::LightGreen),
        AgentMode::Plan     => Style::default().fg(Color::LightYellow),
        AgentMode::Agent    => Style::default().fg(Color::LightMagenta),
    }
}

fn ctx_style(pct: u8) -> Style {
    if pct >= 90 {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if pct >= 70 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Green)
    }
}
