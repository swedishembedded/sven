// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ui::theme::{md_blockquote, md_bullet, md_rule_char};

/// A styled line ready for Ratatui rendering.
pub type StyledLines = Vec<Line<'static>>;

// ── Public API ────────────────────────────────────────────────────────────────

/// Convert a markdown string into a list of styled [`Line`]s for Ratatui.
///
/// `wrap_width` — wrap long text at this column (0 → 80).
/// `ascii`      — use plain-ASCII box chars instead of Unicode.
pub fn render_markdown(md: &str, wrap_width: u16, ascii: bool) -> StyledLines {
    let r = MarkdownRenderer::new(wrap_width, ascii);
    r.render(md)
}

// ── Renderer struct ───────────────────────────────────────────────────────────

struct MarkdownRenderer {
    width: usize,
    ascii: bool,
    lines: StyledLines,
    current_spans: Vec<Span<'static>>,
    style_stack: Vec<Style>,
    // Code-block accumulator
    in_code_block: bool,
    code_lang: String,
    code_buf: String,
    // List state: stack of (is_ordered, current_number)
    list_stack: Vec<Option<u64>>,
    // When Some, the current list item begins with a task-list checkbox.
    // The bool is the checked state; the field is consumed when the Item starts.
    pending_task_marker: Option<bool>,
    // Link URL to show after link text
    pending_link_url: Option<String>,
    // Table state: buffer the whole table before rendering (need all widths first).
    table_alignments: Vec<Alignment>,
    /// Buffered rows: (cell texts, is_header).
    table_rows: Vec<(Vec<String>, bool)>,
    current_table_row: Vec<String>,
    current_table_cell: String,
    in_table_cell: bool,
    in_table_head: bool,
}

impl MarkdownRenderer {
    fn new(wrap_width: u16, ascii: bool) -> Self {
        Self {
            width: if wrap_width == 0 {
                80
            } else {
                wrap_width as usize
            },
            ascii,
            lines: Vec::new(),
            current_spans: Vec::new(),
            style_stack: vec![Style::default()],
            in_code_block: false,
            code_lang: String::new(),
            code_buf: String::new(),
            list_stack: Vec::new(),
            pending_task_marker: None,
            pending_link_url: None,
            table_alignments: Vec::new(),
            table_rows: Vec::new(),
            current_table_row: Vec::new(),
            current_table_cell: String::new(),
            in_table_cell: false,
            in_table_head: false,
        }
    }

    fn push_line(&mut self) {
        if self.current_spans.is_empty() {
            self.lines.push(Line::default());
        } else {
            self.lines
                .push(Line::from(std::mem::take(&mut self.current_spans)));
        }
    }

    fn current_style(&self) -> Style {
        *self.style_stack.last().unwrap_or(&Style::default())
    }

