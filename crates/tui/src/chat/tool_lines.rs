// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Chat lines for tool activity: a tool's one-line row, its expanded detail
//! (arguments, diff, output) and the header of a run of tools.
//!
//! Every function here is bounded: the detail of a tool stops at the line
//! budget it is given, so a 100 000-line result costs no more to draw than
//! a screenful.
//!
//! Swedish Embedded AB implements solutions for supervising autonomous
//! agents' tool use for its clients. If your team needs expertise in
//! terminal interfaces for agent runtimes then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::collections::HashMap;
use std::time::Instant;

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use sven_model::MessageContent;
use sven_tool_api::{ToolDisplay, ToolDisplayRegistry};

use crate::{
    chat::segment::ChatSegment,
    markdown::StyledLines,
    ui::{
        theme::{tool_scan, BAR_ERROR, TEXT, TEXT_DIM},
        tool_renderer,
        width_utils::truncate_to_width,
    },
};

const OK_GREEN: Color = Color::Rgb(80, 200, 120);
const DURATION_GREY: Color = Color::Rgb(120, 120, 140);

/// What the tool lines need from the chat: names, timings, live output and
/// the tools' own display metadata.
pub(crate) struct ToolLineCtx<'a> {
    /// `call_id → seconds` for finished tools.
    pub durations: &'a HashMap<String, f32>,
    /// `call_id → start` for every tool started in this process.
    pub started_at: &'a HashMap<String, Instant>,
    /// `call_id → start` for the tools running right now.
    pub running: &'a HashMap<String, Instant>,
    /// `call_id → output so far` for running tools that stream it.
    pub streaming: &'a HashMap<String, String>,
    /// `call_id → tokens used` for tools that ran a subagent.
    pub agent_tokens: &'a HashMap<String, u64>,
    pub displays: Option<&'a ToolDisplayRegistry>,
    pub width: u16,
    pub anim_frame: u8,
    pub ascii: bool,
}

impl ToolLineCtx<'_> {
    fn display(&self, name: &str) -> Option<&dyn ToolDisplay> {
        self.displays.and_then(|d| d.get(name))
    }
}

/// One tool as the chat shows it: its call and, once it finished, its output.
pub(crate) struct ToolView<'a> {
    pub call_id: &'a str,
    pub name: &'a str,
    pub args: serde_json::Value,
    pub output: Option<String>,
}

/// How a tool stands.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolStatus {
    Running,
    Succeeded,
    Failed,
    /// No result and not running: the turn ended (aborted) before it did.
    Unfinished,
}

impl<'a> ToolView<'a> {
    /// The tool made of a call and/or its result; `None` if neither is a
    /// tool segment.
    pub fn from_segments(
        call: Option<&'a ChatSegment>,
        result: Option<&'a ChatSegment>,
        tool_names: &'a HashMap<String, String>,
    ) -> Option<Self> {
        let output = result.and_then(|seg| match seg {
            ChatSegment::Message(m) => match &m.content {
                MessageContent::ToolResult {
                    tool_call_id,
                    content,
                } => Some((tool_call_id.as_str(), content.to_string())),
                _ => None,
            },
            _ => None,
        });
        let called = call.and_then(|seg| match seg {
            ChatSegment::Message(m) => match &m.content {
                MessageContent::ToolCall {
                    tool_call_id,
                    function,
                } => Some((tool_call_id.as_str(), function)),
                _ => None,
            },
            _ => None,
        });
        match (called, output) {
            (Some((call_id, function)), output) => Some(Self {
                call_id,
                name: function.name.as_str(),
                args: serde_json::from_str(&function.arguments).unwrap_or(serde_json::Value::Null),
                output: output.map(|(_, text)| text),
            }),
            (None, Some((call_id, text))) => Some(Self {
                call_id,
                name: tool_names.get(call_id).map_or("tool", String::as_str),
                args: serde_json::Value::Null,
                output: Some(text),
            }),
            (None, None) => None,
        }
    }

    pub fn status(&self, ctx: &ToolLineCtx<'_>) -> ToolStatus {
        match &self.output {
            Some(out) if out.starts_with("error:") => ToolStatus::Failed,
            Some(_) => ToolStatus::Succeeded,
            None if ctx.running.contains_key(self.call_id) => ToolStatus::Running,
            None => ToolStatus::Unfinished,
        }
    }

    /// Seconds the tool ran, or has been running.
    pub fn seconds(&self, ctx: &ToolLineCtx<'_>) -> Option<f32> {
        ctx.durations.get(self.call_id).copied().or_else(|| {
            ctx.running
                .get(self.call_id)
                .map(|start| start.elapsed().as_secs_f32())
        })
    }

