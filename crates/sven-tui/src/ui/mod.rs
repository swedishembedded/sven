// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! TUI widget library — all rendering logic lives here.
//!
//! Each module contains a self-contained [`ratatui::widgets::Widget`]
//! (or [`ratatui::widgets::StatefulWidget`]) implementor plus any data types
//! that are tightly coupled to its rendering.

use ratatui::{
    layout::Rect,
    widgets::{Scrollbar, ScrollbarOrientation},
};

pub(crate) mod chat_list_pane;
pub(crate) mod chat_pane;
pub(crate) mod completion_menu;
pub(crate) mod help_overlay;
pub(crate) mod input_pane;
pub(crate) mod inspector;
pub(crate) mod modals;
pub(crate) mod peers_pane;
pub(crate) mod queue_panel;
pub(crate) mod search_bar;
pub(crate) mod status_bar;
pub(crate) mod team_picker;
pub(crate) mod theme;
pub(crate) mod toast;
pub(crate) mod tool_renderer;
pub(crate) mod welcome;
pub(crate) mod which_key;
pub(crate) mod width_utils;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub(crate) use chat_list_pane::{build_chat_list_items, ChatListPane};
pub(crate) use chat_pane::{nvim_cursor_screen_pos, ChatPane};
pub(crate) use completion_menu::CompletionMenu;
pub(crate) use help_overlay::HelpOverlay;
pub(crate) use input_pane::{input_cursor_screen_pos, InputEditMode, InputPane};
pub(crate) use inspector::{InspectorKind, InspectorOverlay};
pub(crate) use modals::{ConfirmModalView, QuestionModalView};
pub(crate) use peers_pane::{PeerListItem, PeersPane};
pub(crate) use queue_panel::{QueueItem, QueuePanel};
pub(crate) use search_bar::SearchBar;
pub(crate) use status_bar::StatusBar;
pub(crate) use team_picker::{AgentPickerStatus, TeamPickerOverlay};
pub(crate) use theme::open_pane_block;
#[allow(unused)]
pub(crate) use theme::pane_block;
pub(crate) use toast::ToastStack;
pub(crate) use welcome::WelcomeScreen;
pub(crate) use which_key::WhichKeyOverlay;
#[allow(unused_imports)]
pub(crate) use width_utils::{
    char_width, col_to_byte_offset, display_width, fit_to_width, truncate_to_width,
    truncate_to_width_exact,
};

// ── Shared render helpers ─────────────────────────────────────────────────────

/// Standard sven vertical scrollbar (right side, no begin/end symbols).
///
/// Both [`ChatPane`] and [`InputPane`] use identical configuration — this
/// keeps the look consistent and removes duplication.
pub(crate) fn sven_scrollbar() -> Scrollbar<'static> {
    Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .thumb_symbol("|")
        .track_symbol(Some("░"))
}

/// Compute a centered popup rectangle inside `area`.
///
/// The result is clamped so the popup never exceeds `area`'s dimensions.
pub(crate) fn centered_popup(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    Rect::new(x, y, w, h)
}
