// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Completion overlay state.
//!
//! The overlay renders a fuzzy-filtered list of slash command completions
//! above (or below) the input box.  Navigation uses arrow keys / Tab; Enter
//! selects the highlighted item; Esc dismisses.

use crate::commands::CompletionItem;

/// State of the active completion overlay.
pub struct CompletionOverlay {
    /// All completions for the current command/argument (pre-filtered and ranked).
    pub items: Vec<CompletionItem>,

    /// Index of the currently highlighted item.
    pub selected: usize,

    /// Scroll offset within `items` (first visible item index).
    pub scroll_offset: usize,

    /// Maximum number of items to show at once (controls overlay height).
    pub max_visible: usize,
}

impl CompletionOverlay {
    pub fn new(items: Vec<CompletionItem>) -> Self {
        Self {
            items,
            selected: 0,
            scroll_offset: 0,
            max_visible: 8,
        }
    }

    /// Move selection down by one, scrolling if needed.
    pub fn select_next(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.items.len();
        self.adjust_scroll();
    }

    /// Move selection up by one, scrolling if needed.
    pub fn select_prev(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.selected = self.selected
            .checked_sub(1)
            .unwrap_or(self.items.len() - 1);
        self.adjust_scroll();
    }

    /// Return the currently selected item, if any.
    pub fn selected_item(&self) -> Option<&CompletionItem> {
        self.items.get(self.selected)
    }

    /// The visible slice of items.
    pub fn visible_items(&self) -> &[CompletionItem] {
        let end = (self.scroll_offset + self.max_visible).min(self.items.len());
        &self.items[self.scroll_offset..end]
    }

    /// Public alias for `adjust_scroll`, used after externally setting `selected`.
    pub fn adjust_scroll_pub(&mut self) {
        self.adjust_scroll();
    }

    fn adjust_scroll(&mut self) {
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + self.max_visible {
            self.scroll_offset = self.selected + 1 - self.max_visible;
        }
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
        assert_eq!(overlay.selected, 1);
        overlay.select_next();
        overlay.select_next();
        assert_eq!(overlay.selected, 0, "should wrap around");
    }

    #[test]
    fn select_prev_wraps_around() {
        let mut overlay = make_overlay(3);
        overlay.select_prev();
        assert_eq!(overlay.selected, 2, "should wrap to last item");
    }

    #[test]
    fn scroll_adjusts_when_selection_moves_below_viewport() {
        let mut overlay = make_overlay(20);
        overlay.max_visible = 5;
        for _ in 0..6 {
            overlay.select_next();
        }
        assert!(overlay.scroll_offset > 0, "scroll should advance");
        assert!(overlay.selected >= overlay.scroll_offset);
        assert!(overlay.selected < overlay.scroll_offset + overlay.max_visible);
    }

    #[test]
    fn visible_items_respects_scroll_and_max() {
        let mut overlay = make_overlay(10);
        overlay.max_visible = 3;
        overlay.scroll_offset = 2;
        let visible = overlay.visible_items();
        assert_eq!(visible.len(), 3);
        assert_eq!(visible[0].value, "item2");
    }
}
