use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
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
pub fn draw_status(
    frame: &mut Frame,
    area: Rect,
    model_name: &str,
    mode: AgentMode,
    context_pct: u8,
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
        tool_span,
        Span::styled(
            "  F1:help  ^w+k:chat  ^w+j:input  /:search  ^T:pager  F4:mode  ^c:quit",
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
) {
    let block = pane_block("Chat", focused, ascii);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let visible: Vec<Line<'static>> = lines
        .iter()
        .enumerate()
        .skip(scroll_offset as usize)
        .take(inner.height as usize)
        .map(|(i, line)| {
            let is_current = !search_query.is_empty()
                && search_matches.get(search_current) == Some(&i);
            let is_other = !search_query.is_empty()
                && !is_current
                && search_matches.contains(&i);
            if is_current {
                highlight_match_in_line(line.clone(), search_query, search_regex)
            } else if is_other {
                tint_match_line(line.clone())
            } else {
                line.clone()
            }
        })
        .collect();

    let para = Paragraph::new(visible).wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

/// Draw the input box at the bottom.
pub fn draw_input(
    frame: &mut Frame,
    area: Rect,
    content: &str,
    cursor_pos: usize,
    focused: bool,
    queued_steps: usize,
    ascii: bool,
) {
    let title = if queued_steps > 0 {
        format!("Input  [{queued_steps} queued]")
    } else {
        "Input  [Enter:send  Shift+Enter:newline  ^w+k:chat]".into()
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
        Line::from(" j/k      Scroll chat down/up"),
        Line::from(" ^u/^d    Half-page up/down"),
        Line::from(" g / G    Jump to top/bottom"),
        Line::from(" ^T       Open full-screen pager"),
        Line::from(" /        Open search bar"),
        Line::from(" n / N    Next/prev search match"),
        Line::from(" Enter    Submit input"),
        Line::from(" S+Enter  Insert newline"),
        Line::from(" F4       Cycle agent mode"),
        Line::from(" ^c       Interrupt agent / quit"),
        Line::from(" F1       Toggle this help"),
        Line::default(),
        Line::from(Span::styled(
            " Press any key to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let width = 52u16.min(area.width);
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
pub fn draw_question_modal(
    frame: &mut Frame,
    questions: &[String],
    current_q: usize,
    answer_buf: &str,
    answer_cursor: usize,
    ascii: bool,
) {
    let area = frame.area();
    let bt = border_type(ascii);

    // Compute modal size: header + questions + blank + answer + footer
    let q_lines = questions.len() as u16;
    let modal_h = (q_lines + 6).min(area.height.saturating_sub(4));
    let modal_w = (area.width.saturating_sub(8)).min(80);
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

    // Split inner area: questions on top, input + hint at bottom
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(q_lines),
            Constraint::Length(1), // blank
            Constraint::Length(1), // "Answer X/N:"
            Constraint::Length(1), // input
            Constraint::Length(1), // hint
        ])
        .split(inner);

    // Questions
    let q_text: Vec<Line<'static>> = questions
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let is_current = i == current_q;
            let prefix = if is_current { "▶ " } else { "  " };
            let num_style = if is_current {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let text_style = if is_current {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            Line::from(vec![
                Span::styled(format!("{prefix}{}. ", i + 1), num_style),
                Span::styled(q.clone(), text_style),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(q_text), chunks[0]);

    // "Answer X/N:" label
    let label = format!(
        "Answer {}/{}: ",
        current_q + 1,
        questions.len()
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            label,
            Style::default().fg(Color::Yellow),
        ))),
        chunks[2],
    );

    // Input field
    let input_area = chunks[3];
    frame.render_widget(
        Paragraph::new(answer_buf).style(Style::default().fg(Color::White)),
        input_area,
    );
    // Cursor
    let col = (answer_cursor % input_area.width as usize) as u16;
    let row = (answer_cursor / input_area.width as usize) as u16;
    frame.set_cursor_position((input_area.x + col, input_area.y + row));

    // Hint
    frame.render_widget(
        Paragraph::new(Span::styled(
            "Enter: submit   Esc: cancel",
            Style::default().fg(Color::DarkGray),
        )),
        chunks[4],
    );
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
