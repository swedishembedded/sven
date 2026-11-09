// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements solutions for keeping remote agent sessions
// consistent with the server that shows them for its clients. If your team
// needs expertise in distributed agent hosting then you can procure our
// services by sending an email to info@swedishembedded.com.

//! Which answer each event of a session belongs to.
//!
//! The server shows every input as an answer and names it (`answer_id`).
//! The session answers inputs one after another, each ending with one
//! `TurnComplete` or `Aborted`, so the answers still waiting for their ending
//! form a queue: an event belongs to the oldest. Without that name the server
//! could only write an event into whatever answer it has open, and a stopped
//! answer's late ending would close the answer the person started next.
//!
//! A stop is the one place the order needs help. The server ends a stopped
//! answer at once and may send the next input before the session has wound
//! the stopped turn down; posted then, the session would ignore it (a turn is
//! still running) and its answer would never end. So an input arriving
//! behind a stopped answer is held until that answer's ending comes, or for
//! [`HOLD_LIMIT`] when it never does (the turn ended without one).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How long an input waits behind a stopped answer for its ending.
pub(super) const HOLD_LIMIT: Duration = Duration::from_secs(10);

/// An input the server sent: its text, and the answer it shows it as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Input {
    pub text: String,
    pub answer_id: Option<String>,
    /// Files the person attached ([`super::attachments`]).
    pub attachments: Vec<sven_vocab::ArtifactRef>,
    /// Steers the running turn instead of waiting for it to end.
    pub steer: bool,
}

#[derive(Debug)]
struct Waiting {
    id: Option<String>,
    stopped: bool,
}

/// The answers of one session still waiting for their ending, and the
/// inputs held behind a stopped one.
#[derive(Debug, Default)]
pub(super) struct Answers {
    waiting: VecDeque<Waiting>,
    /// The answer that ended last: what an event after every ending (a goal
    /// paused after its stop) trails.
    trailing: Option<String>,
    held: VecDeque<Input>,
    held_since: Option<Instant>,
}

impl Answers {
    /// The session will end an answer for `id`.
    pub(super) fn expect(&mut self, id: Option<String>) {
        self.waiting.push_back(Waiting { id, stopped: false });
    }

    /// The answer just expected is ended by the host instead (the request
    /// failed before the session saw it).
    pub(super) fn withdraw(&mut self) {
        self.waiting.pop_back();
    }

    /// Whether the session is writing an answer.
    pub(super) fn writing(&self) -> bool {
        !self.waiting.is_empty()
    }

    /// The answer the next event of the session belongs to.
    pub(super) fn current(&self) -> Option<String> {
        match self.waiting.front() {
            Some(waiting) => waiting.id.clone(),
            None => self.trailing.clone(),
        }
    }

    /// The session ended its oldest answer.
    pub(super) fn ended(&mut self) {
        if let Some(waiting) = self.waiting.pop_front() {
            self.trailing = waiting.id;
        }
    }

    /// The person stopped the answer being written.
    pub(super) fn stop(&mut self) {
        if let Some(waiting) = self.waiting.front_mut() {
            waiting.stopped = true;
        }
    }

    /// `input` to answer now, or `None` when it is held behind a stopped
    /// answer.
    pub(super) fn admit(&mut self, input: Input, now: Instant) -> Option<Input> {
        let behind_a_stop = self.waiting.front().is_some_and(|w| w.stopped);
        if !behind_a_stop && self.held.is_empty() {
            return Some(input);
        }
        self.held_since.get_or_insert(now);
        self.held.push_back(input);
        None
    }

    /// When the held inputs stop waiting for the stopped answer's ending.
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.held_since.map(|since| since + HOLD_LIMIT)
    }

    /// The stopped answer's ending is not coming: the stopped answers are
    /// taken as ended, so the held inputs go on.
    pub(super) fn give_up(&mut self) {
        while self.waiting.front().is_some_and(|w| w.stopped) {
            self.ended();
        }
    }

    /// The session let its turns go (the conversation was replaced): no
    /// ending of theirs is coming.
    pub(super) fn forget(&mut self) {
        while !self.waiting.is_empty() {
            self.ended();
        }
    }

    /// The next held input, once nothing stopped is in front of it.
    pub(super) fn release(&mut self) -> Option<Input> {
        if self.waiting.front().is_some_and(|w| w.stopped) {
            return None;
        }
        let input = self.held.pop_front();
        if self.held.is_empty() {
            self.held_since = None;
        }
        input
    }
}

/// Waits until `deadline`; forever when there is none.
pub(super) async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(id: &str) -> Input {
        Input {
            text: format!("text of {id}"),
            answer_id: Some(id.into()),
            attachments: Vec::new(),
            steer: false,
        }
    }

    fn id(id: &str) -> Option<String> {
        Some(id.into())
    }

    #[test]
    fn events_belong_to_the_oldest_answer_and_then_trail_it() {
        let mut answers = Answers::default();
        answers.expect(id("a"));
        answers.expect(id("b"));
        assert_eq!(answers.current(), id("a"));
        answers.ended();
        assert_eq!(answers.current(), id("b"));
        answers.ended();
        // A word after every ending (a paused goal) trails the last answer,
        // never an answer started later.
        assert_eq!(answers.current(), id("b"));
    }

    #[test]
    fn a_stopped_answers_late_ending_never_reaches_the_next_answer() {
        let now = Instant::now();
        let mut answers = Answers::default();
        answers.expect(id("a"));
        answers.stop();
        // The next input waits for the stopped answer to end.
        assert_eq!(answers.admit(input("b"), now), None);
        assert_eq!(answers.release(), None);
        assert_eq!(
            answers.current(),
            id("a"),
            "the late ending is the stopped answer's"
        );
        answers.ended();
        assert_eq!(answers.release(), Some(input("b")));
        assert_eq!(answers.release(), None);
        assert_eq!(answers.deadline(), None);
    }

    #[test]
    fn held_inputs_go_on_when_the_stopped_ending_never_comes() {
        let now = Instant::now();
        let mut answers = Answers::default();
        answers.expect(id("a"));
        answers.stop();
        assert_eq!(answers.admit(input("b"), now), None);
        assert_eq!(
            answers.admit(input("c"), now + Duration::from_secs(1)),
            None
        );
        assert_eq!(answers.deadline(), Some(now + HOLD_LIMIT));
        answers.give_up();
        assert_eq!(answers.release(), Some(input("b")));
        assert_eq!(answers.release(), Some(input("c")));
        assert_eq!(answers.deadline(), None);
    }

    #[test]
    fn an_input_behind_held_ones_keeps_their_order() {
        let now = Instant::now();
        let mut answers = Answers::default();
        answers.expect(id("a"));
        answers.stop();
        assert_eq!(answers.admit(input("b"), now), None);
        answers.ended();
        // Not yet released: a new input must not overtake the held one.
        assert_eq!(answers.admit(input("c"), now), None);
        assert_eq!(answers.release(), Some(input("b")));
        assert_eq!(answers.release(), Some(input("c")));
    }

    #[test]
    fn a_withdrawn_answer_leaves_the_queue_as_it_was() {
        let mut answers = Answers::default();
        answers.expect(id("a"));
        answers.expect(id("b"));
        answers.withdraw();
        assert_eq!(answers.current(), id("a"));
        answers.forget();
        assert_eq!(answers.current(), id("a"));
        assert_eq!(answers.admit(input("c"), Instant::now()), Some(input("c")));
    }
}
