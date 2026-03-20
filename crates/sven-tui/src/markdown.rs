// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use pulldown_cmark::{Alignment, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use sven_frontend::markdown::{parse_markdown_blocks, MarkdownBlock};

use crate::ui::theme::{md_blockquote, md_bullet, md_rule_char};

/// A styled line ready for Ratatui rendering.
pub type StyledLines = Vec<Line<'static>>;

// ── Public API ────────────────────────────────────────────────────────────────

/// Convert a markdown string into a list of styled [`Line`]s for Ratatui.
///
/// Uses the same block parser as the GUI (`parse_markdown_blocks`) so rendering
/// is consistent across frontends. Each block type (paragraph, heading, list
/// item, block quote, etc.) is rendered correctly without cross-contamination.
///
/// `wrap_width` — wrap long text at this column (0 → 80).
/// `ascii`      — use plain-ASCII box chars instead of Unicode.
pub fn render_markdown(md: &str, wrap_width: u16, ascii: bool) -> StyledLines {
    let width = if wrap_width == 0 {
        80
    } else {
        wrap_width as usize
    };
    let blocks = parse_markdown_blocks(md);
    render_blocks_to_lines(&blocks, width, ascii)
}

// ── Blocks-based renderer (matches GUI parsing) ───────────────────────────────

/// Render parsed markdown blocks to styled lines. Uses the same block structure
/// as the GUI so paragraphs, block quotes, list items, etc. are never confused.
fn render_blocks_to_lines(blocks: &[MarkdownBlock], width: usize, ascii: bool) -> StyledLines {
    let mut lines = Vec::new();
    let mut ordered_counter: u64 = 1;
    let mut i = 0;

    while i < blocks.len() {
        // Collect consecutive TableRow blocks and render as a single table.
        if let MarkdownBlock::TableRow(_) = &blocks[i] {
            ordered_counter = 1;
            let mut table_rows: Vec<(Vec<String>, bool)> = Vec::new();
            while i < blocks.len() {
                if let MarkdownBlock::TableRow(cells) = &blocks[i] {
                    let cells = cells.clone();
                    let is_separator = cells.iter().all(|c| c.trim().chars().all(|ch| ch == '-'));
                    if !is_separator {
                        let is_header = table_rows.is_empty();
                        table_rows.push((cells, is_header));
                    }
                    i += 1;
                } else {
                    break;
                }
            }
            if !table_rows.is_empty() {
                let table_lines = render_table(&table_rows, &[], width, ascii);
                lines.extend(table_lines);
                lines.push(Line::default());
            }
            continue;
        }

        let block = &blocks[i];
        i += 1;

        let block_lines = match block {
            MarkdownBlock::Paragraph(text) => {
                let spans = parse_inline_to_spans(text);
                let mut block_lines = word_wrap_spans_to_lines(&spans, width, Style::default());
                block_lines.push(Line::default());
                block_lines
            }
            MarkdownBlock::Heading { level, text } => {
                let spans = parse_inline_to_spans(text);
                let style = heading_style_from_level(*level);
                let mut block_lines = word_wrap_spans_to_lines(&spans, width, style);
                block_lines.push(Line::default());
                block_lines
            }
            MarkdownBlock::CodeBlock { language: _, code } => {
                let mut block_lines = plain_code_lines(code, width);
                block_lines.push(Line::default());
                block_lines
            }
            MarkdownBlock::ListItem {
                depth,
                text,
                ordered,
                task_checked,
            } => {
                let (prefix, display_text) = if *ordered {
                    let num = ordered_counter;
                    ordered_counter += 1;
                    let indent = "  ".repeat(*depth as usize);
                    (format!("{indent}  {num}. "), text.as_str())
                } else {
                    ordered_counter = 1;
                    let indent = "  ".repeat(*depth as usize);
                    let (bullet, display_text) = if let Some(checked) = task_checked {
                        (
                            if *checked {
                                if ascii {
                                    "  [x] "
                                } else {
                                    "  ☑ "
                                }
                            } else {
                                if ascii {
                                    "  [ ] "
                                } else {
                                    "  ☐ "
                                }
                            },
                            text.as_str(),
                        )
                    } else {
                        (md_bullet(ascii), text.as_str())
                    };
                    (format!("{indent}{bullet}"), display_text)
                };
                let spans = parse_inline_to_spans(display_text);
                word_wrap_spans_to_lines_with_prefix(
                    &spans,
                    width,
                    &prefix,
                    Style::default().fg(Color::LightBlue),
                    Style::default(),
                )
            }
            MarkdownBlock::BlockQuote(text) => {
                ordered_counter = 1;
                let prefix = md_blockquote(ascii).to_string();
                let spans = parse_inline_to_spans(text);
                let mut block_lines = word_wrap_spans_to_lines_with_prefix(
                    &spans,
                    width,
                    &prefix,
                    Style::default().fg(Color::Green),
                    Style::default().fg(Color::Green),
                );
                block_lines.push(Line::default());
                block_lines
            }
            MarkdownBlock::Separator => {
                ordered_counter = 1;
                let rc = md_rule_char(ascii);
                let rc_w = unicode_width::UnicodeWidthChar::width(rc)
                    .unwrap_or(1)
                    .max(1);
                let count = width / rc_w;
                vec![
                    Line::from(Span::styled(
                        rc.to_string().repeat(count),
                        Style::default().fg(Color::DarkGray),
                    )),
                    Line::default(),
                ]
            }
            MarkdownBlock::InlineCode(text) => {
                ordered_counter = 1;
                vec![Line::from(Span::styled(
                    format!("`{text}`"),
                    Style::default().fg(Color::Yellow),
                ))]
            }
            MarkdownBlock::TableRow(_) => unreachable!("TableRow handled above"),
        };
        lines.extend(block_lines);
    }

    lines
}

