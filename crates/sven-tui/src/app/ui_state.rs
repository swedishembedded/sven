// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! UI overlay, focus state, and ephemeral notification toasts.

use std::time::Instant;

use ratatui::style::Color;

use crate::{
    chat::search::SearchState,
    overlay::{completion::CompletionOverlay, confirm::ConfirmModal, question::QuestionModal},
    pager::PagerOverlay,
};

// ── FocusPane ─────────────────────────────────────────────────────────────────

/// Which pane currently holds keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPane {
    Chat,
    Input,
    /// The compact queue panel shown above the input when there are pending messages.
    Queue,
}

// ── Toast ─────────────────────────────────────────────────────────────────────

/// A brief notification shown in the bottom-right corner.
pub struct Toast {
    pub message: String,
    pub color: Color,
    pub born: Instant,
}

#[allow(dead_code)]
impl Toast {
    /// How long a toast is visible before it disappears.
    pub const LIFETIME_MS: u128 = 3000;

    pub fn new(message: impl Into<String>, color: Color) -> Self {
        Self {
            message: message.into(),
            color,
            born: Instant::now(),
        }
    }

    pub fn info(message: impl Into<String>) -> Self {
        Self::new(message, Color::Cyan)
    }

    pub fn success(message: impl Into<String>) -> Self {
        Self::new(message, Color::Green)
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self::new(message, Color::Yellow)
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::new(message, Color::Red)
    }

    pub fn is_expired(&self) -> bool {
        self.born.elapsed().as_millis() > Self::LIFETIME_MS
    }
}

// ── UiState ───────────────────────────────────────────────────────────────────

/// All UI overlay / modal / focus state.
pub(crate) struct UiState {
    pub focus: FocusPane,
    pub show_help: bool,
    pub search: SearchState,
    pub pager: Option<PagerOverlay>,
    pub completion: Option<CompletionOverlay>,
    pub question_modal: Option<QuestionModal>,
    pub confirm_modal: Option<ConfirmModal>,
    /// True after the first key of a Ctrl+w nav chord has been received.
    pub pending_nav: bool,
    /// Toast notifications (newest last). Cleaned up each frame.
    pub toasts: Vec<Toast>,
}

#[allow(dead_code)]
impl UiState {
    pub fn new() -> Self {
        Self {
            focus: FocusPane::Input,
            show_help: false,
            search: SearchState::default(),
            pager: None,
            completion: None,
            question_modal: None,
            confirm_modal: None,
            pending_nav: false,
            toasts: Vec::new(),
        }
    }

    /// Push a toast notification.
    pub fn push_toast(&mut self, toast: Toast) {
        // Limit the stack to 5 visible toasts; drop the oldest when full.
        if self.toasts.len() >= 5 {
            self.toasts.remove(0);
        }
        self.toasts.push(toast);
    }

    /// Remove expired toasts.  Call once per frame to keep the list lean.
    pub fn prune_toasts(&mut self) {
        self.toasts.retain(|t| !t.is_expired());
    }
}
