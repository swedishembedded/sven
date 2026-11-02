// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for understanding where an LLM
// agent's context window goes for its clients. If your team needs expertise
// in agent context budgets then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The `/context` panel: a bordered view of everything a request carries.
//!
//! The numbers come from [`ContextUsage`] (sven-frontend); this file only
//! draws them: a 10 x 10 grid in which each cell is one percent of the
//! window, coloured by category, beside a legend with aligned token counts,
//! and below it the facts of the compaction budget.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use sven_frontend::{ContextCategory, ContextUsage};

use crate::markdown::StyledLines;
use crate::ui::theme::{BAR_AGENT, BAR_ERROR, BAR_THINKING, BAR_TOOL, BAR_USER, TEXT, TEXT_DIM};

const GRID_SIDE: usize = 10;
/// Width of the legend's label column.
const LABEL_WIDTH: usize = 30;
const TOKENS_WIDTH: usize = 9;
const PERCENT_WIDTH: usize = 7;
const GRID_WIDTH: usize = GRID_SIDE * 2;
/// Text columns inside the border.
const INNER_WIDTH: usize = 1 + GRID_WIDTH + 2 + 2 + LABEL_WIDTH + TOKENS_WIDTH + PERCENT_WIDTH + 1;

/// The colour a category is drawn in, grid and legend alike.
fn colour(category: ContextCategory) -> Color {
    match category {
        ContextCategory::SystemPrompt => Color::Rgb(120, 170, 255),
        ContextCategory::CoreTools => BAR_AGENT,
        ContextCategory::LoadedTools => Color::Rgb(90, 200, 210),
        ContextCategory::McpTools => Color::Rgb(80, 200, 160),
        ContextCategory::Skills => BAR_USER,
        ContextCategory::Indexes => Color::Rgb(170, 200, 90),
        ContextCategory::Memory => BAR_TOOL,
        ContextCategory::Messages => Color::Rgb(235, 140, 90),
        ContextCategory::Thinking => BAR_THINKING,
        ContextCategory::FreeSpace => Color::Rgb(70, 70, 90),
        ContextCategory::CompactionBuffer => Color::Rgb(150, 130, 70),
        ContextCategory::ResponseReserve => BAR_ERROR,
    }
}

/// The two columns a cell of `category` fills.
fn cell_glyph(category: ContextCategory, ascii: bool) -> &'static str {
    match (category, ascii) {
        (ContextCategory::FreeSpace, false) => "░░",
        (ContextCategory::FreeSpace, true) => "..",
        (ContextCategory::CompactionBuffer | ContextCategory::ResponseReserve, false) => "▒▒",
        (ContextCategory::CompactionBuffer | ContextCategory::ResponseReserve, true) => "::",
        (_, false) => "██",
        (_, true) => "##",
    }
}

/// `1234567` as `1,234,567`.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn percent(tokens: u32, window: u32) -> String {
    if window == 0 {
        return "-".to_string();
    }
    format!("{:.1}%", f64::from(tokens) * 100.0 / f64::from(window))
}

/// One row of text inside the border, padded to the panel width.
fn framed(mut spans: Vec<Span<'static>>, ascii: bool) -> Line<'static> {
    let used: usize = spans
        .iter()
        .map(|s| crate::ui::width_utils::display_width(&s.content))
        .sum();
    spans.push(Span::raw(" ".repeat(INNER_WIDTH.saturating_sub(used))));
    let edge = Span::styled(if ascii { "|" } else { "│" }, Style::default().fg(TEXT_DIM));
    spans.insert(0, edge.clone());
    spans.push(edge);
    Line::from(spans)
}

fn border(left: &str, title: &str, right: &str, fill: &str) -> Line<'static> {
    let line = Style::default().fg(TEXT_DIM);
    if title.is_empty() {
        return Line::styled(format!("{left}{}{right}", fill.repeat(INNER_WIDTH)), line);
    }
    let tail_len = INNER_WIDTH.saturating_sub(title.chars().count() + 3);
    Line::from(vec![
        Span::styled(format!("{left}{fill} "), line),
        Span::styled(
            title.to_string(),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {}{right}", fill.repeat(tail_len)), line),
    ])
}

fn text(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), Style::default().fg(TEXT))
}