    fn render(mut self, md: &str) -> StyledLines {
        let mut opts = Options::empty();
        opts.insert(Options::ENABLE_STRIKETHROUGH);
        opts.insert(Options::ENABLE_TABLES);
        opts.insert(Options::ENABLE_TASKLISTS);

        let parser = Parser::new_ext(md, opts);
        for event in parser {
            // ── Inside code block: accumulate text, handle end ────────────────
            if self.in_code_block {
                match event {
                    Event::Text(t) => self.code_buf.push_str(&t),
                    Event::End(TagEnd::CodeBlock) => {
                        let highlighted =
                            highlight_code_block(&self.code_buf, &self.code_lang, self.width);
                        self.lines.extend(highlighted);
                        self.lines.push(Line::default());
                        self.in_code_block = false;
                        self.code_lang.clear();
                        self.code_buf.clear();
                    }
                    _ => {}
                }
                continue;
            }

            // ── Inside table cell: accumulate plain text ──────────────────────
            if self.in_table_cell {
                match event {
                    Event::Text(t) => self.current_table_cell.push_str(&t),
                    Event::Code(t) => {
                        self.current_table_cell.push('`');
                        self.current_table_cell.push_str(&t);
                        self.current_table_cell.push('`');
                    }
                    Event::SoftBreak | Event::HardBreak => {
                        self.current_table_cell.push(' ');
                    }
                    Event::End(TagEnd::TableCell) => {
                        let cell = std::mem::take(&mut self.current_table_cell);
                        self.current_table_row.push(cell.trim().to_string());
                        self.in_table_cell = false;
                    }
                    // Inline markup tags (Strong, Emphasis, etc.) are silently
                    // ignored — their text content is captured above.
                    _ => {}
                }
                continue;
            }

            match event {
                // ── Headings ──────────────────────────────────────────────────
                Event::Start(Tag::Heading { level, .. }) => {
                    self.push_line();
                    self.style_stack.push(heading_style(level));
                }
                Event::End(TagEnd::Heading(_)) => {
                    self.style_stack.pop();
                    self.push_line();
                    self.lines.push(Line::default());
                }

                // ── Inline markup ─────────────────────────────────────────────
                Event::Start(Tag::Strong) => {
                    let base = self.current_style();
                    self.style_stack.push(base.add_modifier(Modifier::BOLD));
                }
                Event::End(TagEnd::Strong) => {
                    self.style_stack.pop();
                }

                Event::Start(Tag::Emphasis) => {
                    let base = self.current_style();
                    self.style_stack.push(base.add_modifier(Modifier::ITALIC));
                }
                Event::End(TagEnd::Emphasis) => {
                    self.style_stack.pop();
                }

                Event::Start(Tag::Strikethrough) => {
                    let base = self.current_style();
                    self.style_stack
                        .push(base.add_modifier(Modifier::CROSSED_OUT));
                }
                Event::End(TagEnd::Strikethrough) => {
                    self.style_stack.pop();
                }

                // ── Links ─────────────────────────────────────────────────────
                Event::Start(Tag::Link { dest_url, .. }) => {
                    let base = self.current_style();
                    self.style_stack
                        .push(base.fg(Color::Cyan).add_modifier(Modifier::UNDERLINED));
                    let url = dest_url.to_string();
                    if !url.is_empty() && !url.starts_with('#') {
                        self.pending_link_url = Some(url);
                    }
                }
                Event::End(TagEnd::Link) => {
                    self.style_stack.pop();
                    if let Some(url) = self.pending_link_url.take() {
                        self.current_spans.push(Span::styled(
                            format!(" ({url})"),
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                }

                // ── Code block (fenced or indented) ───────────────────────────
                Event::Start(Tag::CodeBlock(kind)) => {
                    self.push_line();
                    self.in_code_block = true;
                    self.code_lang = match kind {
                        CodeBlockKind::Fenced(lang) => {
                            // Take only the first token (some fences have `rust,no_run`)
                            lang.split_whitespace().next().unwrap_or("").to_lowercase()
                        }
                        CodeBlockKind::Indented => String::new(),
                    };
                }
                // End handled in the in_code_block branch above

                // ── Lists ─────────────────────────────────────────────────────
                Event::Start(Tag::List(start)) => {
                    self.push_line();
                    self.list_stack.push(start);
                }
                Event::End(TagEnd::List(_)) => {
                    self.list_stack.pop();
                    // Only add blank line after top-level lists
                    if self.list_stack.is_empty() {
                        self.lines.push(Line::default());
                    }
                }
                Event::Start(Tag::Item) => {
                    let bullet: String = match self.list_stack.last_mut() {
                        Some(Some(n)) => {
                            let s = format!("  {}. ", n);
                            *n += 1;
                            s
                        }
                        _ => format!("  {} ", md_bullet(self.ascii)),
                    };
                    // If a task-list marker was pending (set by TaskListMarker
                    // event which arrives before the item content), use a
                    // checkbox prefix instead of the regular bullet.
                    if let Some(checked) = self.pending_task_marker.take() {
                        let (box_char, box_style) = if checked {
                            (
                                if self.ascii { "[x] " } else { "☑ " },
                                Style::default()
                                    .fg(Color::Green)
                                    .add_modifier(Modifier::DIM),
                            )
                        } else {
                            (
                                if self.ascii { "[ ] " } else { "☐ " },
                                Style::default().fg(Color::Rgb(150, 150, 160)),
                            )
                        };
                        self.current_spans
                            .push(Span::styled("  ", Style::default()));
                        self.current_spans
                            .push(Span::styled(box_char.to_string(), box_style));
                    } else {
                        self.current_spans
                            .push(Span::styled(bullet, Style::default().fg(Color::LightBlue)));
                    }
                }
                Event::End(TagEnd::Item) => {
                    self.push_line();
                }
                Event::TaskListMarker(checked) => {
                    // Store the checked state; it will be consumed when the
                    // enclosing Tag::Item is rendered.
                    self.pending_task_marker = Some(checked);
                }

                // ── Block quotes ──────────────────────────────────────────────
                Event::Start(Tag::BlockQuote(_)) => {
                    let base = self.current_style();
                    self.style_stack.push(base.fg(Color::Green));
                    self.current_spans.push(Span::styled(
                        md_blockquote(self.ascii).to_string(),
                        Style::default().fg(Color::Green),
                    ));
                }
                Event::End(TagEnd::BlockQuote(_)) => {
                    self.push_line();
                    self.style_stack.pop();
                    self.lines.push(Line::default());
                }

                // ── Paragraphs ────────────────────────────────────────────────
                Event::Start(Tag::Paragraph) => {}
                Event::End(TagEnd::Paragraph) => {
                    self.push_line();
                    self.lines.push(Line::default());
                }

                // ── Text (with word-wrap) ─────────────────────────────────────
                Event::Text(t) => {
                    let style = self.current_style();
                    let width = self.width;
                    let mut col = current_col(&self.current_spans);
                    let mut buf = String::new();
                    for word in t.split_inclusive(' ') {
                        // Use CJK (conservative) widths: ambiguous characters such
                        // as box-drawing and block elements are treated as 2 wide.
                        // This ensures that lines never exceed the allocated terminal
                        // columns even on terminals that render these chars as 2-wide.
                        let word_w = unicode_width::UnicodeWidthStr::width_cjk(word);
                        if col + word_w > width && !buf.is_empty() {
                            self.current_spans.push(Span::styled(buf.clone(), style));
                            buf.clear();
                            self.push_line();
                            col = 0;
                        }
                        buf.push_str(word);
                        col += word_w;
                    }
                    if !buf.is_empty() {
                        self.current_spans.push(Span::styled(buf, style));
                    }
                }

                // ── Inline code ───────────────────────────────────────────────
                Event::Code(t) => {
                    let style = Style::default().fg(Color::Yellow).bg(Color::Black);
                    self.current_spans
                        .push(Span::styled(format!("`{t}`"), style));
                }

                // ── Line breaks ───────────────────────────────────────────────
                Event::SoftBreak => {
                    self.current_spans.push(Span::raw(" "));
                }
                Event::HardBreak => {
                    self.push_line();
                }

                // ── Horizontal rule ───────────────────────────────────────────
                Event::Rule => {
                    self.push_line();
                    let rc = md_rule_char(self.ascii);
                    // Divide by the CJK display width of the rule character so the
                    // rule never exceeds wrap_width terminal columns.  '─' (U+2500)
                    // is Ambiguous-width and renders as 2 columns on many terminals.
                    let rc_w = unicode_width::UnicodeWidthChar::width_cjk(rc)
                        .unwrap_or(1)
                        .max(1);
                    let count = self.width / rc_w;
                    self.lines.push(Line::from(Span::styled(
                        rc.to_string().repeat(count),
                        Style::default().fg(Color::DarkGray),
                    )));
                    self.lines.push(Line::default());
                }

                // ── Tables ───────────────────────────────────────────────────
                Event::Start(Tag::Table(aligns)) => {
                    self.push_line();
                    self.table_alignments = aligns;
                    self.table_rows.clear();
                    self.current_table_row.clear();
                }
                Event::End(TagEnd::Table) => {
                    // Flush any partial row (shouldn't normally happen).
                    if !self.current_table_row.is_empty() {
                        let row = std::mem::take(&mut self.current_table_row);
                        self.table_rows.push((row, self.in_table_head));
                    }
                    let rendered = render_table(
                        &self.table_rows,
                        &self.table_alignments,
                        self.width,
                        self.ascii,
                    );
                    self.lines.extend(rendered);
                    self.lines.push(Line::default());
                    self.table_rows.clear();
                    self.table_alignments.clear();
                    self.in_table_head = false;
                }
                Event::Start(Tag::TableHead) => {
                    self.in_table_head = true;
                }
                Event::End(TagEnd::TableHead) => {
                    // Finalize the header row when there was no explicit TableRow
                    // inside the head (some pulldown-cmark versions omit it).
                    if !self.current_table_row.is_empty() {
                        let row = std::mem::take(&mut self.current_table_row);
                        self.table_rows.push((row, true));
                    }
                    self.in_table_head = false;
                }
                Event::Start(Tag::TableRow) => {
                    // Row starts empty; cells fill it in via in_table_cell handling.
                }
                Event::End(TagEnd::TableRow) => {
                    let is_header = self.in_table_head;
                    let row = std::mem::take(&mut self.current_table_row);
                    if !row.is_empty() {
                        self.table_rows.push((row, is_header));
                    }
                }
                Event::Start(Tag::TableCell) => {
                    self.current_table_cell.clear();
                    self.in_table_cell = true;
                }

                _ => {}
            }
        }

        // Defensive cleanup: pop style stack so unclosed tags (e.g. malformed markdown)
        // cannot leak style to the rest of the document.
        while self.style_stack.len() > 1 {
            self.style_stack.pop();
        }

        if !self.current_spans.is_empty() {
            self.lines.push(Line::from(self.current_spans));
        }

        self.lines
    }
}

// ── Style helpers ─────────────────────────────────────────────────────────────

fn heading_style(level: HeadingLevel) -> Style {
    match level {
        HeadingLevel::H1 => Style::default()
            .fg(Color::LightBlue)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        HeadingLevel::H2 => Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD),
        HeadingLevel::H3 => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        HeadingLevel::H4 => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::ITALIC),
        _ => Style::default().add_modifier(Modifier::BOLD),
    }
}

fn current_col(spans: &[Span<'_>]) -> usize {
    // Use CJK widths to stay consistent with the word-wrap measurement above:
    // ambiguous chars count as 2 here too so the column tracking is accurate.
    spans
        .iter()
        .map(|s| unicode_width::UnicodeWidthStr::width_cjk(s.content.as_ref()))
        .sum()
}

// ── Syntect code highlighting ─────────────────────────────────────────────────

/// Highlight a fenced code block with plain cyan text for maximum terminal
/// compatibility. Syntect RGB highlighting is disabled to avoid issues with
/// non-standard terminal colors.
fn highlight_code_block(code: &str, _lang: &str, max_width: usize) -> Vec<Line<'static>> {
    // Use plain cyan for all code blocks to ensure compatibility with all terminals
    plain_code_lines(code, max_width)
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
                let cw = unicode_width::UnicodeWidthChar::width_cjk(ch).unwrap_or(0);
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
                let w = unicode_width::UnicodeWidthStr::width_cjk(cell.as_str());
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
            let cell_w = unicode_width::UnicodeWidthStr::width_cjk(cell_text.as_str());
            let max_w = col_widths[i];
            let align = alignments.get(i).copied().unwrap_or(Alignment::None);

            // Truncate with ellipsis when the cell is wider than its column.
            let display = if cell_w > max_w {
                let mut s = String::new();
                let mut cur = 0usize;
                for ch in cell_text.chars() {
                    let cw = unicode_width::UnicodeWidthChar::width_cjk(ch).unwrap_or(0);
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
            let disp_w = unicode_width::UnicodeWidthStr::width_cjk(display.as_str());
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
}
