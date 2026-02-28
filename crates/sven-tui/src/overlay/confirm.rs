// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Generic confirmation / message modal.
//!
//! # Usage
//!
//! ```rust,ignore
//! // Open a yes/no confirmation:
//! self.confirm_modal = Some(ConfirmModal::new(
//!     "Delete message",
//!     "Remove this message from the conversation?",
//!     ConfirmedAction::RemoveSegment(seg_idx),
//! ));
//!
//! // Open an info-only dialog (no confirm action needed):
//! self.confirm_modal = Some(ConfirmModal::info(
//!     "Error",
//!     "Something went wrong.",
//! ));
//! ```

/// The action to execute when the user presses Confirm.
///
/// Add variants here for any new confirmable operation.
#[derive(Debug, Clone)]
pub enum ConfirmedAction {
    /// Remove the chat segment at this index (and its paired segment if any).
    RemoveSegment(usize),
}

/// A generic centred modal dialog with a title, a message, and two buttons.
///
/// Key bindings (handled by `App::handle_confirm_modal_key`):
/// - `←` / `→` or `Tab`: move focus between **Confirm** and **Cancel**
/// - `Enter`: activate the focused button
/// - `Esc`: cancel (same as activating Cancel)
///
/// Mouse clicks on the button zones are handled by the click handler in
/// `app/term_events.rs`.
pub struct ConfirmModal {
    /// Title shown in the modal's border.
    pub title: String,
    /// One-or-two-line message shown in the modal body.
    pub message: String,
    /// Label for the destructive / affirmative button (left-most).
    pub confirm_label: String,
    /// Label for the dismissive button (right-most).
    pub cancel_label: String,
    /// Which button currently has keyboard focus: 0 = confirm, 1 = cancel.
    pub focused_button: usize,
    /// The action to execute when Confirm is activated.
    /// `None` makes this an info-only dialog (only Cancel / Enter closes it).
    pub action: Option<ConfirmedAction>,
}

impl ConfirmModal {
    /// Create a confirmation dialog with a destructive action.
    pub fn new(
        title: impl Into<String>,
        message: impl Into<String>,
        action: ConfirmedAction,
    ) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            confirm_label: " Confirm ".into(),
            cancel_label: " Cancel ".into(),
            focused_button: 1, // default focus on Cancel (safer)
            action: Some(action),
        }
    }

    /// Create an info-only dialog that has no confirm action.
    /// Pressing Enter or Esc both dismiss it.
    #[allow(dead_code)]
    pub fn info(title: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            confirm_label: String::new(),
            cancel_label: " OK ".into(),
            focused_button: 1,
            action: None,
        }
    }

    /// Customise the button labels (builder-style).
    #[allow(dead_code)]
    pub fn labels(mut self, confirm: impl Into<String>, cancel: impl Into<String>) -> Self {
        self.confirm_label = confirm.into();
        self.cancel_label = cancel.into();
        self
    }

    /// True when the modal has a real confirm action (not info-only).
    pub fn has_action(&self) -> bool {
        self.action.is_some()
    }

    /// Move focus to the previous button (wraps).
    #[allow(dead_code)]
    pub fn focus_prev(&mut self) {
        if self.has_action() {
            self.focused_button = if self.focused_button == 0 { 1 } else { 0 };
        }
    }

    /// Move focus to the next button (wraps).
    pub fn focus_next(&mut self) {
        if self.has_action() {
            self.focused_button = if self.focused_button == 0 { 1 } else { 0 };
        }
    }
}
