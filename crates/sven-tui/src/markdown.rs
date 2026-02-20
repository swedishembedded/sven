use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::widgets::{md_blockquote, md_bullet, md_rule_char};

/// A styled line ready for Ratatui rendering.
pub type StyledLines = Vec<Line<'static>>;

/// Convert a markdown string into a list of styled [`Line`]s for Ratatui.
///
/// `ascii` — when true, use plain ASCII characters instead of Unicode
/// box-drawing / Braille glyphs so that fonts without wide Unicode support
/// render cleanly.
pub fn render_markdown(md: &str, wrap_width: u16, ascii: bool) -> StyledLines {
    let width = if wrap_width == 0 { 80 } else { wrap_width as usize };
    let mut lines: StyledLines = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut style_stack: Vec<Style> = vec![Style::default()];

    let push_line = |lines: &mut StyledLines, spans: &mut Vec<Span<'static>>| {
        if spans.is_empty() {
            lines.push(Line::default());
        } else {
            lines.push(Line::from(std::mem::take(spans)));
        }
    };

    let parser = Parser::new(md);
    for event in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                push_line(&mut lines, &mut current_spans);
                let style = heading_style(level);
                style_stack.push(style);
            }
            Event::End(TagEnd::Heading(_)) => {
                style_stack.pop();
                push_line(&mut lines, &mut current_spans);
                lines.push(Line::default());
            }
            Event::Start(Tag::Strong) => {
                let base = *style_stack.last().unwrap_or(&Style::default());
                style_stack.push(base.add_modifier(Modifier::BOLD));
            }
            Event::End(TagEnd::Strong) => { style_stack.pop(); }
            Event::Start(Tag::Emphasis) => {
                let base = *style_stack.last().unwrap_or(&Style::default());
                style_stack.push(base.add_modifier(Modifier::ITALIC));
            }
            Event::End(TagEnd::Emphasis) => { style_stack.pop(); }
            Event::Start(Tag::CodeBlock(_)) => {
                push_line(&mut lines, &mut current_spans);
                style_stack.push(Style::default().fg(Color::Cyan));
            }
            Event::End(TagEnd::CodeBlock) => {
                push_line(&mut lines, &mut current_spans);
                style_stack.pop();
                lines.push(Line::default());
            }
            Event::Start(Tag::List(_)) => {
                push_line(&mut lines, &mut current_spans);
            }
            Event::Start(Tag::Item) => {
                current_spans.push(Span::raw(format!("  {}", md_bullet(ascii))));
            }
            Event::End(TagEnd::Item) => {
                push_line(&mut lines, &mut current_spans);
            }
            Event::Start(Tag::BlockQuote(_)) => {
                let base = *style_stack.last().unwrap_or(&Style::default());
                style_stack.push(base.fg(Color::DarkGray));
                current_spans.push(Span::raw(md_blockquote(ascii).to_string()));
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                push_line(&mut lines, &mut current_spans);
                style_stack.pop();
                lines.push(Line::default());
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                push_line(&mut lines, &mut current_spans);
                lines.push(Line::default());
            }
            Event::Text(t) => {
                let style = *style_stack.last().unwrap_or(&Style::default());
                let words = t.split_inclusive(' ');
                let mut col = current_col(&current_spans);
                let mut buf = String::new();
                for word in words {
                    if col + word.len() > width && !buf.is_empty() {
                        current_spans.push(Span::styled(buf.clone(), style));
                        buf.clear();
                        push_line(&mut lines, &mut current_spans);
                        col = 0;
                    }
                    buf.push_str(word);
                    col += word.len();
                }
                if !buf.is_empty() {
                    current_spans.push(Span::styled(buf, style));
                }
            }
            Event::Code(t) => {
                let style = Style::default().fg(Color::Yellow).bg(Color::DarkGray);
                current_spans.push(Span::styled(format!("`{t}`"), style));
            }
            Event::SoftBreak => {
                current_spans.push(Span::raw(" "));
            }
            Event::HardBreak => {
                push_line(&mut lines, &mut current_spans);
            }
            Event::Rule => {
                push_line(&mut lines, &mut current_spans);
                lines.push(Line::from(Span::styled(
                    md_rule_char(ascii).to_string().repeat(width),
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::default());
            }
            _ => {}
        }
    }

    if !current_spans.is_empty() {
        lines.push(Line::from(current_spans));
    }

    lines
}

fn heading_style(level: HeadingLevel) -> Style {
    match level {
        HeadingLevel::H1 => Style::default().fg(Color::LightBlue).add_modifier(Modifier::BOLD),
        HeadingLevel::H2 => Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
        HeadingLevel::H3 => Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        _ => Style::default().add_modifier(Modifier::BOLD),
    }
}

fn current_col(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
}
