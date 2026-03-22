// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Shared markdown block parser for Sven frontends.
//!
//! Parses markdown text into a flat list of [`MarkdownBlock`]s that can be
//! rendered by any frontend.  The TUI maps these to styled ratatui `Line`s;
//! the Slint GUI maps them to `RichBlock` structs for native rendering.
//!
//! Uses `pulldown-cmark` for reliable parsing.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// A parsed block of markdown content.
///
/// Blocks are the unit of rendering — each frontend decides how to display
/// each variant.
#[derive(Debug, Clone, PartialEq)]
pub enum MarkdownBlock {
    /// A paragraph of inline text (may contain inline markup).
    Paragraph(String),

    /// A heading with level (1–6) and text.
    Heading { level: u8, text: String },

    /// A fenced code block with optional language tag.
    CodeBlock { language: String, code: String },

    /// An inline code span.
    InlineCode(String),

    /// An ordered or unordered list item with nesting depth (0-based).
    /// `task_checked`: Some(true/false) for task lists, None for regular bullets.
    ListItem {
        depth: u8,
        text: String,
        ordered: bool,
        task_checked: Option<bool>,
    },

    /// A thematic break / horizontal rule.
    Separator,

    /// A block quote.
    BlockQuote(String),

    /// A table row (simplified: cells joined by `│`).
    TableRow(Vec<String>),
}

/// Parse a markdown string into a sequence of [`MarkdownBlock`]s.
///
/// The parser handles headings, paragraphs, code blocks, lists, block quotes,
/// thematic breaks, and tables.  Inline formatting (bold, italic, links) is
/// preserved as plain text within the block content.
pub fn parse_markdown_blocks(text: &str) -> Vec<MarkdownBlock> {
    if text.is_empty() {
        return vec![];
    }

    let opts = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;

    let parser = Parser::new_ext(text, opts);

    let mut blocks: Vec<MarkdownBlock> = Vec::new();
    let mut current_text = String::new();
    let mut in_heading: Option<u8> = None;
    let mut in_code_block: bool = false;
    let mut code_lang: String = String::new();
    let mut code_buf: String = String::new();
    let mut list_depth: i32 = -1;
    let mut list_ordered_stack: Vec<bool> = Vec::new();
    let mut in_list_item: bool = false;
    let mut list_item_task_checked: Option<bool> = None;
    let mut in_blockquote: bool = false;
    let mut blockquote_buf: String = String::new();
    let mut in_table: bool = false;
    let mut current_row: Vec<String> = Vec::new();
    let mut cell_buf: String = String::new();
    let mut in_cell: bool = false;

    for event in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                flush_text(&mut current_text, &mut blocks);
                in_heading = Some(heading_level(level));
            }
            Event::End(TagEnd::Heading(_)) => {
                let level = in_heading.take().unwrap_or(1);
                let text = std::mem::take(&mut current_text).trim().to_string();
                if !text.is_empty() {
                    blocks.push(MarkdownBlock::Heading { level, text });
                }
            }
            Event::Start(Tag::Paragraph) => {
                flush_text(&mut current_text, &mut blocks);
            }
            Event::End(TagEnd::Paragraph) => {
                flush_text(&mut current_text, &mut blocks);
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                flush_text(&mut current_text, &mut blocks);
                in_code_block = true;
                code_lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(lang) => lang.to_string(),
                    pulldown_cmark::CodeBlockKind::Indented => String::new(),
                };
                code_buf.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
                let code = std::mem::take(&mut code_buf);
                let code = code.trim_end_matches('\n').to_string();
                if !code.is_empty() {
                    blocks.push(MarkdownBlock::CodeBlock {
                        language: std::mem::take(&mut code_lang),
                        code,
                    });
                }
            }
            Event::Start(Tag::List(start)) => {
                flush_text(&mut current_text, &mut blocks);
                list_depth += 1;
                list_ordered_stack.push(start.is_some());
            }
            Event::End(TagEnd::List(_)) => {
                list_depth -= 1;
                list_ordered_stack.pop();
            }
            Event::Start(Tag::Item) => {
                in_list_item = true;
                list_item_task_checked = None;
                current_text.clear();
            }
            Event::TaskListMarker(checked) => {
                list_item_task_checked = Some(checked);
            }
            Event::End(TagEnd::Item) => {
                in_list_item = false;
                let text = std::mem::take(&mut current_text).trim().to_string();
                if !text.is_empty() {
                    let ordered = list_ordered_stack.last().copied().unwrap_or(false);
                    blocks.push(MarkdownBlock::ListItem {
                        depth: list_depth.max(0) as u8,
                        text,
                        ordered,
                        task_checked: list_item_task_checked,
                    });
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                flush_text(&mut current_text, &mut blocks);
                in_blockquote = true;
                blockquote_buf.clear();
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                in_blockquote = false;
                let text = std::mem::take(&mut blockquote_buf).trim().to_string();
                if !text.is_empty() {
                    blocks.push(MarkdownBlock::BlockQuote(text));
                }
            }
            Event::Rule => {
                flush_text(&mut current_text, &mut blocks);
                blocks.push(MarkdownBlock::Separator);
            }
            Event::Start(Tag::Table(_)) => {
                flush_text(&mut current_text, &mut blocks);
                in_table = true;
            }
            Event::End(TagEnd::Table) => {
                in_table = false;
            }
            // GFM tables emit header cells under `TableHead` without wrapping them in
            // `TableRow` (see pulldown-cmark events: TableHead → TableCell… → End(TableHead)).
            Event::Start(Tag::TableHead) => {
                current_row.clear();
            }
            Event::End(TagEnd::TableHead) => {
                if !current_row.is_empty() {
                    blocks.push(MarkdownBlock::TableRow(std::mem::take(&mut current_row)));
                }
            }
            Event::Start(Tag::TableRow) => {
                current_row.clear();
            }
            Event::End(TagEnd::TableRow) => {
                if !current_row.is_empty() {
                    blocks.push(MarkdownBlock::TableRow(std::mem::take(&mut current_row)));
                }
            }
            Event::Start(Tag::TableCell) => {
                in_cell = true;
                cell_buf.clear();
            }
            Event::End(TagEnd::TableCell) => {
                in_cell = false;
                current_row.push(std::mem::take(&mut cell_buf).trim().to_string());
            }
            Event::Code(s) => {
                if in_blockquote {
                    blockquote_buf.push('`');
                    blockquote_buf.push_str(&s);
                    blockquote_buf.push('`');
                } else if in_cell {
                    cell_buf.push('`');
                    cell_buf.push_str(&s);
                    cell_buf.push('`');
                } else if !in_code_block {
                    current_text.push('`');
                    current_text.push_str(&s);
                    current_text.push('`');
                }
            }
            Event::Text(s) | Event::Html(s) => {
                if in_code_block {
                    code_buf.push_str(&s);
                } else if in_blockquote {
                    blockquote_buf.push_str(&s);
                } else if in_cell {
                    cell_buf.push_str(&s);
                } else if in_table {
                    // text outside cells in table context — skip
                } else {
                    current_text.push_str(&s);
                }
            }
            Event::SoftBreak => {
                if in_code_block {
                    code_buf.push('\n');
                } else if in_blockquote {
                    blockquote_buf.push(' ');
                } else if in_cell {
                    cell_buf.push(' ');
                } else {
                    current_text.push(' ');
                }
            }
            Event::HardBreak => {
                if in_code_block {
                    code_buf.push('\n');
                } else if in_blockquote {
                    blockquote_buf.push('\n');
                } else {
                    current_text.push('\n');
                }
            }
            _ => {}
        }
    }

    flush_text(&mut current_text, &mut blocks);

    let _ = in_list_item;
    let _ = in_table;
    blocks
}

