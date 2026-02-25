// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, Paragraph,
        Scrollbar, ScrollbarOrientation, ScrollbarState,
    },
    Frame,
};

use crate::overlay::completion::CompletionOverlay;

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
    pending_model: Option<&str>,
    pending_mode: Option<AgentMode>,
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

    // Show pending model/mode overrides when set.
    let pending_span: Span<'static> = match (pending_model, pending_mode) {
        (Some(m), Some(pm)) => Span::styled(
            format!(" {separator} next: {m} [{}]", pm),
            Style::default().fg(Color::Magenta),
        ),
        (Some(m), None) => Span::styled(
            format!(" {separator} next: {m}"),
            Style::default().fg(Color::Magenta),
        ),
        (None, Some(pm)) => Span::styled(
            format!(" {separator} next: [{}]", pm),
            Style::default().fg(Color::Magenta),
        ),
        (None, None) => Span::raw(""),
    };

    let line = Line::from(vec![
        Span::styled(
            format!(" {busy_indicator}"),
            Style::default().fg(if agent_busy { Color::Yellow } else { Color::Gray }),
        ),
        Span::styled(format!(" {model_name} "), Style::default().fg(Color::LightCyan)),
        Span::styled(separator, Style::default().fg(Color::Gray)),
        Span::styled(format!(" {mode_str} "), mode_style(mode)),
        Span::styled(separator, Style::default().fg(Color::Gray)),
        Span::styled(format!(" {ctx_str} "), ctx_style(context_pct)),
        cache_span,
        tool_span,
        pending_span,
        Span::styled(
            "  F1:help  ^w k:↑chat  ^w j:↓input  click/e:edit  ^Enter:submit  /:search  ^T:pager  F4:mode  ^c:quit",
            Style::default().fg(Color::White),
        ),
    ]);

    let para = Paragraph::new(line).style(Style::default().bg(Color::Black));
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

    // Never re-wrap content here. In the Neovim path the lines are exact grid
    // rows from the bridge.  In the ratatui path the markdown renderer already
    // wraps to `effective_width` (chat inner width minus the 2-column bar), so
    // a second Ratatui-level wrap would push bar characters onto the next visual
    // row and corrupt `segment_line_ranges`, causing wrong click targets.
    let para = Paragraph::new(visible);
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
///
/// The input is rendered as a properly scrollable multi-line text area:
/// * Text is wrapped by display columns (respecting wide / multibyte chars).
/// * A vertical scrollbar appears on the right when the content is taller
///   than the visible area.
/// * The terminal cursor is placed at the exact column/row of `cursor_pos`
///   (a UTF-8 byte index), accounting for wrapping and the scroll offset.
/// What kind of content the input box is currently editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEditMode {
    /// Normal message composition.
    Normal,
    /// Editing an existing chat-history segment.
    Segment,
    /// Editing a pending queue item.
    Queue,
}

