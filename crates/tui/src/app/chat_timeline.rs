// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The chat as the reader sees it: messages, runs of tools folded into one
//! "Ran N tools" row, and tools that expand in place - all within a line
//! budget of one chat pane per item, so expanding something never buries
//! the conversation. This module lays the chat out into lines and answers
//! the actions on its items (expand, collapse, open in the viewer).
//!
//! Swedish Embedded AB implements solutions for keeping autonomous agents'
//! work readable at a glance for its clients. If your team needs expertise
//! in terminal user interfaces for agent runtimes then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::ops::Range;

use ratatui::text::{Line, Span};
use sven_model::{MessageContent, Role};

use crate::{
    app::{
        chat_state::{ChatFocus, ExpandLevel},
        App,
    },
    chat::{
        markdown::{
            apply_role_style, collapsed_preview, segment_bar_style, segment_to_markdown,
            strip_display_anchors,
        },
        segment::{segment_editable_text, tool_runs, ChatSegment, ToolRun},
        tool_lines::{
            fit_to_budget, run_header, tool_detail, tool_full, tool_row, ToolLineCtx, ToolView,
        },
    },
    markdown::{render_markdown, StyledLines},
    ui::inspector::InspectorOverlay,
};
use sven_tool_api::ToolDisplayRegistry;

/// Runs of fewer tools than this are shown as their tools' rows directly:
/// a header saying "Ran 1 tool" would only hide what the tool was.
const MIN_TOOLS_PER_HEADER: usize = 2;

/// Width of the indent of a tool inside an expanded run.
const RUN_INDENT: &str = "  ";

enum SegKind {
    Call,
    Result,
    Thinking,
    /// User or agent text: the conversation itself, never cut to a budget.
    Text,
    Other,
}

fn seg_kind(seg: &ChatSegment) -> SegKind {
    match seg {
        ChatSegment::Message(m) => match (&m.role, &m.content) {
            (Role::Assistant, MessageContent::ToolCall { .. }) => SegKind::Call,
            (Role::Tool, MessageContent::ToolResult { .. }) => SegKind::Result,
            (Role::User | Role::Assistant, MessageContent::Text(_)) => SegKind::Text,
            _ => SegKind::Other,
        },
        ChatSegment::Thinking { .. } => SegKind::Thinking,
        _ => SegKind::Other,
    }
}

/// If segment `i` is a tool call answered by the very next segment, that
/// result's index.
fn paired_result(segments: &[ChatSegment], i: usize) -> Option<usize> {
    let call_id = match &segments.get(i)? {
        ChatSegment::Message(m) => match &m.content {
            MessageContent::ToolCall { tool_call_id, .. } => tool_call_id,
            _ => return None,
        },
        _ => return None,
    };
    match segments.get(i + 1)? {
        ChatSegment::Message(m) => match &m.content {
            MessageContent::ToolResult { tool_call_id, .. } if tool_call_id == call_id => {
                Some(i + 1)
            }
            _ => None,
        },
        _ => None,
    }
}

/// The chat being laid out: its lines, each segment's line range and the
/// items the highlight can visit.
struct Layout {
    lines: StyledLines,
    ranges: Vec<(usize, usize)>,
    items: Vec<(ChatFocus, (usize, usize))>,
}

impl Layout {
    /// Segment `i` draws nothing (hidden, or drawn as part of another item).
    fn skip(&mut self, i: usize) {
        let at = self.lines.len();
        self.ranges[i] = (at, at);
    }

    /// Draw `lines` as the item `focus`, which shows segment `i`.
    fn item(&mut self, focus: ChatFocus, i: usize, lines: StyledLines) {
        let start = self.lines.len();
        self.lines.extend(lines);
        let range = (start, self.lines.len());
        self.ranges[i] = range;
        if range.1 > range.0 {
            self.items.push((focus, range));
        }
    }
}

fn indented(lines: StyledLines, indent: &'static str) -> StyledLines {
    if indent.is_empty() {
        return lines;
    }
    lines
        .into_iter()
        .map(|line| {
            let mut spans = vec![Span::raw(indent)];
            spans.extend(line.spans);
            Line::from(spans).style(line.style)
        })
        .collect()
}

impl App {
    /// Width the chat is laid out at.
    fn chat_render_width(&self) -> u16 {
        let effective = self.layout.chat_inner_width.max(20);
        match self.tui_config.wrap_width {
            0 => effective,
            w => w.min(effective),
        }
    }