fn flush_text(buf: &mut String, blocks: &mut Vec<MarkdownBlock>) {
    let text = buf.trim_end().to_string();
    buf.clear();
    if !text.is_empty() {
        blocks.push(MarkdownBlock::Paragraph(text));
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_heading() {
        let blocks = parse_markdown_blocks("# Hello");
        assert_eq!(
            blocks,
            vec![MarkdownBlock::Heading {
                level: 1,
                text: "Hello".into()
            }]
        );
    }

    #[test]
    fn parses_paragraph() {
        let blocks = parse_markdown_blocks("Hello world");
        assert_eq!(blocks, vec![MarkdownBlock::Paragraph("Hello world".into())]);
    }

    #[test]
    fn parses_code_block() {
        let md = "```rust\nfn main() {}\n```";
        let blocks = parse_markdown_blocks(md);
        assert!(blocks
            .iter()
            .any(|b| matches!(b, MarkdownBlock::CodeBlock { language, .. } if language == "rust")));
    }

    #[test]
    fn parses_list_items() {
        let md = "- one\n- two\n- three";
        let blocks = parse_markdown_blocks(md);
        let items: Vec<_> = blocks
            .iter()
            .filter(|b| matches!(b, MarkdownBlock::ListItem { .. }))
            .collect();
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn parses_separator() {
        let blocks = parse_markdown_blocks("---");
        assert!(blocks.iter().any(|b| matches!(b, MarkdownBlock::Separator)));
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(parse_markdown_blocks("").is_empty());
    }

    #[test]
    fn parses_gfm_table_including_header_row() {
        let md = "| Name | Age |\n|------|-----|\n| Alice | 30 |\n";
        let blocks = parse_markdown_blocks(md);
        let rows: Vec<_> = blocks
            .iter()
            .filter_map(|b| match b {
                MarkdownBlock::TableRow(cells) => Some(cells.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            rows,
            vec![
                vec!["Name".to_string(), "Age".to_string()],
                vec!["Alice".to_string(), "30".to_string()],
            ]
        );
    }

    #[test]
    fn parses_mixed_content() {
        let md = "# Title\n\nParagraph text.\n\n```\ncode\n```\n\n- item";
        let blocks = parse_markdown_blocks(md);
        assert!(blocks
            .iter()
            .any(|b| matches!(b, MarkdownBlock::Heading { .. })));
        assert!(blocks
            .iter()
            .any(|b| matches!(b, MarkdownBlock::Paragraph(..))));
        assert!(blocks
            .iter()
            .any(|b| matches!(b, MarkdownBlock::CodeBlock { .. })));
        assert!(blocks
            .iter()
            .any(|b| matches!(b, MarkdownBlock::ListItem { .. })));
    }
}