fn dim(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), Style::default().fg(TEXT_DIM))
}

/// The legend entry of `category`: swatch, label, tokens, percent of window.
fn legend_spans(
    usage: &ContextUsage,
    category: ContextCategory,
    ascii: bool,
) -> Vec<Span<'static>> {
    let tokens = usage.of(category);
    let swatch = if ascii { "# " } else { "■ " };
    vec![
        Span::styled(swatch, Style::default().fg(colour(category))),
        text(format!("{:<LABEL_WIDTH$}", category.label())),
        text(format!("{:>TOKENS_WIDTH$}", grouped(u64::from(tokens)))),
        dim(format!("{:>PERCENT_WIDTH$}", percent(tokens, usage.window))),
    ]
}

fn fact(label: &str, value: String) -> Vec<Span<'static>> {
    vec![dim(format!(" {label:<18}")), text(value)]
}

fn tokens_of(n: usize) -> String {
    format!("{} tokens", grouped(n as u64))
}

/// The whole panel as styled lines, for the inspector pager.
pub(crate) fn context_panel_lines(usage: &ContextUsage, ascii: bool) -> StyledLines {
    let (h, tl, tr, bl, br) = if ascii {
        ("-", "+", "+", "+", "+")
    } else {
        ("─", "╭", "╮", "╰", "╯")
    };
    let mut lines = vec![border(tl, "Context usage", tr, h)];

    let window = if usage.window == 0 {
        "unknown window".to_string()
    } else {
        format!(
            "{} / {} tokens ({:.1}%)",
            grouped(u64::from(usage.used())),
            grouped(u64::from(usage.window)),
            usage.used_percent()
        )
    };
    lines.push(framed(
        vec![text(format!(" {}", usage.model)), dim("  "), text(window)],
        ascii,
    ));
    let basis = match (usage.measured, usage.reported_prompt) {
        (false, _) => " no request sent yet: categories are empty".to_string(),
        (true, Some(n)) => format!(
            " categories are estimates (bytes / 4); the provider counted {} tokens",
            grouped(u64::from(n))
        ),
        (true, None) => {
            " categories are estimates (bytes / 4); the provider reported no count".to_string()
        }
    };
    lines.push(framed(vec![dim(basis)], ascii));
    lines.push(framed(vec![], ascii));

    let cells = usage.cells(GRID_SIDE * GRID_SIDE);
    for row in 0..ContextCategory::ALL.len().max(GRID_SIDE) {
        let mut spans = vec![Span::raw(" ")];
        if row < GRID_SIDE {
            for category in &cells[row * GRID_SIDE..(row + 1) * GRID_SIDE] {
                spans.push(Span::styled(
                    cell_glyph(*category, ascii),
                    Style::default().fg(colour(*category)),
                ));
            }
        } else {
            spans.push(Span::raw(" ".repeat(GRID_WIDTH)));
        }
        spans.push(Span::raw("  "));
        if let Some(category) = ContextCategory::ALL.get(row) {
            spans.extend(legend_spans(usage, *category, ascii));
        }
        lines.push(framed(spans, ascii));
    }

    lines.push(framed(vec![], ascii));
    match usage.budget {
        Some(b) => {
            lines.push(framed(
                fact(
                    "Compaction at",
                    format!("~{} of request", tokens_of(b.compaction_at)),
                ),
                ascii,
            ));
            lines.push(framed(
                fact("Response reserve", tokens_of(b.output_reserve)),
                ascii,
            ));
            lines.push(framed(
                fact(
                    "Notes limit",
                    format!("{} per compaction", tokens_of(b.notes_tokens)),
                ),
                ascii,
            ));
            lines.push(framed(
                fact(
                    "Thinking cap",
                    b.thinking_cap
                        .map_or_else(|| "none".to_string(), |c| tokens_of(c as usize)),
                ),
                ascii,
            ));
        }
        None => lines.push(framed(
            fact(
                "Budget",
                "not announced yet (it is sent with the first request)".into(),
            ),
            ascii,
        )),
    }
    lines.push(framed(vec![], ascii));
    lines.push(framed(
        fact(
            "MCP tools",
            format!(
                "{} tools ({} deferred, {} loaded) - {}",
                usage.mcp.total,
                usage.mcp.deferred(),
                usage.mcp.loaded,
                tokens_of(usage.mcp_tokens() as usize)
            ),
        ),
        ascii,
    ));
    lines.push(framed(
        fact(
            "Skills",
            format!(
                "{} skills - {}",
                usage.skill_count,
                tokens_of(usage.of(ContextCategory::Skills) as usize)
            ),
        ),
        ascii,
    ));
    lines.push(border(bl, "", br, h));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};
    use sven_frontend::ContextInputs;
    use sven_tool_registry::ToolSchema;
    use sven_vocab::{ContextBudget, RequestBreakdown};

    fn drawn(usage: &ContextUsage, ascii: bool) -> String {
        let mut pager =
            crate::pager::PagerOverlay::with_title(context_panel_lines(usage, ascii), "CONTEXT");
        pager.scroll_to_line(0);
        let (width, height) = (90, 36);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| pager.render(f, &[], 0, "", None, ascii))
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn usage(breakdown: Option<&RequestBreakdown>) -> ContextUsage {
        let mcp = |name: &str| ToolSchema {
            name: name.into(),
            description: String::new(),
            parameters: serde_json::json!({}),
            is_mcp: true,
        };
        ContextUsage::compute(&ContextInputs {
            model: "gemini-3.6-flash",
            window: 1_048_576,
            budget: Some(ContextBudget {
                window: 1_048_576,
                compaction_at: 1_006_382,
                output_reserve: 20_971,
                notes_tokens: 10_000,
                thinking_cap: None,
            }),
            breakdown,
            registered_tools: &[mcp("a"), mcp("b")],
            skill_count: 3,
        })
    }

    #[test]
    fn the_panel_shows_every_category_and_the_budget_facts() {
        let mut b = RequestBreakdown {
            system_stable: 6_000,
            user: 40_000,
            assistant: 90_000,
            ..RequestBreakdown::default()
        };
        b.system_runtime.insert("Skills".into(), 900);
        b.tool_schemas.insert("shell".into(), 5_000);
        b.tool_results.insert("shell".into(), 60_000);
        let screen = drawn(&usage(Some(&b)), false);
        println!("{screen}");
        for label in ContextCategory::ALL.map(ContextCategory::label) {
            assert!(screen.contains(label), "{label} missing:\n{screen}");
        }
        for needle in [
            "Context usage",
            "gemini-3.6-flash",
            "tokens (",
            "Compaction at",
            "~1,006,382 tokens of request",
            "20,971 tokens",
            "10,000 tokens per compaction",
            "Thinking cap",
            "none",
            "MCP tools",
            "2 tools (2 deferred, 0 loaded)",
            "3 skills",
            "estimates",
            "╭",
            "██",
        ] {
            assert!(screen.contains(needle), "{needle} missing:\n{screen}");
        }
    }

    #[test]
    fn before_any_request_empty_categories_still_appear() {
        let screen = drawn(&usage(None), false);
        println!("{screen}");
        assert!(screen.contains("no request sent yet"), "{screen}");
        for label in ContextCategory::ALL.map(ContextCategory::label) {
            assert!(screen.contains(label), "{label} missing:\n{screen}");
        }
        assert!(screen.contains("Thinking"), "{screen}");
        assert!(!screen.contains("██"), "nothing is used yet:\n{screen}");
    }

    #[test]
    fn the_panel_draws_in_plain_ascii_on_request() {
        let text: String = context_panel_lines(&usage(None), true)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.is_ascii(), "{text}");
        assert!(text.contains("+-"), "{text}");
    }

    #[test]
    fn numbers_are_grouped_by_thousands() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000), "1,000");
        assert_eq!(grouped(1_048_576), "1,048,576");
    }
}
