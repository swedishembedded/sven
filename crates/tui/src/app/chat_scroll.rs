// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Chat pane scrolling: the scroll position, following new output, and
//! keeping a segment in view.

use crate::app::{chat_state::ChatFocus, App};

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

    /// Scroll so the highlighted item is in view. Called after j/k move it.
    pub(crate) fn scroll_chat_to_show_focus(&mut self) {
        let Some((start, end)) = self.focused_item_range() else {
            return;
        };
        let h = usize::from(self.layout.chat_height);
        if start < self.chat.scroll_offset {
            self.chat.scroll_offset = start;
        } else if end > self.chat.scroll_offset + h {
            // An item taller than the pane shows its top.
            self.chat.scroll_offset = end.saturating_sub(h).min(start);
        }
        self.chat.auto_scroll = false;
    }

    /// Put the highlight on the item at the vertical centre of the chat
    /// pane (none when nothing is drawn there). Used when focus moves to
    /// the chat pane.
    pub(crate) fn recompute_focused_segment(&mut self) {
        let center = self.chat.scroll_offset + usize::from(self.layout.chat_height) / 2;
        self.chat.focus = self.item_at_line(center);
    }

    /// The item the highlight moves to, one step down (`forward`) or up
    /// through what is drawn: hidden thinking, results drawn on their
    /// call's row and tools folded into a collapsed run are not visited.
    /// With no highlight yet, moving down starts at the top and up at the
    /// bottom; at either end the highlight stays where it is.
    pub(crate) fn step_highlight(&self, forward: bool) -> Option<ChatFocus> {
        let items = &self.chat.view_items;
        let at = self
            .chat
            .focus
            .and_then(|f| items.iter().position(|(it, _)| *it == f));
        let next = match (at, forward) {
            (Some(i), true) => items.get(i + 1).or(items.get(i)),
            (Some(i), false) => items.get(i.saturating_sub(1)),
            (None, true) => items.first(),
            (None, false) => items.last(),
        };
        next.map(|(it, _)| *it).or(self.chat.focus)
    }
}