    /// When the tool started and when it ended (now, while running).
    fn span(&self, ctx: &ToolLineCtx<'_>) -> Option<(Instant, Instant)> {
        let start = *ctx.started_at.get(self.call_id)?;
        let end = match ctx.durations.get(self.call_id) {
            Some(secs) => start + std::time::Duration::from_secs_f32(*secs),
            None => Instant::now(),
        };
        Some((start, end))
    }
}

/// `0.4s`, `12s`, `3m 05s`, `1h 02m`.
pub(crate) fn format_duration(secs: f32) -> String {
    let whole = secs as u64;
    match whole {
        0..=9 => format!("{secs:.1}s"),
        10..=59 => format!("{whole}s"),
        60..=3599 => format!("{}m {:02}s", whole / 60, whole % 60),
        _ => format!("{}h {:02}m", whole / 3600, (whole % 3600) / 60),
    }
}

fn marker(expanded: bool, ascii: bool) -> &'static str {
    match (expanded, ascii) {
        (true, false) => "▾",
        (false, false) => "▸",
        (true, true) => "v",
        (false, true) => ">",
    }
}

fn status_spans(
    status: ToolStatus,
    seconds: Option<f32>,
    ctx: &ToolLineCtx<'_>,
) -> Vec<Span<'static>> {
    let (sym, color) = match status {
        ToolStatus::Running => (tool_scan(ctx.anim_frame, ctx.ascii), TEXT_DIM),
        ToolStatus::Succeeded => (if ctx.ascii { "ok" } else { "✓" }, OK_GREEN),
        ToolStatus::Failed => (if ctx.ascii { "x" } else { "✗" }, BAR_ERROR),
        ToolStatus::Unfinished => ("-", TEXT_DIM),
    };
    let mut spans = vec![Span::styled(format!("  {sym}"), Style::default().fg(color))];
    if let Some(secs) = seconds {
        spans.push(Span::styled(
            format!("  {}", format_duration(secs)),
            Style::default().fg(DURATION_GREY),
        ));
    }
    spans
}

/// The one-line row of a tool: icon, name, what it worked on, its status
/// and how long it took.
pub(crate) fn tool_row(
    tool: &ToolView<'_>,
    expanded: bool,
    ctx: &ToolLineCtx<'_>,
) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{} ", marker(expanded, ctx.ascii)),
        Style::default().fg(TEXT_DIM),
    )];
    spans.extend(tool_renderer::render_tool_call_collapsed(
        tool.name,
        &tool.args,
        None,
        ctx.display(tool.name),
        usize::from(ctx.width).saturating_sub(16),
    ));
    spans.extend(status_spans(tool.status(ctx), tool.seconds(ctx), ctx));
    if let Some(tokens) = ctx.agent_tokens.get(tool.call_id).filter(|t| **t > 0) {
        spans.push(Span::styled(
            format!(
                " · {} tok",
                crate::ui::activity_panel::format_tokens(*tokens)
            ),
            Style::default().fg(DURATION_GREY),
        ));
    }
    Line::from(spans)
}

/// The footer of a view cut short at its line budget.
pub(crate) fn more_lines_footer(hidden: usize) -> Line<'static> {
    Line::from(Span::styled(
        format!("  … {hidden} more lines · o: open in viewer"),
        Style::default().fg(TEXT_DIM).add_modifier(Modifier::ITALIC),
    ))
}

/// Cut `lines` down to `max_lines`, the last of them a footer saying how
/// many were left out. Views that fit are returned unchanged.
pub(crate) fn fit_to_budget(mut lines: StyledLines, max_lines: usize) -> StyledLines {
    let max_lines = max_lines.max(2);
    if lines.len() > max_lines {
        let hidden = lines.len() - (max_lines - 1);
        lines.truncate(max_lines - 1);
        lines.push(more_lines_footer(hidden));
    }
    lines
}

fn output_line(text: &str, width: u16) -> Line<'static> {
    Line::from(Span::styled(
        format!(
            "    {}",
            truncate_to_width(text, usize::from(width).saturating_sub(4))
        ),
        Style::default().fg(TEXT),
    ))
}