pub fn draw_input(
    frame: &mut Frame,
    area: Rect,
    content: &str,
    cursor_pos: usize,
    scroll_offset: usize,
    focused: bool,
    ascii: bool,
    edit_mode: InputEditMode,
) {
    let title: String = match edit_mode {
        InputEditMode::Queue   => "Edit queue  [Enter:update  Esc:cancel]".to_string(),
        InputEditMode::Segment => "Edit  [Enter:confirm  Esc:cancel]".to_string(),
        InputEditMode::Normal  => "Input  [Enter:send  Shift+Enter:newline  ^w k:↑chat]".to_string(),
    };

    let block = pane_block(&title, focused, ascii);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    use crate::input_wrap::wrap_content;

    let visible_height = inner.height as usize;

    // First pass with full width to decide whether a scrollbar is needed.
    let probe = wrap_content(content, inner.width as usize, cursor_pos);
    let needs_scrollbar = probe.lines.len() > visible_height;

    // When a scrollbar is shown, the text area is 1 column narrower.
    let text_width = if needs_scrollbar && inner.width > 1 {
        inner.width - 1
    } else {
        inner.width
    };

    // Recompute with the final text width (only when it changed).
    let wrap = if needs_scrollbar && inner.width > 1 {
        wrap_content(content, text_width as usize, cursor_pos)
    } else {
        probe
    };

    let total_lines = wrap.lines.len();
    let scroll = scroll_offset.min(total_lines.saturating_sub(visible_height));

    // Text area (may be narrower when scrollbar is shown).
    let text_area = Rect::new(inner.x, inner.y, text_width, inner.height);

    // Render the visible slice of wrapped lines (no Paragraph-level wrap
    // because we handle wrapping ourselves).
    let visible: Vec<Line<'static>> = wrap
        .lines
        .iter()
        .skip(scroll)
        .take(visible_height)
        .map(|l| Line::from(l.clone()))
        .collect();
    frame.render_widget(Paragraph::new(visible), text_area);

    // Scrollbar (only when content overflows).
    if needs_scrollbar && inner.width > 1 {
        let sb_area = Rect::new(inner.x + text_width, inner.y, 1, inner.height);
        let mut sb_state = ScrollbarState::new(total_lines)
            .position(scroll)
            .viewport_content_length(visible_height);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None),
            sb_area,
            &mut sb_state,
        );
    }

    // Terminal cursor — only placed when focused and cursor row is visible.
    if focused {
        let cursor_row = wrap.cursor_row;
        if cursor_row >= scroll && cursor_row < scroll + visible_height {
            let vis_row = (cursor_row - scroll) as u16;
            // Clamp column to the text area width to avoid writing past it.
            let col = (wrap.cursor_col as u16).min(text_area.width.saturating_sub(1));
            frame.set_cursor_position((text_area.x + col, text_area.y + vis_row));
        }
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
        Line::from(" d        Delete queued message at top of chat view"),
        Line::from(" click    Click any message to edit it (ratatui mode)"),
        Line::from("           Live preview as you type; Enter submits"),
        Line::from("           Submitting discards later conversation"),
        Line::from("           Esc to cancel and restore original"),
        Line::from(" click    Toggle tool call / thought collapse"),
        Line::from("           (click again to expand)"),
        Line::from(" Up/Down  Move cursor up/down a line in input box"),
        Line::from(" PgUp/Dn  Scroll input box by a page"),
        Line::from(" scroll   Mouse wheel over input scrolls input box"),
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

/// Draw the queue panel showing pending messages above the input box.
///
/// `items` — the queue contents (in order).
/// `selected` — the currently highlighted row index, if any.
/// `editing` — the row index currently being edited, if any.
/// `focused` — whether the queue pane has keyboard focus.
pub fn draw_queue_panel(
    frame: &mut Frame,
    area: Rect,
    items: &[(String, Option<String>, Option<AgentMode>)],
    selected: Option<usize>,
    editing: Option<usize>,
    focused: bool,
    ascii: bool,
) {
    if area.height == 0 || items.is_empty() {
        return;
    }
    let count = items.len();
    let title = format!("Queue  [{count}]  [↑↓:select  e:edit  d:delete  s:submit  f:force-submit  Esc:close]");
    let block = pane_block(&title, focused, ascii);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    let visible: Vec<Line<'static>> = items
        .iter()
        .enumerate()
        .take(inner.height as usize)
        .map(|(i, (text, model_ov, mode_ov))| {
            let is_selected = selected == Some(i);
            let is_editing  = editing  == Some(i);

            let num_span = Span::styled(
                format!(" {} ", i + 1),
                if is_selected || is_editing {
                    Style::default().fg(Color::Black).bg(Color::LightBlue).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            );

            // Build override badge when present (e.g. "[gpt-4o, research]")
            let badge: String = match (model_ov.as_deref(), mode_ov) {
                (Some(m), Some(mo)) => format!("[{m}, {mo}] "),
                (Some(m), None)     => format!("[{m}] "),
                (None, Some(mo))    => format!("[{mo}] "),
                (None, None)        => String::new(),
            };

            // Truncate preview to fit the inner width minus the number badge and badge.
            let badge_len = badge.chars().count();
            let max_text = inner.width.saturating_sub(6 + badge_len as u16) as usize;
            let preview: String = text.lines().next().unwrap_or("").chars().take(max_text).collect();
            let ellipsis = if text.len() > preview.len() + 1 || text.contains('\n') { "…" } else { "" };
            let text_content = format!(" {preview}{ellipsis}");

            let text_span = Span::styled(
                text_content,
                if is_editing {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::ITALIC)
                } else if is_selected {
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                },
            );

            let badge_span = if badge.is_empty() {
                Span::raw("")
            } else {
                Span::styled(badge, Style::default().fg(Color::Magenta))
            };

            Line::from(vec![num_span, badge_span, text_span])
        })
        .collect();

    frame.render_widget(Paragraph::new(visible), inner);
}

/// Draw the completion overlay for slash commands.
///
/// The overlay is positioned above `input_pane` if there is room, otherwise
/// below it.  The selected item is highlighted; descriptions are shown in a
/// muted colour.  A scroll indicator is appended when there are more items
/// than `max_visible`.
pub fn draw_completion_overlay(
    frame: &mut Frame,
    input_pane: Rect,
    overlay: &CompletionOverlay,
    ascii: bool,
) {
    if overlay.items.is_empty() {
        return;
    }

    let visible = overlay.visible_items();
    let item_count = visible.len();

    // Overlay width: at least 40, at most 70, but not wider than terminal
    let width = 70u16.min(input_pane.width.max(40));

    // Height: item rows + top/bottom border
    let height = (item_count as u16 + 2).min(frame.area().height.saturating_sub(2));

    // Prefer above the input pane; fall back to below
    let y = if input_pane.y >= height {
        input_pane.y - height
    } else {
        input_pane.y + input_pane.height
    };

    let x = input_pane.x;
    let area = Rect::new(
        x.min(frame.area().width.saturating_sub(width)),
        y.min(frame.area().height.saturating_sub(height)),
        width,
        height,
    );

    frame.render_widget(Clear, area);

    let bt = border_type(ascii);
    let total = overlay.items.len();
    let scroll_indicator = if total > overlay.max_visible {
        format!(
            " [{}/{}]",
            overlay.selected + 1,
            total,
        )
    } else {
        String::new()
    };

    let title = format!(" Commands{scroll_indicator} ");
    let block = Block::default()
        .title(Span::styled(title, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
        .borders(Borders::ALL)
        .border_type(bt)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Black));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let max_val_width = (inner.width as usize).saturating_sub(2);

    let lines: Vec<Line<'static>> = visible
        .iter()
        .enumerate()
        .map(|(vis_idx, item)| {
            let actual_idx = overlay.scroll_offset + vis_idx;
            let is_selected = actual_idx == overlay.selected;

            // Truncate display to fit
            let display: String = if item.display.is_empty() {
                item.value.clone()
            } else {
                item.display.clone()
            };

            // Build value + optional description
            let desc_str = item.description.as_deref().unwrap_or("");
            let sep = if desc_str.is_empty() { "" } else { "  " };
            let full = format!("{}{}{}", display, sep, desc_str);
            let truncated: String = full.chars().take(max_val_width).collect();

            if is_selected {
                Line::from(Span::styled(
                    format!(" {truncated} "),
                    Style::default()
                        .bg(Color::Cyan)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                // Split display from description for colour differentiation
                let disp_chars: String = display.chars().take(max_val_width).collect();
                let remaining = max_val_width.saturating_sub(disp_chars.len());

                if !desc_str.is_empty() && remaining > 3 {
                    let short_desc: String = desc_str.chars().take(remaining.saturating_sub(2)).collect();
                    Line::from(vec![
                        Span::styled(
                            format!(" {disp_chars}"),
                            Style::default().fg(Color::White),
                        ),
                        Span::styled(
                            format!("  {short_desc}"),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ])
                } else {
                    Line::from(Span::styled(
                        format!(" {disp_chars}"),
                        Style::default().fg(Color::Gray),
                    ))
                }
            }
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
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
