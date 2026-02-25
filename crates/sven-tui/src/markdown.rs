// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::widgets::{md_blockquote, md_bullet, md_rule_char};

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
    // Link URL to show after link text
    pending_link_url: Option<String>,
}

impl MarkdownRenderer {
    fn new(wrap_width: u16, ascii: bool) -> Self {
        Self {
            width: if wrap_width == 0 { 80 } else { wrap_width as usize },
            ascii,
            lines: Vec::new(),
            current_spans: Vec::new(),
            style_stack: vec![Style::default()],
            in_code_block: false,
            code_lang: String::new(),
            code_buf: String::new(),
            list_stack: Vec::new(),
            pending_link_url: None,
        }
    }

    fn push_line(&mut self) {
        if self.current_spans.is_empty() {
            self.lines.push(Line::default());
        } else {
            self.lines.push(Line::from(std::mem::take(&mut self.current_spans)));
        }
    }

    fn current_style(&self) -> Style {
        *self.style_stack.last().unwrap_or(&Style::default())
    }

    fn render(mut self, md: &str) -> StyledLines {
        let mut opts = Options::empty();
        opts.insert(Options::ENABLE_STRIKETHROUGH);
        opts.insert(Options::ENABLE_TABLES);

        let parser = Parser::new_ext(md, opts);
        for event in parser {
            // ── Inside code block: accumulate text, handle end ────────────────
            if self.in_code_block {
                match event {
                    Event::Text(t) => self.code_buf.push_str(&t),
                    Event::End(TagEnd::CodeBlock) => {
                        let highlighted =
                            highlight_code_block(&self.code_buf, &self.code_lang);
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
                Event::End(TagEnd::Strong) => { self.style_stack.pop(); }

                Event::Start(Tag::Emphasis) => {
                    let base = self.current_style();
                    self.style_stack.push(base.add_modifier(Modifier::ITALIC));
                }
                Event::End(TagEnd::Emphasis) => { self.style_stack.pop(); }

                Event::Start(Tag::Strikethrough) => {
                    let base = self.current_style();
                    self.style_stack.push(base.add_modifier(Modifier::CROSSED_OUT));
                }
                Event::End(TagEnd::Strikethrough) => { self.style_stack.pop(); }

                // ── Links ─────────────────────────────────────────────────────
                Event::Start(Tag::Link { dest_url, .. }) => {
                    let base = self.current_style();
                    self.style_stack.push(
                        base.fg(Color::Cyan).add_modifier(Modifier::UNDERLINED),
                    );
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
                            lang.split_whitespace()
                                .next()
                                .unwrap_or("")
                                .to_lowercase()
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
                    self.current_spans.push(Span::styled(
                        bullet,
                        Style::default().fg(Color::LightBlue),
                    ));
                }
                Event::End(TagEnd::Item) => {
                    self.push_line();
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
                        let word_w = unicode_width::UnicodeWidthStr::width(word);
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
                    self.current_spans.push(Span::styled(format!("`{t}`"), style));
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
                    self.lines.push(Line::from(Span::styled(
                        md_rule_char(self.ascii).to_string().repeat(self.width),
                        Style::default().fg(Color::DarkGray),
                    )));
                    self.lines.push(Line::default());
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
    spans.iter().map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref())).sum()
}

// ── Syntect code highlighting ─────────────────────────────────────────────────

/// Highlight a fenced code block with plain cyan text for maximum terminal
/// compatibility. Syntect RGB highlighting is disabled to avoid issues with
/// non-standard terminal colors.
fn highlight_code_block(code: &str, _lang: &str) -> Vec<Line<'static>> {
    // Use plain cyan for all code blocks to ensure compatibility with all terminals
    plain_code_lines(code)
}

/// Plain (no highlighting) code fallback — cyan text.
fn plain_code_lines(code: &str) -> Vec<Line<'static>> {
    let style = Style::default().fg(Color::Cyan);
    code.lines()
        .map(|l| Line::from(Span::styled(l.to_string(), style)))
        .collect()
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_empty_returns_some_lines() {
        let lines = render_markdown("", 80, false);
        assert!(lines.len() <= 1, "empty input should yield at most one line");
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
}
