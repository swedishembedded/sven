// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Chat pane scrolling: the scroll position, following new output, and
//! keeping a segment in view.

use crate::{app::App, chat::segment::segment_at_line};

impl App {
    pub(crate) fn scroll_up(&mut self, n: u16) {
        self.chat.scroll_offset = self.chat.scroll_offset.saturating_sub(usize::from(n));
        self.chat.auto_scroll = false;
    }

    pub(crate) fn scroll_down(&mut self, n: u16) {
        let max = self.max_chat_scroll();
        self.chat.scroll_offset = (self.chat.scroll_offset + usize::from(n)).min(max);
        if self.chat.scroll_offset >= max {
            self.chat.auto_scroll = true;
        }
    }

    pub(crate) fn scroll_to_bottom(&mut self) {
        if self.nvim.bridge.is_none() && self.chat.auto_scroll {
            self.chat.scroll_offset = self.max_chat_scroll();
        }
    }

    /// The largest useful `chat.scroll_offset`: the one that shows the last
    /// rendered line on the bottom row of the chat pane.
    pub(crate) fn max_chat_scroll(&self) -> usize {
        self.chat
            .lines
            .len()
            .saturating_sub(usize::from(self.layout.chat_height))
    }

    /// Adjust scroll so the segment at `seg_idx` is visible. Called after j/k move the highlight.
    pub(crate) fn scroll_chat_to_show_segment(&mut self, seg_idx: usize) {
        let Some(&(seg_start, seg_end)) = self.chat.segment_line_ranges.get(seg_idx) else {
            return;
        };
        let h = self.layout.chat_height as usize;
        if seg_start < self.chat.scroll_offset {
            self.chat.scroll_offset = seg_start;
        } else if seg_end > self.chat.scroll_offset + h {
            self.chat.scroll_offset = seg_end.saturating_sub(h);
        }
        self.chat.auto_scroll = false;
    }

    /// Recompute the keyboard-focused segment (highlight) based on the current
    /// scroll offset and chat height. Used when focus moves to the chat pane.
    pub(crate) fn recompute_focused_segment(&mut self) {
        let center = self.chat.scroll_offset + self.layout.chat_height as usize / 2;
        self.chat.focused_segment = segment_at_line(&self.chat.segment_line_ranges, center);
    }

    /// The segment the highlight moves to from the current one, one step
    /// down (`forward`) or up, skipping segments that render no lines
    /// (hidden thinking, results drawn on their call's line). With no
    /// highlight yet, moving down starts at the top and up at the bottom;
    /// at either end the highlight stays where it is.
    pub(crate) fn step_highlight(&self, forward: bool) -> Option<usize> {
        let visible = |i: &usize| {
            self.chat
                .segment_line_ranges
                .get(*i)
                .is_some_and(|&(start, end)| end > start)
        };
        let n = self.chat.segments().len();
        let found = match (self.chat.focused_segment, forward) {
            (Some(i), true) => (i + 1..n).find(visible),
            (Some(i), false) => (0..i).rev().find(visible),
            (None, true) => (0..n).find(visible),
            (None, false) => (0..n).rev().find(visible),
        };
        found.or(self.chat.focused_segment)
    }
}
