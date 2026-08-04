// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Message queue state shared by all frontends.
//!
//! When the agent is busy, user messages are queued here and dispatched one
//! at a time as turns complete.  Both the TUI and GUI use this state to
//! implement the same queuing semantics.

use std::collections::VecDeque;

use crate::types::QueuedMessage;

/// State for the pending-message queue and its selection.
#[derive(Clone, Default)]
pub struct QueueState {
    /// Messages waiting to be sent to the agent.
    pub messages: VecDeque<QueuedMessage>,
    /// Keyboard-selected row in the queue panel (GUI/TUI).
    pub selected: Option<usize>,
    /// After an abort, new messages are queued rather than auto-sent until
    /// the user manually submits.
    pub abort_pending: bool,
}

impl QueueState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a message to the back of the queue.
    pub fn push(&mut self, msg: QueuedMessage) {
        self.messages.push_back(msg);
    }

    /// Pop the next message from the front of the queue.
    pub fn pop_front(&mut self) -> Option<QueuedMessage> {
        self.messages.pop_front()
    }

    /// Number of queued messages.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// True when there are no queued messages.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Remove the message at `index` if it exists.
    pub fn remove(&mut self, index: usize) -> Option<QueuedMessage> {
        self.messages.remove(index)
    }
}
