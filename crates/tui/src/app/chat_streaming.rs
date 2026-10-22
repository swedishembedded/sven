// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The live tail of the chat: the model's text (or, when shown, its
//! thinking) while it is still streaming, before it becomes a segment.

use ratatui::style::{Modifier, Style};

use crate::{
    app::App,
    chat::markdown::{apply_role_style, SYM_THINK},
    markdown::{render_markdown, StyledLines},
    ui::theme::{BAR_AGENT, BAR_THINKING},
};

impl App {
    /// Lines for the streaming buffer, drawn below the last segment. Empty
    /// when nothing is streaming, or when the model is thinking and thinking
    /// is hidden (the status bar says "Thinking" instead).
    pub(crate) fn streaming_tail_lines(
        &self,
        render_width: u16,
        ascii: bool,
        anim_frame: u8,
    ) -> StyledLines {
        if self.chat.streaming_buffer.is_empty()
            || (self.chat.streaming_is_thinking && !self.tui_config.show_thinking)
        {
            return StyledLines::new();
        }
        let sep = if self.chat.segments().is_empty() {
            ""
        } else {
            "\n"
        };
        if self.chat.streaming_is_thinking {
            // Scanning dot (side to side), the same as in-progress tool calls,
            // then the thought itself, dimmed and streamed in full.
            let dot = crate::ui::theme::tool_scan(anim_frame, ascii);
            let header = format!("{sep}{SYM_THINK} **Thinking**  {dot}\n");
            let thinking_style = Some(Style::default().fg(BAR_THINKING));
            let mut lines = apply_role_style(
                render_markdown(&header, render_width, ascii),
                thinking_style,
                false,
            );
            let thought = format!("{}\n", self.chat.streaming_buffer);
            let dimmed = render_markdown(&thought, render_width, ascii)
                .into_iter()
                .map(|mut line| {
                    for span in &mut line.spans {
                        span.style = span.style.add_modifier(Modifier::DIM);
                    }
                    line
                })
                .collect();
            lines.extend(apply_role_style(dimmed, thinking_style, false));
            lines
        } else {
            // Blinking ▌ cursor shows the stream is live.
            let cursor = crate::ui::theme::stream_cursor(anim_frame, ascii);
            let text = format!("{sep}**Agent:** {}{}", self.chat.streaming_buffer, cursor);
            let lines = render_markdown(&text, render_width, ascii);
            apply_role_style(lines, Some(Style::default().fg(BAR_AGENT)), false)
        }
    }
}