    /// The most lines one item may take inline: one chat pane.
    fn item_line_budget(&self) -> usize {
        usize::from(self.layout.chat_height).max(4)
    }

    fn tool_line_ctx<'a>(&'a self, displays: Option<&'a ToolDisplayRegistry>) -> ToolLineCtx<'a> {
        ToolLineCtx {
            durations: &self.chat.tool_durations,
            started_at: &self.chat.tool_started_at,
            running: &self.agent.tool_start_times,
            streaming: &self.chat.tool_streaming_content,
            displays,
            width: self.chat_render_width(),
            anim_frame: self.agent.anim_frame,
            ascii: self.ascii(),
        }
    }

    /// The tool shown at segment `j` - a call with the result right after
    /// it, or a result whose call is elsewhere - and that result's index.
    fn tool_at(&self, j: usize) -> (Option<ToolView<'_>>, Option<usize>) {
        let segments = self.chat.segments();
        let result = paired_result(segments, j);
        let view = match seg_kind(&segments[j]) {
            SegKind::Call => ToolView::from_segments(
                Some(&segments[j]),
                result.map(|r| &segments[r]),
                &self.chat.tool_args,
            ),
            SegKind::Result => {
                ToolView::from_segments(None, Some(&segments[j]), &self.chat.tool_args)
            }
            _ => None,
        };
        (view, result)
    }

    /// Rebuild `chat.lines`, the segment line ranges and the view items from
    /// the segments plus the streaming tail.
    pub(crate) fn build_display_from_segments(&mut self) {
        let ascii = self.ascii();
        let width = self.chat_render_width();
        let budget = self.item_line_budget();
        let registry = self.shared_tool_displays.get();
        let displays = registry.as_ref().and_then(|r| r.read().ok());
        let ctx = self.tool_line_ctx(displays.as_deref());
        let segments = self.chat.segments();
        let mut out = Layout {
            lines: Vec::new(),
            ranges: vec![(0, 0); segments.len()],
            items: Vec::new(),
        };

        let mut runs = tool_runs(segments, self.tui_config.show_thinking).into_iter();
        let mut next_run = runs.next();
        let mut i = 0;
        while i < segments.len() {
            if let Some(run) = next_run.as_ref().filter(|r| r.segments.start == i) {
                if run.tools >= MIN_TOOLS_PER_HEADER {
                    self.lay_out_run(run, &ctx, budget, &mut out);
                } else {
                    self.lay_out_tools(run.segments.clone(), "", &ctx, budget, &mut out);
                }
                // Messages open with their own blank line; tools do not, so
                // the block closes with one before whatever follows it.
                out.lines.push(Line::default());
                i = run.segments.end;
                next_run = runs.next();
                continue;
            }
            match seg_kind(&segments[i]) {
                SegKind::Thinking if !self.tui_config.show_thinking => out.skip(i),
                // Every tool is in a run; this only keeps the match total.
                SegKind::Call | SegKind::Result => {
                    self.lay_out_tools(i..i + 1, "", &ctx, budget, &mut out)
                }
                kind => {
                    let lines = self.segment_lines(i, width, ascii);
                    let lines = match kind {
                        SegKind::Text => lines,
                        _ => fit_to_budget(lines, budget),
                    };
                    out.item(ChatFocus::Segment(i), i, lines);
                }
            }
            i += 1;
        }
        drop(displays);

        out.lines
            .extend(self.streaming_tail_lines(width, ascii, self.agent.anim_frame));
        self.chat.lines = out.lines;
        self.chat.segment_line_ranges = out.ranges;
        self.chat.view_items = out.items;
        self.settle_focus();
    }

    /// A run of tools: its header and, when expanded, its tools indented
    /// under it.
    fn lay_out_run(&self, run: &ToolRun, ctx: &ToolLineCtx<'_>, budget: usize, out: &mut Layout) {
        let first = run.segments.start;
        let expanded = self.chat.run_expanded(first);
        let tools = self.run_tools(run.segments.clone());
        let at = out.lines.len();
        out.lines.push(run_header(&tools, expanded, ctx));
        out.items.push((ChatFocus::ToolRun(first), (at, at + 1)));
        if expanded {
            self.lay_out_tools(run.segments.clone(), RUN_INDENT, ctx, budget, out);
        } else {
            for j in run.segments.clone() {
                out.skip(j);
            }
        }
    }