/// A tool expanded: its row, what it was asked to do (the command, the
/// path, an edit's diff) and its output, in at most `max_lines` lines.
/// While it runs, the output is the tail of what it has streamed so far.
pub(crate) fn tool_detail(
    tool: &ToolView<'_>,
    ctx: &ToolLineCtx<'_>,
    max_lines: usize,
) -> StyledLines {
    let mut lines = vec![tool_row(tool, true, ctx)];
    if !tool.args.is_null() {
        lines.extend(
            tool_renderer::render_tool_call_expanded(
                tool.name,
                &tool.args,
                ctx.width.saturating_sub(2),
                ctx.display(tool.name),
            )
            .into_iter()
            .map(|line| {
                let mut spans = vec![Span::raw("  ")];
                spans.extend(line.spans);
                Line::from(spans)
            }),
        );
    }
    let max_lines = max_lines.max(2);
    let streamed = ctx.streaming.get(tool.call_id);
    let output: Vec<&str> = match (&tool.output, streamed) {
        (Some(out), _) => out.lines().collect(),
        (None, Some(live)) => {
            // While it runs, the newest output is what matters.
            let live: Vec<&str> = live.lines().collect();
            let room = max_lines.saturating_sub(lines.len());
            live[live.len().saturating_sub(room)..].to_vec()
        }
        (None, None) => Vec::new(),
    };
    let total = lines.len() + output.len();
    if total <= max_lines {
        lines.extend(output.iter().map(|l| output_line(l, ctx.width)));
        return lines;
    }
    // Over budget: keep what fits, then one line saying how much is left.
    let keep = max_lines - 1;
    lines.truncate(keep);
    let room = keep - lines.len();
    lines.extend(output.iter().take(room).map(|l| output_line(l, ctx.width)));
    lines.push(more_lines_footer(total - keep));
    lines
}

/// Everything about a tool, uncut, for the full-screen viewer.
pub(crate) fn tool_full(tool: &ToolView<'_>, ctx: &ToolLineCtx<'_>) -> StyledLines {
    let mut lines = vec![tool_row(tool, true, ctx)];
    if !tool.args.is_null() {
        lines.extend(tool_renderer::render_tool_call_expanded(
            tool.name,
            &tool.args,
            ctx.width,
            ctx.display(tool.name),
        ));
    }
    let output = tool
        .output
        .as_deref()
        .or(ctx.streaming.get(tool.call_id).map(String::as_str))
        .unwrap_or("");
    lines.extend(output.lines().map(|l| Line::from(l.to_string())));
    lines
}

/// The header of a run of tools: `▸ Ran 4 tools · 1 failed · 3.2s`, or
/// `Running …` with the tool that is running now while any still is.
pub(crate) fn run_header(
    tools: &[ToolView<'_>],
    expanded: bool,
    ctx: &ToolLineCtx<'_>,
) -> Line<'static> {
    let statuses: Vec<ToolStatus> = tools.iter().map(|t| t.status(ctx)).collect();
    let running = tools
        .iter()
        .zip(&statuses)
        .find(|(_, s)| **s == ToolStatus::Running)
        .map(|(t, _)| t);
    let failed = statuses
        .iter()
        .filter(|s| **s == ToolStatus::Failed)
        .count();
    let n = tools.len();
    let noun = if n == 1 { "tool" } else { "tools" };
    let verb = if running.is_some() { "Running" } else { "Ran" };

    let mut spans = vec![
        Span::styled(
            format!("{} ", marker(expanded, ctx.ascii)),
            Style::default().fg(TEXT_DIM),
        ),
        Span::styled(
            format!("{verb} {n} {noun}"),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(tool) = running {
        spans.push(Span::styled(
            format!(" · {} {}", tool.name, tool_scan(ctx.anim_frame, ctx.ascii)),
            Style::default().fg(TEXT_DIM),
        ));
    }
    if failed > 0 {
        spans.push(Span::styled(
            format!(" · {failed} failed"),
            Style::default().fg(BAR_ERROR),
        ));
    }
    // Wall-clock time from the first start to the last end: tools in a
    // batch run in parallel, so their durations do not add up.
    let spans_known: Vec<_> = tools.iter().filter_map(|t| t.span(ctx)).collect();
    if spans_known.len() == n {
        let start = spans_known.iter().map(|s| s.0).min();
        let end = spans_known.iter().map(|s| s.1).max();
        if let (Some(start), Some(end)) = (start, end) {
            spans.push(Span::styled(
                format!(" · {}", format_duration((end - start).as_secs_f32())),
                Style::default().fg(DURATION_GREY),
            ));
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_at_a_glance() {
        assert_eq!(format_duration(0.42), "0.4s");
        assert_eq!(format_duration(12.7), "12s");
        assert_eq!(format_duration(185.0), "3m 05s");
        assert_eq!(format_duration(3720.0), "1h 02m");
    }

    #[test]
    fn a_view_over_budget_ends_in_a_footer_counting_what_it_left_out() {
        let lines: StyledLines = (0..50).map(|i| Line::from(format!("{i}"))).collect();
        let fitted = fit_to_budget(lines, 10);
        assert_eq!(fitted.len(), 10);
        assert!(fitted[9].to_string().contains("41 more lines"));
    }
}
