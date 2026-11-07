// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Message queue state shared by all frontends.
//!
//! When the agent is busy, user messages are queued here; when its turn
//! ends, everything waiting is sent together as the next turn
//! ([`QueueState::take_turn`]). Frontends use this state so they all
//! implement the same queuing semantics.

use std::collections::VecDeque;

use crate::types::QueuedMessage;

/// State for the pending-message queue and its selection.
#[derive(Clone, Default)]
pub struct QueueState {
    /// Messages waiting to be sent to the agent.
    pub messages: VecDeque<QueuedMessage>,
    /// Keyboard-selected row in the queue panel.
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

    /// The next turn: every message waiting, as one message in the order
    /// they were queued. A message that changes the model or the mode starts
    /// a turn of its own, so its change applies from it on and never to the
    /// messages queued before it. The selection follows the rows left.
    pub fn take_turn(&mut self) -> Option<QueuedMessage> {
        let mut turn = self.messages.pop_front()?;
        let mut taken = 1;
        while self
            .messages
            .front()
            .is_some_and(|next| next.model_transition.is_none() && next.mode_transition.is_none())
        {
            let next = self.messages.pop_front().expect("checked above");
            turn.content = join_messages(&turn.content, &next.content);
            taken += 1;
        }
        self.selected = self
            .selected
            .map(|selected| selected.saturating_sub(taken))
            .filter(|_| !self.messages.is_empty());
        Some(turn)
    }
}

/// `next` said after `first`, as one message.
pub fn join_messages(first: &str, next: &str) -> String {
    match (first.trim().is_empty(), next.trim().is_empty()) {
        (true, _) => next.to_string(),
        (_, true) => first.to_string(),
        _ => format!("{first}\n\n{next}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_vocab::AgentMode;

    fn plain(text: &str) -> QueuedMessage {
        QueuedMessage::plain(text.into())
    }

    /// Everything queued while the agent worked is its next turn, as one
    /// message, in order.
    #[test]
    fn the_queue_is_delivered_together_as_one_turn() {
        let mut queue = QueueState::new();
        queue.push(plain("fix the parser"));
        queue.push(plain("and keep the tests"));
        queue.push(plain("then commit"));
        queue.selected = Some(2);
        let turn = queue.take_turn().unwrap();
        assert_eq!(
            turn.content,
            "fix the parser\n\nand keep the tests\n\nthen commit"
        );
        assert!(queue.is_empty());
        assert_eq!(queue.selected, None);
        assert!(queue.take_turn().is_none());
    }

    /// A message that switches the mode starts its own turn: the switch is
    /// never applied to what was queued before it.
    #[test]
    fn a_message_that_switches_mode_starts_a_turn_of_its_own() {
        let mut queue = QueueState::new();
        queue.push(plain("look around"));
        queue.push(QueuedMessage {
            mode_transition: Some(AgentMode::Plan),
            ..plain("now plan it")
        });
        queue.push(plain("in three steps"));
        assert_eq!(queue.take_turn().unwrap().content, "look around");
        let planned = queue.take_turn().unwrap();
        assert_eq!(planned.content, "now plan it\n\nin three steps");
        assert_eq!(planned.mode_transition, Some(AgentMode::Plan));
    }
}
