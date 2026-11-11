// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements solutions for agents a person can redirect
// while they work for its clients. If your team needs expertise in
// interruptible, remotely driven agents then you can procure our services by
// sending an email to info@swedishembedded.com.

//! What happens to messages a person sends while a turn runs: handed to the
//! turn as steering, answered together when the turn takes them, sent as
//! the next message when it ends without them, or queued for after it.

use sven_machines::AgentEvent;

use super::{attachments, SessionActor};

impl SessionActor {
    /// Hands `input` to the running turn, to take at its next safe point.
    /// False when there is no live session to take it.
    pub(super) async fn steer(&self, input: &attachments::Attached) -> bool {
        let Some(controller) = &self.controller else {
            return false;
        };
        let paths = input
            .image_paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        controller.steer(input.text.clone(), paths).await
    }

    /// The turn took every steering message sent so far: the answer it was
    /// writing ends here, and from now on it writes the last steering
    /// message's answer. Ones sent before that are answered together with
    /// it, so theirs end empty.
    pub(super) async fn steered(&mut self) {
        let Some(last) = self.steering.pop_back() else {
            return;
        };
        let ended = self.answers.current();
        self.upload(AgentEvent::TurnComplete, ended).await;
        self.answers.ended();
        for earlier in std::mem::take(&mut self.steering) {
            self.upload(AgentEvent::TurnComplete, earlier.answer_id)
                .await;
        }
        // The turn goes on as the last steering message's answer, of the
        // same authority (only such a message steers it).
        self.start_turn(last.answer_id.clone(), last.ask_every_tool);
        self.answers.expect(last.answer_id);
    }

    /// The turn ended without taking the steering messages sent to it: they
    /// are the next message, as one, answered as the last one's answer. After
    /// a stop they are not: the person stopped the work, so theirs end.
    pub(super) async fn steer_left_over(&mut self, stopped: bool) {
        let Some(last) = self.steering.pop_back() else {
            return;
        };
        let mut earlier = std::mem::take(&mut self.steering);
        if stopped {
            earlier.push_back(last);
            for steer in earlier {
                self.upload(AgentEvent::TurnComplete, steer.answer_id).await;
            }
            return;
        }
        let mut input = attachments::Attached::default();
        for steer in earlier {
            self.upload(AgentEvent::TurnComplete, steer.answer_id).await;
            input.join(steer.input);
        }
        input.join(last.input);
        self.asking = last.answer_id;
        self.expecting = false;
        // Only messages of one authority steer one turn, so the joined ones
        // are of `last`'s.
        self.start_turn(self.asking.clone(), last.ask_every_tool);
        if let Err(e) = self.submit(input).await {
            self.fail(&format!("{e:#}")).await;
        }
        self.asking = None;
    }

    /// The next message sent while a turn ran, now that it ended: answered
    /// as its own answer. The rest wait for the turn this one starts.
    pub(super) async fn answer_queued(&mut self) {
        if self.working {
            return;
        }
        let Some(next) = self.queued.pop_front() else {
            return;
        };
        self.asking = next.answer_id;
        self.expecting = false;
        self.start_turn(self.asking.clone(), next.ask_every_tool);
        if let Err(e) = self.submit(next.input).await {
            self.fail(&format!("{e:#}")).await;
        }
        self.asking = None;
    }

    /// The inputs held behind a stopped answer, once it ended.
    pub(super) async fn release_held(&mut self) {
        while let Some(input) = self.answers.release() {
            self.take(input).await;
        }
    }
}