    /// The tools in `range`: each call with its result, and results whose
    /// call is not here.
    fn run_tools(&self, range: Range<usize>) -> Vec<ToolView<'_>> {
        let mut tools = Vec::new();
        let mut j = range.start;
        while j < range.end {
            let (view, result) = self.tool_at(j);
            tools.extend(view);
            j = result.unwrap_or(j) + 1;
        }
        tools
    }

    /// Each tool in `range` as an item: its row, or its detail when
    /// expanded (a call's expand state is its own, a lone result's its own).
    fn lay_out_tools(
        &self,
        range: Range<usize>,
        indent: &'static str,
        ctx: &ToolLineCtx<'_>,
        budget: usize,
        out: &mut Layout,
    ) {
        let mut j = range.start;
        while j < range.end {
            let (view, result) = self.tool_at(j);
            match view {
                Some(tool) => {
                    let lines = if self.chat.expand_level(j) >= 1 {
                        tool_detail(&tool, ctx, budget)
                    } else {
                        vec![tool_row(&tool, false, ctx)]
                    };
                    out.item(ChatFocus::Segment(j), j, indented(lines, indent));
                }
                None => out.skip(j),
            }
            if let Some(r) = result {
                out.skip(r);
                j = r;
            }
            j += 1;
        }
    }

    /// A message, note or (shown) thinking block, at its expand level.
    fn segment_lines(&self, i: usize, width: u16, ascii: bool) -> StyledLines {
        let seg = &self.chat.segments()[i];
        let expand: ExpandLevel = if self.nvim.disabled {
            self.chat.expand_level(i)
        } else {
            2 // the Neovim buffer always holds everything
        };
        let md = if expand == 0 {
            collapsed_preview(seg, &self.chat.tool_args)
        } else {
            segment_to_markdown(seg, &self.chat.tool_args)
        };
        let lines = render_markdown(&strip_display_anchors(&md), width, ascii);
        let (bar_style, dim) = segment_bar_style(seg);
        apply_role_style(lines, bar_style, dim)
    }

    /// Keep the highlight on something visible: a tool folded into a
    /// collapsed run hands it to the run's header; anything else that is no
    /// longer drawn hands it to whatever is at the middle of the pane.
    fn settle_focus(&mut self) {
        let visible = |f: ChatFocus| self.chat.view_items.iter().any(|(it, _)| *it == f);
        match self.chat.focus {
            Some(f) if visible(f) => {}
            Some(ChatFocus::Segment(j)) => match self.run_containing(j) {
                Some(start) if visible(ChatFocus::ToolRun(start)) => {
                    self.chat.focus = Some(ChatFocus::ToolRun(start));
                }
                _ => self.recompute_focused_segment(),
            },
            _ => self.recompute_focused_segment(),
        }
    }

    /// The first segment of the run of tools (with a header) holding `j`.
    fn run_containing(&self, j: usize) -> Option<usize> {
        tool_runs(self.chat.segments(), self.tui_config.show_thinking)
            .into_iter()
            .find(|r| r.tools >= MIN_TOOLS_PER_HEADER && r.segments.contains(&j))
            .map(|r| r.segments.start)
    }

    // ── Item actions ──────────────────────────────────────────────────────────

    /// Whether the highlighted item is expanded.
    fn item_expanded(&self, focus: ChatFocus) -> bool {
        match focus {
            ChatFocus::ToolRun(start) => self.chat.run_expanded(start),
            ChatFocus::Segment(i) => match self.chat.segments().get(i) {
                Some(ChatSegment::DelegateSummary { expanded, .. }) => *expanded,
                Some(_) => self.chat.expand_level(i) > 0,
                None => false,
            },
        }
    }

    /// Expand or collapse an item. Text messages expand to their full text;
    /// everything else to at most a pane's worth.
    fn set_item_expanded(&mut self, focus: ChatFocus, expanded: bool) {
        match focus {
            ChatFocus::ToolRun(start) => self.chat.set_run_expanded(start, expanded),
            ChatFocus::Segment(i) => {
                let kind = self.chat.segments().get(i).map(seg_kind);
                if let Some(ChatSegment::DelegateSummary { expanded: e, .. }) =
                    self.chat.segments_mut().get_mut(i)
                {
                    *e = expanded;
                    return;
                }
                let level = match (expanded, kind) {
                    (false, _) => 0,
                    (true, Some(SegKind::Text)) => 2,
                    (true, _) => 1,
                };
                self.chat.set_expand_level(i, level);
            }
        }
    }

    /// Redraw after an item changed size, keeping its first line on screen.
    fn relayout_around(&mut self, focus: ChatFocus) {
        self.build_display_from_segments();
        self.ui.search.update_matches(&self.chat.lines);
        self.chat.focus = Some(focus);
        self.settle_focus();
        self.chat.scroll_offset = self.chat.scroll_offset.min(self.max_chat_scroll());
        if let Some((start, _)) = self.focused_item_range() {
            if start < self.chat.scroll_offset {
                self.chat.scroll_offset = start;
            }
        }
    }

    /// The line range of the highlighted item.
    pub(crate) fn focused_item_range(&self) -> Option<(usize, usize)> {
        let focus = self.chat.focus?;
        self.chat
            .view_items
            .iter()
            .find(|(it, _)| *it == focus)
            .map(|(_, range)| *range)
    }

    /// The item drawn on line `line`.
    pub(crate) fn item_at_line(&self, line: usize) -> Option<ChatFocus> {
        self.chat
            .view_items
            .iter()
            .find(|(_, (s, e))| (*s..*e).contains(&line))
            .map(|(it, _)| *it)
    }

    /// Space, or a click: expand the highlighted item, or collapse it.
    pub(crate) fn toggle_chat_item(&mut self) {
        if let Some(focus) = self.chat.focus {
            let expanded = self.item_expanded(focus);
            self.set_item_expanded(focus, !expanded);
            self.relayout_around(focus);
        }
    }

    /// `l` / Right: expand the item; on an expanded run, step into it.
    pub(crate) fn expand_chat_item(&mut self) {
        let Some(focus) = self.chat.focus else {
            return;
        };
        if !self.item_expanded(focus) {
            self.set_item_expanded(focus, true);
            self.relayout_around(focus);
        } else if let ChatFocus::ToolRun(start) = focus {
            self.chat.focus_segment(start);
            self.settle_focus();
        }
    }

    /// `h` / Left: collapse the item; on a collapsed tool inside a run,
    /// collapse the run and put the highlight on its header.
    pub(crate) fn collapse_chat_item(&mut self) {
        let Some(focus) = self.chat.focus else {
            return;
        };
        if self.item_expanded(focus) {
            self.set_item_expanded(focus, false);
            self.relayout_around(focus);
        } else if let Some(start) = self.run_containing(focus.index()) {
            if focus != ChatFocus::ToolRun(start) {
                let run = ChatFocus::ToolRun(start);
                self.set_item_expanded(run, false);
                self.relayout_around(run);
            }
        }
    }

    /// Enter: edit the highlighted message if it is one; otherwise expand
    /// or collapse it.
    pub(crate) fn chat_item_is_editable(&self) -> bool {
        matches!(self.chat.focus, Some(ChatFocus::Segment(i))
            if segment_editable_text(self.chat.segments(), i).is_some())
    }

    /// `o`: open everything the highlighted item holds, uncut, in the
    /// full-screen viewer. The chat itself does not move.
    pub(crate) fn open_chat_item(&mut self) {
        let Some(focus) = self.chat.focus else {
            return;
        };
        let registry = self.shared_tool_displays.get();
        let displays = registry.as_ref().and_then(|r| r.read().ok());
        let ctx = self.tool_line_ctx(displays.as_deref());
        let (title, lines) = match focus {
            ChatFocus::ToolRun(start) => {
                let end = self.run_end(start).unwrap_or(start + 1);
                let tools = self.run_tools(start..end);
                let mut lines = StyledLines::new();
                for tool in &tools {
                    lines.extend(tool_full(tool, &ctx));
                    lines.push(Line::default());
                }
                (format!("RAN {} TOOLS", tools.len()), lines)
            }
            ChatFocus::Segment(i) => match self.tool_at(i).0 {
                Some(tool) => (tool.name.to_uppercase(), tool_full(&tool, &ctx)),
                None => {
                    let md = segment_to_markdown(&self.chat.segments()[i], &self.chat.tool_args);
                    ("MESSAGE".to_string(), render_markdown(&md, 0, ctx.ascii))
                }
            },
        };
        drop(displays);
        self.ui.inspector = Some(InspectorOverlay::for_content(lines, title));
    }

    /// One past the last segment of the run of tools starting at `start`.
    fn run_end(&self, start: usize) -> Option<usize> {
        tool_runs(self.chat.segments(), self.tui_config.show_thinking)
            .into_iter()
            .find(|r| r.segments.start == start)
            .map(|r| r.segments.end)
    }
}
