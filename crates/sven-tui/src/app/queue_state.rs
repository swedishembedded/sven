// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Message queue state.

use std::collections::VecDeque;

use crate::app::QueuedMessage;

/// State for the pending-message queue and its selection.
#[derive(Clone)]
pub(crate) struct QueueState {
    /// Messages waiting to be sent to the agent.
    pub messages: VecDeque<QueuedMessage>,
    /// Keyboard-selected row in the queue panel.
    pub selected: Option<usize>,
    /// After an abort, new messages are queued rather than auto-sent until the
    /// user manually submits.
    pub abort_pending: bool,
}

impl QueueState {
    pub fn new() -> Self {
        Self {
            messages: VecDeque::new(),
            selected: None,
            abort_pending: false,
        }
    }
}