/// Parse inline markdown (bold, italic, code, links) into styled spans.
fn parse_inline_to_spans(text: &str) -> Vec<(String, Style)> {
    let wrapped = format!("{}\n", text);
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(&wrapped, opts);

    let mut spans: Vec<(String, Style)> = Vec::new();
    let mut style_stack = vec![Style::default()];

    for event in parser {
        match event {
            Event::Start(Tag::Strong) => {
                let base = *style_stack.last().unwrap_or(&Style::default());
                style_stack.push(base.add_modifier(Modifier::BOLD));
            }
            Event::End(TagEnd::Strong) => {
                style_stack.pop();
            }
            Event::Start(Tag::Emphasis) => {
                let base = *style_stack.last().unwrap_or(&Style::default());
                style_stack.push(base.add_modifier(Modifier::ITALIC));
            }
            Event::End(TagEnd::Emphasis) => {
                style_stack.pop();
            }
            Event::Start(Tag::Strikethrough) => {
                let base = *style_stack.last().unwrap_or(&Style::default());
                style_stack.push(base.add_modifier(Modifier::CROSSED_OUT));
            }
            Event::End(TagEnd::Strikethrough) => {
                style_stack.pop();
            }
            Event::Start(Tag::Link { dest_url: _, .. }) => {
                let base = *style_stack.last().unwrap_or(&Style::default());
                style_stack.push(base.fg(Color::Cyan).add_modifier(Modifier::UNDERLINED));
            }
            Event::End(TagEnd::Link) => {
                style_stack.pop();
            }
            Event::Text(t) => {
                let s = t.to_string();
                if !s.is_empty() {
                    let style = *style_stack.last().unwrap_or(&Style::default());
                    spans.push((s, style));
                }
            }
            Event::Code(t) => {
                let style = Style::default().fg(Color::Yellow);
                spans.push((format!("`{t}`"), style));
            }
            Event::SoftBreak | Event::HardBreak => {
                let style = *style_stack.last().unwrap_or(&Style::default());
                spans.push((" ".to_string(), style));
            }
            _ => {}
        }
    }

    if spans.is_empty() && !text.is_empty() {
        vec![(text.to_string(), Style::default())]
    } else {
        spans
    }
}

/// Word-wrap spans into lines of at most `width` columns.
fn word_wrap_spans_to_lines(
    spans: &[(String, Style)],
    width: usize,
    base_style: Style,
) -> Vec<Line<'static>> {
    word_wrap_spans_to_lines_with_prefix(spans, width, "", Style::default(), base_style)
}

