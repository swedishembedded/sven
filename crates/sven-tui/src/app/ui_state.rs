// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! UI overlay and focus state.

use crate::{
    chat::search::SearchState,
    overlay::{completion::CompletionOverlay, confirm::ConfirmModal, question::QuestionModal},
    pager::PagerOverlay,
};

/// Which pane currently holds keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPane {
    Chat,
    Input,
    /// The compact queue panel shown above the input when there are pending messages.
    Queue,
}

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
}

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
        }
    }
}
