use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};

use sven_config::AgentMode;

use crate::markdown::StyledLines;

// ── Character sets ────────────────────────────────────────────────────────────

/// Characters used for the status bar separator.
fn sep(ascii: bool) -> &'static str {
    if ascii { "|" } else { "│" }
}

/// Busy indicator character.
fn busy_char(ascii: bool) -> &'static str {
    if ascii { "* " } else { "⠿ " }
}

/// Horizontal rule character (repeated).
fn rule_char(ascii: bool) -> char {
    if ascii { '-' } else { '─' }
}

/// Block-quote prefix.
fn blockquote_prefix(ascii: bool) -> &'static str {
    if ascii { "> " } else { "▌ " }
}

/// List-item bullet.
fn bullet(ascii: bool) -> &'static str {
    if ascii { "- " } else { "• " }
}

/// `BorderType` based on ascii flag.
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
    ascii: bool,
) {
    let busy_indicator = if agent_busy { busy_char(ascii) } else { "  " };
    let mode_str = mode.to_string();
    let ctx_str = format!("ctx:{context_pct}%");
    let separator = sep(ascii);

    let line = Line::from(vec![
        Span::styled(
            format!(" {busy_indicator}"),
            Style::default().fg(if agent_busy { Color::Yellow } else { Color::DarkGray }),
        ),
        Span::styled(
            format!(" {model_name} "),
            Style::default().fg(Color::LightCyan),
        ),
        Span::styled(separator, Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(" {mode_str} "),
            mode_style(mode),
        ),
        Span::styled(separator, Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(" {ctx_str} "),
            ctx_style(context_pct),
        ),
        Span::styled(
            "  F1:help  ^w+k:chat  ^w+j:input  /:search  F4:mode  ^c:quit",
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    let para = Paragraph::new(line)
        .style(Style::default().bg(Color::DarkGray));
    frame.render_widget(para, area);
}

/// Draw the chat / markdown scroll pane.
pub fn draw_chat(
    frame: &mut Frame,
    area: Rect,
    lines: &StyledLines,
    scroll_offset: u16,
    focused: bool,
    ascii: bool,
) {
    let block = pane_block("Chat", focused, ascii);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let visible: Vec<Line<'static>> = lines
        .iter()
        .skip(scroll_offset as usize)
        .take(inner.height as usize)
        .cloned()
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

/// Draw the search bar (shown when in search mode).
pub fn draw_search(
    frame: &mut Frame,
    area: Rect,
    query: &str,
    match_count: usize,
    current_match: usize,
) {
    let text = if match_count == 0 {
        format!("/{query}  (no matches)  Esc:close")
    } else {
        format!(
            "/{query}  ({}/{})  n:next  N:prev  Esc:close",
            current_match + 1,
            match_count,
        )
    };

    let para = Paragraph::new(text)
        .style(Style::default().fg(Color::Yellow).bg(Color::Black));
    frame.render_widget(para, area);
}

/// Draw a help overlay.
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
        Line::from(" ^u/^d    Page up/down in chat"),
        Line::from(" g / G    Jump to top/bottom"),
        Line::from(" scroll   Mouse-wheel scrolls chat"),
        Line::from(" /        Open search in chat"),
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
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let overlay = Rect::new(x, y, width, height);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(bt)
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(overlay);
    frame.render_widget(block, overlay);
    frame.render_widget(Paragraph::new(help_text), inner);
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
                Style::default().add_modifier(Modifier::BOLD).fg(Color::LightBlue)
            } else {
                Style::default().fg(Color::Gray)
            },
        ))
        .borders(Borders::ALL)
        .border_type(border_type(ascii))
        .border_style(border_style)
}

/// Expose helpers for `markdown.rs` so they render consistently.
pub(crate) fn md_rule_char(ascii: bool) -> char   { rule_char(ascii) }
pub(crate) fn md_blockquote(ascii: bool) -> &'static str { blockquote_prefix(ascii) }
pub(crate) fn md_bullet(ascii: bool) -> &'static str     { bullet(ascii) }

fn mode_style(mode: AgentMode) -> Style {
    match mode {
        AgentMode::Research => Style::default().fg(Color::LightGreen),
        AgentMode::Plan => Style::default().fg(Color::LightYellow),
        AgentMode::Agent => Style::default().fg(Color::LightMagenta),
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
