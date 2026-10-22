// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Activity panel widget: what the session's subagents are doing, between
//! the chat and the input. Folded, it is one line summing them up; open, a
//! row per agent - its task, the tool it is in, how many tools it ran, how
//! long it has taken and the tokens it used - with a scrollbar when there
//! are more agents than rows.
//!
//! Swedish Embedded AB implements solutions for supervising parallel
//! autonomous agents for its clients. If your team needs expertise in
//! terminal interfaces for agent runtimes then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::time::Instant;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget, Widget},
};
use sven_frontend::{SubagentActivity, SubagentState};

use super::{
    theme::{open_pane_block, spinner_char, BAR_ERROR, TEXT, TEXT_DIM},
    width_utils::truncate_to_width,
};
use crate::chat::tool_lines::format_duration;

const OK_GREEN: Color = Color::Rgb(80, 200, 120);
const SELECTED_BG: Color = Color::Rgb(35, 45, 55);

/// The activity panel.
pub struct ActivityPanel<'a> {
    pub agents: &'a [&'a SubagentActivity],
    pub expanded: bool,
    pub selected: usize,
    pub scroll: usize,
    pub focused: bool,
    pub ascii: bool,
    pub now: Instant,
    pub anim_frame: u8,
}

/// `950`, `12.3k`, `1.2M`.
pub(crate) fn format_tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

impl ActivityPanel<'_> {
    /// `Agents · 2 running · 1 done · 1m 14s · 12.3k tok`: counts by state,
    /// the wall-clock time from the first start to the last end, and the
    /// tokens all of them used.
    fn summary(&self) -> String {
        let running = self.agents.iter().filter(|a| a.is_running()).count();
        let done = self
            .agents
            .iter()
            .filter(|a| a.state == SubagentState::Completed)
            .count();
        let failed = self.agents.len() - running - done;
        let mut parts = vec!["Agents".to_string()];
        for (n, word) in [(running, "running"), (done, "done"), (failed, "failed")] {
            if n > 0 {
                parts.push(format!("{n} {word}"));
            }
        }
        let start = self.agents.iter().map(|a| a.started).min();
        let end = self
            .agents
            .iter()
            .map(|a| a.ended.unwrap_or(self.now))
            .max();
        if let (Some(start), Some(end)) = (start, end) {
            parts.push(format_duration(
                end.saturating_duration_since(start).as_secs_f32(),
            ));
        }
        let tokens: u64 = self.agents.iter().map(|a| a.total_tokens()).sum();
        if tokens > 0 {
            parts.push(format!("{} tok", format_tokens(tokens)));
        }
        parts.join(" · ")
    }

    fn status(&self, agent: &SubagentActivity) -> Span<'static> {
        match &agent.state {
            SubagentState::Running => Span::styled(
                spinner_char(self.anim_frame, self.ascii),
                Style::default().fg(TEXT_DIM),
            ),
            SubagentState::Completed => Span::styled(
                if self.ascii { "ok" } else { "✓" },
                Style::default().fg(OK_GREEN),
            ),
            SubagentState::Failed { .. } => Span::styled(
                if self.ascii { "x" } else { "✗" },
                Style::default().fg(BAR_ERROR),
            ),
        }
    }

    /// One agent: status, task, then what it is doing or how it ended, its
    /// tool count, time and tokens.
    fn row(&self, agent: &SubagentActivity, width: usize) -> Line<'static> {
        let doing = match (&agent.state, &agent.current_tool) {
            (SubagentState::Running, Some(tool)) => tool.clone(),
            (SubagentState::Running, None) => "thinking".to_string(),
            (SubagentState::Completed, _) => "done".to_string(),
            (SubagentState::Failed { reason }, _) => format!("failed: {reason}"),
        };
        let tools = match agent.tools_run {
            1 => "1 tool".to_string(),
            n => format!("{n} tools"),
        };
        let mut facts = vec![
            doing,
            tools,
            format_duration(agent.elapsed(self.now).as_secs_f32()),
        ];
        if agent.total_tokens() > 0 {
            facts.push(format!("{} tok", format_tokens(agent.total_tokens())));
        }
        let task_width = (width * 2 / 5).max(12);
        let task = truncate_to_width(&agent.description, task_width);
        let pad = task_width.saturating_sub(crate::ui::width_utils::display_width(&task));
        Line::from(vec![
            Span::raw(" "),
            self.status(agent),
            Span::raw(" "),
            Span::styled(
                format!("{task}{}", " ".repeat(pad)),
                Style::default().fg(TEXT),
            ),
            Span::styled(
                format!("  {}", facts.join(" · ")),
                Style::default().fg(TEXT_DIM),
            ),
        ])
    }
}

impl Widget for ActivityPanel<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || self.agents.is_empty() {
            return;
        }
        let marker = match (self.expanded, self.ascii) {
            (true, false) => "▾",
            (false, false) => "▸",
            (true, true) => "v",
            (false, true) => ">",
        };
        if !self.expanded {
            let style = if self.focused {
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(TEXT_DIM)
            };
            let hint = if self.focused {
                "  Enter open · Esc"
            } else {
                "  ^w a"
            };
            Paragraph::new(Line::from(vec![
                Span::styled(format!(" {marker} {}", self.summary()), style),
                Span::styled(hint, Style::default().fg(TEXT_DIM)),
            ]))
            .render(area, buf);
            return;
        }

        let block = open_pane_block(
            &format!("{marker} {}", self.summary()),
            self.focused,
            self.ascii,
        );
        let inner = block.inner(area);
        block.render(area, buf);
        let rows = usize::from(inner.height);
        let overflow = self.agents.len() > rows;
        // The scrollbar takes the last column when the agents do not fit.
        let text_width = usize::from(inner.width).saturating_sub(usize::from(overflow));
        for (row, (i, agent)) in self
            .agents
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(rows)
            .enumerate()
        {
            let y = inner.y + row as u16;
            let line_area = Rect::new(inner.x, y, text_width as u16, 1);
            Paragraph::new(self.row(agent, text_width)).render(line_area, buf);
            if self.focused && i == self.selected {
                buf.set_style(line_area, Style::default().bg(SELECTED_BG));
            }
        }
        if overflow {
            let mut state =
                ScrollbarState::new(self.agents.len().saturating_sub(rows)).position(self.scroll);
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .render(inner, buf, &mut state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_counts_read_at_a_glance() {
        assert_eq!(format_tokens(950), "950");
        assert_eq!(format_tokens(12_300), "12.3k");
        assert_eq!(format_tokens(1_250_000), "1.2M");
    }

    #[test]
    fn more_agents_than_rows_draw_a_scrollbar() {
        let now = Instant::now();
        let agents: Vec<SubagentActivity> = (0..10)
            .map(|i| SubagentActivity::started(format!("c{i}"), format!("agent {i}"), now))
            .collect();
        let refs: Vec<&SubagentActivity> = agents.iter().collect();
        let area = Rect::new(0, 0, 60, 6); // 4 rows inside the borders
        let mut buf = Buffer::empty(area);
        ActivityPanel {
            agents: &refs,
            expanded: true,
            selected: 0,
            scroll: 0,
            focused: true,
            ascii: false,
            now,
            anim_frame: 0,
        }
        .render(area, &mut buf);
        let right: String = (1..5).map(|y| buf[(59, y)].symbol().to_string()).collect();
        assert!(
            right.chars().any(|c| c != ' '),
            "scrollbar column: {right:?}"
        );
        let text: String = (0..6)
            .map(|y| (0..60).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("agent 3") && !text.contains("agent 4"),
            "{text}"
        );
    }
}