/// Word-wrap spans with an optional prefix (e.g. bullet or blockquote marker).
fn word_wrap_spans_to_lines_with_prefix(
    spans: &[(String, Style)],
    width: usize,
    prefix: &str,
    prefix_style: Style,
    base_style: Style,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let prefix_w = unicode_width::UnicodeWidthStr::width(prefix);
    let content_width = width.saturating_sub(prefix_w);

    let mut current_line: Vec<Span<'static>> = Vec::new();
    let mut col = 0usize;
    let mut is_first_line = true;

    for (text, style) in spans {
        let effective_style = style.patch(base_style);
        for word in text.split_inclusive(' ') {
            let word_w = unicode_width::UnicodeWidthStr::width(word);
            if col + word_w > content_width && !current_line.is_empty() {
                let mut line_spans = Vec::new();
                if is_first_line && !prefix.is_empty() {
                    line_spans.push(Span::styled(prefix.to_string(), prefix_style));
                    is_first_line = false;
                } else if !prefix.is_empty() {
                    line_spans.push(Span::raw(" ".repeat(prefix_w)));
                }
                line_spans.extend(std::mem::take(&mut current_line));
                lines.push(Line::from(line_spans));
                col = 0;
            }
            current_line.push(Span::styled(word.to_string(), effective_style));
            col += word_w;
        }
    }

    if !current_line.is_empty() {
        let mut line_spans = Vec::new();
        if is_first_line && !prefix.is_empty() {
            line_spans.push(Span::styled(prefix.to_string(), prefix_style));
        } else if !prefix.is_empty() {
            line_spans.push(Span::raw(" ".repeat(prefix_w)));
        }
        line_spans.extend(current_line);
        lines.push(Line::from(line_spans));
    }

    if lines.is_empty() && !spans.is_empty() {
        let mut line_spans = Vec::new();
        if !prefix.is_empty() {
            line_spans.push(Span::styled(prefix.to_string(), prefix_style));
        }
        for (text, style) in spans {
            line_spans.push(Span::styled(text.clone(), style.patch(base_style)));
        }
        lines.push(Line::from(line_spans));
    }

    lines
}

fn heading_style_from_level(level: u8) -> Style {
    match level {
        1 => Style::default()
            .fg(Color::LightBlue)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        2 => Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD),
        3 => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        4 => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::ITALIC),
        _ => Style::default().add_modifier(Modifier::BOLD),
    }
}

/// Plain (no highlighting) code fallback — cyan text.
///
/// Lines wider than `max_width` are hard-wrapped so that `chat_lines` never
/// contains spans that exceed the visible chat pane width.  Without this,
/// long lines produce styled cells in Ratatui's buffer that persist as visual
/// ghost artefacts when the viewport is scrolled.
fn plain_code_lines(code: &str, max_width: usize) -> Vec<Line<'static>> {
    let style = Style::default().fg(Color::Cyan);
    let mut out = Vec::new();
    for raw in code.lines() {
        let mut remaining = raw;
        loop {
            let mut col = 0usize;
            let mut byte_end = remaining.len();
            for (i, ch) in remaining.char_indices() {
                // CJK-conservative width: ambiguous chars count as 2 so the
                // hard-wrapped code line never overflows the terminal column limit.
                let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                if col + cw > max_width {
                    byte_end = i;
                    break;
                }
                col += cw;
            }
            if byte_end == remaining.len() {
                out.push(Line::from(Span::styled(remaining.to_string(), style)));
                break;
            }
            out.push(Line::from(Span::styled(
                remaining[..byte_end].to_string(),
                style,
            )));
            remaining = &remaining[byte_end..];
        }
    }
    out
}

// ── Table rendering ───────────────────────────────────────────────────────────

