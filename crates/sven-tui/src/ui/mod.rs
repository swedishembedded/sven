// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! TUI widget library — all rendering logic lives here.
//!
//! Each module contains a self-contained [`ratatui::widgets::Widget`]
//! (or [`ratatui::widgets::StatefulWidget`]) implementor plus any data types
//! that are tightly coupled to its rendering.

pub(crate) mod chat_pane;
pub(crate) mod completion_menu;
pub(crate) mod help_overlay;
pub(crate) mod input_pane;
pub(crate) mod inspector;
pub(crate) mod modals;
pub(crate) mod queue_panel;
pub(crate) mod search_bar;
pub(crate) mod status_bar;
pub(crate) mod team_picker;
pub(crate) mod theme;
pub(crate) mod toast;
pub(crate) mod welcome;
pub(crate) mod which_key;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub(crate) use chat_pane::{nvim_cursor_screen_pos, ChatLabels, ChatPane};
pub(crate) use completion_menu::CompletionMenu;
pub(crate) use help_overlay::HelpOverlay;
pub(crate) use input_pane::{input_cursor_screen_pos, InputEditMode, InputPane};
pub(crate) use inspector::{InspectorKind, InspectorOverlay};
pub(crate) use modals::{ConfirmModalView, QuestionModalView};
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
