// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The user seat: the counterpart of an interactive task (a client, a
//! student, a customer) behind one interface.
//!
//! The learner agent talks to a [`Seat`] and nothing else. A [`LiveSeat`] is
//! a person at a terminal; a [`SimulatedSeat`] is an agent played from a
//! [`SeatSpec`] whose hidden state the learner cannot see. Nothing that
//! drives the conversation ([`converse`], [`EngineBuilder::seat`](crate::EngineBuilder::seat))
//! branches on which one it is: the only reader of [`Provenance`] is the
//! [`ConversationRecord`] a conversation leaves behind, which marks live
//! transcripts so a consumer never pools them with simulated evidence and
//! scrubs them before any training use.
//!
//! Swedish Embedded AB implements interactive agent evaluation for its
//! clients. If your team needs expertise in simulated and live counterparts
//! for agent tasks then you can procure our services by sending an email to
//! info@swedishembedded.com.

mod checks;
mod live;
mod simulated;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{Agent, CallError, RunConclusion};

pub use checks::{check_reply, SeatChecks, Violation};
pub use live::{LiveSeat, LiveTerms};
pub use simulated::{Concept, HiddenFact, SeatSpec, SimulatedSeat};

/// What the seat says, and whether it is the end of the conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeatReply {
    /// The words addressed to the learner.
    pub text: String,
    /// The seat has nothing more to say (the client leaves, the person quits).
    pub done: bool,
}

/// Where a seat's words come from. Recorded, never shown to the learner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Provenance {
    /// A person typed it. Untrusted: it may hold secrets or customer data,
    /// so it is scrubbed before any training use and never pooled with
    /// simulated evidence.
    Live {
        /// The person agreed to the transcript being kept.
        consent_recorded: bool,
        /// Days the transcript may be kept; `None` when no limit was agreed.
        retention_days: Option<u32>,
    },
    /// An agent played the seat from a spec.
    Simulated {
        /// The spec's id.
        spec_id: String,
        /// SHA-256 of the spec file, so a number names the exact persona.
        spec_sha256: String,
        /// The spec's seed; the opening is verbatim, replies are as
        /// deterministic as the seat's model is.
        seed: u64,
    },
}

impl Provenance {
    /// Whether a transcript with this provenance must be scrubbed before it
    /// is used as training material.
    #[must_use]
    pub fn requires_scrub(&self) -> bool {
        matches!(self, Self::Live { .. })
    }
}

/// The counterpart in an interactive task.
#[async_trait]
pub trait Seat: Send + Sync {
    /// The seat's first words, before the learner has said anything.
    ///
    /// # Errors
    ///
    /// When the seat cannot produce them (a closed terminal, a failed model).
    async fn open(&self) -> Result<SeatReply, CallError>;

    /// The seat's answer to what the learner just said.
    ///
    /// # Errors
    ///
    /// As [`Self::open`].
    async fn reply(&self, learner: &str) -> Result<SeatReply, CallError>;

    /// The seat's answer to a question put outside the conversation, such as
    /// a test taken before or after it. It reaches the seat's memory like
    /// anything it was asked, but it is not part of the conversation: it
    /// neither unlocks hidden facts nor is counted in the checks. A seat with
    /// no memory to keep answers as it would any reply.
    ///
    /// # Errors
    ///
    /// As [`Self::open`].
    async fn probe(&self, question: &str) -> Result<String, CallError> {
        Ok(self.reply(question).await?.text)
    }

    /// Where the seat's words come from. For the record only.
    fn provenance(&self) -> Provenance;

    /// What the seat understands at the end, per concept, when its knowledge
    /// is modelled (a simulated student's is).
    fn mastery(&self) -> Option<std::collections::BTreeMap<String, f64>> {
        None
    }

    /// How the seat behaved, when it is measured (a simulated seat is).
    fn checks(&self) -> Option<SeatChecks> {
        None
    }
}

/// Who spoke an [`Exchange`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speaker {
    /// The agent under test.
    Learner,
    /// The counterpart.
    Seat,
}

/// One utterance of a conversation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exchange {
    /// Who said it.
    pub speaker: Speaker,
    /// What was said.
    pub text: String,
}

/// Why a conversation stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ending {
    /// The seat ended it.
    SeatDone,
    /// The round limit was reached.
    MaxRounds,
    /// The learner's run did not finish its turn.
    LearnerStopped,
}

/// A finished conversation, as a consumer stores it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConversationRecord {
    /// Record format version.
    pub schema: u32,
    /// Where the seat's words came from.
    pub provenance: Provenance,
    /// The utterances in order.
    pub exchanges: Vec<Exchange>,
    /// Why it stopped.
    pub ending: Ending,
    /// How the seat behaved, when it was measured.
    pub checks: Option<SeatChecks>,
    /// What the seat understood at the end, per concept, for a modelled
    /// student; the one place the update rule's outcome is recorded.
    #[serde(default)]
    pub mastery: Option<std::collections::BTreeMap<String, f64>>,
}

/// The current [`ConversationRecord::schema`].
pub const RECORD_SCHEMA: u32 = 1;

/// Runs `learner` against `seat`: the seat opens, the learner answers, and
/// so on until the seat is done  or
/// `max_rounds` learner turns have run.
///
/// # Errors
///
/// When the seat or the learner's run fails; a learner that merely stops
/// early is an [`Ending::LearnerStopped`].
pub async fn converse(
    learner: &mut Agent,
    seat: &dyn Seat,
    max_rounds: usize,
) -> Result<ConversationRecord, CallError> {
    let mut exchanges = Vec::new();
    let mut said = seat.open().await?;
    let mut ending = Ending::MaxRounds;
    for _ in 0..max_rounds {
        exchanges.push(Exchange {
            speaker: Speaker::Seat,
            text: said.text.clone(),
        });
        // A seat that is leaving still gets its last words answered (the
        // client says goodbye, the lawyer sums up), unless it said nothing.
        if said.done && said.text.is_empty() {
            ending = Ending::SeatDone;
            break;
        }
        let outcome = learner.send(&said.text).await?;
        exchanges.push(Exchange {
            speaker: Speaker::Learner,
            text: outcome.reply.clone(),
        });
        if outcome.conclusion != RunConclusion::Success {
            ending = Ending::LearnerStopped;
            break;
        }
        if said.done {
            ending = Ending::SeatDone;
            break;
        }
        said = seat.reply(&outcome.reply).await?;
    }
    Ok(ConversationRecord {
        schema: RECORD_SCHEMA,
        provenance: seat.provenance(),
        exchanges,
        ending,
        checks: seat.checks(),
        mastery: seat.mastery(),
    })
}