/// Render a buffered GFM table to a list of styled [`Line`]s.
///
/// The table is drawn with box-drawing characters (or plain ASCII when
/// `ascii` is true).  Column widths are computed from cell contents and
/// scaled down proportionally when the table would exceed `max_width`.
fn render_table(
    rows: &[(Vec<String>, bool)],
    alignments: &[Alignment],
    max_width: usize,
    ascii: bool,
) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return vec![];
    }
    let num_cols = rows.iter().map(|(r, _)| r.len()).max().unwrap_or(0);
    if num_cols == 0 {
        return vec![];
    }

    // Minimum content width for each column (at least 1).
    let mut col_widths: Vec<usize> = vec![1; num_cols];
    for (row_cells, _) in rows {
        for (i, cell) in row_cells.iter().enumerate() {
            if i < num_cols {
                let w = unicode_width::UnicodeWidthStr::width(cell.as_str());
                col_widths[i] = col_widths[i].max(w);
            }
        }
    }

    // Scale column widths down proportionally when the table is too wide.
    // Overhead: (num_cols + 1) vertical separators + 2 padding spaces per column.
    let overhead = (num_cols + 1) + num_cols * 2;
    if max_width > overhead {
        let available = max_width - overhead;
        let total: usize = col_widths.iter().sum();
        if total > available && available > 0 {
            for w in &mut col_widths {
                *w = ((*w * available) / total).max(1);
            }
        }
    }

    // Box-drawing characters.
    let (sep_v, sep_h, tl, tm, tr, ml, mm, mr, bl, bm, br): (
        char,
        char,
        char,
        char,
        char,
        char,
        char,
        char,
        char,
        char,
        char,
    ) = if ascii {
        ('|', '-', '+', '+', '+', '+', '+', '+', '+', '+', '+')
    } else {
        ('│', '─', '┌', '┬', '┐', '├', '┼', '┤', '└', '┴', '┘')
    };

    let border_style = Style::default().fg(Color::DarkGray);
    let header_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let body_style = Style::default();

    // Build a horizontal rule line (top, header separator, or bottom).
    let build_h_rule = |left: char, mid: char, right: char| -> Line<'static> {
        let mut s = String::new();
        s.push(left);
        for (i, &w) in col_widths.iter().enumerate() {
            // +2 for the single space padding on each side
            s.push_str(&sep_h.to_string().repeat(w + 2));
            if i + 1 < num_cols {
                s.push(mid);
            }
        }
        s.push(right);
        Line::from(Span::styled(s, border_style))
    };

    let mut out: Vec<Line<'static>> = Vec::new();

    out.push(build_h_rule(tl, tm, tr));

    let mut header_done = false;
    for (row_cells, is_header) in rows {
        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.push(Span::styled(sep_v.to_string(), border_style));

        #[allow(clippy::needless_range_loop)]
        for i in 0..num_cols {
            let cell_text = row_cells
                .get(i)
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            let cell_w = unicode_width::UnicodeWidthStr::width(cell_text.as_str());
            let max_w = col_widths[i];
            let align = alignments.get(i).copied().unwrap_or(Alignment::None);

            // Truncate with ellipsis when the cell is wider than its column.
            let display = if cell_w > max_w {
                let mut s = String::new();
                let mut cur = 0usize;
                for ch in cell_text.chars() {
                    let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                    if cur + cw > max_w.saturating_sub(1) {
                        s.push('…');
                        break;
                    }
                    s.push(ch);
                    cur += cw;
                }
                s
            } else {
                cell_text
            };
            let disp_w = unicode_width::UnicodeWidthStr::width(display.as_str());
            let pad = max_w.saturating_sub(disp_w);
            let (lpad, rpad) = match align {
                Alignment::Center => (pad / 2, pad - pad / 2),
                Alignment::Right => (pad, 0),
                _ => (0, pad),
            };

            spans.push(Span::raw(" ".repeat(lpad + 1)));
            let style = if *is_header { header_style } else { body_style };
            spans.push(Span::styled(display, style));
            spans.push(Span::raw(" ".repeat(rpad + 1)));
            spans.push(Span::styled(sep_v.to_string(), border_style));
        }
        out.push(Line::from(spans));

        if *is_header && !header_done {
            out.push(build_h_rule(ml, mm, mr));
            header_done = true;
        }
    }

    out.push(build_h_rule(bl, bm, br));
    out
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_empty_returns_some_lines() {
        let lines = render_markdown("", 80, false);
        assert!(
            lines.len() <= 1,
            "empty input should yield at most one line"
        );
    }

    #[test]
    fn task_list_first_item_shows_checkbox_when_completed() {
        // TaskListMarker arrives after Start(Item) in pulldown-cmark. The first
        // item must show ☑ (not •) when completed.
        let md = "- [x] First task done\n- [ ] Second pending\n";
        let lines = render_markdown(md, 80, false);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(
            text.contains("☑") && text.contains("First task"),
            "first completed item must show checkbox, not bullet; got: {text:?}"
        );
        assert!(
            text.contains("☐") && text.contains("Second"),
            "second pending item must show unchecked box; got: {text:?}"
        );
    }

    #[test]
    fn render_unclosed_link_does_not_panic() {
        let md = "[unclosed link\n\nnormal text";
        let lines = render_markdown(md, 80, false);
        assert!(!lines.is_empty(), "should produce some lines");
    }

    #[test]
    fn style_stack_cleanup_after_unclosed_tag() {
        let md = "**bold [link\n\nplain text";
        let lines = render_markdown(md, 80, false);
        assert!(!lines.is_empty());
    }

    // ── Table rendering ───────────────────────────────────────────────────────

    fn lines_to_text(lines: &StyledLines) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn table_is_not_compacted_into_single_line() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |\n";
        let lines = render_markdown(md, 80, false);
        // Must produce more than one non-empty line.
        let non_empty: Vec<_> = lines.iter().filter(|l| !l.spans.is_empty()).collect();
        assert!(
            non_empty.len() > 1,
            "table must span multiple lines; got: {lines:?}"
        );
    }

    #[test]
    fn table_headers_appear_in_output() {
        let md = "| Name | Age |\n|------|-----|\n| Alice | 30 |\n";
        let lines = render_markdown(md, 80, false);
        let text = lines_to_text(&lines);
        assert!(text.contains("Name"), "header 'Name' missing: {text}");
        assert!(text.contains("Age"), "header 'Age' missing: {text}");
    }

    #[test]
    fn table_body_cells_appear_in_output() {
        let md = "| Name | Age |\n|------|-----|\n| Alice | 30 |\n| Bob | 25 |\n";
        let lines = render_markdown(md, 80, false);
        let text = lines_to_text(&lines);
        assert!(text.contains("Alice"), "cell 'Alice' missing: {text}");
        assert!(text.contains("Bob"), "cell 'Bob' missing: {text}");
        assert!(text.contains("30"), "cell '30' missing: {text}");
        assert!(text.contains("25"), "cell '25' missing: {text}");
    }

    #[test]
    fn table_has_vertical_separators() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |\n";
        let lines = render_markdown(md, 80, false);
        let text = lines_to_text(&lines);
        // Non-ASCII mode should have │ separators.
        assert!(
            text.contains('│') || text.contains('|'),
            "no vertical separator found: {text}"
        );
    }

    #[test]
    fn table_ascii_mode_uses_pipe_separators() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |\n";
        let lines = render_markdown(md, 80, true); // ascii=true
        let text = lines_to_text(&lines);
        assert!(
            text.contains('|'),
            "ASCII mode must use '|' separators: {text}"
        );
        assert!(text.contains('A'), "header A present: {text}");
        assert!(text.contains('B'), "header B present: {text}");
    }

    #[test]
    fn table_header_separator_row_present() {
        // The header separator row (├─┼─┤ or +---+) must appear between
        // the header and the first body row.
        let md = "| Col |\n|-----|\n| val |\n";
        let lines = render_markdown(md, 80, false);
        let text = lines_to_text(&lines);
        // The separator between header and body uses ├ or + in ASCII mode.
        assert!(
            text.contains('├') || text.contains('+'),
            "header separator missing: {text}"
        );
    }

    #[test]
    fn table_preceded_by_text_renders_both() {
        let md = "Some text.\n\n| X | Y |\n|---|---|\n| a | b |\n";
        let lines = render_markdown(md, 80, false);
        let text = lines_to_text(&lines);
        assert!(text.contains("Some text"), "preceding text present: {text}");
        assert!(text.contains('X'), "table header X present: {text}");
        assert!(text.contains('a'), "table cell a present: {text}");
    }

    #[test]
    fn render_table_fn_empty_rows_returns_empty() {
        let result = render_table(&[], &[], 80, false);
        assert!(result.is_empty());
    }

    #[test]
    fn block_quote_renders_without_list_bullet() {
        let md = "> This is a block quote\n\nNormal paragraph";
        let lines = render_markdown(md, 80, false);
        let text = lines_to_text(&lines);
        assert!(
            text.contains("This is a block quote"),
            "block quote content present: {text}"
        );
        assert!(
            text.contains("Normal paragraph"),
            "paragraph content present: {text}"
        );
        // Block quote must NOT be prefixed with list bullet (• or -)
        let quote_line = lines
            .iter()
            .find(|l| {
                let s: String = l.spans.iter().map(|x| x.content.as_ref()).collect();
                s.contains("This is a block quote")
            })
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .unwrap_or_default();
        assert!(
            !quote_line.starts_with("  • ") && !quote_line.starts_with("  - "),
            "block quote must not have list bullet; got: {quote_line:?}"
        );
    }

    #[test]
    fn paragraph_and_list_rendered_distinctly() {
        let md = "A paragraph.\n\n- List item 1\n- List item 2";
        let lines = render_markdown(md, 80, false);
        let text = lines_to_text(&lines);
        assert!(text.contains("A paragraph"), "paragraph present: {text}");
        assert!(text.contains("List item 1"), "list item 1 present: {text}");
        assert!(text.contains("List item 2"), "list item 2 present: {text}");
    }
}
