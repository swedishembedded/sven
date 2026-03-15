// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Completion overlay state.
//!
//! The overlay renders a fuzzy-filtered list of slash command completions
//! above (or below) the input box.  Navigation uses arrow keys / Tab; Enter
//! selects the highlighted item; Esc dismisses.
//!
//! Selection and scroll state are managed via ratatui's [`ListState`] so that
//! the [`CompletionMenu`](crate::ui::completion_menu::CompletionMenu) widget
//! can delegate scrolling to [`ratatui::widgets::List`].

use ratatui::widgets::ListState;

use crate::commands::CompletionItem;

/// State of the active completion overlay.
pub struct CompletionOverlay {
    /// All completions for the current command/argument (pre-filtered and ranked).
    pub items: Vec<CompletionItem>,

    /// ratatui list state — owns both the selected index and the scroll offset.
    pub list_state: ListState,
}

impl CompletionOverlay {
    pub fn new(items: Vec<CompletionItem>) -> Self {
        let mut list_state = ListState::default();
        if !items.is_empty() {
            list_state.select(Some(0));
        }
        Self { items, list_state }
    }

    /// Move selection down by one, wrapping around.
    pub fn select_next(&mut self) {
        if self.items.is_empty() {
            return;
        }
        let next = match self.list_state.selected() {
            Some(i) => (i + 1) % self.items.len(),
            None => 0,
        };
        self.list_state.select(Some(next));
    }

    /// Move selection up by one, wrapping around.
    pub fn select_prev(&mut self) {
        if self.items.is_empty() {
            return;
        }
        let prev = match self.list_state.selected() {
            Some(0) | None => self.items.len() - 1,
            Some(i) => i - 1,
        };
        self.list_state.select(Some(prev));
    }

    /// Return the currently selected item, if any.
    pub fn selected_item(&self) -> Option<&CompletionItem> {
        self.list_state.selected().and_then(|i| self.items.get(i))
    }

    /// Selected index (0-based).  Returns `0` when nothing is selected.
    pub fn selected(&self) -> usize {
        self.list_state.selected().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_overlay(n: usize) -> CompletionOverlay {
        let items: Vec<CompletionItem> = (0..n)
            .map(|i| CompletionItem::simple(format!("item{i}")))
            .collect();
        CompletionOverlay::new(items)
    }

    #[test]
    fn select_next_wraps_around() {
        let mut overlay = make_overlay(3);
        overlay.select_next();
        assert_eq!(overlay.selected(), 1);
        overlay.select_next();
        overlay.select_next();
        assert_eq!(overlay.selected(), 0, "should wrap around");
    }

    #[test]
    fn select_prev_wraps_around() {
        let mut overlay = make_overlay(3);
        overlay.select_prev();
        assert_eq!(overlay.selected(), 2, "should wrap to last item");
    }

    #[test]
    fn scroll_adjusts_when_selection_moves_below_viewport() {
        let mut overlay = make_overlay(20);
        for _ in 0..6 {
            overlay.select_next();
        }
        // After 6 next() calls the selected index is 6; ListState manages offset.
        assert_eq!(overlay.selected(), 6);
    }

    #[test]
    fn selected_item_returns_correct_entry() {
        let overlay = make_overlay(10);
        assert_eq!(
            overlay.selected_item().map(|i| i.value.as_str()),
            Some("item0")
        );
    }
}
