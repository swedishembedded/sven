// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements solutions for agents a person can stop and
// redirect safely for its clients. If your team needs expertise in
// interruptible, remotely driven agents then you can procure our services by
// sending an email to info@swedishembedded.com.

//! What a stop reaches, and a turn whose ending never came.
//!
//! A server names the answer the person stopped. The host stops only that
//! one: the turn running it, a message steered into a turn that is winding
//! down, or a message waiting to be answered. A stop for anything else (an
//! answer that already ended: a second click, a page that had not caught
//! up) reaches nothing, so it can never stop the turn that runs next.
//!
//! `formal/lean/Sven/HostStops.lean` is a model of [`stop_of`], rule for
//! rule, and proves it: a stop stops the running turn only when it names it,
//! reports only the answer it names, leaves every other answer where it was,
//! and changes nothing for an answer the host no longer has. The protocol
//! around it (the page, the server, late and repeated stops) is the
//! server's `TurnStops.tla`.

use sven_machines::AgentEvent;

use super::SessionActor;
use crate::printable::printable;

/// The kernel's state label when its agent is at rest between turns.
pub(super) const AT_REST: &str = "Idle";

impl SessionActor {
    /// The stop for `target` (`None`: from a server that names no answer,
    /// the turn running).
    pub(super) async fn stop(&mut self, target: Option<String>) {
        let Some(target) = target else {
            return self.stop_running().await;
        };
        let running = if self.working {
            self.turn.get().answer_id
        } else {
            None
        };
        let reach = stop_of(
            &target,
            running.as_deref(),
            self.answers.stopping(),
            self.steering.iter().map(|steer| steer.answer_id.as_deref()),
            self.answers
                .held_ids()
                .chain(self.queued.iter().map(|queued| queued.answer_id.as_deref())),
        );
        match reach {
            StopOf::Turn => self.stop_running().await,
            StopOf::Steer => {
                self.steering
                    .retain(|steer| steer.answer_id.as_deref() != Some(&target));
                self.dropped(target).await;
            }
            StopOf::Waiting => {
                if !self.answers.drop_held(&target) {
                    self.queued
                        .retain(|queued| queued.answer_id.as_deref() != Some(&target));
                }
                self.dropped(target).await;
            }
            StopOf::Nothing => {
                println!(
                    "[{}]   no answer {} to stop",
                    self.short(),
                    printable(&target)
                );
            }
        }
    }

    async fn stop_running(&mut self) {
        if self.working {
            self.answers.stop();
        }
        if let Some(controller) = &self.controller {
            controller.cancel().await;
        }
    }

    /// A message the person stopped before its turn: never answered, and
    /// reported stopped.
    async fn dropped(&mut self, answer: String) {
        println!(
            "[{}]   answer {} stopped before it began",
            self.short(),
            printable(&answer)
        );
        self.upload(
            AgentEvent::Aborted {
                partial_text: String::new(),
            },
            Some(answer),
        )
        .await;
    }

    /// The agent has rested past the hold limit and the running turn's ending
    /// never came (a fault that dropped it): the host ends the turn itself,
    /// as stopped when it was being stopped, and takes what waits. Without
    /// it every later message waits for good (`TurnStops.tla`, `GiveUpRests`).
    pub(super) async fn end_unheard(&mut self) {
        self.rest_since = None;
        if !self.working {
            return;
        }
        tracing::warn!(
            "session {}: its agent came to rest but the turn's ending never came; ending it",
            self.id
        );
        let ending = if self.answers.stopping() {
            AgentEvent::Aborted {
                partial_text: String::new(),
            }
        } else {
            AgentEvent::TurnComplete
        };
        self.forward(ending).await;
    }
}

/// Which of the session's answers a stop reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StopOf {
    /// The turn running, which is not yet being stopped.
    Turn,
    /// A message steered into the running turn while it winds down after a
    /// stop: it will never be taken now.
    Steer,
    /// A message waiting to be answered after the running turn.
    Waiting,
    /// Nothing the host has: the answer ended, or was never this host's.
    Nothing,
}

/// What the stop for `target` reaches, given the running turn's answer, whether
/// that turn is already being stopped, and the answers steering it and
/// waiting behind it. The order of the rules is `HostStops.lean`'s `stopOf`.
pub(super) fn stop_of<'a>(
    target: &str,
    running: Option<&str>,
    stopping: bool,
    mut steering: impl Iterator<Item = Option<&'a str>>,
    mut waiting: impl Iterator<Item = Option<&'a str>>,
) -> StopOf {
    if running == Some(target) && !stopping {
        StopOf::Turn
    } else if stopping && steering.any(|id| id == Some(target)) {
        StopOf::Steer
    } else if waiting.any(|id| id == Some(target)) {
        StopOf::Waiting
    } else {
        StopOf::Nothing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(
        target: &str,
        running: Option<&str>,
        stopping: bool,
        steering: &[&str],
        waiting: &[&str],
    ) -> StopOf {
        stop_of(
            target,
            running,
            stopping,
            steering.iter().map(|id| Some(*id)),
            waiting.iter().map(|id| Some(*id)),
        )
    }

    /// Spec: the stop names what it stops, and nothing else is reached.
    #[test]
    fn a_stop_reaches_only_the_answer_it_names() {
        assert_eq!(of("a", Some("a"), false, &[], &[]), StopOf::Turn);
        // The running turn is someone else's answer: an ended one's stop
        // reaches nothing, however late it comes.
        assert_eq!(of("a", Some("b"), false, &[], &[]), StopOf::Nothing);
        assert_eq!(of("a", None, false, &[], &[]), StopOf::Nothing);
        // A turn already being stopped is not stopped twice.
        assert_eq!(of("a", Some("a"), true, &[], &[]), StopOf::Nothing);
        assert_eq!(of("c", Some("a"), false, &[], &["b", "c"]), StopOf::Waiting);
        // A steering message is dropped only from a turn winding down: one a
        // running turn may still take stays.
        assert_eq!(of("s", Some("a"), true, &["s"], &[]), StopOf::Steer);
        assert_eq!(of("s", Some("a"), false, &["s"], &[]), StopOf::Nothing);
    }
}
